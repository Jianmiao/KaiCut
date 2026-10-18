//! Explicit device validation of streaming decode, not an application backend.
use super::{
    async_renderer::{AsyncRenderer, Frame, SubmitStatus},
    audio_scheduling::ThreadScheduling,
    hd_reference::HdReference,
    presentation_clock::{AudioTimelineClock, ClockEpoch},
    preview_session::{NativePreviewSession, SessionPhase},
    stream_reference::{reference_pcm_hash, reference_video_hash},
    streaming_decoder::{AudioBlock, SeekMode, StreamConfig, StreamingDecoder, VideoFrame},
    wasapi_output::{WaitOutcome, WasapiOutput},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum AudioScheduling {
    Poll,
    Event,
    EventMmcss,
}
impl AudioScheduling {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "poll" => Ok(Self::Poll),
            "event" => Ok(Self::Event),
            "event-mmcss" => Ok(Self::EventMmcss),
            _ => Err("Audio scheduling must be poll, event, or event-mmcss".into()),
        }
    }
}

#[derive(Default)]
struct PacingStats {
    waits: u64,
    event_signals: u64,
    event_timeouts: u64,
    max_wait: Duration,
}

const TRACE_LIMIT: usize = 16384;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum RendererLifetime {
    Scoped,
    Shared,
}
impl RendererLifetime {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "scoped" => Ok(Self::Scoped),
            "shared" => Ok(Self::Shared),
            _ => Err("Renderer lifetime must be scoped or shared".into()),
        }
    }
}

/// Own the GPU worker independently of each stopped audio/decoder session.
/// Every control flow closes it explicitly and Drop remains a fallback.
#[derive(Default)]
struct RendererOwner {
    active: Option<AsyncRenderer>,
    creations: u32,
    shutdown_calls: u32,
    events: Vec<Value>,
}
impl RendererOwner {
    fn acquire(&mut self, width: u32, height: u32) -> Result<&mut AsyncRenderer, String> {
        if self.active.is_none() {
            let started = Instant::now();
            let renderer = AsyncRenderer::start(width, height, 0)?;
            self.creations += 1;
            self.events
                .push(json!({"kind":"created","instanceId":renderer.instance_id(),
                "elapsedMs":started.elapsed().as_secs_f64()*1000.0,"info":renderer.info()}));
            self.active = Some(renderer);
        }
        let renderer = self.active.as_mut().ok_or("Missing renderer owner")?;
        renderer.health()?;
        require(
            renderer.info().width == width && renderer.info().height == height,
            "Cannot reuse a renderer with mismatched dimensions",
        )?;
        Ok(renderer)
    }
    fn shutdown(&mut self) -> Result<(), String> {
        if let Some(mut renderer) = self.active.take() {
            self.shutdown_calls += 1;
            let result = renderer.shutdown();
            self.events.push(
                json!({"kind":"shutdown","instanceId":renderer.instance_id(),
                "result":result.as_ref().ok(),"error":result.as_ref().err()}),
            );
            let closed = result?;
            require(
                closed.joined
                    && closed.error.is_none()
                    && closed.stats.failures == 0
                    && !closed.stats.in_flight
                    && closed.stats.pending_depth == 0
                    && !closed.stats.completion_pending,
                "Renderer owner did not shut down cleanly",
            )?;
        }
        Ok(())
    }
    fn report(&self) -> Value {
        json!({"creations":self.creations,"shutdownCalls":self.shutdown_calls,
            "active":self.active.is_some(),"events":self.events})
    }
}

fn validate_fixture_frame(index: usize, pts_ns: u64, total_ns: u64) -> Result<(), String> {
    require(
        (index as u64) < total_ns * 30 / 1_000_000_000,
        "Streamed fixture frame index is outside its source",
    )?;
    require(
        pts_ns == index as u64 * 1_000_000_000 / 30,
        "Streamed fixture PTS does not match its independently known source position",
    )
}

fn fingerprint(path: &Path) -> Result<Value, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let info = file.metadata().map_err(|e| e.to_string())?;
    require(info.is_file(), "Fixture must be a regular file")?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(
        json!({"path":path,"size":info.len(),"modified":format!("{:?}",info.modified().map_err(|e|e.to_string())?),"sha256":format!("{:x}",hash.finalize())}),
    )
}

// Audio and decoder belong to the session; GPU ownership lives in RendererOwner.
// The session is always dropped before the outer owner closes its GPU worker.
struct Playback<'renderer> {
    output: WasapiOutput,
    decoder: Option<StreamingDecoder>,
    renderer: &'renderer mut AsyncRenderer,
    session: NativePreviewSession,
    config: StreamConfig,
    mapper: AudioTimelineClock,
    clock_epoch: ClockEpoch,
    render_epoch: u64,
    running: bool,
    audio_scheduling: AudioScheduling,
    pending_audio: Option<AudioBlock>,
    audio_offset: usize,
    next_video: Option<VideoFrame>,
    current_video: Option<VideoFrame>,
    video_submit_pending: bool,
    last_verified_video: Option<usize>,
    selected_video: Option<usize>,
    first_video_ready: bool,
    source_start_frame: u64,
    source_next_frame: u64,
    submitted_hash: Sha256,
    total_frames: u64,
    total_ns: u64,
    records: Vec<Value>,
}

impl<'renderer> Playback<'renderer> {
    fn new(
        video: &Path,
        audio: &Path,
        duration_seconds: u64,
        audio_scheduling: AudioScheduling,
        seek_mode: SeekMode,
        renderer: &'renderer mut AsyncRenderer,
    ) -> Result<Self, String> {
        let output = if audio_scheduling == AudioScheduling::Poll {
            WasapiOutput::open_pcm_muted()?
        } else {
            WasapiOutput::open_pcm_muted_event()?
        };
        require(
            output.info().session_muted,
            "Owned audio session is not muted",
        )?;
        // Reference fixtures are deliberately narrow. Do not label untested
        // resampling/device matrices as validated by this one device run.
        require(
            output.info().sample_rate == 48000 && output.info().channels == 2,
            "This fixture acceptance requires a 48 kHz stereo endpoint",
        )?;
        let render_epoch = renderer.advance_epoch()?;
        let session = NativePreviewSession::new();
        let binaries = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let config = StreamConfig {
            ffmpeg: binaries.join("ffmpeg-x86_64-pc-windows-msvc.exe"),
            ffprobe: binaries.join("ffprobe-x86_64-pc-windows-msvc.exe"),
            video: video.into(),
            audio: audio.into(),
            sample_rate: 48000,
            channels: 2,
            video_width: renderer.info().width,
            video_height: renderer.info().height,
            epoch: session.epoch(),
            start_audio_frame: 0,
            start_video_ns: 0,
            seek_mode,
        };
        let decoder = StreamingDecoder::start(config.clone())?;
        let mut mapper = AudioTimelineClock::new(30, 1).map_err(|e| e.to_string())?;
        let sample = output.sample()?;
        require(
            sample.reliable && sample.position == 0,
            "New output clock must be reliable and zero",
        )?;
        let clock_epoch = mapper
            .rebase(0, sample.frequency, 0)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            output,
            decoder: Some(decoder),
            renderer,
            session,
            config,
            mapper,
            clock_epoch,
            render_epoch,
            running: false,
            audio_scheduling,
            pending_audio: None,
            audio_offset: 0,
            next_video: None,
            current_video: None,
            video_submit_pending: false,
            last_verified_video: None,
            selected_video: None,
            first_video_ready: false,
            source_start_frame: 0,
            source_next_frame: 0,
            submitted_hash: Sha256::new(),
            total_frames: duration_seconds * 48000,
            total_ns: duration_seconds * 1_000_000_000,
            records: Vec::new(),
        })
    }

    fn decoder(&self) -> Result<&StreamingDecoder, String> {
        self.decoder
            .as_ref()
            .ok_or_else(|| "Decoder session is closed".into())
    }

    fn pump_audio(&mut self) -> Result<(), String> {
        for _ in 0..16 {
            if self.output.padding_frames()? == self.output.info().buffer_frames {
                break;
            }
            if self.pending_audio.is_none() {
                self.pending_audio = self
                    .decoder
                    .as_mut()
                    .ok_or("Missing decoder")?
                    .poll_audio()?;
                self.audio_offset = 0;
            }
            let Some(block) = self.pending_audio.as_ref() else {
                break;
            };
            require(
                block.epoch == self.session.epoch(),
                "Stale PCM reached output owner",
            )?;
            require(
                block.samples.len() % 2 == 0 && block.samples.len() <= 960,
                "PCM block exceeded its declared frame bound",
            )?;
            require(
                block.source_start_frame + (self.audio_offset / 2) as u64 == self.source_next_frame,
                "PCM stream has a gap, duplicate or reversed source offset",
            )?;
            let written = self.output.write_pcm(&block.samples[self.audio_offset..])? as usize;
            require(
                self.source_next_frame + written as u64 <= self.total_frames,
                "Fixture decoded beyond its expected source end",
            )?;
            let end = self.audio_offset + written * 2;
            for sample in &block.samples[self.audio_offset..end] {
                self.submitted_hash.update(sample.to_le_bytes());
            }
            self.source_next_frame += written as u64;
            self.audio_offset = end;
            if end == block.samples.len() {
                self.pending_audio = None;
            }
            if written == 0 {
                break;
            }
        }
        Ok(())
    }

    fn video_due(&mut self, source_ns: u64) -> Result<(), String> {
        let mut latest = None;
        for _ in 0..8 {
            if self.next_video.is_none() {
                self.next_video = self
                    .decoder
                    .as_mut()
                    .ok_or("Missing decoder")?
                    .poll_video()?;
            }
            let Some(frame) = self.next_video.as_ref() else {
                break;
            };
            require(
                frame.epoch == self.session.epoch(),
                "Stale video reached output owner",
            )?;
            if frame.pts_ns > source_ns {
                break;
            }
            latest = self.next_video.take();
        }
        if let Some(frame) = latest {
            require(
                self.selected_video.is_none_or(|index| frame.index > index),
                "Source video index repeated or regressed",
            )?;
            self.selected_video = Some(frame.index);
            self.first_video_ready = true;
            self.current_video = Some(frame);
            self.video_submit_pending = true;
        }
        if self.video_submit_pending {
            let frame = self.current_video.as_ref().ok_or("Missing current frame")?;
            let status = self.renderer.submit(
                self.render_epoch,
                Frame {
                    index: frame.index,
                    pts_ns: frame.pts_ns,
                    rgb: frame.rgb.clone(),
                },
            );
            require(
                matches!(
                    status,
                    SubmitStatus::Queued | SubmitStatus::Evicted | SubmitStatus::Busy
                ),
                "Renderer rejected a current-session frame",
            )?;
            self.video_submit_pending = status == SubmitStatus::Busy;
        }
        Ok(())
    }

    fn prepare(&mut self) -> Result<Value, String> {
        let started = Instant::now();
        loop {
            require(
                started.elapsed() < Duration::from_secs(15),
                "Streaming preparation timed out",
            )?;
            self.pump_audio()?;
            self.video_due(self.config.start_video_ns)?;
            if self.output.padding_frames()? == self.output.info().buffer_frames {
                self.session
                    .mark_audio_ready(self.session.epoch())
                    .map_err(|e| e.to_string())?;
            }
            if self.first_video_ready {
                self.session
                    .mark_video_ready(self.session.epoch())
                    .map_err(|e| e.to_string())?;
            }
            if self.session.phase() == SessionPhase::Ready {
                let stats = self.decoder()?.stats()?;
                require(
                    stats.output_width == self.renderer.info().width
                        && stats.output_height == self.renderer.info().height,
                    "Decoder and renderer dimensions differ",
                )?;
                if self.renderer.info().width == 1920 {
                    require(
                        stats.source_width == 1920 && stats.source_height == 1080,
                        "HD source is not native 1920x1080",
                    )?;
                }
                require(
                    !stats.audio_eof && !stats.video_eof,
                    "Streaming preparation decoded the complete fixture before playback",
                )?;
                require(
                    stats.audio_source_frames < self.total_frames
                        && stats.video_source_frames < self.total_ns * 30 / 1_000_000_000,
                    "Decoder source counters show full decode before playback",
                )?;
                require(
                    stats.audio_high_water <= 8 && stats.video_high_water <= 4,
                    "Preparation exceeded decoder queue bounds",
                )?;
                return Ok(json!({"elapsedMs":started.elapsed().as_secs_f64()*1000.0,
                    "session":self.session.snapshot(),"decoder":stats,"device":self.output.info(),
                    "renderer":self.renderer.info(),"prefilledFrames":self.output.padding_frames()?,
                    "rendererInstanceId":self.renderer.instance_id(),"renderEpoch":self.render_epoch,
                    "firstSourceVideoIndex":self.selected_video,"fullDecodeBeforeStart":false}));
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn play_until(&mut self, end_ns: u64, expect_eof: bool, label: &str) -> Result<Value, String> {
        let mut scheduling =
            ThreadScheduling::enter(self.audio_scheduling == AudioScheduling::EventMmcss)?;
        let mut result = self.play_loop(end_ns, expect_eof, label);
        if result.is_err() && self.running {
            match self.output.stop() {
                Ok(()) => self.running = false,
                Err(stop) => {
                    let error = result.as_ref().err().unwrap();
                    result = Err(format!("{error}; stopping failed audio phase: {stop}"));
                }
            }
        }
        // Revert even after a failed phase; the guard also retries on Drop.
        let restored = scheduling.finish();
        require(
            self.records.len() < TRACE_LIMIT,
            "Streaming trace capacity exceeded",
        )?;
        self.records
            .push(json!({"kind":"threadScheduling","phase":label,
            "scheduling":scheduling.info(),"restoreError":restored.as_ref().err()}));
        match (result, restored) {
            (Ok(mut phase), Ok(())) => {
                phase["threadScheduling"] = json!(scheduling.info());
                Ok(phase)
            }
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(restore)) => {
                Err(format!("{error}; MMCSS restoration failed: {restore}"))
            }
        }
    }

    fn wait_for_audio(&mut self, stats: &mut PacingStats, label: &str) -> Result<(), String> {
        let started = Instant::now();
        stats.waits += 1;
        let outcome = if self.audio_scheduling == AudioScheduling::Poll {
            thread::sleep(Duration::from_millis(2));
            None
        } else {
            Some(self.output.wait_ready(Duration::from_millis(25))?)
        };
        let elapsed = started.elapsed();
        stats.max_wait = stats.max_wait.max(elapsed);
        match outcome {
            Some(WaitOutcome::Signaled) => stats.event_signals += 1,
            Some(WaitOutcome::TimedOut) => {
                stats.event_timeouts += 1;
                require(
                    self.records.len() < TRACE_LIMIT,
                    "Streaming trace capacity exceeded",
                )?;
                self.records
                    .push(json!({"kind":"audioWaitTimeout","phase":label,
                    "elapsedMs":elapsed.as_secs_f64()*1000.0,"timeoutMs":25}));
                return Err(
                    "WASAPI buffer event timed out; event mode did not fall back to polling".into(),
                );
            }
            None => {}
        }
        Ok(())
    }

    fn play_loop(&mut self, end_ns: u64, expect_eof: bool, label: &str) -> Result<Value, String> {
        self.session.play().map_err(|e| e.to_string())?;
        self.output.start()?;
        self.running = true;
        let started = Instant::now();
        let mut last_pump = started;
        let mut max_pump_gap = Duration::ZERO;
        let mut max_pump_call = Duration::ZERO;
        let mut max_video_call = Duration::ZERO;
        let mut max_stats_call = Duration::ZERO;
        let mut max_poll_call = Duration::ZERO;
        let mut max_sleep = Duration::ZERO;
        let mut pacing = PacingStats::default();
        let mut last_sample = self.output.sample()?;
        self.record_clock(&last_sample, label, "initial")?;
        require(
            last_sample.reliable,
            "Initial streaming clock sample was unreliable",
        )?;
        let mut max_clock_gap = Duration::ZERO;
        let mut next_sample = Duration::ZERO;
        let mut consumed = 0u64;
        let mut first_verified_video = None;
        let mut last_render_completion = started;
        let mut max_render_completion_gap = Duration::ZERO;
        let mut underruns = 0u64;
        loop {
            require(
                started.elapsed() < Duration::from_secs(self.total_ns / 1_000_000_000 + 8),
                "Streaming phase exceeded its deadline",
            )?;
            require(
                last_sample.wall.elapsed() < Duration::from_millis(250),
                "Device clock coverage lost",
            )?;
            let now = Instant::now();
            max_pump_gap = max_pump_gap.max(now.duration_since(last_pump));
            last_pump = now;
            if self.output.padding_frames()? == 0 && self.source_next_frame < self.total_frames {
                underruns += 1;
            }
            let call_started = Instant::now();
            self.pump_audio()?;
            max_pump_call = max_pump_call.max(call_started.elapsed());
            self.renderer.health()?;
            if started.elapsed() >= next_sample {
                let sample = self.output.sample()?;
                self.record_clock(&sample, label, "poll")?;
                if !sample.reliable {
                    self.wait_for_audio(&mut pacing, label)?;
                    continue;
                }
                require(
                    sample.qpc_100ns >= last_sample.qpc_100ns,
                    "Device QPC moved backwards",
                )?;
                max_clock_gap =
                    max_clock_gap.max(sample.wall.saturating_duration_since(last_sample.wall));
                require(
                    max_clock_gap <= Duration::from_millis(250),
                    "Late sample concealed missing clock coverage",
                )?;
                let position = self
                    .mapper
                    .observe(self.clock_epoch, sample.position, sample.frequency)
                    .map_err(|e| e.to_string())?;
                let source_ns = position.project_time_ns.min(self.total_ns);
                let call_started = Instant::now();
                self.video_due(source_ns)?;
                max_video_call = max_video_call.max(call_started.elapsed());
                let call_started = Instant::now();
                let completed = self.renderer.poll_latest();
                max_poll_call = max_poll_call.max(call_started.elapsed());
                let mut record = json!({"phase":label,"elapsedMs":started.elapsed().as_secs_f64()*1000.0,
                    "sessionEpoch":self.session.epoch(),"renderEpoch":self.render_epoch,
                    "sourceTimeNs":source_ns.to_string(),"devicePosition":sample.position.to_string(),
                    "reliable":true,"hresult":sample.hresult,
                    "maxPumpGapMs":max_pump_gap.as_secs_f64()*1000.0,
                    "maxPumpCallMs":max_pump_call.as_secs_f64()*1000.0,
                    "underrunObservations":underruns,"eventSignalCount":pacing.event_signals,
                    "maxAudioWaitMs":pacing.max_wait.as_secs_f64()*1000.0,
                    "clockFrequency":sample.frequency,"qpc100ns":sample.qpc_100ns.to_string(),
                    "paddingFrames":self.output.padding_frames()?,"sourceNextFrame":self.source_next_frame});
                if let Some(frame) = completed {
                    validate_fixture_frame(frame.index, frame.pts_ns, self.total_ns)?;
                    require(
                        frame.epoch == self.render_epoch && frame.verification.exact,
                        "Stale or incorrect GPU result reached the session consumer",
                    )?;
                    self.last_verified_video = Some(frame.index);
                    first_verified_video.get_or_insert(frame.index);
                    require(
                        frame.pts_ns <= source_ns,
                        "A future video frame was consumed early",
                    )?;
                    record["completedRender"] = json!({"index":frame.index,"ptsNs":frame.pts_ns.to_string(),
                        "renderEpoch":frame.epoch,"inputRgbSha256":frame.verification.input_rgb_sha256,
                        "expectedRgbaSha256":frame.verification.expected_rgba_sha256,
                        "actualRgbaSha256":frame.verification.actual_rgba_sha256,
                        "rowPitch":frame.result.row_pitch,"readbackWaitMs":frame.result.readback_wait_ms,
                        "exact":frame.verification.exact,"uploadDrawReadbackMs":frame.result.upload_draw_readback_ms});
                    consumed += 1;
                    max_render_completion_gap =
                        max_render_completion_gap.max(last_render_completion.elapsed());
                    last_render_completion = Instant::now();
                }
                require(
                    self.records.len() < TRACE_LIMIT,
                    "Streaming trace capacity exceeded",
                )?;
                self.records.push(record);
                let call_started = Instant::now();
                let stats = self.decoder()?.stats()?;
                max_stats_call = max_stats_call.max(call_started.elapsed());
                require(
                    stats.audio_high_water <= 8 && stats.video_high_water <= 4,
                    "Decode queue overflow",
                )?;
                if expect_eof {
                    if stats.audio_eof && !self.session.snapshot().audio_source_eof {
                        self.session
                            .mark_audio_eof(self.session.epoch())
                            .map_err(|e| e.to_string())?;
                    }
                    if stats.video_eof
                        && stats.video_pending == 0
                        && self.next_video.is_none()
                        && !self.session.snapshot().video_eof
                        && self.last_verified_video
                            == Some((self.total_ns * 30 / 1_000_000_000 - 1) as usize)
                    {
                        self.session
                            .mark_video_eof(self.session.epoch())
                            .map_err(|e| e.to_string())?;
                    }
                    if stats.audio_eof
                        && self.pending_audio.is_none()
                        && stats.audio_pending == 0
                        && self.source_next_frame == self.total_frames
                        && self.output.padding_frames()? == 0
                        && position.project_time_ns >= self.total_ns
                        && !self.session.snapshot().device_drained
                    {
                        self.session
                            .mark_device_drained(self.session.epoch())
                            .map_err(|e| e.to_string())?;
                    }
                }
                if (expect_eof && self.session.phase() == SessionPhase::Ended)
                    || (!expect_eof && source_ns >= end_ns)
                {
                    require(
                        underruns == 0,
                        "Audio buffer emptied before complete PCM submission",
                    )?;
                    require(
                        consumed > 0,
                        "Playback phase consumed no verified GPU frames",
                    )?;
                    return Ok(
                        json!({"phase":label,"elapsedMs":started.elapsed().as_secs_f64()*1000.0,
                        "session":self.session.snapshot(),"decoder":stats,"sourceTimeNs":source_ns.to_string(),
                        "sourceStartFrame":self.source_start_frame,"sourceNextFrame":self.source_next_frame,
                        "submittedFrames":self.source_next_frame-self.source_start_frame,
                        "submittedF32InputSha256":format!("{:x}",self.submitted_hash.clone().finalize()),
                        "consumedFrames":consumed,"underrunObservations":underruns,
                        "firstVerifiedVideoIndex":first_verified_video,
                        "maxConsumedRenderGapMs":max_render_completion_gap.max(last_render_completion.elapsed()).as_secs_f64()*1000.0,
                        "lastVerifiedVideoIndex":self.last_verified_video,
                        "maxPumpGapMs":max_pump_gap.as_secs_f64()*1000.0,
                        "maxPumpCallMs":max_pump_call.as_secs_f64()*1000.0,
                        "maxVideoDispatchMs":max_video_call.as_secs_f64()*1000.0,
                        "maxRendererPollMs":max_poll_call.as_secs_f64()*1000.0,
                        "maxDecoderStatsMs":max_stats_call.as_secs_f64()*1000.0,
                        "maxRequested2msSleepMs":max_sleep.as_secs_f64()*1000.0,
                        "audioScheduling":self.audio_scheduling,"waitCount":pacing.waits,
                        "eventSignalCount":pacing.event_signals,"eventTimeoutCount":pacing.event_timeouts,
                        "maxAudioWaitMs":pacing.max_wait.as_secs_f64()*1000.0,
                        "endpointBufferMs":1000.0*f64::from(self.output.info().buffer_frames)/f64::from(self.output.info().sample_rate),
                        "feedGapBelowEndpointBuffer":max_pump_gap.as_secs_f64() < f64::from(self.output.info().buffer_frames)/f64::from(self.output.info().sample_rate),
                        "maxReliableSampleGapMs":max_clock_gap.as_secs_f64()*1000.0,
                        "paddingFrames":self.output.padding_frames()?,"clockReachedSourceEnd":position.project_time_ns>=self.total_ns}),
                    );
                }
                last_sample = sample;
                next_sample = started.elapsed() + Duration::from_millis(10);
            }
            self.wait_for_audio(&mut pacing, label)?;
            if self.audio_scheduling == AudioScheduling::Poll {
                max_sleep = pacing.max_wait;
            }
        }
    }

    fn pause(&mut self) -> Result<Value, String> {
        self.output.stop()?;
        self.running = false;
        self.session.pause().map_err(|e| e.to_string())?;
        self.render_epoch = self.renderer.advance_epoch()?;
        self.last_verified_video = None;
        self.video_submit_pending = self.current_video.is_some();
        let before = self.output.sample()?;
        self.record_clock(&before, "pause", "before")?;
        let padding = self.output.padding_frames()?;
        thread::sleep(Duration::from_millis(200));
        let after = self.output.sample()?;
        self.record_clock(&after, "pause", "after")?;
        require(
            before.reliable
                && after.reliable
                && before.position == after.position
                && padding == self.output.padding_frames()?,
            "Paused output advanced",
        )?;
        Ok(
            json!({"frozen":true,"milliseconds":200,"position":after.position.to_string(),
            "session":self.session.snapshot(),"renderEpoch":self.render_epoch,"decoder":self.decoder()?.stats()?}),
        )
    }

    fn seek(&mut self, frame: u64) -> Result<Value, String> {
        let seek_started = Instant::now();
        if self.running {
            self.output.stop()?;
            self.running = false;
        }
        self.output.reset()?;
        let old_epoch = self.session.epoch();
        let new_epoch = self.session.begin_seek().map_err(|e| e.to_string())?;
        self.render_epoch = self.renderer.advance_epoch()?;
        let stopped = self.decoder.as_mut().ok_or("Missing decoder")?.close()?;
        let shutdown = serde_json::to_value(stopped).map_err(|e| e.to_string())?;
        require(
            shutdown["joined"] == true
                && shutdown["processesReaped"] == true
                && shutdown["error"].is_null(),
            "Old decode session did not close cleanly",
        )?;
        self.decoder = None;
        self.pending_audio = None;
        self.audio_offset = 0;
        self.next_video = None;
        self.current_video = None;
        self.video_submit_pending = false;
        self.last_verified_video = None;
        self.selected_video = None;
        self.first_video_ready = false;
        self.source_start_frame = frame * 48000 / 30;
        self.source_next_frame = self.source_start_frame;
        self.submitted_hash = Sha256::new();
        self.config.epoch = new_epoch;
        self.config.start_audio_frame = self.source_start_frame;
        self.config.start_video_ns = frame * 1_000_000_000 / 30;
        let reset = self.output.sample()?;
        self.record_clock(&reset, "seek", "reset")?;
        require(
            reset.reliable && reset.position == 0 && self.output.padding_frames()? == 0,
            "Seek left queued PCM or nonzero device clock",
        )?;
        self.clock_epoch = self
            .mapper
            .rebase(0, reset.frequency, frame)
            .map_err(|e| e.to_string())?;
        require(
            self.session.mark_audio_ready(old_epoch).is_err(),
            "Old decoder callback revived after seek",
        )?;
        let decoder_started = Instant::now();
        self.decoder = Some(StreamingDecoder::start(self.config.clone())?);
        let decoder_start_ms = decoder_started.elapsed().as_secs_f64() * 1000.0;
        let preparation = self.prepare()?;
        require(
            preparation["firstSourceVideoIndex"] == frame,
            "Seek did not prepare the exact requested video frame",
        )?;
        Ok(
            json!({"oldEpoch":old_epoch,"newEpoch":new_epoch,"renderEpoch":self.render_epoch,
            "sourceStartFrame":self.source_start_frame,"oldDecoderShutdown":shutdown,
            "decoderStartMs":decoder_start_ms,"totalSeekToPreparedMs":seek_started.elapsed().as_secs_f64()*1000.0,
            "seekMode":self.config.seek_mode,"staleCallbackRejected":true,"preparation":preparation}),
        )
    }

    fn cancel(&mut self) -> Result<Value, String> {
        if self.running {
            self.output.stop()?;
            self.running = false;
        }
        self.output.reset()?;
        let old_epoch = self.session.epoch();
        self.session.cancel().map_err(|e| e.to_string())?;
        self.render_epoch = self.renderer.advance_epoch()?;
        let shutdown = self.decoder.as_mut().ok_or("Missing decoder")?.close()?;
        let shutdown = serde_json::to_value(shutdown).map_err(|e| e.to_string())?;
        self.decoder = None;
        self.pending_audio = None;
        self.next_video = None;
        self.current_video = None;
        require(
            self.session.mark_video_ready(old_epoch).is_err(),
            "Cancelled callback was accepted",
        )?;
        let render_before_retirement = self.renderer.stats()?;
        let retired = self.renderer.retire_epoch(Duration::from_secs(3))?;
        self.render_epoch = retired.new_epoch;
        require(
            retired.stats.error.is_none()
                && retired.stats.failures == 0
                && retired.stats.pending_depth == 0
                && !retired.stats.in_flight
                && !retired.stats.completion_pending
                && !retired.stats.closed,
            "Renderer session did not retire to a reusable idle state",
        )?;
        require(
            shutdown["joined"] == true
                && shutdown["processesReaped"] == true
                && shutdown["error"].is_null(),
            "Decoder resources were not reclaimed",
        )?;
        let clock = self.output.sample()?;
        self.record_clock(&clock, "cancel", "reset")?;
        require(
            clock.reliable && clock.position == 0 && self.output.padding_frames()? == 0,
            "Cancel did not reset the output",
        )?;
        self.session.close().map_err(|e| e.to_string())?;
        Ok(
            json!({"decoder":shutdown,"rendererBeforeRetirement":render_before_retirement,"rendererRetirement":retired,
            "rendererRetained":true,"gpuResourcesReleased":false,"session":self.session.snapshot(),
            "paddingFrames":0,"devicePosition":"0","staleCallbackRejected":true,"audioAndDecoderStopped":true}),
        )
    }

    fn record_clock(
        &mut self,
        sample: &super::wasapi_output::ClockSample,
        phase: &str,
        checkpoint: &str,
    ) -> Result<(), String> {
        require(
            self.records.len() < TRACE_LIMIT,
            "Streaming trace capacity exceeded",
        )?;
        self.records.push(
            json!({"kind":"rawClock","phase":phase,"checkpoint":checkpoint,
            "sessionEpoch":self.session.epoch(),"reliable":sample.reliable,"hresult":sample.hresult,
            "devicePosition":sample.position.to_string(),"clockFrequency":sample.frequency,
            "qpc100ns":sample.qpc_100ns.to_string()}),
        );
        Ok(())
    }

    fn cleanup_failure(&mut self) -> Value {
        let mut errors = Vec::new();
        if self.running {
            if let Err(error) = self.output.stop() {
                errors.push(error)
            } else {
                self.running = false;
            }
        }
        let decoder = self.decoder.as_mut().map(|decoder| decoder.close());
        let decoder = match decoder {
            Some(Ok(report)) => json!(report),
            Some(Err(error)) => {
                errors.push(error);
                Value::Null
            }
            None => Value::Null,
        };
        // The outer owner closes the worker once, after this audio/decoder
        // session has dropped. Failure must never return it for another case.
        let renderer_invalidation = match self.renderer.advance_epoch() {
            Ok(epoch) => json!({"newEpoch":epoch}),
            Err(error) => {
                errors.push(error);
                Value::Null
            }
        };
        json!({"audioStopSucceeded":!self.running,"decoder":decoder,"rendererInvalidation":renderer_invalidation,
            "rendererRetainedForFinalShutdown":true,"errors":errors})
    }
}

fn verify_phase_pcm(phase: &Value, reference: &Path) -> Result<(), String> {
    let start = phase["sourceStartFrame"]
        .as_u64()
        .ok_or("Missing PCM start")?;
    let count = phase["submittedFrames"]
        .as_u64()
        .ok_or("Missing PCM count")?;
    require(
        phase["submittedF32InputSha256"] == reference_pcm_hash(reference, start, count, 48000, 2)?,
        "Streamed PCM submission differs from independently decoded source interval",
    )
}

fn case(
    video: &Path,
    audio: &Path,
    reference_pcm: &Path,
    reference_rgb: &Path,
    long: bool,
    audio_scheduling: AudioScheduling,
    full_long_playback: bool,
    hd_reference: Option<&HdReference>,
    seek_mode: SeekMode,
    seek_only: bool,
    renderer: &mut AsyncRenderer,
    report: &mut Value,
) -> Result<(), String> {
    let original = [fingerprint(video)?, fingerprint(audio)?];
    report["sourcesBefore"] = json!(original);
    report["rendererAtSessionStart"] =
        json!({"instanceId":renderer.instance_id(),"stats":renderer.stats()?});
    report["creationTimingScope"] = "Audio/decoder session creation and preparation only; GPU creation is recorded separately in rendererLifecycle".into();
    let creation_started = Instant::now();
    let duration_seconds = if hd_reference.is_some() {
        60
    } else if long {
        20
    } else {
        4
    };
    let mut playback = Playback::new(
        video,
        audio,
        duration_seconds,
        audio_scheduling,
        seek_mode,
        renderer,
    )?;
    report["sessionCreationMs"] = (creation_started.elapsed().as_secs_f64() * 1000.0).into();
    let outcome = (|| {
        report["prepare"] = playback.prepare()?;
        report["totalCreationAndPrepareMs"] =
            (creation_started.elapsed().as_secs_f64() * 1000.0).into();
        report["first"] = playback.play_until(
            if long || hd_reference.is_some() {
                1_000_000_000
            } else {
                2_000_000_000
            },
            false,
            "initial",
        )?;
        report["pause"] = playback.pause()?;
        report["resume"] = playback.play_until(
            if seek_only {
                2_000_000_000
            } else if hd_reference.is_some() {
                60_000_000_000
            } else if long {
                if full_long_playback {
                    20_000_000_000
                } else {
                    2_000_000_000
                }
            } else {
                4_000_000_000
            },
            !seek_only && (hd_reference.is_some() || !long || full_long_playback),
            "resume",
        )?;
        report["seek"] = playback.seek(if hd_reference.is_some() {
            900
        } else if long {
            300
        } else {
            45
        })?;
        report["afterSeek"] = playback.play_until(
            if hd_reference.is_some() {
                31_000_000_000
            } else if long {
                11_000_000_000
            } else {
                2_500_000_000
            },
            false,
            "seek",
        )?;
        if hd_reference.is_some() {
            require(
                report["afterSeek"]["firstVerifiedVideoIndex"] == 900,
                "First verified HD result after seek was not source frame 900",
            )?;
        }
        report["shutdown"] = playback.cancel()?;
        Ok::<(), String>(())
    })();
    if let Err(error) = outcome {
        let _ = playback.session.fail(playback.session.epoch(), &error);
        report["failedSession"] = json!(playback.session.snapshot());
        // Stop before copying diagnostics, then retain actual cleanup outcomes.
        report["failureCleanup"] = playback.cleanup_failure();
        report["observations"] = json!(playback.records);
        return Err(error);
    }
    report["observations"] = json!(playback.records);
    drop(playback);
    // Independent reference I/O happens only after all playback is stopped.
    verify_phase_pcm(&report["resume"], reference_pcm)?;
    report["initialPcmVerified"] = true.into();
    verify_phase_pcm(&report["afterSeek"], reference_pcm)?;
    report["seekPcmVerified"] = true.into();
    let observations = report["observations"]
        .as_array()
        .ok_or("Missing observations")?;
    let mut checked = 0;
    for record in observations {
        if let Some(frame) = record.get("completedRender") {
            let index = frame["index"].as_u64().ok_or("Missing completed index")? as usize;
            let pts_ns = frame["ptsNs"]
                .as_str()
                .ok_or("Missing frame PTS")?
                .parse::<u64>()
                .map_err(|e| e.to_string())?;
            validate_fixture_frame(index, pts_ns, duration_seconds * 1_000_000_000)?;
            if let Some(reference) = hd_reference {
                reference.verify_record(frame)?;
            } else {
                require(
                    frame["inputRgbSha256"] == reference_video_hash(reference_rgb, index % 120)?,
                    "Streamed video frame differs from its independently decoded source index",
                )?;
            }
            checked += 1;
        }
    }
    report["independentVideoHashChecks"] = checked.into();
    require(checked > 0, "No streamed frame independently checked")?;
    require(
        fingerprint(video)? == original[0] && fingerprint(audio)? == original[1],
        "Input media changed during streaming validation",
    )?;
    report["sourceIntegrityVerified"] = true.into();
    report["completeSourcePlayback"] =
        (!seek_only && (hd_reference.is_some() || !long || full_long_playback)).into();
    report["functionalPassed"] = true.into();
    report["feedGapsWithinEndpointBudget"] = ["first", "resume", "afterSeek"]
        .iter()
        .all(|key| report[*key]["feedGapBelowEndpointBuffer"] == true)
        .into();
    report["ok"] = true.into();
    Ok(())
}

fn write_report(path: &Path, value: &Value) -> Result<(), String> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing report directory")?)
            .map_err(|e| e.to_string())?;
    let write = serde_json::to_writer_pretty(&mut file, value)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            file.write_all(b"\n")
                .and_then(|_| file.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = write {
        let retained = file.keep().map(|(_, path)| path);
        return Err(format!("{error}; partial report retained: {retained:?}"));
    }
    file.persist_noclobber(path).map(|_| ()).map_err(|error| {
        let reason = error.error.to_string();
        let retained = error.file.keep().map(|(_, path)| path);
        format!("{reason}; report retained: {retained:?}")
    })
}

pub fn run() -> Result<(), String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut fixture =
        manifest.join("target/workflow-validation/98ee1285-41b7-452a-bbce-e38c83b1a2aa");
    let mut reference =
        manifest.join("target/native-d3d-validation/run-ec927ae2-fcfe-4e7c-8ce0-475746a10284");
    let mut output = None;
    let mut long_fixture = None;
    let mut audio_scheduling = AudioScheduling::Poll;
    let mut renderer_lifetime = RendererLifetime::Scoped;
    let mut full_long_playback = false;
    let mut hd_fixture = None;
    let mut seek_mode = SeekMode::Sequential;
    let mut seek_only = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(key) = args.next() {
        let value = PathBuf::from(args.next().ok_or("Expected CLI option value")?);
        match key.to_str() {
            Some("--output-dir") if output.is_none() => output = Some(value),
            Some("--fixture-dir") => fixture = value,
            Some("--reference-dir") => reference = value,
            Some("--long-fixture-dir") => long_fixture = Some(value),
            Some("--hd-fixture-dir") => hd_fixture = Some(value),
            Some("--seek-mode") => {
                seek_mode = match value.to_str() {
                    Some("sequential") => SeekMode::Sequential,
                    Some("indexed-cfr") => SeekMode::IndexedCfr,
                    _ => return Err("Seek mode must be sequential or indexed-cfr".into()),
                }
            }
            Some("--seek-only") => {
                seek_only = match value.to_str() {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err("Seek-only must be true or false".into()),
                }
            }
            Some("--audio-scheduling") => {
                audio_scheduling =
                    AudioScheduling::parse(value.to_str().ok_or("Invalid scheduling option")?)?
            }
            Some("--renderer-lifetime") => {
                renderer_lifetime =
                    RendererLifetime::parse(value.to_str().ok_or("Invalid renderer lifetime")?)?
            }
            Some("--long-playback") => {
                full_long_playback = match value.to_str() {
                    Some("partial") => false,
                    Some("full") => true,
                    _ => return Err("Long playback must be partial or full".into()),
                }
            }
            _ => {
                return Err(
                    "Expected --output-dir, --fixture-dir, --long-fixture-dir, --reference-dir, --audio-scheduling, --renderer-lifetime or --long-playback"
                        .into(),
                );
            }
        }
    }
    let output = output.ok_or("--output-dir is required")?;
    if hd_fixture.is_none() && long_fixture.is_none() {
        return Err("--long-fixture-dir or --hd-fixture-dir is required".into());
    }
    require(
        !seek_only || hd_fixture.is_some(),
        "Seek-only validation requires the HD fixture",
    )?;
    if hd_fixture.is_some() {
        require(
            !cfg!(debug_assertions),
            "HD performance probe requires an optimized release build",
        )?;
        require(
            audio_scheduling == AudioScheduling::EventMmcss
                && renderer_lifetime == RendererLifetime::Shared,
            "HD profile requires explicit event-mmcss and shared renderer lifetime",
        )?;
    }
    let target = manifest
        .join("target")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    require(
        output.is_absolute()
            && output.starts_with(manifest.join("target"))
            && !output
                .components()
                .any(|x| matches!(x, std::path::Component::ParentDir)),
        "Output must stay inside checkout target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|x| x.exists())
        .ok_or("Missing output ancestor")?;
    require(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped through a link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    require(output.starts_with(&target), "Output escaped target")?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let mut report = json!({"schemaVersion":if hd_fixture.is_some(){5}else{3},"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"audioScheduling":audio_scheduling,
        "seekMode":seek_mode,"seekOnly":seek_only,
        "profile":if hd_fixture.is_some(){"hd1080p60"}else{"legacy"},"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},
        "rendererLifetime":renderer_lifetime,"longPlayback":if full_long_playback{"full"}else{"partial"},"traceLimitPerCase":TRACE_LIMIT,
        "scope":{"offscreen":true,"presentCalled":false,"visibleWindow":false,"ownedMutedAudioSession":true,
        "applicationBackendChanged":false,"streamingDecode":true,"audioQueueBlocks":8,"audioBlockFrames":480,"videoQueueFrames":4,
        "seekStrategy":seek_mode},
        "short":{"ok":false},"long":{"ok":false}});
    let mut renderer_owner = RendererOwner::default();
    let work = (|| {
        if let Some(fixture) = &hd_fixture {
            report["short"] = Value::Null;
            report["long"] = Value::Null;
            report["hd"] = json!({"ok":false});
            report["longPlayback"] = "full".into();
            let video = fixture.join("video-1080p60s.mp4");
            let audio = fixture.join("audio-60s.wav");
            let source = fingerprint(&video)?;
            let ledger_path = fixture.join("video-reference.json");
            let ledger = HdReference::load(
                &ledger_path,
                source["sha256"].as_str().ok_or("Missing source hash")?,
            )?;
            report["hd"]["referenceLedger"] = fingerprint(&ledger_path)?;
            case(
                &video,
                &audio,
                &audio,
                &ledger_path,
                true,
                audio_scheduling,
                true,
                Some(&ledger),
                seek_mode,
                seek_only,
                renderer_owner.acquire(1920, 1080)?,
                &mut report["hd"],
            )?;
            return Ok(());
        }
        let long_fixture = long_fixture.as_ref().ok_or("Missing long fixture")?;
        let rgb = reference.join("decoded-video.rgb");
        case(
            &fixture.join("原始 视频;[literal].mp4"),
            &fixture.join("原始 音频.wav"),
            &reference.join("decoded-audio.wav"),
            &rgb,
            false,
            audio_scheduling,
            full_long_playback,
            None,
            seek_mode,
            false,
            renderer_owner.acquire(160, 90)?,
            &mut report["short"],
        )?;
        if renderer_lifetime == RendererLifetime::Scoped {
            renderer_owner.shutdown()?;
        }
        case(
            &long_fixture.join("video-20s.mp4"),
            &long_fixture.join("audio-20s.wav"),
            &long_fixture.join("audio-20s.wav"),
            &rgb,
            true,
            audio_scheduling,
            full_long_playback,
            None,
            seek_mode,
            false,
            renderer_owner.acquire(160, 90)?,
            &mut report["long"],
        )?;
        Ok::<(), String>(())
    })();
    let shutdown = renderer_owner.shutdown();
    report["rendererLifecycle"] = renderer_owner.report();
    let result = match (work, shutdown) {
        (Ok(()), Ok(())) => {
            let count = if renderer_lifetime == RendererLifetime::Shared {
                1
            } else {
                2
            };
            require(
                renderer_owner.creations == count && renderer_owner.shutdown_calls == count,
                "Renderer lifecycle did not match requested reuse mode",
            )
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(format!(
            "{error}; final renderer shutdown failed: {shutdown}"
        )),
    };
    report["ok"] = result.is_ok().into();
    report["functionalPassed"] = result.is_ok().into();
    report["feedGapsWithinEndpointBudget"] = (if hd_fixture.is_some() {
        report["hd"]["feedGapsWithinEndpointBudget"] == true
    } else {
        report["short"]["feedGapsWithinEndpointBudget"] == true
            && report["long"]["feedGapsWithinEndpointBudget"] == true
    })
    .into();
    report["realtimePerformanceQualified"] = false.into();
    report["performanceNote"] = "Functional success on controlled fixtures does not qualify realtime performance or display frame rate. Per-frame readback and content verification add diagnostic cost; final shutdown and historical scheduling variability remain open.".into();
    if let Err(error) = &result {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-stream-validation.json");
    if let Err(error) = write_report(&path, &report) {
        eprintln!("{report}");
        return Err(error);
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"error":report.get("error"),
        "shortPrepare":report["short"]["prepare"],"shortShutdown":report["short"]["shutdown"],
        "longPrepare":report["long"]["prepare"],"longShutdown":report["long"]["shutdown"],
        "hdPrepare":report["hd"]["prepare"],"hdShutdown":report["hd"]["shutdown"]})
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_scheduling_is_explicit_and_unknown_modes_do_not_fallback() {
        assert_eq!(
            AudioScheduling::parse("poll").unwrap(),
            AudioScheduling::Poll
        );
        assert_eq!(
            AudioScheduling::parse("event").unwrap(),
            AudioScheduling::Event
        );
        assert_eq!(
            AudioScheduling::parse("event-mmcss").unwrap(),
            AudioScheduling::EventMmcss
        );
        assert!(AudioScheduling::parse("auto").is_err());
    }
    #[test]
    fn fixture_frame_identity_rejects_wrapped_indices_and_wrong_timestamps() {
        assert!(validate_fixture_frame(119, 3_966_666_666, 4_000_000_000).is_ok());
        assert!(validate_fixture_frame(120, 4_000_000_000, 4_000_000_000).is_err());
        assert!(validate_fixture_frame(45, 1_500_000_000, 20_000_000_000).is_ok());
        assert!(validate_fixture_frame(45, 1_500_000_001, 20_000_000_000).is_err());
    }
    #[test]
    fn report_collision_preserves_existing_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        fs::write(&path, b"original").unwrap();
        assert!(write_report(&path, &json!({"new":true})).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
