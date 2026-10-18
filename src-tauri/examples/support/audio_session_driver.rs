//! Exercise one owned muted WASAPI output across pause, seeks and cancellation.
use super::{
    audio_preview_adapter::{AdapterPhase, AudioPreviewAdapter, TickOutcome},
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
type Adapter = AudioPreviewAdapter<AudioTransfer, WasapiOutput>;

pub(crate) fn clock(adapter: &Adapter) -> Result<Value, String> {
    let c = adapter.output().sample()?;
    Ok(
        json!({"position":c.position.to_string(),"frequency":c.frequency,"qpc100ns":c.qpc_100ns.to_string(),"reliable":c.reliable,"hresult":c.hresult,"paddingFrames":adapter.output().padding_frames()?}),
    )
}
pub(crate) fn accept(
    outcome: TickOutcome,
    samples: &mut Vec<f32>,
    trace: &mut Vec<Value>,
) -> Result<(), String> {
    need(
        outcome.accepted_samples.len() == outcome.written_frames as usize * 2,
        "Adapter returned a nonmatching accepted prefix",
    )?;
    need(
        samples.len() + outcome.accepted_samples.len() <= 96000 * 2,
        "Session segment capture exceeds two seconds",
    )?;
    samples.extend_from_slice(&outcome.accepted_samples);
    need(
        trace.len() < 4096,
        "Session submission trace exceeded limit",
    )?;
    trace.push(json!({"ticket":outcome.ticket,"writtenFrames":outcome.written_frames,"before":outcome.before,"after":outcome.after}));
    Ok(())
}
pub(crate) fn oracle(
    timeline: &AudioTimeline,
    start: u64,
    count: u64,
    reference: &Path,
) -> Result<Vec<f32>, String> {
    let d = timeline.diagnostics();
    let first = d
        .first_source_timeline_ns
        .parse::<i128>()
        .map_err(|e| e.to_string())?;
    let lead = if first > i128::from(start) {
        u64::try_from(((first - i128::from(start)) * 48000 + 999_999_999) / 1_000_000_000)
            .map_err(|e| e.to_string())?
            .min(count)
    } else {
        0
    };
    let source_start = timeline.span(start, 0)?.source_start_frame;
    let source_count = (count - lead).min(d.total_frames - source_start);
    let mut samples = vec![0.0; lead as usize * 2];
    let mut cursor = 0;
    while cursor < source_count {
        let n = (source_count - cursor).min(4800) as u32;
        samples.extend(reference_pcm_samples(
            reference,
            source_start + cursor,
            n,
            48000,
            2,
        )?);
        cursor += u64::from(n);
    }
    samples.resize(count as usize * 2, 0.0);
    Ok(samples)
}
pub(crate) fn save_capture(run: &Path, name: &str, samples: &[f32]) -> Result<Value, String> {
    let path = run.join(name);
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    for sample in samples {
        f.write_all(&sample.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(
        json!({"path":path,"frames":samples.len()/2,"bytes":samples.len()*4,"sha256":sample_hash(samples)}),
    )
}

fn segment(
    adapter: &mut Adapter,
    project_start: u64,
    device_frames: u64,
    pause: bool,
    report: &mut Value,
) -> Result<Vec<f32>, String> {
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut trace = Vec::new();
    let mut clocks = Vec::new();
    let outcome = (|| {
        report["beforePrepare"] = json!({"state":adapter.snapshot(),"clock":clock(adapter)?});
        while adapter.snapshot().phase == AdapterPhase::Preparing {
            need(
                started.elapsed() < Duration::from_secs(10),
                "Adapter preparation timed out",
            )?;
            accept(adapter.prepare_tick(4800)?, &mut samples, &mut trace)?;
            if adapter.snapshot().phase == AdapterPhase::Preparing {
                thread::sleep(Duration::from_millis(1));
            }
        }
        need(
            adapter.snapshot().phase == AdapterPhase::Ready,
            "Adapter did not become Ready",
        )?;
        report["prepared"] = json!({"state":adapter.snapshot(),"clock":clock(adapter)?});
        report["prepareMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
        let mut scheduling = ThreadScheduling::enter(true)?;
        let playback = (|| {
            adapter.start()?;
            let active = Instant::now();
            let mut last_pump = Instant::now();
            let mut max_gap = Duration::ZERO;
            let mut last_position = 0;
            let mut last_qpc = 0;
            let mut paused = false;
            let mut underruns = 0;
            let mut signals = 0;
            loop {
                need(
                    active.elapsed() < Duration::from_secs(4),
                    "Adapter playback segment exceeded budget",
                )?;
                max_gap = max_gap.max(last_pump.elapsed());
                last_pump = Instant::now();
                if adapter.output().padding_frames()? == 0 {
                    underruns += 1;
                }
                for _ in 0..16 {
                    if adapter.output().padding_frames()? == adapter.output().info().buffer_frames {
                        break;
                    }
                    let outcome = adapter.pump(4800)?;
                    let accepted = outcome.written_frames;
                    accept(outcome, &mut samples, &mut trace)?;
                    if accepted == 0 {
                        break;
                    }
                }
                let c = adapter.output().sample()?;
                clocks.push(clock(adapter)?);
                need(
                    c.reliable && c.position >= last_position && c.qpc_100ns >= last_qpc,
                    "Device clock unreliable or regressed",
                )?;
                last_position = c.position;
                last_qpc = c.qpc_100ns;
                let elapsed_frames = u128::from(c.position) * 48000 / u128::from(c.frequency);
                if pause
                    && !paused
                    && elapsed_frames >= 19200
                    && adapter.snapshot().cursor.pending.is_some()
                {
                    let before = adapter.snapshot();
                    let before_clock = clock(adapter)?;
                    adapter.pause()?;
                    let frozen = adapter.snapshot();
                    let held = clock(adapter)?;
                    thread::sleep(Duration::from_millis(200));
                    let after = clock(adapter)?;
                    need(
                        held["position"] == after["position"]
                            && held["paddingFrames"] == after["paddingFrames"]
                            && adapter.snapshot().cursor == frozen.cursor,
                        "Paused adapter changed held output",
                    )?;
                    adapter.start()?;
                    need(
                        adapter.snapshot().cursor.pending == before.cursor.pending,
                        "Resume changed pending data",
                    )?;
                    report["pause"] = json!({"before":before,"beforeClock":before_clock,"paused":frozen,"heldClock":held,"afterWaitClock":after,"resumed":adapter.snapshot(),"milliseconds":200});
                    paused = true;
                    last_pump = Instant::now();
                }
                if elapsed_frames >= u128::from(device_frames)
                    && adapter.snapshot().cursor.pending.is_some()
                    && adapter.output().padding_frames()? > 0
                {
                    report["endState"] = json!(adapter.snapshot());
                    report["endClock"] = clock(adapter)?;
                    report["maxPumpGapMs"] = (max_gap.as_secs_f64() * 1000.0).into();
                    report["underrunObservations"] = underruns.into();
                    report["eventSignals"] = signals.into();
                    report["feedGapWithinBuffer"] = (max_gap.as_secs_f64()
                        < f64::from(adapter.output().info().buffer_frames) / 48000.0)
                        .into();
                    need(!pause || paused, "Pause did not occur")?;
                    need(
                        underruns == 0,
                        "Output queue emptied during adapter playback",
                    )?;
                    break;
                }
                match adapter.output().wait_ready(Duration::from_millis(25))? {
                    WaitOutcome::Signaled => signals += 1,
                    WaitOutcome::TimedOut => return Err("Adapter audio event timed out".into()),
                }
            }
            Ok::<(), String>(())
        })();
        if playback.is_err() {
            let _ = adapter.cancel();
        }
        let reverted = scheduling.finish();
        report["threadScheduling"] = json!(scheduling.info());
        report["restoreError"] = json!(reverted.as_ref().err());
        playback?;
        reverted?;
        Ok::<(), String>(())
    })();
    report["targetNs"] = project_start.to_string().into();
    report["submissionTrace"] = json!(trace);
    report["clockSamples"] = json!(clocks);
    report["acceptedFrames"] = (samples.len() / 2).into();
    report["elapsedMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
    if outcome.is_err() {
        report["acceptedSha256"] = sample_hash(&samples).into();
    }
    outcome?;
    Ok(samples)
}

pub fn run() -> Result<(), String> {
    let mut fixture = None;
    let mut output = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(k) = args.next() {
        let p = PathBuf::from(args.next().ok_or("Missing option value")?);
        match k.to_str() {
            Some("--fixture-dir") => fixture = Some(p),
            Some("--output-dir") => output = Some(p),
            _ => return Err("Expected --fixture-dir and --output-dir".into()),
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
        "Output escaped target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output parent")?;
    need(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped via link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let out = output.canonicalize().map_err(|e| e.to_string())?;
    need(out.starts_with(&target), "Output escaped target")?;
    let run = out.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let manifest_path = root.join("audio-timing-fixtures.json");
    let m = json_file(&manifest_path)?;
    let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},"scope":{"audioDevice":true,"ownedMutedSession":true,"gpu":false,"visibleWindow":false,"applicationBackendChanged":false,"capturedBytes":"Accepted endpoint input; buffered samples may be discarded by seek/cancel, not proof of audible playback."},"fixtureManifest":identity(&manifest_path)?,"segments":[],"seeks":[]});
    let result = (|| {
        let a = m["audioCases"]
            .as_array()
            .ok_or("Missing audio cases")?
            .iter()
            .find(|a| a["label"] == "aac-offset-aperiodic")
            .ok_or("Missing offset AAC")?;
        let v = m["videoCases"]
            .as_array()
            .ok_or("Missing video cases")?
            .iter()
            .find(|v| v["label"] == "vfr-zero-origin")
            .ok_or("Missing zero-origin video")?;
        let audio = local(&root, text(a, "source")?)?;
        let video = local(&root, text(v, "video")?)?;
        let reference = local(&root, text(a, "referenceWav")?)?;
        let aid = identity(&audio)?;
        let vid = identity(&video)?;
        let rid = identity(&reference)?;
        need(
            aid["sha256"] == text(a, "sourceSha256")?
                && vid["sha256"] == text(v, "sourceSha256")?
                && rid["sha256"] == text(a, "referenceSha256")?,
            "Fixture identity changed",
        )?;
        let ao = origin(a, true)?;
        let vo = origin(v, false)?;
        let total = number(a, "decodedFrames")?;
        validate_audio_metadata(&json_file(&local(&root, text(a, "metadata")?)?)?, ao, total)?;
        let timeline = AudioTimeline::new(ao, vo, 48000, total)?;
        report["timeline"] = json!(timeline.diagnostics());
        report["audioSource"] = aid.clone();
        report["videoSource"] = vid.clone();
        report["pcmReference"] = rid.clone();
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
        let transfer = AudioTransfer::new(timeline.clone(), 2_200_000_000, config)?;
        let output = WasapiOutput::open_pcm_muted_event()?;
        report["device"] = json!(output.info());
        let mut adapter = AudioPreviewAdapter::new(transfer, output)?;
        let mut captures = Vec::new();
        let work = (|| {
            for (index, (start, frames)) in [
                (2_200_000_000u64, 48000u64),
                (7_123_000_000, 48000),
                (0, 14400),
                (12_000_000_000, 14400),
            ]
            .into_iter()
            .enumerate()
            {
                if index > 0 {
                    let before = adapter.snapshot();
                    let before_clock = clock(&adapter)?;
                    let old = before
                        .cursor
                        .pending
                        .ok_or("Seek requires an old pending packet")?;
                    need(
                        before_clock["paddingFrames"]
                            .as_u64()
                            .is_some_and(|n| n > 0),
                        "Seek test requires queued device data",
                    )?;
                    adapter.seek(start)?;
                    let after = adapter.snapshot();
                    let reset = clock(&adapter)?;
                    need(
                        after.phase == AdapterPhase::Preparing
                            && !after.device_running
                            && reset["position"] == "0"
                            && reset["paddingFrames"] == 0,
                        "Seek did not reset device to stopped Preparing",
                    )?;
                    need(
                        adapter.reject_stale_ticket(old)?,
                        "Old queued ticket survived device seek",
                    )?;
                    report["seeks"].as_array_mut().unwrap().push(json!({"targetNs":start.to_string(),"before":before,"beforeClock":before_clock,"after":after,"resetClock":reset,"oldTicket":old,"staleRejected":true,"evidence":adapter.evidence()}));
                }
                let mut entry = json!({"ok":false});
                let played = segment(&mut adapter, start, frames, index == 0, &mut entry);
                match played {
                    Ok(samples) => {
                        captures.push((index, start, samples));
                        report["segments"].as_array_mut().unwrap().push(entry);
                    }
                    Err(error) => {
                        entry["error"] = error.clone().into();
                        report["segments"].as_array_mut().unwrap().push(entry);
                        return Err(error);
                    }
                }
            }
            let before = adapter.snapshot();
            let before_clock = clock(&adapter)?;
            let old = before
                .cursor
                .pending
                .ok_or("Cancel test requires pending packet")?;
            need(
                before_clock["paddingFrames"]
                    .as_u64()
                    .is_some_and(|n| n > 0),
                "Cancel test requires device queue contents",
            )?;
            adapter.cancel()?;
            let state = adapter.snapshot();
            let after_clock = clock(&adapter)?;
            thread::sleep(Duration::from_millis(150));
            let frozen = clock(&adapter)?;
            need(
                state.phase == AdapterPhase::Cancelled
                    && !state.device_running
                    && after_clock["position"] == "0"
                    && after_clock["paddingFrames"] == 0
                    && after_clock["position"] == frozen["position"]
                    && frozen["paddingFrames"] == 0,
                "Cancellation left a live clock or queue",
            )?;
            let stale = adapter.reject_stale_ticket(old).is_err() && adapter.snapshot() == state;
            let rejected = adapter.start().is_err()
                && adapter.prepare_tick(4800).is_err()
                && adapter.pump(4800).is_err();
            adapter.cancel()?;
            need(
                stale && rejected,
                "Cancelled adapter accepted a late or new operation",
            )?;
            report["cancel"] = json!({"before":before,"beforeClock":before_clock,"after":state,"resetClock":after_clock,"afterWaitClock":frozen,"milliseconds":150,"oldTicketRejected":stale,"commandsRejected":rejected,"idempotent":true,"evidence":adapter.evidence()});
            Ok::<(), String>(())
        })();
        if let Err(error) = &work {
            let cleanup = adapter.cancel();
            report["failureCleanup"] = json!({"error":cleanup.as_ref().err(),"state":adapter.snapshot(),"evidence":adapter.evidence()});
            report["error"] = error.clone().into();
        }
        drop(adapter);
        // Reference reads, hashing and file writes are outside the live device
        // loop. Accepted data is bounded to two seconds per diagnostic segment.
        for (index, start, samples) in captures {
            let wanted = oracle(&timeline, start, (samples.len() / 2) as u64, &reference)?;
            let exact = samples == wanted;
            report["segments"][index]["capture"] =
                save_capture(&run, &format!("accepted-{index}.f32le"), &samples)?;
            report["segments"][index]["expectedSha256"] = sample_hash(&wanted).into();
            report["segments"][index]["ok"] = exact.into();
            need(
                exact,
                "Adapter accepted PCM does not match the new timeline anchor",
            )?;
        }
        work?;
        need(
            identity(&audio)? == aid && identity(&video)? == vid && identity(&reference)? == rid,
            "Sources changed during device session",
        )?;
        Ok::<(), String>(())
    })();
    report["ok"] = result.is_ok().into();
    report["functionalPassed"] = result.is_ok().into();
    report["realtimePerformanceQualified"] = false.into();
    if let Err(error) = &result {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-audio-session-validation.json");
    let mut temp = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    let write = serde_json::to_writer_pretty(&mut temp, &report)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            temp.write_all(b"\n")
                .and_then(|_| temp.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = write {
        let kept = temp.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{error}; retained={kept:?}"));
    }
    if let Err(error) = temp.persist_noclobber(&path) {
        let reason = error.error.to_string();
        let kept = error.file.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{reason}; retained={kept:?}"));
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"error":report.get("error")})
    );
    result
}
