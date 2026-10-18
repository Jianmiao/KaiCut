//! Continuous PCM transport validation, with an explicit muted device case.
use super::{
    audio_scheduling::ThreadScheduling,
    audio_timeline::AudioTimeline,
    audio_timing_driver::{
        identity, json_file, local, need, number, origin, sample_hash, text,
        validate_audio_metadata,
    },
    audio_transfer::AudioTransfer,
    stream_reference::reference_pcm_samples,
    streaming_decoder::{SeekMode, StreamConfig},
    wasapi_output::{WaitOutcome, WasapiOutput},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const FRAMES: u64 = 13 * 48000;
const REQUESTS: [u32; 5] = [1, 479, 997, 4093, 4800];
const ACCEPT: [u32; 6] = [0, 1, 17, 128, 511, 4096];
fn expected(
    timeline: &AudioTimeline,
    target: u64,
    frames: u64,
    reference: &Path,
) -> Result<Vec<f32>, String> {
    let diag = timeline.diagnostics();
    let first = diag
        .first_source_timeline_ns
        .parse::<i128>()
        .map_err(|e| e.to_string())?;
    let leading = if first > i128::from(target) {
        u64::try_from(((first - i128::from(target)) * 48000 + 999_999_999) / 1_000_000_000)
            .map_err(|e| e.to_string())?
            .min(frames)
    } else {
        0
    };
    let start = timeline.span(target, 0)?.source_start_frame;
    let available = diag.total_frames - start;
    let source = available.min(frames - leading);
    let mut result = vec![0.0; leading as usize * 2];
    let mut read = 0;
    while read < source {
        let count = (source - read).min(4800) as u32;
        result.extend(reference_pcm_samples(
            reference,
            start + read,
            count,
            48000,
            2,
        )?);
        read += u64::from(count);
    }
    result.resize(frames as usize * 2, 0.0);
    Ok(result)
}
fn capture(run: &Path, name: &str, samples: &[f32]) -> Result<Value, String> {
    need(
        samples.len() <= FRAMES as usize * 2,
        "Capture exceeds 13-second byte limit",
    )?;
    let path = run.join(name);
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(
        json!({"path":path,"frames":samples.len()/2,"bytes":bytes.len(),"sha256":sample_hash(samples)}),
    )
}

fn offline_segment(
    transfer: &mut AudioTransfer,
    frames: u64,
    report: &mut Value,
) -> Result<Vec<f32>, String> {
    let started = Instant::now();
    let mut samples = Vec::with_capacity(frames as usize * 2);
    let mut commits = Vec::new();
    let mut requests = 0usize;
    let mut accepts = 0usize;
    let mut paused = false;
    let mut replay = false;
    while transfer.snapshot().output_frames < frames {
        need(
            started.elapsed() < Duration::from_secs(30),
            "Continuous offline segment timed out",
        )?;
        let left = frames - transfer.snapshot().output_frames;
        let request = REQUESTS[requests % REQUESTS.len()].min(left as u32);
        if !transfer.prepare(request)? {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        if transfer.snapshot().output_frames > 100 && !paused {
            let before = transfer.snapshot();
            let stopped = transfer.pause()?;
            need(
                transfer.paused_commit_rejected(),
                "Paused transport accepted PCM",
            )?;
            thread::sleep(Duration::from_millis(5));
            let after = transfer.snapshot();
            need(after == stopped, "Paused cursor advanced")?;
            transfer.resume()?;
            need(
                transfer.snapshot().pending == before.pending
                    && transfer.snapshot().output_frames == before.output_frames,
                "Resume altered pending audio",
            )?;
            report["pause"] = json!({"before":before,"paused":stopped,"afterWait":after,"resumed":transfer.snapshot(),"commitRejected":true});
            paused = true;
        }
        let (packet, ticket) = transfer.packet()?;
        let accepted = ACCEPT[accepts % ACCEPT.len()]
            .min((packet.len() / 2) as u32)
            .min(left as u32);
        accepts += 1;
        samples.extend_from_slice(&packet[..accepted as usize * 2]);
        let before = transfer.snapshot();
        let after = transfer.commit(accepted)?;
        if accepted > 0 && !replay {
            need(
                transfer.reject_replay(ticket),
                "Repeated acknowledgment advanced output twice",
            )?;
            replay = true;
        }
        need(
            commits.len() < 16384,
            "Commit evidence exceeded fixed trace limit",
        )?;
        commits
            .push(json!({"ticket":ticket,"acceptedFrames":accepted,"before":before,"after":after}));
        if after.pending.is_none() {
            requests += 1;
        }
    }
    need(
        paused && replay,
        "Continuous test did not exercise pause and replay rejection",
    )?;
    report["commits"] = json!(commits);
    report["replayRejected"] = replay.into();
    report["snapshot"] = json!(transfer.snapshot());
    report["decoderStarts"] = transfer.decoder_starts.into();
    report["elapsedMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
    Ok(samples)
}

fn device_segment(transfer: &mut AudioTransfer, report: &mut Value) -> Result<Vec<f32>, String> {
    let mut output = WasapiOutput::open_pcm_muted_event()?;
    need(
        output.info().session_muted
            && output.info().sample_rate == 48000
            && output.info().channels == 2,
        "Unexpected owned audio endpoint format",
    )?;
    report["device"] = json!(output.info());
    let mut scheduling = ThreadScheduling::enter(true)?;
    let mut samples = Vec::with_capacity(FRAMES as usize * 2);
    let mut requests = 0usize;
    let mut running = false;
    let mut paused = false;
    let mut signal_count = 0u64;
    let mut max_gap = Duration::ZERO;
    let started = Instant::now();
    let mut last_pump = started;
    let mut initial_position = None;
    let mut last_position = 0u64;
    let mut last_qpc = 0u64;
    let mut partial_accepts = 0u64;
    let mut zero_accepts = 0u64;
    let mut commit_calls = 0u64;
    let mut underruns = 0u64;
    let work = (|| {
        loop {
            need(
                started.elapsed() < Duration::from_secs(25),
                "Device transport exceeded 25 seconds",
            )?;
            let before = output.padding_frames()?;
            if running {
                max_gap = max_gap.max(last_pump.elapsed());
                if before == 0 && transfer.snapshot().output_frames < FRAMES {
                    underruns += 1;
                }
            }
            last_pump = Instant::now();
            for _ in 0..24 {
                let state = transfer.snapshot();
                if state.output_frames == FRAMES
                    || output.padding_frames()? == output.info().buffer_frames
                {
                    break;
                }
                let request =
                    REQUESTS[requests % REQUESTS.len()].min((FRAMES - state.output_frames) as u32);
                if !transfer.prepare(request)? {
                    break;
                }
                let (packet, _) = transfer.packet()?;
                let offered = (packet.len() / 2) as u32;
                let accepted = output.write_pcm(packet)?;
                commit_calls += 1;
                if accepted == 0 {
                    zero_accepts += 1;
                } else if accepted < offered {
                    partial_accepts += 1;
                }
                samples.extend_from_slice(&packet[..accepted as usize * 2]);
                let state = transfer.commit(accepted)?;
                if state.pending.is_none() {
                    requests += 1;
                }
                if accepted == 0 {
                    break;
                }
            }
            if !running {
                if output.padding_frames()? == output.info().buffer_frames {
                    let clock = output.sample()?;
                    need(
                        clock.reliable && clock.position == 0,
                        "New device clock is not zero",
                    )?;
                    initial_position = Some(clock.position);
                    last_position = clock.position;
                    last_qpc = clock.qpc_100ns;
                    output.start()?;
                    running = true;
                    last_pump = Instant::now();
                } else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
            }
            if !paused
                && transfer.snapshot().output_frames >= 3 * 48000
                && transfer.snapshot().source_frame > 0
                && transfer.snapshot().pending.is_some()
            {
                output.stop()?;
                let before = transfer.snapshot();
                transfer.pause()?;
                need(
                    transfer.paused_commit_rejected(),
                    "Paused device cursor accepted output",
                )?;
                let frozen = output.sample()?;
                thread::sleep(Duration::from_millis(200));
                let after = output.sample()?;
                need(
                    frozen.reliable && after.reliable && frozen.position == after.position,
                    "Paused output clock advanced",
                )?;
                transfer.resume()?;
                need(
                    transfer.snapshot().output_frames == before.output_frames
                        && transfer.snapshot().pending == before.pending,
                    "Device resume changed pending samples",
                )?;
                report["pause"] = json!({"frozen":true,"milliseconds":200,"cursor":before,"position":after.position.to_string()});
                output.start()?;
                paused = true;
                last_pump = Instant::now();
            }
            let clock = output.sample()?;
            need(clock.reliable, "Device clock became unreliable")?;
            need(
                clock.position >= last_position && clock.qpc_100ns >= last_qpc,
                "Device clock regressed during continuous transfer",
            )?;
            last_position = clock.position;
            last_qpc = clock.qpc_100ns;
            let drained =
                output.padding_frames()? == 0 && transfer.snapshot().output_frames == FRAMES;
            let covered =
                u128::from(clock.position - initial_position.ok_or("Missing device baseline")?)
                    * 48000
                    >= u128::from(FRAMES) * u128::from(clock.frequency);
            if drained && covered {
                report["finalClock"] = json!({"position":clock.position.to_string(),"frequency":clock.frequency,"padding":0,"clockCoversAllFrames":true});
                break;
            }
            match output.wait_ready(Duration::from_millis(25))? {
                WaitOutcome::Signaled => signal_count += 1,
                WaitOutcome::TimedOut => {
                    return Err("Device audio readiness event timed out".into());
                }
            }
        }
        need(
            underruns == 0,
            "Device PCM queue became empty before all output samples were submitted",
        )?;
        need(
            paused && partial_accepts > 0,
            "Device run did not exercise pending pause and partial acceptance",
        )?;
        Ok::<(), String>(())
    })();
    let stop = if running { output.stop() } else { Ok(()) };
    let restore = scheduling.finish();
    report["threadScheduling"] = json!(scheduling.info());
    report["stopError"] = json!(stop.as_ref().err());
    report["restoreError"] = json!(restore.as_ref().err());
    report["eventSignals"] = signal_count.into();
    report["partialAccepts"] = partial_accepts.into();
    report["zeroAccepts"] = zero_accepts.into();
    report["commitCalls"] = commit_calls.into();
    report["underrunObservations"] = underruns.into();
    report["maxPumpGapMs"] = (max_gap.as_secs_f64() * 1000.0).into();
    report["feedGapWithinBuffer"] =
        (max_gap.as_secs_f64() < f64::from(output.info().buffer_frames) / 48000.0).into();
    report["snapshot"] = json!(transfer.snapshot());
    report["elapsedMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
    work?;
    stop?;
    restore?;
    Ok(samples)
}

pub fn run() -> Result<(), String> {
    let mut fixture = None;
    let mut output = None;
    let mut device = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(k) = args.next() {
        let v = args.next().ok_or("Missing argument value")?;
        match k.to_str() {
            Some("--fixture-dir") => fixture = Some(PathBuf::from(v)),
            Some("--output-dir") => output = Some(PathBuf::from(v)),
            Some("--device") => {
                device = match v.to_str() {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err("Device must be true or false".into()),
                }
            }
            _ => return Err("Expected --fixture-dir, --output-dir, --device".into()),
        }
    }
    let root = fixture
        .ok_or("Missing fixture")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let output = output.ok_or("Missing output")?;
    let lexical = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    need(
        output.is_absolute()
            && output.starts_with(&lexical)
            && !output
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir)),
        "Output escaped checkout target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing ancestor")?;
    need(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped via link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let run = output
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let manifest_path = root.join("audio-timing-fixtures.json");
    let manifest = json_file(&manifest_path)?;
    let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},"deviceRequested":device,"fixtureManifest":identity(&manifest_path)?,"cases":[],"scope":{"visibleWindow":false,"gpu":false,"audioDevice":device,"applicationBackendChanged":false,"ownedMutedSession":device}});
    let work = (|| {
        let audios = manifest["audioCases"]
            .as_array()
            .ok_or("Missing audio cases")?;
        let videos = manifest["videoCases"]
            .as_array()
            .ok_or("Missing video cases")?;
        need(
            audios.len() == 3 && videos.len() == 2,
            "Expected bounded audio timing fixture matrix",
        )?;
        for (ai, a) in audios.iter().enumerate() {
            for (vi, v) in videos.iter().enumerate() {
                if device && (ai != 2 || vi != 0) {
                    continue;
                }
                let audio = local(&root, text(a, "source")?)?;
                let video = local(&root, text(v, "video")?)?;
                let reference = local(&root, text(a, "referenceWav")?)?;
                let a_id = identity(&audio)?;
                let v_id = identity(&video)?;
                let r_id = identity(&reference)?;
                need(
                    a_id["sha256"] == text(a, "sourceSha256")?
                        && r_id["sha256"] == text(a, "referenceSha256")?
                        && v_id["sha256"] == text(v, "sourceSha256")?,
                    "Fixture identity mismatch",
                )?;
                let ao = origin(a, true)?;
                let vo = origin(v, false)?;
                let total = number(a, "decodedFrames")?;
                validate_audio_metadata(
                    &json_file(&local(&root, text(a, "metadata")?)?)?,
                    ao,
                    total,
                )?;
                let timeline = AudioTimeline::new(ao, vo, 48000, total)?;
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
                let mut transport = AudioTransfer::new(timeline.clone(), 0, config)?;
                let mut c = json!({"ok":false,"audioLabel":a["label"],"videoLabel":v["label"],"audioSource":a_id,"videoSource":v_id,"pcmReference":r_id,"audioOrigin":ao,"videoOrigin":vo,"totalSourceFrames":total,"segments":[]});
                let operation = (|| {
                    let targets: Vec<u64> = if device {
                        vec![0]
                    } else {
                        vec![0, 20833, 2_000_020_833, 7_123_000_000]
                    };
                    for (si, start_ns) in targets.into_iter().enumerate() {
                        let count = if si == 0 { FRAMES } else { 12001 };
                        let mut segment =
                            json!({"targetNs":start_ns.to_string(),"requestedFrames":count});
                        if si > 0 {
                            // Prepare an old pending ticket so seek rejection is exercised.
                            let wait = Instant::now();
                            while !transport.prepare(257)? {
                                need(
                                    wait.elapsed() < Duration::from_secs(5),
                                    "Preparing pre-seek packet timed out",
                                )?;
                                thread::sleep(Duration::from_millis(1));
                            }
                            segment["seek"] = transport.seek(start_ns)?;
                        }
                        let result = if device {
                            device_segment(&mut transport, &mut segment)
                        } else {
                            offline_segment(&mut transport, count, &mut segment)
                        };
                        match result {
                            Ok(samples) => {
                                let oracle = expected(&timeline, start_ns, count, &reference)?;
                                let exact = samples == oracle;
                                segment["capture"] = capture(
                                    &run,
                                    &format!("audio-{ai}-{vi}-segment-{si}.f32le"),
                                    &samples,
                                )?;
                                segment["expectedSha256"] = sample_hash(&oracle).into();
                                segment["ok"] = exact.into();
                                c["segments"].as_array_mut().unwrap().push(segment);
                                need(
                                    exact,
                                    "Continuous PCM differs from complete source/silence oracle",
                                )?;
                            }
                            Err(error) => {
                                segment["error"] = error.clone().into();
                                c["segments"].as_array_mut().unwrap().push(segment);
                                return Err(error);
                            }
                        }
                    }
                    Ok::<(), String>(())
                })();
                let closed = transport.close();
                c["close"] = json!(closed.as_ref().ok());
                if let Err(error) = &closed {
                    c["closeError"] = error.clone().into();
                }
                if let Err(error) = &operation {
                    c["error"] = error.clone().into();
                }
                c["ok"] = (operation.is_ok() && closed.is_ok()).into();
                report["cases"].as_array_mut().unwrap().push(c);
                operation?;
                closed?;
                need(
                    identity(&audio)? == a_id
                        && identity(&video)? == v_id
                        && identity(&reference)? == r_id,
                    "Continuous source/reference changed",
                )?;
            }
        }
        Ok::<(), String>(())
    })();
    report["ok"] = work.is_ok().into();
    report["functionalPassed"] = work.is_ok().into();
    report["realtimePerformanceQualified"] = false.into();
    if let Err(error) = &work {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-audio-transport-validation.json");
    let mut tmp = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    let write = serde_json::to_writer_pretty(&mut tmp, &report)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            tmp.write_all(b"\n")
                .and_then(|_| tmp.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = write {
        let retained = tmp.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{error}; retained {retained:?}"));
    }
    if let Err(error) = tmp.persist_noclobber(&path) {
        let why = error.error.to_string();
        let retained = error.file.keep().map(|(_, p)| p);
        return Err(format!("{why}; retained {retained:?}"));
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"cases":report["cases"].as_array().unwrap().len(),"error":report.get("error")})
    );
    work
}
