//! Explicit, standalone diagnostic: silent WASAPI output plus synthetic PTS selection.
//! No Tauri window, capture device, application project, or video renderer is involved.

#[path = "support/presentation_clock.rs"]
mod presentation_clock;
#[cfg(windows)]
// The shared adapter also exposes PCM entry points exercised by native_media_probe.
#[allow(dead_code)]
#[path = "support/wasapi_output.rs"]
mod wasapi_output;

#[cfg(windows)]
mod probe {
    use super::{presentation_clock::*, wasapi_output::*};
    use serde_json::{Value, json};
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        thread,
        time::{Duration, Instant},
    };

    const FPS: u32 = 30;

    fn checked(condition: bool, message: &str) -> Result<(), String> {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    fn record_sample(
        records: &mut Vec<Value>,
        label: &str,
        elapsed: Duration,
        sample: &ClockSample,
    ) -> usize {
        records.push(json!({"phase":label,"elapsedMs":elapsed.as_secs_f64()*1000.0,
            "position":sample.position.to_string(),"frequency":sample.frequency,
            "qpc100ns":sample.qpc_100ns.to_string(),"hresult":sample.hresult,"reliable":sample.reliable}));
        records.len() - 1
    }

    fn record_checked_sample(
        records: &mut Vec<Value>,
        label: &str,
        elapsed: Duration,
        previous: &ClockSample,
        sample: &ClockSample,
    ) -> Result<usize, String> {
        let index = record_sample(records, label, elapsed, sample);
        if sample.reliable {
            let result = if sample.frequency != previous.frequency {
                Err("Device frequency changed without a new epoch".to_owned())
            } else if sample.position < previous.position {
                Err("Device position regressed".to_owned())
            } else if sample.qpc_100ns < previous.qpc_100ns {
                Err("Device-associated QPC regressed".to_owned())
            } else {
                Ok(())
            };
            if let Err(error) = result {
                records[index]["validationError"] = error.clone().into();
                return Err(error);
            }
        }
        Ok(index)
    }

    fn check_reliable_age(last: Instant, now: Instant) -> Result<(), String> {
        checked(
            now.saturating_duration_since(last) <= Duration::from_millis(250),
            "No reliable audio-clock observation within 250 ms; phase coverage is degraded",
        )
    }

    fn clock_sample(
        output: &WasapiOutput,
        label: &str,
        records: &mut Vec<Value>,
    ) -> Result<ClockSample, String> {
        // S_FALSE is successful but less accurate. Keep its distinction and
        // bound retries rather than using it as proof of precise device time.
        let start = Instant::now();
        for _ in 0..8 {
            let sample = output.sample().map_err(|error| {
                records.push(json!({"phase":label,"elapsedMs":start.elapsed().as_secs_f64()*1000.0,"sampleError":error}));
                error
            })?;
            record_sample(records, label, start.elapsed(), &sample);
            if sample.reliable {
                return Ok(sample);
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err("IAudioClock returned no S_OK sample within eight attempts".into())
    }

    fn info(output: &WasapiOutput) -> Value {
        let info = output.info();
        json!({
            "sampleRate": info.sample_rate, "channels": info.channels,
            "bitsPerSample": info.bits_per_sample, "blockAlign": info.block_align,
            "formatTag": info.format_tag, "bufferFrames": info.buffer_frames,
            "defaultPeriod100ns": info.default_period_100ns,
            "minimumPeriod100ns": info.minimum_period_100ns,
            "streamLatency100ns": info.stream_latency_100ns,
            "requestedBufferDuration100ns": info.requested_buffer_duration_100ns,
            "clockFrequency": info.clock_frequency,
        })
    }

    fn collect(
        output: &mut WasapiOutput,
        mapper: &mut AudioTimelineClock,
        epoch: ClockEpoch,
        milliseconds: u64,
        label: &str,
        records: &mut Vec<Value>,
    ) -> Result<Value, String> {
        let initial = clock_sample(output, &format!("{label}-initial"), records)?;
        let position = mapper
            .observe(epoch, initial.position, initial.frequency)
            .map_err(|e| format!("{e:?}"))?;
        let mut scheduler = FrameScheduler::new(
            epoch,
            position.project_frame,
            position
                .project_frame
                .checked_add(100_000)
                .ok_or("Scheduler frame range overflow")?,
        )
        .map_err(|e| format!("{e:?}"))?;
        let start = Instant::now();
        let mut next_sample = Duration::ZERO;
        let mut last = initial;
        let mut first_progress_ms = None;
        let mut written = 0u64;
        let mut unreliable = 0u64;
        let mut max_poll_ns = 0u128;
        let mut last_poll = start;
        let mut last_progress = start;
        let mut sampled = 0u64;
        while start.elapsed() < Duration::from_millis(milliseconds) {
            check_reliable_age(last.wall, Instant::now())?;
            checked(
                last_progress.elapsed() < Duration::from_secs(2),
                "Running output clock stalled for two seconds",
            )?;
            written += u64::from(output.fill_silence()?);
            if start.elapsed() >= next_sample {
                let sample = output.sample().map_err(|error| {
                    records.push(json!({"phase":label,"elapsedMs":start.elapsed().as_secs_f64()*1000.0,"sampleError":error}));
                    error
                })?;
                let elapsed = start.elapsed();
                let index = record_checked_sample(records, label, elapsed, &last, &sample)?;
                // Check the previous reliable age before accepting this sample,
                // so a delayed poll cannot conceal an unobserved long interval.
                check_reliable_age(last.wall, sample.wall)?;
                max_poll_ns =
                    max_poll_ns.max(sample.wall.saturating_duration_since(last_poll).as_nanos());
                last_poll = sample.wall;
                if !sample.reliable {
                    unreliable += 1;
                } else {
                    if sample.position > last.position {
                        first_progress_ms.get_or_insert(elapsed.as_secs_f64() * 1000.0);
                        last_progress = Instant::now();
                    }
                    checked(
                        last_progress.elapsed() < Duration::from_secs(2),
                        "Running output clock stalled for two seconds",
                    )?;
                    let mapped = mapper
                        .observe(epoch, sample.position, sample.frequency)
                        .map_err(|e| format!("{e:?}"))?;
                    let selected = scheduler.observe(&mapped).map_err(|e| format!("{e:?}"))?;
                    records[index]["epoch"] = epoch.value().into();
                    records[index]["projectFrame"] = mapped.project_frame.into();
                    records[index]["projectTimeNs"] = mapped.project_time_ns.to_string().into();
                    records[index]["subframeNumerator"] =
                        mapped.subframe_numerator.to_string().into();
                    records[index]["subframeDenominator"] =
                        mapped.subframe_denominator.to_string().into();
                    records[index]["syntheticVideo"] = selected.map(|item| json!({"frame":item.frame,
                            "ptsNs": (u128::from(item.frame)*1_000_000_000/u128::from(FPS)).to_string(),
                            "duplicateObservation":item.duplicate, "skippedPts":item.skipped_frames})).unwrap_or(Value::Null);
                    last = sample;
                    sampled += 1;
                }
                next_sample = elapsed + Duration::from_millis(10);
            }
            thread::sleep(Duration::from_millis(2));
        }
        check_reliable_age(last.wall, Instant::now())?;
        let wall_seconds = last
            .wall
            .saturating_duration_since(initial.wall)
            .as_secs_f64();
        let device_seconds = (last.position - initial.position) as f64 / last.frequency as f64;
        let qpc_seconds = (last.qpc_100ns - initial.qpc_100ns) as f64 / 10_000_000.0;
        checked(
            sampled > 10 && device_seconds > 0.0,
            "Not enough progressing device samples",
        )?;
        checked(
            first_progress_ms.is_some_and(|time| time < 2000.0),
            "Output clock never started",
        )?;
        let rate = device_seconds / wall_seconds;
        checked(
            (0.85..1.15).contains(&rate),
            "Device time did not approximately match elapsed wall time",
        )?;
        // QPC is paired with the observed audio device position; do not compare
        // it to raw QueryPerformanceCounter ticks without converting units.
        checked(
            (device_seconds - qpc_seconds).abs() < 0.1,
            "Device and paired-QPC elapsed times differ by over 100 ms",
        )?;
        let totals = scheduler.totals();
        Ok(
            json!({"phase":label, "durationRequestedMs":milliseconds, "sampleCount":sampled,
            "unreliableSamples":unreliable, "firstObservedProgressMs":first_progress_ms,
            "progressTimingOrigin":"Sampling-loop start after Start returned and an initial S_OK reading; not exact API-to-device startup latency.",
            "lastReliableAgeAtCompletionMs":Instant::now().saturating_duration_since(last.wall).as_secs_f64()*1000.0,
            "deviceElapsedSeconds":device_seconds, "wallElapsedSeconds":wall_seconds,
            "pairedQpcElapsedSeconds":qpc_seconds, "deviceToWallRate":rate,
            "maxObservedPollGapMs":max_poll_ns as f64/1_000_000.0, "silentFramesSubmitted":written,
            "syntheticSelectedFrames":totals.selected_frames, "syntheticSkippedPts":totals.skipped_frames,
            "duplicateObservations":totals.duplicate_observations,
            "evidence":"Video entries are synthetic due-PTS decisions only; no video decode/presentation occurred."}),
        )
    }

    fn pause_check(
        output: &mut WasapiOutput,
        label: &str,
        records: &mut Vec<Value>,
    ) -> Result<Value, String> {
        output.stop()?;
        let before = clock_sample(output, &format!("{label}-before"), records)?;
        thread::sleep(Duration::from_millis(250));
        let after = clock_sample(output, &format!("{label}-after"), records)?;
        checked(
            after.frequency == before.frequency,
            "Device frequency changed while stopped",
        )?;
        checked(
            after.qpc_100ns >= before.qpc_100ns,
            "Device-associated QPC regressed while stopped",
        )?;
        checked(
            after.position == before.position,
            "IAudioClient::Stop did not freeze stream clock",
        )?;
        Ok(
            json!({"pauseMs":250,"before":before.position.to_string(),"after":after.position.to_string(),
            "frequency":after.frequency,"frozen":true}),
        )
    }

    fn exercise(seconds: u64, report: &mut Value, samples: &mut Vec<Value>) -> Result<(), String> {
        let mut phases = Vec::new();
        let mut cycles = Vec::new();
        for cycle in 0..3 {
            let mut output = WasapiOutput::open()?;
            let metadata = info(&output);
            let mut mapper = AudioTimelineClock::new(FPS, 1).map_err(|e| format!("{e:?}"))?;
            let baseline = clock_sample(&output, &format!("cycle-{cycle}-baseline"), samples)?;
            checked(
                baseline.position == 0,
                "A newly initialized stream clock did not start at zero",
            )?;
            let epoch = mapper
                .rebase(baseline.position, baseline.frequency, 0)
                .map_err(|e| format!("{e:?}"))?;
            output.fill_silence()?;
            output.start()?;
            phases.push(collect(
                &mut output,
                &mut mapper,
                epoch,
                1500,
                &format!("cycle-{cycle}-start"),
                samples,
            )?);
            report["phases"] = json!(phases);
            let paused = pause_check(&mut output, &format!("cycle-{cycle}-pause"), samples)?;
            report["lastCompletedPause"] = paused.clone();
            output.fill_silence()?;
            output.start()?;
            phases.push(collect(
                &mut output,
                &mut mapper,
                epoch,
                1000,
                &format!("cycle-{cycle}-resume"),
                samples,
            )?);
            report["phases"] = json!(phases);
            output.stop()?;
            let before_reset =
                clock_sample(&output, &format!("cycle-{cycle}-before-reset"), samples)?;
            output.reset()?;
            let reset = clock_sample(&output, &format!("cycle-{cycle}-after-reset"), samples)?;
            checked(
                reset.position == 0,
                "IAudioClient::Reset did not reset clock to zero",
            )?;
            let new_epoch = mapper
                .rebase(reset.position, reset.frequency, 900)
                .map_err(|e| format!("{e:?}"))?;
            checked(
                mapper
                    .observe(epoch, before_reset.position, before_reset.frequency)
                    .is_err(),
                "Old seek epoch was accepted",
            )?;
            let mapped = mapper
                .observe(new_epoch, reset.position, reset.frequency)
                .map_err(|e| format!("{e:?}"))?;
            checked(
                mapped.project_frame == 900,
                "Reset/rebase did not select the new project position",
            )?;
            output.fill_silence()?;
            output.start()?;
            phases.push(collect(
                &mut output,
                &mut mapper,
                new_epoch,
                1000,
                &format!("cycle-{cycle}-seek"),
                samples,
            )?);
            report["phases"] = json!(phases);
            output.stop()?;
            cycles.push(json!({"cycle":cycle, "format":metadata, "pause":paused,
                "positionBeforeReset":before_reset.position.to_string(),"resetPosition":reset.position.to_string(),
                "oldEpoch":epoch.value(),"newEpoch":new_epoch.value(),"newProjectFrame":mapped.project_frame}));
            drop(output);
            report["cycles"] = json!(cycles);
            report["phases"] = json!(phases);
        }
        let mut output = WasapiOutput::open()?;
        report["continuousFormat"] = info(&output);
        let mut mapper = AudioTimelineClock::new(FPS, 1).map_err(|e| format!("{e:?}"))?;
        let baseline = clock_sample(&output, "continuous-baseline", samples)?;
        let epoch = mapper
            .rebase(baseline.position, baseline.frequency, 0)
            .map_err(|e| format!("{e:?}"))?;
        output.fill_silence()?;
        output.start()?;
        phases.push(collect(
            &mut output,
            &mut mapper,
            epoch,
            seconds * 1000,
            "continuous",
            samples,
        )?);
        report["phases"] = json!(phases);
        report["continuousPause"] = pause_check(&mut output, "continuous-pause", samples)?;
        output.reset()?;
        checked(
            clock_sample(&output, "continuous-after-reset", samples)?.position == 0,
            "Final stream reset did not return zero",
        )?;
        drop(output);
        report["cycles"] = json!(cycles);
        report["phases"] = json!(phases);
        report["resourcesReleased"] = true.into();
        Ok(())
    }

    fn preserve_temporary(file: tempfile::NamedTempFile, message: String) -> String {
        match file.keep() {
            Ok((_file, path)) => format!(
                "{message}; incomplete/unpublished evidence retained at {}",
                path.display()
            ),
            Err(error) => format!(
                "{message}; cannot retain temporary evidence: {}. Full report fallback is printed to stderr.",
                error.error
            ),
        }
    }

    fn save_report(path: &Path, report: &Value) -> Result<(), String> {
        let parent = path.parent().ok_or("Report path has no directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        if let Err(error) = serde_json::to_writer_pretty(&mut temporary, report) {
            return Err(preserve_temporary(
                temporary,
                format!("Cannot serialize report: {error}"),
            ));
        }
        if let Err(error) = temporary
            .write_all(b"\n")
            .and_then(|_| temporary.flush())
            .and_then(|_| temporary.as_file().sync_all())
        {
            return Err(preserve_temporary(
                temporary,
                format!("Cannot flush report: {error}"),
            ));
        }
        match temporary.persist_noclobber(path) {
            Ok(_) => Ok(()),
            Err(error) => Err(preserve_temporary(
                error.file,
                format!("Cannot publish report without overwriting: {}", error.error),
            )),
        }
    }

    pub fn main() -> Result<(), String> {
        let mut args = std::env::args_os().skip(1);
        let mut output = None;
        let mut seconds = 20u64;
        while let Some(key) = args.next() {
            let value = args.next().ok_or("Every option requires a value")?;
            if key == "--output-dir" && output.is_none() {
                output = Some(PathBuf::from(value));
            } else if key == "--seconds" {
                seconds = value
                    .to_str()
                    .ok_or("Invalid duration")?
                    .parse()
                    .map_err(|_| "Invalid duration")?;
            } else {
                return Err("Usage: native_clock_probe --output-dir <directory under src-tauri/target> [--seconds 2..60]".into());
            }
        }
        checked(
            (2..=60).contains(&seconds),
            "Continuous probe duration must be between 2 and 60 seconds",
        )?;
        let output = output.ok_or("--output-dir is required")?;
        checked(output.is_absolute(), "Output directory must be absolute")?;
        let lexical_target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
        checked(
            output.starts_with(&lexical_target)
                && !output
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
            "Output must remain under this checkout src-tauri/target",
        )?;
        let target = lexical_target.canonicalize().map_err(|e| e.to_string())?;
        let ancestor = output
            .ancestors()
            .find(|path| path.exists())
            .ok_or("Output has no existing ancestor")?;
        checked(
            ancestor
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&target),
            "Output ancestor escaped through a directory link",
        )?;
        fs::create_dir_all(&output).map_err(|e| e.to_string())?;
        let output = output.canonicalize().map_err(|e| e.to_string())?;
        checked(
            output.starts_with(&target),
            "Output escaped through a directory link",
        )?;
        let run_dir = output.join(format!("run-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&run_dir).map_err(|e| e.to_string())?;
        let mut report = json!({"ok":false,"probe":"WASAPI shared silent-render clock","schemaVersion":1,
            "createdAt":chrono::Utc::now().to_rfc3339(),"platform":std::env::consts::OS,
            "continuousSeconds":seconds,"samples":[],"cycles":[],"phases":[],
            "limitations":["No video decoder or D3D renderer is attached.","Synthetic PTS selection does not measure displayed pixels or audio/video skew.","IAudioClock is an output-stream observation, not a microphone loopback measurement of physical speaker latency.","This diagnostic does not enable a native backend in the KaiCut application."]});
        let mut samples = Vec::new();
        let result = exercise(seconds, &mut report, &mut samples);
        report["samples"] = json!(samples);
        report["unreliableSampleCountIncludingCheckpoints"] = report["samples"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item["reliable"] == false)
                    .count()
            })
            .unwrap_or(0)
            .into();
        report["ok"] = result.is_ok().into();
        if let Err(error) = &result {
            report["error"] = error.clone().into();
        }
        let report_path = run_dir.join("native-clock-validation.json");
        if let Err(error) = save_report(&report_path, &report) {
            report["ok"] = false.into();
            report["reportSaveError"] = error.clone().into();
            eprintln!("Native clock probe report could not be published; full evidence follows:");
            eprintln!("{report}");
            return Err(error);
        }
        println!(
            "{}",
            json!({"ok":report["ok"],"report":report_path,"error":report.get("error"),
            "sampleCount":report["samples"].as_array().map(Vec::len),"format":report.get("continuousFormat"),"phases":report["phases"]})
        );
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unreliable_tail_cannot_pass_as_complete_clock_coverage() {
            let start = Instant::now();
            assert!(check_reliable_age(start, start + Duration::from_millis(250)).is_ok());
            assert!(check_reliable_age(start, start + Duration::from_millis(251)).is_err());
            // A long phase with valid early samples still fails when its tail
            // has no recent reliable sample; no physical device is involved.
            assert!(
                check_reliable_age(
                    start + Duration::from_secs(5),
                    start + Duration::from_secs(6)
                )
                .is_err()
            );
        }

        #[test]
        fn regressed_raw_sample_is_preserved_before_validation_fails() {
            let wall = Instant::now();
            let previous = ClockSample {
                position: 100,
                frequency: 1_000,
                qpc_100ns: 10_000,
                reliable: true,
                hresult: 0,
                wall,
            };
            let invalid = ClockSample {
                position: 99,
                ..previous
            };
            let mut records = Vec::new();
            assert!(
                record_checked_sample(
                    &mut records,
                    "regression",
                    Duration::ZERO,
                    &previous,
                    &invalid
                )
                .is_err()
            );
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["position"], "99");
            assert_eq!(records[0]["reliable"], true);
            assert!(
                records[0]["validationError"]
                    .as_str()
                    .unwrap()
                    .contains("regressed")
            );
        }

        #[test]
        fn report_collision_preserves_both_existing_report_and_new_evidence() {
            let folder = tempfile::tempdir().unwrap();
            let destination = folder.path().join("report.json");
            fs::write(&destination, b"original report").unwrap();
            let evidence = json!({"ok":false,"samples":[{"position":"123"}]});
            assert!(save_report(&destination, &evidence).is_err());
            assert_eq!(fs::read(&destination).unwrap(), b"original report");
            let recovery = fs::read_dir(folder.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path != &destination)
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(recovery).unwrap()).unwrap(),
                evidence
            );
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = probe::main() {
        eprintln!("Native clock probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This diagnostic requires Windows WASAPI; it does not run the application.");
    std::process::exit(1);
}
