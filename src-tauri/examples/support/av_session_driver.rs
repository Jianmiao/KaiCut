//! Audio-device-clock-driven video selection, with offscreen validation only.
use super::{
    async_renderer::{AsyncRenderer, Frame, SubmitStatus},
    audio_preview_adapter::{AdapterPhase, AudioPreviewAdapter},
    audio_scheduling::ThreadScheduling,
    audio_session_driver::{accept, clock, oracle, save_capture},
    audio_timeline::AudioTimeline,
    audio_timing_driver::{
        identity, json_file, local, need, number, origin, sample_hash, text,
        validate_audio_metadata,
    },
    audio_transfer::AudioTransfer,
    streaming_decoder::{SeekMode, StreamConfig},
    wasapi_output::{WaitOutcome, WasapiOutput},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
type Audio = AudioPreviewAdapter<AudioTransfer, WasapiOutput>;

fn project_time(target: u64, position: u64, frequency: u64) -> Result<u64, String> {
    need(frequency > 0, "Zero audio device clock frequency")?;
    let elapsed = u128::from(position) * 1_000_000_000 / u128::from(frequency);
    u64::try_from(u128::from(target) + elapsed).map_err(|_| "Audio project time overflow".into())
}

struct VideoCoordinator {
    renderer: AsyncRenderer,
    epoch: u64,
    source_epoch: u64,
    reference: Vec<Value>,
    times: Vec<u64>,
    last_submitted: Option<usize>,
    last_completed: Option<usize>,
    submitted: BTreeMap<usize, (u64, u64)>,
    events: Vec<Value>,
}
impl VideoCoordinator {
    fn new(reference: Value, source_epoch: u64) -> Result<Self, String> {
        let frames = reference["frames"]
            .as_array()
            .ok_or("Missing VFR reference")?
            .clone();
        need(
            !frames.is_empty() && frames.len() <= 100000,
            "VFR reference size outside bound",
        )?;
        let mut times = Vec::new();
        for (i, frame) in frames.iter().enumerate() {
            need(
                number(frame, "index")? == i as u64,
                "Reference indices not consecutive",
            )?;
            let ns = text(frame, "ptsNs")?
                .parse::<u64>()
                .map_err(|e| e.to_string())?;
            need(
                times.last().is_none_or(|last| ns > *last),
                "Reference timestamps not increasing",
            )?;
            times.push(ns);
        }
        let renderer = AsyncRenderer::start(320, 180, 0)?;
        let epoch = renderer.advance_epoch()?;
        Ok(Self {
            renderer,
            epoch,
            source_epoch,
            reference: frames,
            times,
            last_submitted: None,
            last_completed: None,
            submitted: BTreeMap::new(),
            events: Vec::new(),
        })
    }
    fn expected(&self, time: u64) -> Result<usize, String> {
        self.times
            .partition_point(|pts| *pts <= time)
            .checked_sub(1)
            .ok_or_else(|| "No video frame before project time".into())
    }
    fn update(&mut self, audio: &mut Audio, time: u64, stage: &str) -> Result<(), String> {
        need(
            audio.snapshot().cursor.epoch == self.source_epoch,
            "Audio/video source epochs differ",
        )?;
        let selected = audio.video_frame(time)?;
        if let Some(frame) = selected {
            need(
                frame.epoch == self.source_epoch && frame.pts_ns <= time,
                "Stale or future video selected",
            )?;
            need(
                self.times.get(frame.index) == Some(&frame.pts_ns),
                "Selected frame PTS differs from actual source reference",
            )?;
            if self.last_submitted != Some(frame.index) {
                need(
                    self.last_submitted.is_none_or(|last| frame.index > last),
                    "Video selection regressed within render epoch",
                )?;
                let index = frame.index;
                let pts = frame.pts_ns;
                let status = self.renderer.submit(
                    self.epoch,
                    Frame {
                        index,
                        pts_ns: pts,
                        rgb: frame.rgb,
                    },
                );
                need(
                    matches!(
                        status,
                        SubmitStatus::Queued | SubmitStatus::Evicted | SubmitStatus::Busy
                    ),
                    "Renderer rejected current video epoch",
                )?;
                self.events.push(json!({"kind":"submit","stage":stage,"sourceEpoch":self.source_epoch,"renderEpoch":self.epoch,"projectTimeNs":time.to_string(),"index":index,"ptsNs":pts.to_string(),"expectedDueIndex":self.expected(time)?,"status":format!("{status:?}"),"selection":audio.video_snapshot()}));
                if status != SubmitStatus::Busy {
                    self.last_submitted = Some(index);
                    self.submitted.insert(index, (time, self.source_epoch));
                }
            }
        }
        self.renderer.health()?;
        if let Some(frame) = self.renderer.poll_latest() {
            need(
                frame.epoch == self.epoch
                    && self.last_completed.is_none_or(|last| frame.index >= last),
                "Late or reversed GPU completion escaped epoch isolation",
            )?;
            let (submitted_at, source_epoch) = *self
                .submitted
                .get(&frame.index)
                .ok_or("GPU result has no current submission")?;
            need(
                source_epoch == self.source_epoch && frame.pts_ns <= time,
                "GPU completion belongs to wrong source time",
            )?;
            let expected = self
                .reference
                .get(frame.index)
                .ok_or("GPU index outside reference")?;
            let v = &frame.verification;
            need(
                v.exact
                    && v.input_rgb_sha256 == text(expected, "rgbSha256")?
                    && v.expected_rgba_sha256 == text(expected, "rgbaSha256")?
                    && v.actual_rgba_sha256 == text(expected, "rgbaSha256")?,
                "GPU pixels differ from independent VFR source",
            )?;
            self.last_completed = Some(frame.index);
            self.events.push(json!({"kind":"complete","stage":stage,"sourceEpoch":self.source_epoch,"renderEpoch":self.epoch,"projectTimeNs":time.to_string(),"submittedProjectTimeNs":submitted_at.to_string(),"index":frame.index,"ptsNs":frame.pts_ns.to_string(),"rgbSha256":v.input_rgb_sha256,"rgbaSha256":v.actual_rgba_sha256,"exact":v.exact,"uploadDrawReadbackMs":frame.result.upload_draw_readback_ms,"readbackWaitMs":frame.result.readback_wait_ms,"rowPitch":frame.result.row_pitch}));
        }
        need(
            self.events.len() < 2048,
            "AV video event trace limit exceeded",
        )?;
        Ok(())
    }
    fn ready(&mut self, audio: &mut Audio, time: u64) -> Result<Value, String> {
        let expected = self.expected(time)?;
        let began = Instant::now();
        loop {
            need(
                began.elapsed() < Duration::from_secs(5),
                "Video readiness timed out",
            )?;
            need(
                !audio.snapshot().device_running,
                "Cannot wait for GPU while audio is running",
            )?;
            self.update(audio, time, "prepare")?;
            if self.last_completed == Some(expected) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(
            json!({"expectedIndex":expected,"completedIndex":self.last_completed,"projectTimeNs":time.to_string(),"sourceEpoch":self.source_epoch,"renderEpoch":self.epoch,"elapsedMs":began.elapsed().as_secs_f64()*1000.0,"audioDeviceRunning":audio.snapshot().device_running,"selection":audio.video_snapshot()}),
        )
    }
    fn retire(&mut self, audio: &Audio) -> Result<Value, String> {
        need(
            !audio.snapshot().device_running,
            "Stop audio before waiting for GPU retirement",
        )?;
        let retired = self.renderer.retire_epoch(Duration::from_secs(3))?;
        self.epoch = retired.new_epoch;
        self.source_epoch = audio.snapshot().cursor.epoch;
        self.last_submitted = None;
        self.last_completed = None;
        self.submitted.clear();
        Ok(json!(retired))
    }
}

fn segment(
    audio: &mut Audio,
    video: &mut VideoCoordinator,
    target: u64,
    frames: u64,
    pause: bool,
    entry: &mut Value,
) -> Result<Vec<f32>, String> {
    let mut accepted = Vec::new();
    let mut submissions = Vec::new();
    let mut observations = Vec::new();
    let started = Instant::now();
    let event_start = video.events.len();
    let outcome = (|| {
        while audio.snapshot().phase == AdapterPhase::Preparing {
            need(
                started.elapsed() < Duration::from_secs(10),
                "AV audio preparation timed out",
            )?;
            accept(audio.prepare_tick(4800)?, &mut accepted, &mut submissions)?;
            video.update(audio, target, "prepare")?;
            if audio.snapshot().phase == AdapterPhase::Preparing {
                thread::sleep(Duration::from_millis(1));
            }
        }
        need(
            audio.snapshot().phase == AdapterPhase::Ready,
            "Audio did not become ready",
        )?;
        entry["videoReady"] = video.ready(audio, target)?;
        entry["prepared"] = json!({"state":audio.snapshot(),"clock":clock(audio)?,"decoder":audio.decoder_stats()?});
        let mut scheduling = ThreadScheduling::enter(true)?;
        let play = (|| {
            entry["audioStartAfterVideoReady"] = true.into();
            audio.start()?;
            let running = Instant::now();
            let mut last_pump = Instant::now();
            let mut gap = Duration::ZERO;
            let mut last_position = 0;
            let mut last_qpc = 0;
            let mut paused = false;
            let mut underruns = 0u64;
            let mut signals = 0u64;
            loop {
                need(
                    running.elapsed() < Duration::from_secs(4),
                    "AV segment exceeded deadline",
                )?;
                gap = gap.max(last_pump.elapsed());
                last_pump = Instant::now();
                if audio.output().padding_frames()? == 0 {
                    underruns += 1;
                }
                for _ in 0..16 {
                    if audio.output().padding_frames()? == audio.output().info().buffer_frames {
                        break;
                    }
                    let tick = audio.pump(4800)?;
                    let n = tick.written_frames;
                    accept(tick, &mut accepted, &mut submissions)?;
                    if n == 0 {
                        break;
                    }
                }
                let c = audio.output().sample()?;
                observations.push(json!({"position":c.position.to_string(),"frequency":c.frequency,"qpc100ns":c.qpc_100ns.to_string(),"reliable":c.reliable,"paddingFrames":audio.output().padding_frames()?,"sourceEpoch":audio.snapshot().cursor.epoch,"renderEpoch":video.epoch}));
                need(
                    c.reliable && c.position >= last_position && c.qpc_100ns >= last_qpc,
                    "AV audio clock unreliable or reversed",
                )?;
                last_position = c.position;
                last_qpc = c.qpc_100ns;
                let time = project_time(target, c.position, c.frequency)?;
                video.update(audio, time, "playing")?;
                let consumed = u128::from(c.position) * 48000 / u128::from(c.frequency);
                if pause && !paused && consumed >= 9600 && audio.snapshot().cursor.pending.is_some()
                {
                    let before = audio.snapshot();
                    audio.pause()?;
                    let held = clock(audio)?;
                    let retirement = video.retire(audio)?;
                    let time = project_time(
                        target,
                        text(&held, "position")?
                            .parse()
                            .map_err(|e: std::num::ParseIntError| e.to_string())?,
                        number(&held, "frequency")?,
                    )?;
                    thread::sleep(Duration::from_millis(200));
                    let frozen = clock(audio)?;
                    need(
                        held["position"] == frozen["position"]
                            && held["paddingFrames"] == frozen["paddingFrames"],
                        "Audio advanced while AV session paused",
                    )?;
                    let ready = video.ready(audio, time)?;
                    need(
                        audio.snapshot().cursor.pending == before.cursor.pending,
                        "Paused video preparation mutated audio packet",
                    )?;
                    entry["pause"] = json!({"before":before,"heldClock":held,"afterWaitClock":frozen,"milliseconds":200,"rendererRetirement":retirement,"videoReady":ready,"audioStillPaused":audio.snapshot()});
                    audio.start()?;
                    paused = true;
                    last_pump = Instant::now();
                }
                if consumed >= u128::from(frames)
                    && audio.snapshot().cursor.pending.is_some()
                    && audio.output().padding_frames()? > 0
                {
                    entry["endState"] = json!(audio.snapshot());
                    entry["endClock"] = clock(audio)?;
                    entry["maxPumpGapMs"] = (gap.as_secs_f64() * 1000.0).into();
                    entry["underrunObservations"] = underruns.into();
                    entry["eventSignals"] = signals.into();
                    entry["feedGapWithinBuffer"] = (gap.as_secs_f64()
                        < f64::from(audio.output().info().buffer_frames) / 48000.0)
                        .into();
                    need(
                        underruns == 0 && (!pause || paused),
                        "AV output underrun or missing pause check",
                    )?;
                    break;
                }
                match audio.output().wait_ready(Duration::from_millis(25))? {
                    WaitOutcome::Signaled => signals += 1,
                    WaitOutcome::TimedOut => return Err("AV audio event timed out".into()),
                }
            }
            Ok::<(), String>(())
        })();
        if play.is_err() {
            let _ = audio.cancel();
        }
        let restored = scheduling.finish();
        entry["threadScheduling"] = json!(scheduling.info());
        entry["restoreError"] = json!(restored.as_ref().err());
        play?;
        restored?;
        need(
            video.events[event_start..]
                .iter()
                .any(|event| event["kind"] == "complete" && event["stage"] == "playing"),
            "No video frame completed during this running audio segment",
        )?;
        Ok::<(), String>(())
    })();
    entry["targetNs"] = target.to_string().into();
    entry["acceptedFrames"] = (accepted.len() / 2).into();
    entry["submissionTrace"] = json!(submissions);
    entry["clockSamples"] = json!(observations);
    entry["videoEventStart"] = event_start.into();
    entry["videoEventEnd"] = video.events.len().into();
    entry["elapsedMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
    outcome?;
    Ok(accepted)
}

pub fn run() -> Result<(), String> {
    let mut fixture = None;
    let mut out = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(k) = args.next() {
        let p = PathBuf::from(args.next().ok_or("Missing argument value")?);
        match k.to_str() {
            Some("--fixture-dir") => fixture = Some(p),
            Some("--output-dir") => out = Some(p),
            _ => return Err("Expected --fixture-dir and --output-dir".into()),
        }
    }
    let root = fixture
        .ok_or("Missing fixture")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let output = out.ok_or("Missing output")?;
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
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    need(output.starts_with(&target), "Output escaped target")?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let manifest_path = root.join("audio-timing-fixtures.json");
    let manifest = json_file(&manifest_path)?;
    let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},"scope":{"ownedMutedAudio":true,"audioDevice":true,"gpuOffscreen":true,"presentCalled":false,"visibleWindow":false,"applicationBackendChanged":false},"fixtureManifest":identity(&manifest_path)?,"segments":[],"seeks":[]});
    let result = (|| {
        let a = manifest["audioCases"]
            .as_array()
            .ok_or("Missing audios")?
            .iter()
            .find(|v| v["label"] == "aac-offset-aperiodic")
            .ok_or("Missing offset AAC")?;
        let v = manifest["videoCases"]
            .as_array()
            .ok_or("Missing videos")?
            .iter()
            .find(|v| v["label"] == "vfr-zero-origin")
            .ok_or("Missing VFR video")?;
        let audio_path = local(&root, text(a, "source")?)?;
        let video_path = local(&root, text(v, "video")?)?;
        let pcm = local(&root, text(a, "referenceWav")?)?;
        let video_ref = local(&root, text(v, "reference")?)?;
        let aid = identity(&audio_path)?;
        let vid = identity(&video_path)?;
        let pid = identity(&pcm)?;
        let rid = identity(&video_ref)?;
        need(
            aid["sha256"] == text(a, "sourceSha256")?
                && vid["sha256"] == text(v, "sourceSha256")?
                && pid["sha256"] == text(a, "referenceSha256")?,
            "Source identity mismatch",
        )?;
        let ledger = json_file(&video_ref)?;
        need(
            ledger["sourceHash"] == vid["sha256"],
            "Video oracle source mismatch",
        )?;
        let ao = origin(a, true)?;
        let vo = origin(v, false)?;
        let total = number(a, "decodedFrames")?;
        validate_audio_metadata(&json_file(&local(&root, text(a, "metadata")?)?)?, ao, total)?;
        let timeline = AudioTimeline::new(ao, vo, 48000, total)?;
        report["timeline"] = json!(timeline.diagnostics());
        report["audioSource"] = aid.clone();
        report["videoSource"] = vid.clone();
        report["pcmReference"] = pid.clone();
        report["videoReference"] = rid.clone();
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let config = StreamConfig {
            ffmpeg: bin.join("ffmpeg-x86_64-pc-windows-msvc.exe"),
            ffprobe: bin.join("ffprobe-x86_64-pc-windows-msvc.exe"),
            video: video_path.clone(),
            audio: audio_path.clone(),
            sample_rate: 48000,
            channels: 2,
            video_width: 320,
            video_height: 180,
            seek_mode: SeekMode::IndexedPts,
            epoch: 1,
            start_audio_frame: 0,
            start_video_ns: 0,
        };
        let transfer = AudioTransfer::new_av(timeline.clone(), 2_200_000_000, config)?;
        let mut audio = AudioPreviewAdapter::new(transfer, WasapiOutput::open_pcm_muted_event()?)?;
        report["device"] = json!(audio.output().info());
        report["initialAudioAdapterEvidence"] = json!(audio.evidence());
        let mut video = VideoCoordinator::new(ledger, audio.snapshot().cursor.epoch)?;
        report["renderer"] =
            json!({"instanceId":video.renderer.instance_id(),"info":video.renderer.info()});
        let mut captures = Vec::new();
        let work = (|| {
            for (index, (time, frames)) in [
                (2_200_000_000u64, 28800u64),
                (7_123_000_000, 28800),
                (0, 19200),
                (9_500_000_000, 19200),
            ]
            .into_iter()
            .enumerate()
            {
                if index > 0 {
                    let before = audio.snapshot();
                    let queued = clock(&audio)?;
                    audio.pause()?;
                    let retired = video.retire(&audio)?;
                    audio.seek(time)?;
                    video.source_epoch = audio.snapshot().cursor.epoch;
                    let reset = clock(&audio)?;
                    need(
                        reset["position"] == "0"
                            && reset["paddingFrames"] == 0
                            && !audio.snapshot().device_running,
                        "AV seek did not reset audio",
                    )?;
                    report["seeks"].as_array_mut().unwrap().push(json!({"targetNs":time.to_string(),"before":before,"queuedClock":queued,"rendererRetirement":retired,"after":audio.snapshot(),"resetClock":reset,"sourceEpoch":video.source_epoch,"renderEpoch":video.epoch,"evidence":audio.evidence(),"selectionAfterSeek":audio.video_snapshot()}));
                }
                let mut entry = json!({"ok":false});
                let rendered =
                    segment(&mut audio, &mut video, time, frames, index == 0, &mut entry);
                match rendered {
                    Ok(samples) => {
                        captures.push((index, time, samples));
                        report["segments"].as_array_mut().unwrap().push(entry);
                    }
                    Err(error) => {
                        entry["error"] = error.clone().into();
                        report["segments"].as_array_mut().unwrap().push(entry);
                        return Err(error);
                    }
                }
            }
            let before = audio.snapshot();
            let queued = clock(&audio)?;
            audio.cancel()?;
            let retired = video.retire(&audio)?;
            let stopped = clock(&audio)?;
            thread::sleep(Duration::from_millis(150));
            let frozen = clock(&audio)?;
            need(
                stopped["position"] == "0"
                    && stopped["paddingFrames"] == 0
                    && stopped["position"] == frozen["position"]
                    && frozen["paddingFrames"] == 0,
                "Cancelled AV session remained active",
            )?;
            let rejected = audio.video_frame(0).is_err() && audio.start().is_err();
            need(rejected, "Cancelled AV session accepted new work")?;
            report["cancel"] = json!({"before":before,"queuedClock":queued,"after":audio.snapshot(),"resetClock":stopped,"afterWaitClock":frozen,"milliseconds":150,"rendererRetirement":retired,"commandsRejected":rejected,"evidence":audio.evidence()});
            Ok::<(), String>(())
        })();
        if let Err(error) = &work {
            let closed = audio.cancel();
            report["failureCleanup"] =
                json!({"error":closed.err(),"state":audio.snapshot(),"evidence":audio.evidence()});
            report["error"] = error.clone().into();
        }
        report["videoEvents"] = json!(video.events);
        drop(audio);
        let shutdown = video.renderer.shutdown();
        report["rendererShutdown"] = json!(shutdown.as_ref().ok());
        report["rendererShutdownError"] = json!(shutdown.as_ref().err());
        for (index, time, samples) in captures {
            let expected = oracle(&timeline, time, (samples.len() / 2) as u64, &pcm)?;
            let exact = samples == expected;
            report["segments"][index]["capture"] =
                save_capture(&run, &format!("accepted-{index}.f32le"), &samples)?;
            report["segments"][index]["expectedSha256"] = sample_hash(&expected).into();
            report["segments"][index]["ok"] = exact.into();
            need(exact, "AV PCM stream does not match source timeline")?;
        }
        work?;
        let closed = shutdown?;
        need(
            closed.joined && closed.error.is_none() && closed.stats.failures == 0,
            "AV renderer cleanup failed",
        )?;
        need(
            identity(&audio_path)? == aid
                && identity(&video_path)? == vid
                && identity(&pcm)? == pid
                && identity(&video_ref)? == rid,
            "AV source/reference changed",
        )?;
        Ok::<(), String>(())
    })();
    report["ok"] = result.is_ok().into();
    report["functionalPassed"] = result.is_ok().into();
    report["realtimePerformanceQualified"] = false.into();
    if let Err(error) = &result {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-av-session-validation.json");
    let mut tmp = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    let save = serde_json::to_writer_pretty(&mut tmp, &report)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            tmp.write_all(b"\n")
                .and_then(|_| tmp.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = save {
        let kept = tmp.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{error}; retained {kept:?}"));
    }
    if let Err(error) = tmp.persist_noclobber(&path) {
        let why = error.error.to_string();
        let kept = error.file.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{why}; retained {kept:?}"));
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"error":report.get("error")})
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_clock_uses_device_frequency_not_pcm_rate() {
        assert_eq!(
            project_time(7_123_000_000, 38400, 384000).unwrap(),
            7_223_000_000
        );
        assert!(project_time(0, 1, 0).is_err());
        assert!(project_time(u64::MAX, 1, 1).is_err());
    }
}
