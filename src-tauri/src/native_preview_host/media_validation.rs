//! Controlled real-media integration into a hidden Tauri preview child.
use super::{
    audio_session_driver::{oracle, save_capture},
    audio_timeline::AudioTimeline,
    audio_timing_driver::{
        identity, json_file, local, need, number, origin, text, validate_audio_metadata,
    },
    media_actor::{MediaActor, Sink},
    media_sink::MainThreadSink,
    platform,
    streaming_decoder::{SeekMode, StreamConfig},
    validation,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tauri::AppHandle;
static REPORT: Mutex<Option<(PathBuf, Value)>> = Mutex::new(None);
pub(super) fn record_closed(cleanup: Value) {
    if let Ok(mut report) = REPORT.lock() {
        if let Some((path, mut value)) = report.take() {
            if !cleanup["error"].is_null() {
                value["ok"] = false.into();
            }
            value["windowCleanup"] = cleanup;
            if let Err(error) = validation::save(&path, &value) {
                eprintln!("Media evidence save failed: {error}; {value}");
            }
        }
    }
}
fn wait_time(actor: &MediaActor, target: u64) -> Result<Value, String> {
    let start = Instant::now();
    loop {
        let state = actor.status();
        if !state["error"].is_null() {
            return Err(format!("Media worker failed: {state}"));
        }
        if state["projectTimeNs"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|v| v >= target)
        {
            return Ok(state);
        }
        need(
            start.elapsed() < Duration::from_secs(5),
            "Media clock did not reach target",
        )?;
        thread::sleep(Duration::from_millis(5));
    }
}
fn barrier(app: &AppHandle) -> Result<(), String> {
    validation::main_call(app, |_| Ok(Value::Null))?;
    Ok(())
}
fn store_bytes(path: &Path, bytes: &[u8]) -> Result<Value, String> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    identity(path)
}

fn run(app: &AppHandle, run: &Path, fixture: &Path, report: &mut Value) -> Result<(), String> {
    let manifest_path = fixture.join("audio-timing-fixtures.json");
    let manifest = json_file(&manifest_path)?;
    let a = manifest["audioCases"]
        .as_array()
        .ok_or("Missing audio cases")?
        .iter()
        .find(|v| v["label"] == "aac-offset-aperiodic")
        .ok_or("Missing AAC fixture")?;
    let v = manifest["videoCases"]
        .as_array()
        .ok_or("Missing video cases")?
        .iter()
        .find(|v| v["label"] == "vfr-zero-origin")
        .ok_or("Missing VFR fixture")?;
    let audio = local(fixture, text(a, "source")?)?;
    let video = local(fixture, text(v, "video")?)?;
    let pcm = local(fixture, text(a, "referenceWav")?)?;
    let video_ref = local(fixture, text(v, "reference")?)?;
    let fingerprints = json!({"manifest":identity(&manifest_path)?,"audio":identity(&audio)?,"video":identity(&video)?,"pcm":identity(&pcm)?,"videoReference":identity(&video_ref)?});
    need(
        fingerprints["audio"]["sha256"] == a["sourceSha256"]
            && fingerprints["video"]["sha256"] == v["sourceSha256"]
            && fingerprints["pcm"]["sha256"] == a["referenceSha256"],
        "Media fixture identity mismatch",
    )?;
    let ao = origin(a, true)?;
    let vo = origin(v, false)?;
    let total = number(a, "decodedFrames")?;
    validate_audio_metadata(
        &json_file(&local(fixture, text(a, "metadata")?)?)?,
        ao,
        total,
    )?;
    let timeline = AudioTimeline::new(ao, vo, 48000, total)?;
    report["fixture"] = fingerprints.clone();
    report["timeline"] = json!(timeline.diagnostics());
    let initial = validation::wait_for(app, |s| !s["child"].is_null() && s["error"].is_null())?;
    report["initialHost"] = validation::snapshot(app, "initial-host", initial.clone())?;
    let sink = MainThreadSink::new(
        app.clone(),
        text(&initial, "sessionId")?.to_owned(),
        text(&initial["child"], "surfaceId")?.to_owned(),
    );
    let bind = Arc::clone(&sink);
    validation::main_call(app, move |w| platform::bind_media(&w, bind))?;
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
    let config = StreamConfig {
        ffmpeg: bin.join("ffmpeg-x86_64-pc-windows-msvc.exe"),
        ffprobe: bin.join("ffprobe-x86_64-pc-windows-msvc.exe"),
        video: video.clone(),
        audio: audio.clone(),
        sample_rate: 48000,
        channels: 2,
        video_width: 320,
        video_height: 180,
        seek_mode: SeekMode::IndexedPts,
        epoch: 1,
        start_audio_frame: 0,
        start_video_ns: 0,
    };
    let actor_sink: Arc<dyn Sink> = sink.clone();
    let actor = MediaActor::start(config, timeline.clone(), 2_200_000_000, actor_sink)?;
    let mut controls = Vec::new();
    let work = (|| {
        controls.push(json!({"kind":"prepared","targetNs":"2200000000","state":actor.prepare()?}));
        actor.play()?;
        wait_time(&actor, 2_500_000_000)?;
        let paused = actor.pause()?;
        barrier(app)?;
        let held = actor.observe()?;
        thread::sleep(Duration::from_millis(160));
        let frozen = actor.observe()?;
        need(
            held["lastClock"]["position"] == frozen["lastClock"]["position"]
                && held["lastClock"]["paddingFrames"] == frozen["lastClock"]["paddingFrames"],
            "Paused device clock advanced",
        )?;
        controls.push(json!({"kind":"pause","state":paused,"held":held,"afterWait":frozen,"milliseconds":160}));
        let before = validation::status(app)?;
        let rev = number(&before, "revision")?;
        validation::eval(
            app,
            "document.querySelector('.preview-canvas').style.width='300px'",
        )?;
        let resized = validation::wait_for(app, |s| {
            s["revision"].as_u64().unwrap_or(0) > rev
                && s["lastRequest"]["width"].as_f64().unwrap_or(99999.0) <= 300.0
                && s["error"].is_null()
        })?;
        controls.push(json!({"kind":"resizeWhilePaused","host":validation::snapshot(app,"resized-paused",resized)?,"audio":actor.observe()?}));
        controls.push(json!({"kind":"resumedPrepared","state":actor.prepare()?}));
        actor.play()?;
        wait_time(&actor, 2_800_000_000)?;
        actor.pause()?;
        barrier(app)?;
        for (index, target) in [7_123_000_000u64, 0, 9_500_000_000].into_iter().enumerate() {
            let seek = actor.seek(target)?;
            barrier(app)?;
            let prepared = actor.prepare()?;
            controls.push(json!({"kind":"seekPrepared","targetNs":target.to_string(),"seek":seek,"prepared":prepared}));
            actor.play()?;
            if index == 0 {
                wait_time(&actor, target + 150_000_000)?;
                let rev = number(&validation::status(app)?, "revision")?;
                validation::eval(
                    app,
                    "(()=>{const d=document.createElement('div');d.id='native-media-overlay';d.setAttribute('role','dialog');d.setAttribute('data-native-preview-blocker','true');d.style.cssText='position:fixed;inset:100px;';document.body.append(d)})()",
                )?;
                let host = validation::wait_for(app, |s| {
                    s["revision"].as_u64().unwrap_or(0) > rev && s["lastRequest"]["blocked"] == true
                })?;
                barrier(app)?;
                let before = sink.report()?;
                let clock_before = actor.observe()?;
                let now = text(&clock_before, "projectTimeNs")?
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?;
                let clock_after = wait_time(&actor, now + 140_000_000)?;
                let after = sink.report()?;
                need(
                    before["presentCalls"] == after["presentCalls"],
                    "Overlay allowed native video Present",
                )?;
                controls.push(json!({"kind":"overlay","host":validation::snapshot(app,"overlay",host)?,"audioBefore":clock_before,"audioAfter":clock_after,"sinkBefore":before,"sinkAfter":after}));
                validation::eval(
                    app,
                    "document.querySelector('#native-media-overlay').remove()",
                )?;
                let restored = validation::wait_for(app, |s| {
                    s["lastRequest"]["blocked"] == false && s["error"].is_null()
                })?;
                controls.push(json!({"kind":"overlayRestored","host":validation::snapshot(app,"overlay-restored",restored)?}));
                wait_time(&actor, target + 600_000_000)?;
            } else {
                wait_time(&actor, target + 350_000_000)?;
            }
            if index < 2 {
                actor.pause()?;
                barrier(app)?;
            }
        }
        let cancelled = actor.cancel()?;
        barrier(app)?;
        let before = sink.report()?;
        let stale = sink.submit(super::media_actor::MediaFrame {
            epoch: 1,
            source_epoch: 1,
            index: 0,
            pts_ns: 0,
            project_ns: 0,
            width: 320,
            height: 180,
            rgb: Arc::from(vec![0; 320 * 180 * 3]),
            diagnostic: false,
        })?;
        need(
            stale == super::media_actor::Submission::Stale,
            "Retired media generation was admitted",
        )?;
        let held = actor.observe()?;
        thread::sleep(Duration::from_millis(120));
        let frozen = actor.observe()?;
        barrier(app)?;
        let after = sink.report()?;
        need(
            before["presentCalls"] == after["presentCalls"]
                && held["lastClock"]["position"] == "0"
                && frozen["lastClock"]["position"] == "0"
                && frozen["lastClock"]["paddingFrames"] == 0,
            "Cancelled media continued output",
        )?;
        controls.push(json!({"kind":"cancelled","staleSubmission":stale,"state":cancelled,"held":held,"afterWait":frozen,"milliseconds":120,"sinkBefore":before,"sinkAfter":after}));
        Ok::<(), String>(())
    })();
    report["controls"] = json!(controls);
    if let Err(error) = &work {
        report["operationError"] = error.clone().into();
    }
    let finished = actor.finish();
    barrier(app)?;
    sink.close();
    report["sink"] = sink.report()?;
    let finished = finished?;
    report["actor"] = finished.json;
    for (index, (target, samples)) in finished.captures.into_iter().enumerate() {
        let expected = oracle(&timeline, target, (samples.len() / 2) as u64, &pcm)?;
        let exact = expected == samples;
        report["actor"]["segments"][index]["capture"] =
            save_capture(run, &format!("accepted-{index}.f32le"), &samples)?;
        report["actor"]["segments"][index]["independentPcmExact"] = exact.into();
        need(exact, "Accepted media PCM differs from source reference")?;
    }
    let mut diagnostics = Vec::new();
    for (index, frame) in sink.take_diagnostics()?.into_iter().enumerate() {
        diagnostics.push(json!({"evidence":frame.evidence,"sourceRgb":store_bytes(&run.join(format!("source-{index}.rgb")),&frame.source)?,"actualRgba":store_bytes(&run.join(format!("target-{index}.rgba")),&frame.rgba)?}));
    }
    report["diagnostics"] = json!(diagnostics);
    report["finalHost"] = validation::status(app)?;
    work?;
    need(
        report["actor"]["ok"] == true,
        "Media actor failed or cleanup incomplete",
    )?;
    need(report["sink"]["error"].is_null(), "Media sink failed")?;
    need(
        fingerprints
            == json!({"manifest":identity(&manifest_path)?,"audio":identity(&audio)?,"video":identity(&video)?,"pcm":identity(&pcm)?,"videoReference":identity(&video_ref)?}),
        "Source fixture changed during native media playback",
    )?;
    Ok(())
}
pub(super) fn start(app: &AppHandle) -> Result<bool, String> {
    let args: Vec<_> = std::env::args_os().collect();
    let Some(i) = args.iter().position(|v| v == "--native-media-check-dir") else {
        return Ok(false);
    };
    need(
        cfg!(debug_assertions)
            && ["--mcp", "--mcp-hidden", "--mcp-validation"]
                .iter()
                .all(|flag| args.iter().any(|v| v == flag)),
        "Media check requires isolated hidden debug mode",
    )?;
    let output = PathBuf::from(args.get(i + 1).ok_or("Missing media output")?);
    let fixture_i = args
        .iter()
        .position(|v| v == "--native-media-fixture-dir")
        .ok_or("Missing media fixture directory")?;
    let fixture = PathBuf::from(args.get(fixture_i + 1).ok_or("Missing fixture value")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let lexical = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    need(
        output.is_absolute()
            && output.starts_with(&lexical)
            && !output
                .components()
                .any(|v| matches!(v, std::path::Component::ParentDir)),
        "Media evidence must remain inside target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    need(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Media output escaped target",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let run_dir = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run_dir).map_err(|e| e.to_string())?;
    let app = app.clone();
    thread::spawn(move || {
        let mut report = json!({"schemaVersion":1,"createdAt":chrono::Utc::now().to_rfc3339(),"processId":std::process::id(),"scope":{"actualTauriChild":true,"decodedMedia":true,"ownedMutedAudio":true,"hiddenParent":true,"screenDisplayed":false,"defaultPlayerChanged":false,"userTransportIntegrated":false},"ok":false});
        let work = run(&app, &run_dir, &fixture, &mut report);
        report["ok"] = work.is_ok().into();
        if let Err(error) = work {
            report["error"] = error.into();
        }
        *REPORT.lock().unwrap() =
            Some((run_dir.join("native-tauri-media-validation.json"), report));
        if let Err(error) = validation::main_call(&app, |w| {
            w.destroy().map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }) {
            record_closed(json!({"error":error}));
            app.exit(1);
        }
    });
    Ok(true)
}
