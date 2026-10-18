//! Bounded offline decode + real muted-session PCM submission. No video window.

#[path = "decoded_media.rs"]
mod decoded_media;
// Shared probe modules expose additional entry points used by the clock example.
#[cfg(all(windows, feature = "native-queue-probe"))]
#[path = "async_renderer.rs"]
mod async_renderer;
#[cfg(all(windows, feature = "native-d3d-probe"))]
#[path = "d3d11_renderer.rs"]
mod d3d11_renderer;
#[cfg(all(windows, feature = "native-d3d-probe"))]
#[path = "frame_verification.rs"]
mod frame_verification;
#[allow(dead_code)]
#[path = "presentation_clock.rs"]
mod presentation_clock;
#[cfg(windows)]
#[allow(dead_code)]
#[path = "wasapi_output.rs"]
mod wasapi_output;

#[cfg(windows)]
mod probe {
    use super::{decoded_media::*, presentation_clock::*, wasapi_output::*};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        thread,
        time::{Duration, Instant, UNIX_EPOCH},
    };

    struct FrameOutput {
        #[cfg(feature = "native-d3d-probe")]
        renderer: Option<super::d3d11_renderer::D3d11Renderer>,
        #[cfg(feature = "native-queue-probe")]
        worker: Option<super::async_renderer::AsyncRenderer>,
        #[cfg(feature = "native-queue-probe")]
        queued_frames: Vec<std::sync::Arc<[u8]>>,
        #[cfg(feature = "native-queue-probe")]
        render_epoch: u64,
        #[cfg(feature = "native-d3d-probe")]
        rgb: Vec<u8>,
        #[cfg(feature = "native-d3d-probe")]
        width: u32,
        #[cfg(feature = "native-d3d-probe")]
        height: u32,
    }
    impl FrameOutput {
        fn new(
            enabled: bool,
            async_delay_ms: Option<u64>,
            decoded: &DecodedMedia,
            run: &Path,
            report: &mut Value,
        ) -> Result<Self, String> {
            #[cfg(not(feature = "native-queue-probe"))]
            let _ = async_delay_ms;
            #[cfg(feature = "native-d3d-probe")]
            {
                if enabled {
                    use super::{
                        d3d11_renderer::D3d11Renderer,
                        frame_verification::{calibration_rgb, verify_readback},
                    };
                    let calibration = calibration_rgb(161, 91)?;
                    let mut calibration_renderer = D3d11Renderer::new(161, 91)?;
                    let result = calibration_renderer.render_rgb(&calibration)?;
                    let verification = verify_readback(&calibration, &result.rgba, 161, 91)?;
                    report["calibration"] = json!({"width":161,"height":91,"renderer":calibration_renderer.info(),
                        "rowPitch":result.row_pitch,"uploadDrawReadbackMs":result.upload_draw_readback_ms,"readbackWaitMs":result.readback_wait_ms,
                        "inputRgbSha256":verification.input_rgb_sha256,"expectedRgbaSha256":verification.expected_rgba_sha256,
                        "actualRgbaSha256":verification.actual_rgba_sha256,"mismatchedPixels":verification.mismatched_pixels,
                        "maxChannelError":verification.max_channel_error,"alphaErrors":verification.alpha_errors,"exact":verification.exact});
                    write_new(&run.join("calibration.rgb"), &calibration)?;
                    write_new(&run.join("calibration-rendered.rgba"), &result.rgba)?;
                    require(
                        verification.exact,
                        "D3D calibration draw did not reproduce the asymmetric source pixels",
                    )?;
                    drop(calibration_renderer);
                    let rgb = fs::read(&decoded.video_path).map_err(|e| e.to_string())?;
                    let frame_bytes =
                        decoded.video_width as usize * decoded.video_height as usize * 3;
                    require(
                        rgb.len() == frame_bytes * decoded.video.len(),
                        "Decoded video byte length changed before GPU upload",
                    )?;
                    for (bytes, frame) in rgb.chunks_exact(frame_bytes).zip(&decoded.video) {
                        require(
                            format!("{:x}", Sha256::digest(bytes)) == frame.sha256,
                            "Decoded frame bytes changed before GPU upload",
                        )?;
                    }
                    #[cfg(feature = "native-queue-probe")]
                    if let Some(delay_ms) = async_delay_ms {
                        let queued_frames = rgb
                            .chunks_exact(frame_bytes)
                            .map(std::sync::Arc::<[u8]>::from)
                            .collect();
                        let worker = super::async_renderer::AsyncRenderer::start(
                            decoded.video_width,
                            decoded.video_height,
                            delay_ms,
                        )?;
                        let render_epoch = worker.advance_epoch()?;
                        report["renderer"] =
                            serde_json::to_value(worker.info()).map_err(|e| e.to_string())?;
                        report["renderingScope"] = json!({"offscreen":true,"hardwareRequired":true,"asyncWorker":true,"queueCapacity":2,
                            "inFlightLimit":1,"latestCompletionLimit":1,"artificialRenderDelayMs":delay_ms,
                            "swapChain":false,"presentCalled":false,"visibleWindow":false,
                            "fixtureDecodedFullyBeforePlayback":true,"timing":"Worker upload/draw/readback duration; not display latency"});
                        return Ok(Self {
                            renderer: None,
                            worker: Some(worker),
                            queued_frames,
                            render_epoch,
                            rgb: Vec::new(),
                            width: decoded.video_width,
                            height: decoded.video_height,
                        });
                    }
                    let renderer = D3d11Renderer::new(decoded.video_width, decoded.video_height)?;
                    report["renderer"] =
                        serde_json::to_value(renderer.info()).map_err(|e| e.to_string())?;
                    report["renderingScope"] = json!({"offscreen":true,"hardwareRequired":true,"swapChain":false,"presentCalled":false,"visibleWindow":false,
                        "readbackEachUniqueSelectedFrame":true,"timing":"CPU upload/draw/flush/wait/map/readback duration, not GPU timestamp or display latency"});
                    return Ok(Self {
                        renderer: Some(renderer),
                        #[cfg(feature = "native-queue-probe")]
                        worker: None,
                        #[cfg(feature = "native-queue-probe")]
                        queued_frames: Vec::new(),
                        #[cfg(feature = "native-queue-probe")]
                        render_epoch: 0,
                        rgb,
                        width: decoded.video_width,
                        height: decoded.video_height,
                    });
                }
                Ok(Self {
                    renderer: None,
                    #[cfg(feature = "native-queue-probe")]
                    worker: None,
                    #[cfg(feature = "native-queue-probe")]
                    queued_frames: Vec::new(),
                    #[cfg(feature = "native-queue-probe")]
                    render_epoch: 0,
                    rgb: Vec::new(),
                    width: 0,
                    height: 0,
                })
            }
            #[cfg(not(feature = "native-d3d-probe"))]
            {
                let _ = (decoded, run, report, async_delay_ms);
                require(!enabled, "D3D probe feature is not enabled")?;
                Ok(Self {})
            }
        }
        fn render(&mut self, index: usize, pts_ns: u64) -> Result<Option<Value>, String> {
            #[cfg(feature = "native-queue-probe")]
            if let Some(worker) = self.worker.as_mut() {
                let frame = super::async_renderer::Frame {
                    index,
                    pts_ns,
                    rgb: self
                        .queued_frames
                        .get(index)
                        .ok_or("Missing async source frame")?
                        .clone(),
                };
                let submitted_at = Instant::now();
                let status = worker.submit(self.render_epoch, frame);
                let submit_ms = submitted_at.elapsed().as_secs_f64() * 1000.0;
                require(
                    !matches!(
                        status,
                        super::async_renderer::SubmitStatus::Closed
                            | super::async_renderer::SubmitStatus::Failed
                    ),
                    "Renderer worker stopped accepting frames",
                )?;
                return Ok(Some(
                    json!({"queuedFrame":true,"index":index,"ptsNs":pts_ns.to_string(),"epoch":self.render_epoch,"status":format!("{status:?}"),"submitMs":submit_ms,"screenPresented":false}),
                ));
            }
            let _ = pts_ns;
            #[cfg(feature = "native-d3d-probe")]
            {
                use super::frame_verification::verify_readback;
                let Some(renderer) = self.renderer.as_mut() else {
                    return Ok(None);
                };
                let frame_bytes = self.width as usize * self.height as usize * 3;
                let begin = index
                    .checked_mul(frame_bytes)
                    .ok_or("Frame offset overflow")?;
                let rgb = self
                    .rgb
                    .get(begin..begin + frame_bytes)
                    .ok_or("Missing decoded frame bytes")?;
                let result = renderer.render_rgb(rgb)?;
                let verification = verify_readback(rgb, &result.rgba, self.width, self.height)?;
                Ok(Some(
                    json!({"index":index,"offscreenDrawCompleted":true,"screenPresented":false,
                    "rowPitch":result.row_pitch,"uploadDrawReadbackMs":result.upload_draw_readback_ms,"readbackWaitMs":result.readback_wait_ms,
                    "inputRgbSha256":verification.input_rgb_sha256,"expectedRgbaSha256":verification.expected_rgba_sha256,"actualRgbaSha256":verification.actual_rgba_sha256,
                    "mismatchedPixels":verification.mismatched_pixels,"maxChannelError":verification.max_channel_error,"alphaErrors":verification.alpha_errors,"exact":verification.exact}),
                ))
            }
            #[cfg(not(feature = "native-d3d-probe"))]
            {
                let _ = index;
                Ok(None)
            }
        }
        fn poll(&mut self, decoded: &DecodedMedia) -> Result<Option<Value>, String> {
            #[cfg(not(feature = "native-queue-probe"))]
            let _ = decoded;
            #[cfg(feature = "native-queue-probe")]
            if let Some(worker) = self.worker.as_mut() {
                worker.health()?;
                if let Some(completed) = worker.poll_latest() {
                    require(
                        completed.epoch == self.render_epoch,
                        "A stale rendered frame escaped epoch validation",
                    )?;
                    let v = completed.verification;
                    require(v.exact, "Asynchronous GPU readback changed decoded pixels")?;
                    let source = decoded
                        .video
                        .get(completed.index)
                        .ok_or("Completed GPU frame has an invalid source index")?;
                    require(
                        completed.pts_ns == source.pts_ns && v.input_rgb_sha256 == source.sha256,
                        "Completed GPU frame does not match its decoded source PTS/hash",
                    )?;
                    return Ok(Some(
                        json!({"epoch":completed.epoch,"index":completed.index,"ptsNs":completed.pts_ns.to_string(),"exact":v.exact,
                        "inputRgbSha256":v.input_rgb_sha256,"expectedRgbaSha256":v.expected_rgba_sha256,"actualRgbaSha256":v.actual_rgba_sha256,
                        "mismatchedPixels":v.mismatched_pixels,"alphaErrors":v.alpha_errors,"maxChannelError":v.max_channel_error,
                        "rowPitch":completed.result.row_pitch,"uploadDrawReadbackMs":completed.result.upload_draw_readback_ms,
                        "readbackWaitMs":completed.result.readback_wait_ms,"offscreenDrawCompleted":true,"screenPresented":false}),
                    ));
                }
            }
            Ok(None)
        }
        fn invalidate(&mut self) -> Result<Option<Value>, String> {
            #[cfg(feature = "native-queue-probe")]
            if let Some(worker) = self.worker.as_mut() {
                let old = self.render_epoch;
                self.render_epoch = worker.advance_epoch()?;
                let rejected = worker.submit(
                    old,
                    super::async_renderer::Frame {
                        index: 0,
                        pts_ns: 0,
                        rgb: self.queued_frames[0].clone(),
                    },
                );
                require(
                    matches!(
                        rejected,
                        super::async_renderer::SubmitStatus::Stale
                            | super::async_renderer::SubmitStatus::Busy
                    ),
                    "Old render submission unexpectedly accepted",
                )?;
                require(
                    worker.poll_latest().is_none(),
                    "A previous completion remained after render epoch changed",
                )?;
                return Ok(Some(
                    json!({"oldEpoch":old,"newEpoch":self.render_epoch,"oldSubmission":format!("{rejected:?}"),"stats":worker.stats()?}),
                ));
            }
            Ok(None)
        }
        fn finish(&mut self) -> Result<Option<Value>, String> {
            #[cfg(feature = "native-queue-probe")]
            if let Some(worker) = self.worker.as_mut() {
                let shutdown = worker.shutdown()?;
                return Ok(Some(
                    serde_json::to_value(shutdown).map_err(|e| e.to_string())?,
                ));
            }
            Ok(None)
        }
    }
    #[cfg(feature = "native-d3d-probe")]
    fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())
    }

    fn require(ok: bool, message: &str) -> Result<(), String> {
        if ok { Ok(()) } else { Err(message.into()) }
    }
    fn fingerprint(path: &Path) -> Result<Value, String> {
        let info = fs::metadata(path).map_err(|e| e.to_string())?;
        require(
            info.is_file() && info.len() <= 64 * 1024 * 1024,
            "Probe source must be a bounded regular file",
        )?;
        Ok(
            json!({"path":path,"size":info.len(),"modifiedNs":info.modified().map_err(|e|e.to_string())?.duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos().to_string(),"sha256":format!("{:x}",Sha256::digest(fs::read(path).map_err(|e|e.to_string())?))}),
        )
    }
    fn frame_at_time(ns: u64, rate: u32) -> Result<usize, String> {
        usize::try_from(u128::from(ns) * u128::from(rate) / 1_000_000_000)
            .map_err(|_| "PCM source offset overflow".into())
    }
    fn video_due(frames: &[DecodedVideoFrame], time_ns: u64) -> Option<usize> {
        frames
            .partition_point(|frame| frame.pts_ns <= time_ns)
            .checked_sub(1)
    }
    fn output_reached_end(
        all_source_submitted: bool,
        padding: u32,
        clock_ns: u64,
        source_end_ns: u64,
    ) -> bool {
        all_source_submitted && padding == 0 && clock_ns >= source_end_ns
    }
    fn validate_fixture_video(frames: &[DecodedVideoFrame]) -> Result<(), String> {
        require(
            frames.len() == 120,
            "Expected the generated 120-frame fixture",
        )?;
        require(
            frames.iter().enumerate().all(|(index, frame)| {
                frame.pts_ns.abs_diff(index as u64 * 1_000_000_000 / 30) <= 1
            }),
            "Decoded fixture PTS do not cover the expected four seconds at 30fps",
        )
    }
    fn pcm_input_hash(pcm: &DecodedPcm, start: usize, end: usize) -> Result<String, String> {
        require(
            start <= end && end <= pcm.frame_count as usize,
            "Invalid source range for PCM integrity",
        )?;
        let mut hash = Sha256::new();
        for sample in
            &pcm.samples[start * usize::from(pcm.channels)..end * usize::from(pcm.channels)]
        {
            hash.update(sample.to_le_bytes());
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    fn raw(sample: &ClockSample, phase: &str, elapsed: Duration) -> Value {
        json!({"phase":phase,"elapsedMs":elapsed.as_secs_f64()*1000.0,"position":sample.position.to_string(),"frequency":sample.frequency,"qpc100ns":sample.qpc_100ns.to_string(),"hresult":sample.hresult,"reliable":sample.reliable})
    }
    fn reliable(
        output: &WasapiOutput,
        phase: &str,
        records: &mut Vec<Value>,
    ) -> Result<ClockSample, String> {
        let start = Instant::now();
        for _ in 0..8 {
            let sample = output.sample()?;
            require(records.len() < 4096, "Observation trace capacity exceeded")?;
            records.push(raw(&sample, phase, start.elapsed()));
            if sample.reliable {
                return Ok(sample);
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err("No reliable device-clock checkpoint in eight reads".into())
    }
    struct Submission {
        start_frame: usize,
        cursor: usize,
        frames: u64,
        nonzero_samples: u64,
        input_hash: Sha256,
    }
    impl Submission {
        fn new(start_frame: usize) -> Self {
            Self {
                start_frame,
                cursor: start_frame,
                frames: 0,
                nonzero_samples: 0,
                input_hash: Sha256::new(),
            }
        }
        fn summary(&self) -> Value {
            json!({"sourceStartFrame":self.start_frame,"sourceNextFrame":self.cursor,"submittedFrames":self.frames,"nonzeroSubmittedInputSamples":self.nonzero_samples,"submittedF32InputSha256":format!("{:x}",self.input_hash.clone().finalize())})
        }
    }
    fn pump(
        output: &mut WasapiOutput,
        pcm: &DecodedPcm,
        submission: &mut Submission,
    ) -> Result<u32, String> {
        let channels = usize::from(pcm.channels);
        let end = (submission.cursor + output.info().buffer_frames as usize)
            .min(pcm.frame_count as usize);
        let input = &pcm.samples[submission.cursor * channels..end * channels];
        if input.is_empty() {
            return Ok(0);
        }
        let written = output.write_pcm(input)?;
        require(
            written as usize <= end - submission.cursor,
            "Output consumed more PCM than supplied",
        )?;
        for sample in &input[..written as usize * channels] {
            submission.input_hash.update(sample.to_le_bytes());
            if *sample != 0.0 {
                submission.nonzero_samples += 1;
            }
        }
        submission.cursor += written as usize;
        submission.frames += u64::from(written);
        Ok(written)
    }
    fn play_until(
        output: &mut WasapiOutput,
        decoded: &DecodedMedia,
        video_output: &mut FrameOutput,
        mapper: &mut AudioTimelineClock,
        epoch: ClockEpoch,
        submission: &mut Submission,
        source_stop_ns: Option<u64>,
        label: &str,
        records: &mut Vec<Value>,
    ) -> Result<Value, String> {
        let start = Instant::now();
        let mut next_sample = Duration::ZERO;
        let mut last = reliable(output, &format!("{label}-initial"), records)?;
        let mut last_progress = start;
        let mut selected = None;
        let mut selected_count = 0u64;
        let mut skipped = 0u64;
        let mut underrun_observations = 0u64;
        let mut completed_render_count = 0u64;
        let mut last_pump = start;
        let mut max_pump_gap = Duration::ZERO;
        let mut max_pump_duration = Duration::ZERO;
        let mut max_reliable_sample_gap = Duration::ZERO;
        let mut final_source_ns;
        loop {
            require(
                start.elapsed() < Duration::from_secs(8),
                "PCM playback/drain exceeded eight seconds",
            )?;
            require(
                last_progress.elapsed() < Duration::from_secs(2),
                "Device clock stopped advancing",
            )?;
            require(
                last.wall.elapsed() < Duration::from_millis(250),
                "Reliable clock coverage exceeded 250ms",
            )?;
            let before = output.padding_frames()?;
            if before == 0
                && submission.cursor < decoded.audio.frame_count as usize
                && start.elapsed() > Duration::from_millis(100)
            {
                underrun_observations += 1;
            }
            let pump_started = Instant::now();
            max_pump_gap = max_pump_gap.max(pump_started.duration_since(last_pump));
            last_pump = pump_started;
            pump(output, &decoded.audio, submission)?;
            max_pump_duration = max_pump_duration.max(pump_started.elapsed());
            let padding = output.padding_frames()?;
            if start.elapsed() >= next_sample {
                let completed = video_output.poll(decoded)?;
                let observed = output.sample()?;
                let elapsed = start.elapsed();
                require(records.len() < 4096, "Observation trace capacity exceeded")?;
                records.push(raw(&observed, label, elapsed));
                let record = records.last_mut().unwrap();
                if let Some(rendered) = completed {
                    completed_render_count += 1;
                    record["completedRender"] = rendered;
                }
                record["paddingFrames"] = padding.into();
                record["submittedFrames"] = submission.frames.into();
                record["sourceNextFrame"] = submission.cursor.into();
                require(
                    observed.wall.saturating_duration_since(last.wall)
                        <= Duration::from_millis(250),
                    "A late observation concealed missing clock coverage",
                )?;
                if observed.reliable {
                    max_reliable_sample_gap = max_reliable_sample_gap
                        .max(observed.wall.saturating_duration_since(last.wall));
                    require(
                        observed.qpc_100ns >= last.qpc_100ns,
                        "Associated QPC regressed",
                    )?;
                    let mapped = mapper
                        .observe(epoch, observed.position, observed.frequency)
                        .map_err(|e| e.to_string())?;
                    if observed.position > last.position {
                        last_progress = Instant::now();
                    }
                    let source_duration_ns = u64::try_from(
                        u128::from(decoded.audio.frame_count) * 1_000_000_000
                            / u128::from(decoded.audio.sample_rate),
                    )
                    .map_err(|_| "Duration overflow")?;
                    final_source_ns = mapped.project_time_ns.min(source_duration_ns);
                    record["sourceTimeNs"] = final_source_ns.to_string().into();
                    record["unclampedClockTimeNs"] = mapped.project_time_ns.to_string().into();
                    record["clockBeyondEofNs"] = mapped
                        .project_time_ns
                        .saturating_sub(source_duration_ns)
                        .to_string()
                        .into();
                    record["epoch"] = epoch.value().into();
                    if let Some(index) = video_due(&decoded.video, final_source_ns) {
                        let frame = &decoded.video[index];
                        record["decodedVideoSelection"] = json!({"index":index,"ptsNs":frame.pts_ns.to_string(),"rgbSha256":frame.sha256,"duplicateObservation":selected==Some(index),"latenessNs":final_source_ns-frame.pts_ns});
                        if selected != Some(index) {
                            if let Some(rendered) = video_output.render(index, frame.pts_ns)? {
                                if rendered["queuedFrame"] == true {
                                    record["renderSubmission"] = rendered;
                                } else {
                                    let exact = rendered["exact"] == true;
                                    record["gpuReadback"] = rendered;
                                    require(
                                        exact,
                                        "Offscreen rendered frame differs from the decoded pixels",
                                    )?;
                                    let completion = output.sample()?;
                                    record["clockAfterRender"] =
                                        raw(&completion, label, start.elapsed());
                                    require(
                                        completion.reliable,
                                        "Clock after GPU readback was not reliable",
                                    )?;
                                    require(
                                        completion.wall.saturating_duration_since(observed.wall)
                                            <= Duration::from_millis(250),
                                        "GPU readback caused a gap in reliable device-clock coverage",
                                    )?;
                                    require(
                                        completion.qpc_100ns >= observed.qpc_100ns,
                                        "Device QPC regressed during GPU readback",
                                    )?;
                                    let after_render_padding = output.padding_frames()?;
                                    record["paddingAfterRender"] = after_render_padding.into();
                                    if after_render_padding == 0
                                        && submission.cursor < decoded.audio.frame_count as usize
                                    {
                                        underrun_observations += 1;
                                        record["audioQueueEmptyDuringRender"] = true.into();
                                    }
                                    let completion_position = mapper
                                        .observe(epoch, completion.position, completion.frequency)
                                        .map_err(|e| e.to_string())?;
                                    record["gpuCompletionSourceTimeNs"] =
                                        completion_position.project_time_ns.to_string().into();
                                    record["gpuCompletionMinusPtsNs"] = completion_position
                                        .project_time_ns
                                        .saturating_sub(frame.pts_ns)
                                        .to_string()
                                        .into();
                                }
                            }
                            skipped += selected
                                .map_or(0, |previous: usize| index.saturating_sub(previous + 1))
                                as u64;
                            selected_count += 1;
                            selected = Some(index);
                        }
                    }
                    last = observed;
                    let drained =
                        submission.cursor == decoded.audio.frame_count as usize && padding == 0;
                    if source_stop_ns.is_some_and(|end| final_source_ns >= end)
                        || (source_stop_ns.is_none()
                            && output_reached_end(
                                submission.cursor == decoded.audio.frame_count as usize,
                                padding,
                                mapped.project_time_ns,
                                source_duration_ns,
                            ))
                    {
                        require(
                            underrun_observations == 0,
                            "PCM queue became empty before all source frames were submitted",
                        )?;
                        return Ok(
                            json!({"phase":label,"elapsedMs":start.elapsed().as_secs_f64()*1000.0,"sourceTimeNs":final_source_ns.to_string(),
                            "lastPosition":last.position.to_string(),"queueDrained":drained,"clockReachedSourceEnd":mapped.project_time_ns >= source_duration_ns,"paddingFrames":padding,"submission":submission.summary(),
                            "underrunObservations":underrun_observations,"selectedDecodedFrames":selected_count,"skippedDueDecodedFrames":skipped,
                            "completedRenderCount":completed_render_count,"maxPumpGapMs":max_pump_gap.as_secs_f64()*1000.0,
                            "maxPumpDurationMs":max_pump_duration.as_secs_f64()*1000.0,"maxReliableSampleGapMs":max_reliable_sample_gap.as_secs_f64()*1000.0,
                            "lastVideoIndex":selected,"videoPresented":false,"sessionMuted":output.info().session_muted}),
                        );
                    }
                }
                next_sample = elapsed + Duration::from_millis(10);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn frozen(
        output: &mut WasapiOutput,
        label: &str,
        records: &mut Vec<Value>,
        milliseconds: u64,
    ) -> Result<Value, String> {
        // The caller stops audio and invalidates video before this observation
        // interval, so pending old frames cannot remain valid during pause.
        let before = reliable(output, &format!("{label}-before"), records)?;
        let padding = output.padding_frames()?;
        thread::sleep(Duration::from_millis(milliseconds));
        let after = reliable(output, &format!("{label}-after"), records)?;
        require(
            before.position == after.position && before.frequency == after.frequency,
            "Stopped PCM stream clock changed",
        )?;
        require(
            padding == output.padding_frames()?,
            "Stopped PCM queue changed",
        )?;
        Ok(
            json!({"milliseconds":milliseconds,"positionBefore":before.position.to_string(),"positionAfter":after.position.to_string(),"paddingFrames":padding,"frozen":true}),
        )
    }
    fn exercise(
        run: &Path,
        fixtures: &Path,
        enable_renderer: bool,
        async_delay_ms: Option<u64>,
        report: &mut Value,
        records: &mut Vec<Value>,
    ) -> Result<(), String> {
        let binaries = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let ffmpeg = binaries.join("ffmpeg-x86_64-pc-windows-msvc.exe");
        let ffprobe = binaries.join("ffprobe-x86_64-pc-windows-msvc.exe");
        let tools = [fingerprint(&ffmpeg)?, fingerprint(&ffprobe)?];
        require(
            tools[0]["sha256"]
                == "9752fd44c9e1867c8d26702ef81df1aad1442c1099888d02fed7c29de63aff39"
                && tools[1]["sha256"]
                    == "3e3dfe5c77e7f3b8f9de1c8ec2ccfea4ec1e9f34e930b6f918894c9d2838219a",
            "Packaged FFmpeg identity differs from the audited probe build",
        )?;
        report["tools"] = json!(tools);
        let original_video = fixtures.join("原始 视频;[literal].mp4");
        let original_audio = fixtures.join("原始 音频.wav");
        let originals = [fingerprint(&original_video)?, fingerprint(&original_audio)?];
        report["originalSources"] = json!(originals);
        let video = run.join("原始 视频;[literal].mp4");
        let audio = run.join("原始 音频.wav");
        fs::copy(&original_video, &video).map_err(|e| e.to_string())?;
        fs::copy(&original_audio, &audio).map_err(|e| e.to_string())?;
        let copies = [fingerprint(&video)?, fingerprint(&audio)?];
        require(
            copies[0]["sha256"] == originals[0]["sha256"]
                && copies[1]["sha256"] == originals[1]["sha256"],
            "Copied fixture contents differ from their originals",
        )?;
        report["sourceCopies"] = json!(copies);
        let output = WasapiOutput::open_pcm_muted()?;
        report["device"] = serde_json::to_value(output.info()).map_err(|e| e.to_string())?;
        require(
            output.info().session_muted,
            "Probe must mute only its isolated render session before submitting PCM",
        )?;
        let decoded = decode_fixture(
            &ffmpeg,
            &ffprobe,
            &video,
            &audio,
            run,
            output.info().sample_rate,
            output.info().channels,
        )?;
        require(
            decoded.audio.frame_count == u64::from(decoded.audio.sample_rate) * 4,
            "Expected the generated four-second audio fixture",
        )?;
        validate_fixture_video(&decoded.video)?;
        require(
            decoded.source_first_pts == 0,
            "Controlled fixture video must begin at source PTS zero",
        )?;
        require(
            decoded.audio.samples.iter().any(|value| *value != 0.0),
            "Decoded audio must include nonzero real samples",
        )?;
        report["decoded"] = json!({"audioFrames":decoded.audio.frame_count,"sampleRate":decoded.audio.sample_rate,"channels":decoded.audio.channels,
            "audioSha256":decoded.audio.sha256,"audioPath":decoded.audio.path,"videoFrames":decoded.video.len(),"videoWidth":decoded.video_width,"videoHeight":decoded.video_height,"videoPath":decoded.video_path,"probePath":decoded.probe_path,
            "audioProbePath":decoded.audio_probe_path,"sourceFirstPts":decoded.source_first_pts.to_string(),"sourceTimebaseNumerator":decoded.source_time_base_numerator,"sourceTimebaseDenominator":decoded.source_time_base_denominator,
            "firstVideoPtsNs":decoded.video[0].pts_ns.to_string(),"lastVideoPtsNs":decoded.video.last().unwrap().pts_ns.to_string()});
        let mut video_output =
            FrameOutput::new(enable_renderer, async_delay_ms, &decoded, run, report)?;
        // On any subsequent `?` error, this later owner drops/stops the audio
        // stream before the renderer's Drop joins its worker. An exceptional
        // GPU wait must never leave an unpumped audio stream alive behind it.
        let mut output = output;
        let mut mapper = AudioTimelineClock::new(30, 1).map_err(|e| e.to_string())?;
        let baseline = reliable(&output, "initial", records)?;
        require(
            baseline.position == 0,
            "New PCM stream position is not zero",
        )?;
        let epoch = mapper
            .rebase(0, baseline.frequency, 0)
            .map_err(|e| e.to_string())?;
        let mut submitted = Submission::new(0);
        pump(&mut output, &decoded.audio, &mut submitted)?;
        output.start()?;
        report["start"] = play_until(
            &mut output,
            &decoded,
            &mut video_output,
            &mut mapper,
            epoch,
            &mut submitted,
            Some(2_000_000_000),
            "start-to-two-seconds",
            records,
        )?;
        output.stop()?;
        report["renderPauseEpoch"] = video_output.invalidate()?.unwrap_or(Value::Null);
        report["pause"] = frozen(&mut output, "pause", records, 250)?;
        output.start()?;
        report["resume"] = play_until(
            &mut output,
            &decoded,
            &mut video_output,
            &mut mapper,
            epoch,
            &mut submitted,
            None,
            "resume-to-queue-drain",
            records,
        )?;
        require(
            submitted.frames == decoded.audio.frame_count,
            "The complete source PCM was not submitted exactly once",
        )?;
        require(
            submitted.nonzero_samples > 0,
            "No nonzero decoded sample reached the submission path",
        )?;
        require(
            format!("{:x}", submitted.input_hash.clone().finalize())
                == pcm_input_hash(&decoded.audio, 0, decoded.audio.frame_count as usize)?,
            "Submitted PCM sequence differs from the full decoded source",
        )?;
        report["fullPcmSubmissionVerified"] = true.into();
        output.stop()?;
        output.reset()?;
        report["renderSeekEpoch"] = video_output.invalidate()?.unwrap_or(Value::Null);
        let reset = reliable(&output, "seek-reset", records)?;
        require(
            reset.position == 0 && output.padding_frames()? == 0,
            "Reset did not flush old PCM and reset the clock",
        )?;
        let seek_epoch = mapper
            .rebase(0, reset.frequency, 45)
            .map_err(|e| e.to_string())?;
        require(
            mapper.observe(epoch, 0, reset.frequency).is_err(),
            "A stale decoder epoch was accepted after seek",
        )?;
        let seek_frame = frame_at_time(1_500_000_000, decoded.audio.sample_rate)?;
        let mut seek_submission = Submission::new(seek_frame);
        pump(&mut output, &decoded.audio, &mut seek_submission)?;
        output.start()?;
        report["seek"] = play_until(
            &mut output,
            &decoded,
            &mut video_output,
            &mut mapper,
            seek_epoch,
            &mut seek_submission,
            Some(2_500_000_000),
            "seek-source-1.5s",
            records,
        )?;
        require(
            format!("{:x}", seek_submission.input_hash.clone().finalize())
                == pcm_input_hash(&decoded.audio, seek_frame, seek_submission.cursor)?,
            "Seek submitted a different PCM source interval",
        )?;
        report["seekPcmSubmissionVerified"] = true.into();
        output.stop()?;
        report["renderCancelEpoch"] = video_output.invalidate()?.unwrap_or(Value::Null);
        report["cancel"] = frozen(&mut output, "cancel", records, 150)?;
        output.reset()?;
        let cancelled = reliable(&output, "cancel-reset", records)?;
        require(
            cancelled.position == 0 && output.padding_frames()? == 0,
            "Cancelled PCM remained queued",
        )?;
        let cancelled_epoch = mapper
            .rebase(0, cancelled.frequency, 0)
            .map_err(|e| e.to_string())?;
        require(
            mapper.observe(seek_epoch, 0, cancelled.frequency).is_err(),
            "Pre-cancellation time was accepted",
        )?;
        report["epochs"] = json!({"initial":epoch.value(),"seek":seek_epoch.value(),"cancel":cancelled_epoch.value(),"seekAudioSourceFrame":seek_frame,"cancelledPaddingFrames":output.padding_frames()?});
        drop(output);
        report["renderShutdown"] = video_output.finish()?.unwrap_or(Value::Null);
        if async_delay_ms.is_some() {
            require(
                ["start", "resume", "seek"].iter().all(|phase| {
                    report[*phase]["completedRenderCount"]
                        .as_u64()
                        .is_some_and(|count| count > 0)
                }),
                "A playback phase consumed no verified GPU results",
            )?;
            let shutdown = &report["renderShutdown"];
            require(
                shutdown["joined"] == true
                    && shutdown["error"].is_null()
                    && shutdown["stats"]["failures"] == 0,
                "Renderer did not shut down cleanly",
            )?;
            require(
                shutdown["stats"]["pendingDepth"] == 0
                    && shutdown["stats"]["completionPending"] == false
                    && shutdown["stats"]["inFlight"] == false,
                "Renderer shutdown left queued resources",
            )?;
            require(
                shutdown["stats"]["highWater"]
                    .as_u64()
                    .is_some_and(|n| n <= 2),
                "Frame queue exceeded its capacity",
            )?;
            require(
                shutdown["stats"]["completed"]
                    .as_u64()
                    .is_some_and(|n| n > 0),
                "No real GPU frame completed",
            )?;
            if async_delay_ms.unwrap_or(0) >= 100 {
                require(
                    shutdown["stats"]["evicted"].as_u64().is_some_and(|n| n > 0),
                    "Stress workload did not actually exercise queue backpressure",
                )?;
            }
        }
        drop(video_output);
        require(
            fingerprint(&original_video)? == originals[0]
                && fingerprint(&original_audio)? == originals[1]
                && fingerprint(&video)? == copies[0]
                && fingerprint(&audio)? == copies[1],
            "Source media changed during validation",
        )?;
        report["sourceIntegrityVerified"] = true.into();
        report["resourcesReleased"] = true.into();
        Ok(())
    }
    fn save_report(path: &Path, report: &Value) -> Result<(), String> {
        let mut file =
            tempfile::NamedTempFile::new_in(path.parent().ok_or("Invalid report directory")?)
                .map_err(|e| e.to_string())?;
        let write = serde_json::to_writer_pretty(&mut file, report)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                file.write_all(b"\n")
                    .and_then(|_| file.flush())
                    .and_then(|_| file.as_file().sync_all())
                    .map_err(|e| e.to_string())
            });
        if let Err(error) = write {
            return Err(match file.keep() {
                Ok((_, path)) => {
                    format!("{error}; incomplete report retained at {}", path.display())
                }
                Err(keep) => format!("{error}; retention failed: {}", keep.error),
            });
        }
        file.persist_noclobber(path).map(|_| ()).map_err(|failure| {
            let reason = failure.error.to_string();
            match failure.file.keep() {
                Ok((_, path)) => format!(
                    "{reason}; unpublished report retained at {}",
                    path.display()
                ),
                Err(keep) => format!("{reason}; retention failed: {}", keep.error),
            }
        })
    }
    pub fn main(enable_renderer: bool, async_mode: bool) -> Result<(), String> {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut output = None;
        let mut render_delay_ms = 120u64;
        let mut fixtures =
            manifest.join("target/workflow-validation/98ee1285-41b7-452a-bbce-e38c83b1a2aa");
        let mut args = std::env::args_os().skip(1);
        while let Some(key) = args.next() {
            let value = args.next().ok_or("Expected option value")?;
            if key == "--output-dir" && output.is_none() {
                output = Some(PathBuf::from(value));
            } else if key == "--fixture-dir" {
                fixtures = PathBuf::from(value);
            } else if key == "--render-delay-ms" && async_mode {
                render_delay_ms = value
                    .to_str()
                    .ok_or("Invalid renderer delay")?
                    .parse()
                    .map_err(|_| "Invalid renderer delay")?;
                require(
                    render_delay_ms <= 250,
                    "Renderer diagnostic delay must be 0..250ms",
                )?;
            } else {
                return Err("Usage: native_media_probe --output-dir <absolute target directory> [--fixture-dir <generated four-second fixtures>]".into());
            }
        }
        let output = output.ok_or("--output-dir is required")?;
        let lexical = manifest.join("target");
        require(
            output.is_absolute()
                && output.starts_with(&lexical)
                && !output
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "Output must stay inside this checkout target",
        )?;
        let target = lexical.canonicalize().map_err(|e| e.to_string())?;
        let ancestor = output
            .ancestors()
            .find(|part| part.exists())
            .ok_or("Missing output parent")?;
        require(
            ancestor
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&target),
            "Output escaped through a directory link",
        )?;
        fs::create_dir_all(&output).map_err(|e| e.to_string())?;
        let output = output.canonicalize().map_err(|e| e.to_string())?;
        require(output.starts_with(&target), "Output escaped target")?;
        let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&run).map_err(|e| e.to_string())?;
        let mut report = json!({"ok":false,"sourceIntegrityVerified":null,"createdAt":chrono::Utc::now().to_rfc3339(),"probe":"Decoded PCM / actual decoded video PTS / WASAPI output clock","schemaVersion":1,
            "limitations":["Only the owned audio session is muted; PCM is really copied into its render buffers.","Optional D3D output is offscreen Draw/readback only; no swap chain, Present or visible video window.","Queue drain is not proof of physical speaker drain or audible fidelity.","Offline short-fixture decoding does not establish streaming, VFR, long-media or multi-track support.","The application still uses its existing WebView player."]});
        let mut records = Vec::new();
        report["d3dRequested"] = enable_renderer.into();
        report["asyncRequested"] = async_mode.into();
        if async_mode {
            report["artificialRenderDelayMs"] = render_delay_ms.into();
        }
        if enable_renderer {
            report["probe"] = "Decoded RGB D3D11 offscreen rendering with WASAPI PCM clock".into();
        }
        let result = exercise(
            &run,
            &fixtures,
            enable_renderer,
            async_mode.then_some(render_delay_ms),
            &mut report,
            &mut records,
        );
        report["observations"] = json!(records);
        report["ok"] = result.is_ok().into();
        if let Err(error) = &result {
            report["error"] = error.clone().into();
        }
        let path = run.join(if async_mode {
            "native-queue-validation.json"
        } else if enable_renderer {
            "native-d3d-validation.json"
        } else {
            "native-media-validation.json"
        });
        if let Err(error) = save_report(&path, &report) {
            report["ok"] = false.into();
            report["reportSaveError"] = error.clone().into();
            eprintln!("{report}");
            return Err(error);
        }
        println!(
            "{}",
            json!({"ok":report["ok"],"report":path,"error":report.get("error"),"renderer":report.get("renderer"),"calibration":report.get("calibration"),"device":report.get("device"),"decoded":report.get("decoded"),"start":report.get("start"),"resume":report.get("resume"),"seek":report.get("seek"),"epochs":report.get("epochs"),"renderShutdown":report.get("renderShutdown")})
        );
        result
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn source_seek_uses_audio_sample_rate_not_device_units() {
            assert_eq!(frame_at_time(1_500_000_000, 48000).unwrap(), 72000);
            assert_eq!(frame_at_time(1_500_000_000, 44100).unwrap(), 66150);
        }
        #[test]
        fn empty_engine_queue_alone_does_not_complete_the_device_clock_tail() {
            assert!(!output_reached_end(true, 0, 3_984_666_666, 4_000_000_000));
            assert!(!output_reached_end(false, 0, 4_000_000_000, 4_000_000_000));
            assert!(!output_reached_end(true, 480, 4_000_000_000, 4_000_000_000));
            assert!(output_reached_end(true, 0, 4_000_000_000, 4_000_000_000));
        }
        #[test]
        fn video_selection_uses_nonuniform_decoded_pts() {
            let frames = vec![
                DecodedVideoFrame {
                    pts_ns: 0,
                    sha256: "a".into(),
                },
                DecodedVideoFrame {
                    pts_ns: 40_000_000,
                    sha256: "b".into(),
                },
                DecodedVideoFrame {
                    pts_ns: 100_000_000,
                    sha256: "c".into(),
                },
            ];
            assert_eq!(video_due(&frames, 39_999_999), Some(0));
            assert_eq!(video_due(&frames, 99_999_999), Some(1));
            assert_eq!(video_due(&frames, 100_000_000), Some(2));
            assert_eq!(video_due(&[], 0), None);
        }
        #[test]
        fn fixture_frame_count_does_not_hide_a_wrong_video_duration() {
            let frames: Vec<_> = (0..120)
                .map(|index| DecodedVideoFrame {
                    pts_ns: index * 1_000_000_000 / 60,
                    sha256: "decoded".into(),
                })
                .collect();
            assert!(validate_fixture_video(&frames).is_err());
            let correct: Vec<_> = (0..120)
                .map(|index| DecodedVideoFrame {
                    pts_ns: index * 1_000_000_000 / 30,
                    sha256: "decoded".into(),
                })
                .collect();
            assert!(validate_fixture_video(&correct).is_ok());
        }
    }
}
#[cfg(windows)]
#[allow(dead_code)] // Shared driver wrappers select either sync or async entry.
pub fn run(enable_renderer: bool) {
    if let Err(error) = probe::main(enable_renderer, false) {
        eprintln!("Native media probe failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(all(windows, feature = "native-queue-probe"))]
#[allow(dead_code)] // The media and D3D wrappers use the synchronous entry.
pub fn run_async() {
    if let Err(error) = probe::main(true, true) {
        eprintln!("Native queue probe failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
pub fn run_async() {
    eprintln!("Native queue probe requires Windows.");
    std::process::exit(1);
}
#[cfg(not(windows))]
pub fn run(_enable_renderer: bool) {
    eprintln!("Native media probe requires Windows WASAPI.");
    std::process::exit(1);
}
