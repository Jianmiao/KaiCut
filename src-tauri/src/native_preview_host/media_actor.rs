//! Development-only media owner: FFmpeg, WASAPI and its clock never run on UI.
//! The sink queues bounded work; it must not wait for the main event thread.
#[path = "mailbox.rs"]
mod mailbox;
use super::{
    audio_diagnostics::{
        AudioDiagnostics, ClockObservation, PumpObservation, RawClockSample, StartObservation,
    },
    audio_preview_adapter::{AdapterPhase, AudioPreviewAdapter, TickOutcome},
    audio_scheduling::ThreadScheduling,
    audio_timeline::AudioTimeline,
    audio_transfer::AudioTransfer,
    streaming_decoder::StreamConfig,
    wasapi_output::WasapiOutput,
};
use mailbox::{RenderPolicy, latest_due_ready, remaining_audio_frames};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct MediaFrame {
    pub epoch: u64,
    pub source_epoch: u64,
    pub index: usize,
    pub pts_ns: u64,
    pub project_ns: u64,
    pub width: u32,
    pub height: u32,
    pub rgb: Arc<[u8]>,
    pub diagnostic: bool,
}
pub struct MediaAck {
    pub epoch: u64,
    pub source_epoch: u64,
    pub index: usize,
    pub accepted: bool,
    pub evidence: Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Submission {
    Queued,
    Busy,
    Evicted,
    Stale,
}
pub trait Sink: Send + Sync {
    fn submit(&self, frame: MediaFrame) -> Result<Submission, String>;
    fn poll(&self) -> Result<Option<MediaAck>, String>;
    /// Invalidates queued work/results. An already executing Present is not reversible.
    fn retire(&self, new_epoch: u64) -> Result<(), String>;
}
pub struct ActorReport {
    pub json: Value,
    /// Exact prefixes accepted by the device, grouped by seek target. Never played PCM inferred from wall time.
    pub captures: Vec<(u64, Vec<f32>)>,
}
#[derive(Clone, Copy, Debug)]
pub struct SourceOptions {
    /// Exclusive source endpoint in normalized video timeline nanoseconds.
    pub end_ns: u64,
    /// Hidden validation must mute its own session; visible playback may opt out.
    pub muted: bool,
}
#[derive(Clone)]
pub struct SourcePreparationControl {
    pub checkpoint: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    pub observe: Arc<dyn Fn(Value) + Send + Sync>,
}
#[derive(Debug)]
pub struct SourceStartError {
    pub message: String,
    pub joined: bool,
    pub report: Option<Value>,
}
struct PreparationTrace {
    control: Option<SourcePreparationControl>,
    state: Value,
}
impl PreparationTrace {
    fn new(control: Option<SourcePreparationControl>) -> Self {
        Self {
            control,
            state: json!({"stage":"initializing", "decoderCreationAttempted":false,"decoderCreated":false,"deviceOpenAttempted":false, "deviceOpened":false,"deviceReleased":false,"audioStartAttempted":false,"audioStarted":false,"workerJoined":false,"cleanupConfirmed":false,"cleanupErrors":[],"decoderClose":null}),
        }
    }
    fn checkpoint(
        &mut self,
        stage: &str,
        evidence: Value,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let changed = self.state["stage"] != stage;
        self.state["stage"] = json!(stage);
        if let Some(control) = &self.control {
            if changed {
                (control.observe)(
                    json!({"event":"actor-preparation-checkpoint","stage":stage,"preparation":self.state,"resources":evidence}),
                );
            }
            (control.checkpoint)(stage)?;
        }
        if stop.load(Ordering::Acquire) {
            return Err("Media preparation cancelled".into());
        }
        Ok(())
    }
}
trait InitializationResources {
    fn step(&mut self, step: &'static str) -> Result<(), String>;
    fn evidence(&self) -> Value;
}
fn initialize_resources(
    resources: &mut impl InitializationResources,
    preparation: &mut PreparationTrace,
    stop: &AtomicBool,
) -> Result<(), String> {
    preparation.checkpoint("actor-starting", resources.evidence(), stop)?;
    preparation.state["decoderCreationAttempted"] = json!(true);
    resources.step("decoder-create")?;
    preparation.state["decoderCreated"] = json!(true);
    preparation.checkpoint("decoder-ready", resources.evidence(), stop)?;
    resources.step("sink-retire")?;
    preparation.checkpoint("device-before-open", resources.evidence(), stop)?;
    let opened = resources.step("device-open");
    preparation.state["deviceOpenAttempted"] = json!(
        resources.evidence()["deviceOpenAttempted"]
            .as_bool()
            .unwrap_or(true)
    );
    opened?;
    preparation.state["deviceOpened"] = json!(true);
    preparation.checkpoint("device-opened", resources.evidence(), stop)?;
    resources.step("adapter-create")?;
    preparation.checkpoint("adapter-ready", resources.evidence(), stop)?;
    resources.step("scheduler-enter")?;
    preparation.checkpoint("actor-initialized", resources.evidence(), stop)?;
    Ok(())
}
enum Action {
    Prepare,
    Play,
    Pause,
    Observe,
    Seek(u64),
    Cancel,
    Finish,
}
struct Command {
    action: Action,
    reply: SyncSender<Result<Value, String>>,
}
pub struct MediaActor {
    tx: SyncSender<Command>,
    status: Arc<Mutex<Value>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<ActorReport>>,
}
impl MediaActor {
    /// Call from a control/background thread. No COM/device object crosses this boundary.
    pub fn start(
        config: StreamConfig,
        timeline: AudioTimeline,
        target_ns: u64,
        sink: Arc<dyn Sink>,
    ) -> Result<Self, String> {
        Self::configured(config, timeline, target_ns, sink, None, None)
            .map_err(|error| error.message)
    }
    pub fn start_source(
        config: StreamConfig,
        timeline: AudioTimeline,
        target_ns: u64,
        sink: Arc<dyn Sink>,
        options: SourceOptions,
    ) -> Result<Self, String> {
        Self::configured(config, timeline, target_ns, sink, Some(options), None)
            .map_err(|error| error.message)
    }
    pub fn start_source_controlled(
        config: StreamConfig,
        timeline: AudioTimeline,
        target_ns: u64,
        sink: Arc<dyn Sink>,
        options: SourceOptions,
        control: SourcePreparationControl,
    ) -> Result<Self, SourceStartError> {
        Self::configured(
            config,
            timeline,
            target_ns,
            sink,
            Some(options),
            Some(control),
        )
    }
    fn configured(
        config: StreamConfig,
        timeline: AudioTimeline,
        target_ns: u64,
        sink: Arc<dyn Sink>,
        options: Option<SourceOptions>,
        control: Option<SourcePreparationControl>,
    ) -> Result<Self, SourceStartError> {
        if options.is_some_and(|options| {
            options.end_ns == 0 || options.end_ns > 60_000_000_000 || target_ns > options.end_ns
        }) {
            return Err(no_worker_error(
                "Source preview requires 0..=60 seconds and a target within its endpoint".into(),
            ));
        }
        let (tx, rx) = mpsc::sync_channel(4);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let status = Arc::new(Mutex::new(json!({"phase":"initializing"})));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_status = Arc::clone(&status);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("kaicut-preview-media".into())
            .spawn(move || {
                match Worker::new(
                    config,
                    timeline,
                    target_ns,
                    sink,
                    worker_status,
                    worker_stop,
                    options,
                    control,
                ) {
                    Ok(worker) => {
                        worker.publish();
                        let _ = started_tx.send(Ok(()));
                        worker.run(rx)
                    }
                    Err((error, report)) => {
                        let _ = started_tx.send(Err(error));
                        ActorReport {
                            json: report,
                            captures: Vec::new(),
                        }
                    }
                }
            })
            .map_err(|error| no_worker_error(error.to_string()))?;
        let mut actor = Self {
            tx,
            status,
            stop,
            worker: Some(worker),
        };
        match started_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(actor),
            result => {
                actor.stop.store(true, Ordering::Release);
                let message = match result {
                    Ok(Err(error)) => error,
                    Err(error) => format!("Media actor initialization unavailable: {error}"),
                    Ok(Ok(())) => unreachable!(),
                };
                let joined = actor.worker.take().unwrap().join();
                super::source_faults::device_worker_joined(joined.is_ok());
                match joined {
                    Ok(mut report) => {
                        mark_worker_joined(&mut report.json);
                        Err(SourceStartError {
                            message,
                            joined: true,
                            report: Some(report.json),
                        })
                    }
                    Err(_) => Err(SourceStartError {
                        message: format!("{message}; worker panicked during cleanup"),
                        joined: false,
                        report: None,
                    }),
                }
            }
        }
    }
    fn command(&self, action: Action) -> Result<Value, String> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command { action, reply })
            .map_err(|e| format!("Media control queue unavailable: {e}"))?;
        receive
            .recv_timeout(Duration::from_secs(15))
            .map_err(|e| format!("Media control response unavailable: {e}"))?
    }
    pub fn prepare(&self) -> Result<Value, String> {
        self.command(Action::Prepare)
    }
    pub fn play(&self) -> Result<Value, String> {
        self.command(Action::Play)
    }
    pub fn pause(&self) -> Result<Value, String> {
        self.command(Action::Pause)
    }
    /// Samples the owned device, including after a successful cancel. Failed
    /// cleanup instead exposes the retired state without resampling that device.
    pub fn observe(&self) -> Result<Value, String> {
        self.command(Action::Observe)
    }
    pub fn seek(&self, target_ns: u64) -> Result<Value, String> {
        self.command(Action::Seek(target_ns))
    }
    pub fn cancel(&self) -> Result<Value, String> {
        self.command(Action::Cancel)
    }
    pub fn status(&self) -> Value {
        self.status
            .try_lock()
            .map(|v| v.clone())
            .unwrap_or_else(|_| json!({"busy":true}))
    }
    /// Joins only on the background control caller, retaining failure evidence.
    pub fn finish(mut self) -> Result<ActorReport, String> {
        self.stop.store(true, Ordering::Release);
        let (reply, _) = mpsc::sync_channel(1);
        let _ = self.tx.try_send(Command {
            action: Action::Finish,
            reply,
        });
        let mut report = self
            .worker
            .take()
            .ok_or("Media worker already joined")?
            .join()
            .map_err(|_| "Media worker panicked".to_owned())?;
        mark_worker_joined(&mut report.json);
        Ok(report)
    }
}
impl Drop for MediaActor {
    fn drop(&mut self) {
        // A UI dropping its handle signals cancellation, never waits for COM teardown.
        self.stop.store(true, Ordering::Release);
    }
}

type Audio = AudioPreviewAdapter<AudioTransfer, WasapiOutput>;
fn mark_worker_joined(report: &mut Value) {
    let cleanup_errors_empty = report["cleanupErrors"]
        .as_array()
        .is_some_and(Vec::is_empty);
    let preparation = &mut report["preparation"];
    preparation["workerJoined"] = json!(true);
    preparation["cleanupConfirmed"] = json!(
        preparation["resourcesReleased"] == true
            && preparation["cleanupErrors"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && cleanup_errors_empty
    );
}
fn no_worker_error(message: String) -> SourceStartError {
    let mut preparation = PreparationTrace::new(None).state;
    preparation["resourcesReleased"] = json!(true);
    preparation["workerNeverCreated"] = json!(true);
    // No worker was launched, so joining is vacuous rather than evidence of an OS join.
    let mut report = json!({"ok":false,"error":message,"initializationFailed":true,"cleanupErrors":[],"preparation":preparation});
    mark_worker_joined(&mut report);
    SourceStartError {
        message,
        joined: true,
        report: Some(report),
    }
}
struct InitialResources {
    config: Option<StreamConfig>,
    timeline: Option<AudioTimeline>,
    target: u64,
    source: Option<SourceOptions>,
    sink: Arc<dyn Sink>,
    transfer: Option<AudioTransfer>,
    output: Option<WasapiOutput>,
    output_info: Value,
    device_open_attempted: bool,
    audio: Option<Audio>,
    scheduling: Option<ThreadScheduling>,
    adapter_error: Option<Value>,
    #[cfg(debug_assertions)]
    validation_decoder_failure: Option<Box<dyn Fn() -> Result<Value, String> + Send>>,
}
impl InitializationResources for InitialResources {
    fn step(&mut self, step: &'static str) -> Result<(), String> {
        match step {
            "decoder-create" => {
                let timeline = self
                    .timeline
                    .take()
                    .ok_or("Missing initial audio timeline")?;
                let config = self
                    .config
                    .take()
                    .ok_or("Missing initial decoder configuration")?;
                self.transfer = Some(if self.source.is_some() {
                    AudioTransfer::new_av_source(timeline, self.target, config)?
                } else {
                    AudioTransfer::new_av(timeline, self.target, config)?
                });
            }
            "sink-retire" => {
                let transfer = self.transfer.as_ref().ok_or("Missing initial decoder")?;
                self.sink
                    .retire(RenderPolicy::new(transfer.snapshot().epoch).epoch)?;
                #[cfg(debug_assertions)]
                if self.source.is_some()
                    && std::env::args_os().any(|v| v == "--native-source-check-dir")
                    && std::env::args_os().any(|v| v == "decoder-failure")
                {
                    self.validation_decoder_failure = Some(transfer.validation_video_failure()?);
                }
            }
            "device-open" => {
                if self.source.is_some() && super::source_faults::device_failure() {
                    return Err("Injected WASAPI initialization failure before device open".into());
                }
                self.device_open_attempted = true;
                self.output = Some(WasapiOutput::open_pcm_event(
                    self.source.is_none_or(|source| source.muted),
                )?);
                self.output_info = json!(self.output.as_ref().unwrap().info());
            }
            "adapter-create" => {
                // Compute all fallible scalar inputs before transferring resource ownership.
                let remaining = self
                    .source
                    .map(|source| remaining_audio_frames(self.target, source.end_ns, 0))
                    .transpose()?;
                let transfer = self.transfer.take().ok_or("Missing initial decoder")?;
                let output = self.output.take().ok_or("Missing initial output")?;
                if let Some(remaining) = remaining {
                    match AudioPreviewAdapter::new_source_with_report(transfer, output, remaining) {
                        Ok(audio) => self.audio = Some(audio),
                        Err(error) => {
                            self.adapter_error =
                                Some(json!({"state":error.state,"adapterEvidence":error.evidence}));
                            return Err(error.message);
                        }
                    }
                } else {
                    // The diagnostic path retains its existing API and semantics.
                    self.audio = Some(AudioPreviewAdapter::new(transfer, output)?);
                }
            }
            "scheduler-enter" => self.scheduling = Some(ThreadScheduling::enter(true)?),
            _ => return Err("Unknown initialization resource step".into()),
        }
        Ok(())
    }
    fn evidence(&self) -> Value {
        let decoder = if let Some(audio) = &self.audio {
            json!(audio.decoder_stats())
        } else if let Some(transfer) = &self.transfer {
            json!(transfer.decoder_stats())
        } else {
            Value::Null
        };
        json!({"decoder":decoder,"state":self.audio.as_ref().map(Audio::snapshot),"output":self.output_info,"deviceOpenAttempted":self.device_open_attempted})
    }
}
impl InitialResources {
    fn cleanup(mut self, error: &str, mut preparation: PreparationTrace) -> Value {
        let output_info = self.evidence()["output"].clone();
        let mut errors = Vec::new();
        let mut decoder_close = Value::Null;
        let mut adapter_evidence = Value::Null;
        let mut state = Value::Null;
        let mut decoder = Value::Null;
        if let Some(audio) = &mut self.audio {
            if let Err(error) = audio.cancel() {
                errors.push(error);
            }
            state = json!(audio.snapshot());
            adapter_evidence = json!(audio.evidence());
            decoder_close = audio
                .evidence()
                .transfer_close
                .as_ref()
                .map(|value| json!({"Ok":value}))
                .unwrap_or(Value::Null);
            decoder = json!(audio.decoder_stats());
            errors.extend(audio.snapshot().cleanup_errors);
        } else {
            if let Some(output) = &mut self.output {
                if let Err(error) = output.reset() {
                    errors.push(error);
                }
            }
            if let Some(transfer) = &mut self.transfer {
                decoder = json!(transfer.decoder_stats());
                let close = transfer.close();
                if error == "Injected WASAPI initialization failure before device open" {
                    super::source_faults::device_cleanup(error, &close);
                }
                if let Err(error) = &close {
                    errors.push(error.clone());
                }
                decoder_close = json!(close);
            }
            if let Some(report) = &self.adapter_error {
                state = report["state"].clone();
                adapter_evidence = report["adapterEvidence"].clone();
                decoder_close = if report["adapterEvidence"]["transferClose"].is_null() {
                    Value::Null
                } else {
                    json!({"Ok":report["adapterEvidence"]["transferClose"]})
                };
                if let Some(cleanup) = report["state"]["cleanupErrors"].as_array() {
                    errors.extend(cleanup.iter().filter_map(Value::as_str).map(str::to_owned));
                }
            }
        }
        if let Some(scheduling) = &mut self.scheduling {
            if let Err(error) = scheduling.finish() {
                errors.push(error);
            }
        }
        // Releasing the owning fields happens on this media worker. Only now may
        // a report assert the successfully opened output was released.
        drop(self.audio.take());
        drop(self.output.take());
        drop(self.transfer.take());
        drop(self.scheduling.take());
        preparation.state["deviceReleased"] = preparation.state["deviceOpened"].clone();
        preparation.state["resourcesReleased"] = json!(
            !(preparation.state["decoderCreationAttempted"] == true
                && preparation.state["decoderCreated"] != true)
                && !(preparation.state["deviceOpenAttempted"] == true
                    && preparation.state["deviceOpened"] != true)
        );
        preparation.state["cleanupErrors"] = json!(errors);
        preparation.state["decoderClose"] = decoder_close;
        let report = json!({"ok":false,"initializationFailed":true,"error":error,"cleanupErrors":errors,"preparation":preparation.state,"adapterEvidence":adapter_evidence,"state":state,"decoder":decoder,"output":output_info});
        if let Some(control) = preparation.control {
            (control.observe)(json!({"event":"actor-initialization-released","report":report}));
        }
        report
    }
}
struct Segment {
    target: u64,
    samples: Vec<f32>,
    submissions: Vec<Value>,
    clocks: Vec<Value>,
    max_feed_gap_ms: f64,
    underruns: u64,
    accepted_frames: u64,
    audio_diagnostics: AudioDiagnostics,
}
impl Segment {
    fn new(target: u64, sequence: u64) -> Self {
        Self {
            target,
            samples: Vec::new(),
            submissions: Vec::new(),
            clocks: Vec::new(),
            max_feed_gap_ms: 0.0,
            underruns: 0,
            accepted_frames: 0,
            audio_diagnostics: AudioDiagnostics::new(sequence, target),
        }
    }
    fn summary(&self) -> Value {
        json!({"targetNs":self.target.to_string(),"acceptedFrames":self.accepted_frames,"submissionTrace":self.submissions,"clockSamples":self.clocks,"maxPumpGapMs":self.max_feed_gap_ms,"underrunObservations":self.underruns,"audioDiagnostics":self.audio_diagnostics.report()})
    }
}
#[derive(Default)]
struct EventTrace {
    events: Vec<Value>,
    count: u64,
    lifecycle_events: Vec<Value>,
    lifecycle_count: u64,
}
const MAX_LIFECYCLE_EVENTS: usize = 64;
impl EventTrace {
    fn record(&mut self, source_mode: bool, event: Value) -> Result<(), String> {
        if source_mode && self.events.len() >= 128 {
            self.events.remove(0);
        } else if self.events.len() >= 3000 {
            return Err("Media video/control trace limit exceeded".into());
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or("Media event count exhausted")?;
        // Keep raw control evidence independently from the rolling frame trace.
        // Unknown/malformed kinds are retained conservatively: only these three
        // known per-frame events may be excluded from the lifecycle ledger.
        if !matches!(
            event.get("kind").and_then(Value::as_str),
            Some("submit" | "complete" | "staleAckDiscarded")
        ) {
            // This is a subset of count, whose checked increment succeeded.
            self.lifecycle_count += 1;
            if self.lifecycle_events.len() < MAX_LIFECYCLE_EVENTS {
                self.lifecycle_events.push(event.clone());
            }
        }
        self.events.push(event);
        Ok(())
    }
    fn summary(&self) -> Value {
        let dropped = self.lifecycle_count - self.lifecycle_events.len() as u64;
        json!({"eventCount":self.count,"retainedEvents":self.events.len(),"lifecycleEventCount":self.lifecycle_count,"retainedLifecycleEvents":self.lifecycle_events.len(),"lifecycleEventsDropped":dropped,"lifecycleEventsComplete":dropped==0})
    }
    fn report(&self) -> Value {
        let mut report = self.summary();
        report["videoEvents"] = json!(self.events);
        report["lifecycleEvents"] = json!(self.lifecycle_events);
        report
    }
}
struct Worker {
    audio: Audio,
    scheduling: ThreadScheduling,
    sink: Arc<dyn Sink>,
    video: RenderPolicy,
    width: u32,
    height: u32,
    target: u64,
    project_ns: u64,
    ready: bool,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Value>>,
    segments: Vec<Segment>,
    events: EventTrace,
    last_clock: Value,
    last_position: u64,
    last_qpc: u64,
    clock_reads: u64,
    last_pump: Instant,
    error: Option<String>,
    cleanup_errors: Vec<String>,
    source: Option<SourceOptions>,
    preparation: PreparationTrace,
    first_prepare_complete: bool,
    prepare_tick_count: u64,
    ended: bool,
    segment_count: u64,
    #[cfg(debug_assertions)]
    validation_decoder_failure: Option<Box<dyn Fn() -> Result<Value, String> + Send>>,
}
impl Worker {
    fn new(
        config: StreamConfig,
        timeline: AudioTimeline,
        target: u64,
        sink: Arc<dyn Sink>,
        status: Arc<Mutex<Value>>,
        stop: Arc<AtomicBool>,
        source: Option<SourceOptions>,
        control: Option<SourcePreparationControl>,
    ) -> Result<Self, (String, Value)> {
        let width = config.video_width;
        let height = config.video_height;
        let mut resources = InitialResources {
            config: Some(config),
            timeline: Some(timeline),
            target,
            source,
            sink: Arc::clone(&sink),
            transfer: None,
            output: None,
            output_info: Value::Null,
            device_open_attempted: false,
            audio: None,
            scheduling: None,
            adapter_error: None,
            #[cfg(debug_assertions)]
            validation_decoder_failure: None,
        };
        let mut preparation = PreparationTrace::new(control);
        if let Err(error) = initialize_resources(&mut resources, &mut preparation, &stop) {
            let report = resources.cleanup(&error, preparation);
            return Err((error, report));
        }
        let audio = resources.audio.take().expect("Initialized adapter");
        let scheduling = resources
            .scheduling
            .take()
            .expect("Initialized thread scheduling");
        let video = RenderPolicy::new(audio.snapshot().cursor.epoch);
        #[cfg(debug_assertions)]
        let validation_decoder_failure = resources.validation_decoder_failure.take();
        Ok(Self {
            audio,
            scheduling,
            sink,
            video,
            width,
            height,
            target,
            project_ns: target,
            ready: false,
            stop,
            status,
            segments: vec![Segment::new(target, 1)],
            events: EventTrace::default(),
            last_clock: Value::Null,
            last_position: 0,
            last_qpc: 0,
            clock_reads: 0,
            last_pump: Instant::now(),
            error: None,
            cleanup_errors: Vec::new(),
            source,
            preparation,
            first_prepare_complete: false,
            prepare_tick_count: 0,
            ended: false,
            segment_count: 1,
            #[cfg(debug_assertions)]
            validation_decoder_failure,
        })
    }
    fn snapshot(&self) -> Value {
        let s = self.segments.last().unwrap();
        let state = self.audio.snapshot();
        let ended = self.ended && matches!(state.phase, AdapterPhase::Ready | AdapterPhase::Paused);
        let playback_phase = if ended {
            json!("ended")
        } else {
            json!(state.phase)
        };
        let mut snapshot = json!({"state":state,"playbackPhase":playback_phase,"ended":ended,"renderEpoch":self.video.epoch,"sourceEpoch":self.video.source_epoch,"projectTimeNs":self.project_ns.to_string(),"videoReady":self.ready,"lastSubmittedIndex":self.video.last_submitted,"lastAcceptedIndex":self.video.last_completed,"lastClock":self.last_clock,"maxPumpGapMs":s.max_feed_gap_ms,"underrunObservations":s.underruns,"acceptedFrames":s.accepted_frames,"audioDiagnostics":s.audio_diagnostics.summary(),"sourceEndNs":self.source.map(|s|s.end_ns.to_string()),"segmentCount":self.segment_count,"error":self.error,"cleanupErrors":self.cleanup_errors});
        snapshot
            .as_object_mut()
            .unwrap()
            .append(self.events.summary().as_object_mut().unwrap());
        snapshot
    }
    fn preparation_checkpoint(&mut self, stage: &str) -> Result<(), String> {
        if self.first_prepare_complete {
            return Ok(());
        }
        let evidence = json!({"state":self.audio.snapshot(),"decoder":self.audio.decoder_stats(),"output":self.audio.output().info(),"prepareTickCount":self.prepare_tick_count,"acceptedFrames":self.segments.last().map(|segment|segment.accepted_frames),"lastSubmittedIndex":self.video.last_submitted,"lastAcceptedIndex":self.video.last_completed});
        self.preparation.checkpoint(stage, evidence, &self.stop)
    }
    fn publish(&self) {
        if let Ok(mut status) = self.status.try_lock() {
            *status = self.snapshot();
        }
    }
    fn record(&mut self, event: Value) -> Result<(), String> {
        self.events.record(self.source.is_some(), event)
    }
    fn capture(&mut self, tick: TickOutcome) -> Result<(), String> {
        let segment = self.segments.last_mut().unwrap();
        segment.accepted_frames = segment
            .accepted_frames
            .checked_add(u64::from(tick.written_frames))
            .ok_or("Accepted frame count exhausted")?;
        if self.source.is_some() {
            if !tick.accepted_samples.is_empty() {
                return Err(
                    "Source playback unexpectedly retained a diagnostic PCM capture".into(),
                );
            }
            return Ok(());
        }
        if tick.accepted_samples.len() != tick.written_frames as usize * 2 {
            return Err("Device accepted-prefix size mismatch".into());
        }
        if segment.samples.len() + tick.accepted_samples.len() > 192000
            || segment.submissions.len() >= 8192
        {
            return Err("Diagnostic segment exceeded two seconds or trace bound".into());
        }
        segment.samples.extend_from_slice(&tick.accepted_samples);
        if tick.written_frames > 0 {
            segment.submissions.push(json!({"ticket":tick.ticket,"writtenFrames":tick.written_frames,"before":tick.before,"after":tick.after}));
        }
        Ok(())
    }
    fn clock(&mut self, record: bool) -> Result<(), String> {
        let clock = self.audio.output().sample()?;
        if !clock.reliable
            || clock.frequency == 0
            || clock.position < self.last_position
            || clock.qpc_100ns < self.last_qpc
        {
            return Err("Media audio clock unreliable or reversed".into());
        }
        self.project_ns = project_time(self.target, clock.position, clock.frequency)?;
        if let Some(source) = self.source {
            self.project_ns = self.project_ns.min(source.end_ns);
            if self.ended {
                self.project_ns = source.end_ns;
            }
        }
        self.last_position = clock.position;
        self.last_qpc = clock.qpc_100ns;
        self.clock_reads = self
            .clock_reads
            .checked_add(1)
            .ok_or("Audio sample sequence exhausted")?;
        self.last_clock = json!({"position":clock.position.to_string(),"frequency":clock.frequency,"qpc100ns":clock.qpc_100ns.to_string(),"reliable":clock.reliable,"paddingFrames":self.audio.output().padding_frames()?,"sourceEpoch":self.video.source_epoch,"renderEpoch":self.video.epoch,"sampleSequence":self.clock_reads});
        if record {
            let samples = &mut self.segments.last_mut().unwrap().clocks;
            if self.source.is_some() && samples.len() >= 64 {
                samples.remove(0);
            } else if samples.len() >= 8192 {
                return Err("Media clock trace exceeded bound".into());
            }
            samples.push(self.last_clock.clone());
        }
        Ok(())
    }
    fn video_update(&mut self, diagnostic: bool) -> Result<(), String> {
        if self.audio.snapshot().cursor.epoch != self.video.source_epoch {
            return Err("Audio/video source epochs diverged".into());
        }
        if let Some(frame) = self.audio.video_frame(self.project_ns)? {
            if frame.epoch != self.video.source_epoch || frame.pts_ns > self.project_ns {
                return Err("Stale or future decoded frame selected".into());
            }
            if self.video.last_submitted != Some(frame.index) {
                let index = frame.index;
                let pts_ns = frame.pts_ns;
                let status = self.sink.submit(MediaFrame {
                    epoch: self.video.epoch,
                    source_epoch: frame.epoch,
                    index,
                    pts_ns,
                    project_ns: self.project_ns,
                    width: self.width,
                    height: self.height,
                    rgb: frame.rgb,
                    diagnostic,
                })?;
                if status == Submission::Stale {
                    return Err("Sink rejected current render generation".into());
                }
                if status != Submission::Busy {
                    self.video.submitted(index, self.project_ns, diagnostic)?;
                }
                self.record(json!({"kind":"submit","stage":if diagnostic {"prepare"} else {"playing"},"renderEpoch":self.video.epoch,"sourceEpoch":self.video.source_epoch,"index":index,"ptsNs":pts_ns.to_string(),"projectTimeNs":self.project_ns.to_string(),"status":status,"diagnostic":diagnostic}))?;
            }
        }
        if let Some(ack) = self.sink.poll()? {
            if let Some((submitted_at, diagnostic)) =
                self.video
                    .acknowledge(ack.epoch, ack.source_epoch, ack.index, ack.accepted)?
            {
                self.record(json!({"kind":"complete","renderEpoch":ack.epoch,"sourceEpoch":ack.source_epoch,"index":ack.index,"projectTimeNs":self.project_ns.to_string(),"submittedProjectTimeNs":submitted_at.to_string(),"accepted":ack.accepted,"diagnostic":diagnostic,"evidence":ack.evidence}))?;
            } else {
                self.record(json!({"kind":"staleAckDiscarded","renderEpoch":ack.epoch,"sourceEpoch":ack.source_epoch,"index":ack.index}))?;
            }
        }
        Ok(())
    }
    fn prepare(&mut self) -> Result<(), String> {
        if self.audio.snapshot().device_running {
            return Err("Cannot prepare while audio is running".into());
        }
        let began = Instant::now();
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Err("Media preparation cancelled".into());
            }
            if began.elapsed() > Duration::from_secs(10) {
                return Err(
                    "Media preparation timed out waiting for audio or accepted due frame".into(),
                );
            }
            self.preparation_checkpoint("actor-preparing")?;
            let first_tick = self.prepare_tick_count == 0;
            if self.audio.snapshot().phase == AdapterPhase::Preparing {
                let tick = self.audio.prepare_tick(4800)?;
                self.capture(tick)?;
                self.prepare_tick_count = self.prepare_tick_count.saturating_add(1);
            }
            self.video_update(self.source.is_none())?;
            if first_tick && self.prepare_tick_count > 0 {
                self.preparation_checkpoint("first-prepare-tick")?;
            }
            let selected = self
                .audio
                .video_snapshot()
                .ok_or("Missing shared video selector")?;
            // A real future frame proves the current frame is the latest due one;
            // a temporarily empty decoder queue alone cannot establish readiness.
            let (eof, pending, indexed) =
                if self.source.is_some() && selected.future_pts_ns.is_none() {
                    let decoder = self
                        .audio
                        .decoder_stats()?
                        .ok_or("Missing source decoder state")?;
                    (
                        decoder.video_eof,
                        decoder.video_pending,
                        decoder.source_index_frames,
                    )
                } else {
                    (false, 0, 0)
                };
            self.ready = matches!(
                self.audio.snapshot().phase,
                AdapterPhase::Ready | AdapterPhase::Paused
            ) && latest_due_ready(
                self.project_ns,
                selected.future_pts_ns,
                selected.current_index,
                self.video.last_completed,
                eof,
                pending,
                indexed,
            );
            if self.ready {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.clock(false)?;
        if let Some(source) = self.source {
            if remaining_audio_frames(
                self.target,
                source.end_ns,
                self.audio.snapshot().cursor.output_frames,
            )? == 0
                && self.audio.output().padding_frames()? == 0
                && self.project_ns.saturating_add(20_834) >= source.end_ns
            {
                let selected = self
                    .audio
                    .video_snapshot()
                    .ok_or("Missing source video selection")?;
                let decoder = self
                    .audio
                    .decoder_stats()?
                    .ok_or("Missing source decoder state")?;
                if selected.future_pts_ns.is_none()
                    && latest_due_ready(
                        source.end_ns,
                        None,
                        selected.current_index,
                        self.video.last_completed,
                        decoder.video_eof,
                        decoder.video_pending,
                        decoder.source_index_frames,
                    )
                {
                    self.ended = true;
                    self.project_ns = source.end_ns;
                }
            }
        }
        self.record(json!({"kind":"prepared","state":self.snapshot(),"elapsedMs":began.elapsed().as_secs_f64()*1000.0,"audioDeviceRunning":false}))?;
        self.first_prepare_complete = true;
        self.preparation.control = None;
        Ok(())
    }
    fn retire(&mut self) -> Result<(), String> {
        let epoch = self.video.retire(self.audio.snapshot().cursor.epoch)?;
        self.ready = false;
        self.sink.retire(epoch)?;
        self.record(json!({"kind":"retire","renderEpoch":epoch,"sourceEpoch":self.video.source_epoch,"audioDeviceRunning":self.audio.snapshot().device_running}))
    }
    fn action(&mut self, action: &Action) -> Result<(), String> {
        match action {
            Action::Prepare => self.prepare()?,
            Action::Play => {
                if self.audio.snapshot().phase == AdapterPhase::Playing {
                    return Ok(());
                }
                if self.ended {
                    self.action(&Action::Seek(0))?;
                }
                if !self.ready {
                    self.prepare()?;
                }
                // Pause can arrive after the endpoint drained but before the
                // next playback tick recorded EOF. Preparation resolves that
                // boundary before attempting to start an empty audio buffer.
                if self.ended {
                    self.action(&Action::Seek(0))?;
                    self.prepare()?;
                }
                self.record(json!({"kind":"start","firstFrameAccepted":self.ready,"renderEpoch":self.video.epoch,"sourceEpoch":self.video.source_epoch,"projectTimeNs":self.project_ns.to_string()}))?;
                self.preparation.state["audioStartAttempted"] = json!(true);
                let cursor = self.audio.snapshot().cursor;
                let observation = StartObservation {
                    source_epoch: self.video.source_epoch,
                    render_epoch: self.video.epoch,
                    accepted_frames: self.segments.last().unwrap().accepted_frames,
                    output_frames: cursor.output_frames,
                    pending_frames: pending_audio_frames(cursor.pending),
                    // Diagnostic failure does not replace the actual Start
                    // result or prevent the unchanged playback attempt.
                    pre_start_padding: self.audio.output().padding_frames(),
                    capacity_frames: self.audio.output().info().buffer_frames,
                    sample_rate: self.audio.output().info().sample_rate,
                };
                let call_began = Instant::now();
                let started = self.audio.start();
                let returned = Instant::now();
                self.segments
                    .last_mut()
                    .unwrap()
                    .audio_diagnostics
                    .record_start(
                        observation,
                        call_began,
                        returned,
                        started.as_ref().err().map(String::as_str),
                    );
                started?;
                self.preparation.state["audioStarted"] = json!(true);
                self.last_pump = returned;
            }
            Action::Pause => {
                if self.source.is_some() && self.audio.snapshot().phase != AdapterPhase::Playing {
                    return Ok(());
                }
                self.audio.pause()?;
                self.clock(false)?;
                self.retire()?;
                self.record(json!({"kind":"paused","state":self.snapshot()}))?;
            }
            Action::Observe => {
                let state = self.audio.snapshot();
                observe_device_clock(
                    state.phase,
                    state.device_running || !self.cleanup_errors.is_empty(),
                    || self.clock(false),
                )?;
                self.record(json!({"kind":"observed","state":self.snapshot()}))?;
            }
            Action::Seek(target) => {
                if self.source.is_some_and(|source| *target > source.end_ns) {
                    return Err("Source seek exceeds its finite endpoint".into());
                }
                if self.source.is_some() && self.segments.len() >= 8 {
                    self.segments.remove(0);
                } else if self.segments.len() >= 8 {
                    return Err("Diagnostic segment count exceeded eight".into());
                }
                // Retire before decoder teardown; queued GPU work cannot use old source bytes.
                if self.audio.snapshot().phase == AdapterPhase::Playing {
                    self.audio.pause()?;
                }
                self.retire()?;
                self.audio.seek(*target)?;
                if let Some(source) = self.source {
                    self.audio.set_source_output_limit(remaining_audio_frames(
                        *target,
                        source.end_ns,
                        0,
                    )?)?;
                }
                self.ended = false;
                self.video.source_epoch = self.audio.snapshot().cursor.epoch;
                self.target = *target;
                self.project_ns = *target;
                self.last_position = 0;
                self.last_qpc = 0;
                self.last_clock = Value::Null;
                self.segment_count = self
                    .segment_count
                    .checked_add(1)
                    .ok_or("Source segment count exhausted")?;
                self.segments
                    .push(Segment::new(*target, self.segment_count));
                self.record(
                    json!({"kind":"seek","targetNs":target.to_string(),"state":self.snapshot(),"seekEvidence":self.audio.evidence().last_seek}),
                )?;
            }
            Action::Cancel | Action::Finish => self.cleanup(),
        }
        Ok(())
    }
    fn pump(&mut self) -> Result<(), String> {
        if self.source.is_some() {
            super::source_audio_validation::before_pump(
                self.audio.output().info().buffer_frames,
                self.audio.output().info().sample_rate,
                self.target,
                self.video.source_epoch,
                self.video.epoch,
            )?;
        }
        let gap = self.last_pump.elapsed().as_secs_f64() * 1000.0;
        self.last_pump = Instant::now();
        let segment = self.segments.last_mut().unwrap();
        segment.max_feed_gap_ms = segment.max_feed_gap_ms.max(gap);
        let cursor = self.audio.snapshot().cursor;
        let remaining = self
            .source
            .map(|source| remaining_audio_frames(self.target, source.end_ns, cursor.output_frames))
            .transpose()?;
        let padding_frames = self.audio.output().padding_frames()?;
        let observed_at = Instant::now();
        if padding_frames == 0 && remaining != Some(0) {
            segment.underruns += 1;
        }
        segment.audio_diagnostics.record_pump(
            PumpObservation {
                observed_at,
                source_epoch: self.video.source_epoch,
                render_epoch: self.video.epoch,
                accepted_frames: segment.accepted_frames,
                output_frames: cursor.output_frames,
                pending_frames: pending_audio_frames(cursor.pending),
                padding_frames,
                remaining_frames: remaining,
                since_last_pump_ms: gap,
            },
            || match self.audio.output().sample() {
                Ok(sample) => ClockObservation {
                    sample: Some(RawClockSample {
                        position: sample.position.to_string(),
                        frequency: sample.frequency,
                        qpc_100ns: sample.qpc_100ns.to_string(),
                        reliable: sample.reliable,
                        hresult: sample.hresult,
                    }),
                    error: None,
                },
                Err(error) => ClockObservation {
                    sample: None,
                    error: Some(error),
                },
            },
        );
        for _ in 0..16 {
            if self.audio.output().padding_frames()? == self.audio.output().info().buffer_frames {
                break;
            }
            let tick = self.audio.pump(4800)?;
            let written = tick.written_frames;
            self.capture(tick)?;
            if written == 0 {
                break;
            }
        }
        self.clock(true)?;
        self.video_update(false)?;
        if let Some(source) = self.source {
            let remaining = remaining_audio_frames(
                self.target,
                source.end_ns,
                self.audio.snapshot().cursor.output_frames,
            )?;
            if remaining == 0
                && self.audio.output().padding_frames()? == 0
                && self.project_ns.saturating_add(20_834) >= source.end_ns
            {
                // Poll at the exclusive endpoint to drain any final delayed due
                // frame. Acceptance alone is insufficient without true EOF.
                self.project_ns = source.end_ns;
                self.video_update(false)?;
                let selected = self
                    .audio
                    .video_snapshot()
                    .ok_or("Missing source video selection")?;
                let decoder = self
                    .audio
                    .decoder_stats()?
                    .ok_or("Missing source decoder state")?;
                if selected.future_pts_ns.is_none()
                    && latest_due_ready(
                        source.end_ns,
                        None,
                        selected.current_index,
                        self.video.last_completed,
                        decoder.video_eof,
                        decoder.video_pending,
                        decoder.source_index_frames,
                    )
                {
                    self.audio.pause()?;
                    self.ended = true;
                    self.ready = true;
                    self.record(json!({"kind":"ended","state":self.snapshot(),"audioPaddingFrames":0,"decoderVideoEof":true}))?;
                }
            }
        }
        self.publish();
        // Timeout is a scheduling wake-up, not EOF or a successful device signal.
        if self.audio.snapshot().device_running {
            let _ = self.audio.output().wait_ready(Duration::from_millis(10))?;
        }
        Ok(())
    }
    fn cleanup(&mut self) {
        if self.audio.snapshot().phase != AdapterPhase::Cancelled {
            if self.audio.snapshot().phase == AdapterPhase::Playing {
                if let Err(error) = self.audio.pause() {
                    self.cleanup_errors.push(error);
                }
            }
            // Invalidate queued pictures before decoder shutdown can wait on children.
            if let Err(error) = self.retire() {
                self.cleanup_errors.push(error);
            }
            if let Err(error) = self.audio.cancel() {
                self.cleanup_errors.push(error);
            }
            self.video.source_epoch = self.audio.snapshot().cursor.epoch;
            // A successful Reset establishes a stopped zero position. Discard
            // cached clock samples even on failure; device_running and cleanup
            // errors retain that uncertainty until Finish releases the device.
            self.last_position = 0;
            self.last_qpc = 0;
            self.last_clock = Value::Null;
        }
    }
    fn run(mut self, rx: Receiver<Command>) -> ActorReport {
        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            #[cfg(debug_assertions)]
            if self.source.is_some()
                && self.audio.snapshot().phase == AdapterPhase::Paused
                && super::source_faults::decoder_failure(self.project_ns)
            {
                let result = self
                    .validation_decoder_failure
                    .take()
                    .ok_or("Missing validation decoder handle".to_owned())
                    .and_then(|terminate| terminate());
                super::source_faults::decoder_terminated(self.project_ns, &result);
                if let Err(error) = result {
                    self.error = Some(error);
                    break;
                }
            }
            if self.source.is_some() {
                if let Err(error) = inspect_idle_decoder(self.audio.snapshot().phase, || {
                    let Some(stats) = self.audio.decoder_stats()? else {
                        return Ok(None);
                    };
                    Ok(stats.error.or_else(|| {
                        stats
                            .processes
                            .iter()
                            .find(|p| p.exited && p.exit_code != Some(0))
                            .map(|p| {
                                format!(
                                    "{} exited unexpectedly while preview was idle: {:?}",
                                    p.label, p.exit_code
                                )
                            })
                    }))
                }) {
                    self.error = Some(error);
                    break;
                }
            }
            let audio_state = self.audio.snapshot();
            let command = if should_pump_device(audio_state.phase, audio_state.device_running) {
                match rx.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            } else {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(command) => Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };
            if let Some(command) = command {
                if self.audio.snapshot().phase == AdapterPhase::Cancelled
                    && !matches!(
                        command.action,
                        Action::Observe | Action::Cancel | Action::Finish
                    )
                {
                    let _ = command.reply.try_send(Err(
                        "Cancelled media accepts only observation or finish".into(),
                    ));
                    continue;
                }
                let done = matches!(command.action, Action::Finish);
                let action_result = self.action(&command.action);
                let result = settle_command_result(
                    &command.action,
                    action_result,
                    &self.cleanup_errors,
                    &mut self.error,
                );
                if result.is_err() {
                    self.cleanup();
                }
                self.publish();
                let _ = command.reply.try_send(result.map(|_| self.snapshot()));
                if done || self.error.is_some() {
                    break;
                }
            }
            let audio_state = self.audio.snapshot();
            if should_pump_device(audio_state.phase, audio_state.device_running) {
                if let Err(error) = self.pump() {
                    self.error = Some(error);
                    break;
                }
            }
        }
        self.cleanup();
        if let Err(error) = self.scheduling.finish() {
            self.cleanup_errors.push(error);
        }
        self.publish();
        let state = self.snapshot();
        let segments: Vec<_> = self.segments.iter().map(Segment::summary).collect();
        let decoder = self.audio.decoder_stats().map_err(|e| e.to_string());
        let source_mode = self.source.is_some();
        self.preparation.state["cleanupErrors"] = json!(self.cleanup_errors);
        self.preparation.state["decoderClose"] = self
            .audio
            .evidence()
            .transfer_close
            .as_ref()
            .map(|value| json!({"Ok":value}))
            .unwrap_or(Value::Null);
        let mut report = json!({"ok":self.error.is_none() && self.cleanup_errors.is_empty(),"error":self.error,"cleanupErrors":self.cleanup_errors,"state":state,"segments":segments,"adapterEvidence":self.audio.evidence(),"decoder":decoder,"output":self.audio.output().info(),"threadScheduling":self.scheduling.info(),"preparation":self.preparation.state,"scope":{"sourcePlayback":source_mode,"mutedSession":self.audio.output().info().session_muted,"pcmCapture":!source_mode,"diagnosticReadback":!source_mode,"audioOnUiThread":false,"presentAcceptanceIsDisplayProof":false,"commandCapacity":4,"maxFramesTrace":if source_mode {128} else {3000},"maxLifecycleEvents":MAX_LIFECYCLE_EVENTS,"maxClockSamplesPerSegment":if source_mode {64} else {8192},"maxAcceptedFramesPerSegment":if source_mode {Value::Null} else {json!(96000)},"retainedSegments":8}});
        report
            .as_object_mut()
            .unwrap()
            .append(self.events.report().as_object_mut().unwrap());
        let captures = self
            .segments
            .into_iter()
            .filter(|_| !source_mode)
            .map(|s| (s.target, s.samples))
            .collect();
        drop(self.audio);
        drop(self.scheduling);
        report["preparation"]["deviceReleased"] = json!(true);
        report["preparation"]["resourcesReleased"] = json!(true);
        ActorReport {
            json: report,
            captures,
        }
    }
}
fn project_time(target: u64, position: u64, frequency: u64) -> Result<u64, String> {
    if frequency == 0 {
        return Err("Zero audio device clock frequency".into());
    }
    u64::try_from(u128::from(target) + u128::from(position) * 1_000_000_000 / u128::from(frequency))
        .map_err(|_| "Audio project time overflow".into())
}

fn inspect_idle_decoder(
    phase: AdapterPhase,
    read_error: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<(), String> {
    if matches!(phase, AdapterPhase::Ready | AdapterPhase::Paused) {
        if let Some(error) = read_error()? {
            return Err(error);
        }
    }
    Ok(())
}

fn settle_command_result(
    action: &Action,
    result: Result<(), String>,
    cleanup_errors: &[String],
    primary_error: &mut Option<String>,
) -> Result<(), String> {
    if matches!(action, Action::Cancel | Action::Finish) {
        return if cleanup_errors.is_empty() {
            result
        } else {
            Err(format!(
                "Media cleanup failed: {}",
                cleanup_errors.join("; ")
            ))
        };
    }
    if let Err(error) = &result {
        primary_error.get_or_insert_with(|| error.clone());
    }
    result
}

/// Reserved cursor span only; this does not prove decoded samples are ready.
fn pending_audio_frames(ticket: Option<super::audio_cursor::Ticket>) -> Option<u32> {
    ticket.map(|ticket| {
        ticket.span.leading_silence_frames
            + ticket.span.source_frames
            + ticket.span.trailing_silence_frames
    })
}

fn should_pump_device(phase: AdapterPhase, device_running: bool) -> bool {
    phase == AdapterPhase::Playing && device_running
}

fn observe_device_clock(
    phase: AdapterPhase,
    cleanup_unconfirmed: bool,
    sample: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if phase == AdapterPhase::Cancelled && cleanup_unconfirmed {
        Ok(())
    } else {
        sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_trace_keeps_seek_shutdown_evidence_after_frame_flood() {
        let mut trace = EventTrace::default();
        let seek = json!({"kind":"seek","targetNs":"7000000000","seekEvidence":{"oldDecoderShutdown":{"joined":true,"processesReaped":true,"cleanupErrors":[],"processes":[{"pid":321,"exitCode":0}]}}});
        let retire =
            json!({"kind":"retire","renderEpoch":2,"sourceEpoch":1,"audioDeviceRunning":false});
        trace.record(true, retire.clone()).unwrap();
        trace.record(true, seek.clone()).unwrap();
        for index in 0..400 {
            for kind in ["submit", "complete", "staleAckDiscarded"] {
                trace
                    .record(true, json!({"kind":kind,"index":index}))
                    .unwrap();
            }
        }
        let report = trace.report();
        assert_eq!(report["lifecycleEvents"], json!([retire, seek]));
        assert_eq!(report["eventCount"], 1202);
        assert_eq!(report["videoEvents"].as_array().unwrap().len(), 128);
        assert_eq!(report["lifecycleEventCount"], 2);
        assert_eq!(report["lifecycleEventsDropped"], 0);
        assert_eq!(report["lifecycleEventsComplete"], true);
        let summary = trace.summary();
        assert_eq!(summary["retainedLifecycleEvents"], 2);
        assert!(summary.get("videoEvents").is_none());
        assert!(summary.get("lifecycleEvents").is_none());
    }
    #[test]
    fn lifecycle_trace_overflow_marks_incomplete_without_stopping_recording() {
        let mut trace = EventTrace::default();
        for index in 0..67 {
            trace
                .record(true, json!({"kind":"observed","observation":index}))
                .unwrap();
        }
        trace
            .record(true, json!({"kind":"submit","index":999}))
            .unwrap();
        let report = trace.report();
        assert_eq!(report["lifecycleEventCount"], 67);
        assert_eq!(report["retainedLifecycleEvents"], 64);
        assert_eq!(report["lifecycleEventsDropped"], 3);
        assert_eq!(report["lifecycleEventsComplete"], false);
        assert_eq!(report["lifecycleEvents"][0]["observation"], 0);
        assert_eq!(report["lifecycleEvents"][63]["observation"], 63);
        assert_eq!(report["eventCount"], 68);
        assert_eq!(report["videoEvents"][67]["index"], 999);
    }
    #[test]
    fn lifecycle_trace_preserves_unknown_kinds_for_both_actor_modes() {
        let events = vec![
            json!({"kind":"futureControl","raw":{"ok":true}}),
            json!({"missingKind":true}),
            json!({"kind":17}),
        ];
        for source_mode in [false, true] {
            let mut trace = EventTrace::default();
            for event in &events {
                trace.record(source_mode, event.clone()).unwrap();
            }
            assert_eq!(trace.report()["lifecycleEvents"], json!(events));
            assert_eq!(trace.report()["lifecycleEventCount"], 3);
            assert_eq!(trace.report()["lifecycleEventsComplete"], true);
        }
    }
    #[test]
    fn cancelled_device_is_not_pumped_even_if_stop_failed() {
        assert!(!should_pump_device(AdapterPhase::Cancelled, true));
        assert!(!should_pump_device(AdapterPhase::Cancelled, false));
        assert!(should_pump_device(AdapterPhase::Playing, true));
        assert!(!should_pump_device(AdapterPhase::Playing, false));
    }
    #[test]
    fn failed_cancel_observation_does_not_resample_retired_device() {
        assert!(
            observe_device_clock(AdapterPhase::Cancelled, true, || {
                Err("retired device clock unavailable".into())
            })
            .is_ok()
        );
        for phase in [
            AdapterPhase::Preparing,
            AdapterPhase::Ready,
            AdapterPhase::Playing,
            AdapterPhase::Paused,
            AdapterPhase::Cancelled,
        ] {
            assert_eq!(
                observe_device_clock(phase, false, || Err("device invalidated".into()))
                    .unwrap_err(),
                "device invalidated"
            );
        }
    }
    #[test]
    fn cleanup_command_reply_retains_first_failure_on_cancel_and_finish() {
        let errors = vec![
            "Stop failed: device invalidated".to_owned(),
            "Reset failed".to_owned(),
        ];
        let mut primary = None;
        for action in [Action::Cancel, Action::Cancel, Action::Finish] {
            let reply = settle_command_result(&action, Ok(()), &errors, &mut primary);
            let reason =
                reply.expect_err("cleanup failure must not acknowledge cancellation success");
            assert!(reason.contains(&errors[0]));
            assert!(reason.contains(&errors[1]));
            assert!(
                primary.is_none(),
                "cleanup failure is not a playback failure"
            );
        }
        assert!(settle_command_result(&Action::Observe, Ok(()), &errors, &mut primary).is_ok());
    }
    #[test]
    fn cleanup_command_does_not_replace_original_playback_error() {
        let mut primary = Some("RGB decoder failed".to_owned());
        let errors = vec!["sink retirement failed".to_owned()];
        let reply = settle_command_result(&Action::Cancel, Ok(()), &errors, &mut primary);
        assert!(reply.unwrap_err().contains("sink retirement failed"));
        assert_eq!(primary.as_deref(), Some("RGB decoder failed"));
        assert!(settle_command_result(&Action::Finish, Ok(()), &[], &mut primary).is_ok());
        assert_eq!(primary.as_deref(), Some("RGB decoder failed"));
    }
    #[test]
    fn cleanup_command_success_and_playback_failure_remain_distinct() {
        let mut primary = None;
        for action in [Action::Cancel, Action::Finish] {
            assert!(settle_command_result(&action, Ok(()), &[], &mut primary).is_ok());
            assert!(primary.is_none());
        }
        let reply = settle_command_result(
            &Action::Play,
            Err("RGB decoder failed".into()),
            &[],
            &mut primary,
        );
        assert_eq!(reply.unwrap_err(), "RGB decoder failed");
        assert_eq!(primary.as_deref(), Some("RGB decoder failed"));
    }
    #[test]
    fn finish_joins_and_returns_report_even_when_cleanup_reply_fails() {
        let (tx, rx) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let command = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let Command { action, reply } = command;
            assert!(matches!(action, Action::Finish));
            assert!(worker_stop.load(Ordering::Acquire));
            let _ = reply.try_send(Err("Reset failed".into()));
            ActorReport {
                json: json!({"ok":false,"error":null,"cleanupErrors":["Reset failed"],"preparation":{"resourcesReleased":true,"cleanupErrors":["Reset failed"]}}),
                captures: vec![(12, vec![0.25])],
            }
        });
        let actor = MediaActor {
            tx,
            status: Arc::new(Mutex::new(Value::Null)),
            stop,
            worker: Some(worker),
        };
        let report = actor
            .finish()
            .expect("finish must retain failed cleanup evidence");
        assert_eq!(report.json["cleanupErrors"], json!(["Reset failed"]));
        assert_eq!(report.json["preparation"]["workerJoined"], true);
        assert_eq!(report.json["preparation"]["cleanupConfirmed"], false);
        assert_eq!(report.captures, vec![(12, vec![0.25])]);
    }
    #[test]
    fn controlled_initialization_stops_at_each_resource_boundary() {
        for cancelled_at in [
            "decoder-ready",
            "device-before-open",
            "device-opened",
            "adapter-ready",
            "actor-initialized",
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let checkpoint_calls = Arc::clone(&calls);
            let control = SourcePreparationControl {
                checkpoint: Arc::new(move |stage| {
                    checkpoint_calls
                        .lock()
                        .unwrap()
                        .push(format!("check:{stage}"));
                    if stage == cancelled_at {
                        Err("cancelled by owner".into())
                    } else {
                        Ok(())
                    }
                }),
                observe: Arc::new(|_| {}),
            };
            let mut resources = MockInitialization {
                calls: Arc::clone(&calls),
                fail: None,
            };
            let mut preparation = PreparationTrace::new(Some(control));
            let result =
                initialize_resources(&mut resources, &mut preparation, &AtomicBool::new(false));
            assert_eq!(result.unwrap_err(), "cancelled by owner");
            let recorded = calls.lock().unwrap();
            assert_eq!(recorded.last().unwrap(), &format!("check:{cancelled_at}"));
            if matches!(cancelled_at, "decoder-ready" | "device-before-open") {
                assert!(!recorded.iter().any(|v| v == "device-open"));
            }
            assert!(!recorded.iter().any(|v| v == "audio-start"));
        }
    }
    #[test]
    fn controlled_initialization_preserves_resource_failure_and_skips_later_stages() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut resources = MockInitialization {
            calls: Arc::clone(&calls),
            fail: Some("device-open"),
        };
        let mut preparation = PreparationTrace::new(None);
        assert_eq!(
            initialize_resources(&mut resources, &mut preparation, &AtomicBool::new(false))
                .unwrap_err(),
            "device-open failed"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            ["decoder-create", "sink-retire", "device-open"]
        );
    }
    #[test]
    fn preparation_observation_precedes_checkpoint_and_stop_is_rechecked_after_it() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&events);
        let checked = Arc::clone(&events);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let control = SourcePreparationControl {
            observe: Arc::new(move |value| observed.lock().unwrap().push(value)),
            checkpoint: Arc::new(move |stage| {
                checked.lock().unwrap().push(json!({"checkpoint":stage}));
                stopped.store(true, Ordering::Release);
                Ok(())
            }),
        };
        let mut preparation = PreparationTrace::new(Some(control));
        let result = preparation.checkpoint(
            "first-prepare-tick",
            json!({"prepareTickCount":1,"acceptedFrames":0}),
            &stop,
        );
        assert!(result.unwrap_err().contains("cancelled"));
        let events = events.lock().unwrap();
        assert_eq!(events[0]["resources"]["prepareTickCount"], 1);
        assert_eq!(events[0]["resources"]["acceptedFrames"], 0);
        assert_eq!(events[1]["checkpoint"], "first-prepare-tick");
        assert_eq!(preparation.state["audioStartAttempted"], false);
    }
    #[test]
    fn joined_worker_does_not_erase_unconfirmed_resource_cleanup() {
        for (released, errors) in [(false, json!([])), (true, json!(["close failed"]))] {
            let mut report = json!({"cleanupErrors":errors,"preparation":{"resourcesReleased":released,"cleanupErrors":errors,"workerJoined":false}});
            mark_worker_joined(&mut report);
            assert_eq!(report["preparation"]["workerJoined"], true);
            assert_eq!(report["preparation"]["cleanupConfirmed"], false);
        }
        let mut report = json!({"cleanupErrors":[],"preparation":{"resourcesReleased":true,"cleanupErrors":[],"workerJoined":false}});
        assert!(report["preparation"]["cleanupConfirmed"].is_null());
        mark_worker_joined(&mut report);
        assert_eq!(report["preparation"]["cleanupConfirmed"], true);
    }
    #[test]
    fn invalid_source_range_never_launches_worker_or_device() {
        let failure = no_worker_error("invalid source range".into());
        assert!(failure.joined);
        let report = failure.report.unwrap();
        assert_eq!(report["preparation"]["workerNeverCreated"], true);
        assert_eq!(report["preparation"]["decoderCreationAttempted"], false);
        assert_eq!(report["preparation"]["decoderCreated"], false);
        assert_eq!(report["preparation"]["deviceOpenAttempted"], false);
        assert_eq!(report["preparation"]["deviceOpened"], false);
        assert_eq!(report["preparation"]["cleanupConfirmed"], true);
    }
    struct MockInitialization {
        calls: Arc<Mutex<Vec<String>>>,
        fail: Option<&'static str>,
    }
    impl InitializationResources for MockInitialization {
        fn step(&mut self, step: &'static str) -> Result<(), String> {
            self.calls.lock().unwrap().push(step.into());
            if self.fail == Some(step) {
                Err(format!("{step} failed"))
            } else {
                Ok(())
            }
        }
        fn evidence(&self) -> Value {
            Value::Null
        }
    }
    #[test]
    fn idle_source_errors_are_observed_without_starting_output() {
        for phase in [AdapterPhase::Ready, AdapterPhase::Paused] {
            assert!(inspect_idle_decoder(phase, || Ok(Some("RGB pipe failed".into()))).is_err());
            assert!(
                inspect_idle_decoder(phase, || Err("decoder status unavailable".into())).is_err()
            );
            assert!(inspect_idle_decoder(phase, || Ok(None)).is_ok());
        }
        for phase in [AdapterPhase::Playing, AdapterPhase::Cancelled] {
            assert!(
                inspect_idle_decoder(phase, || panic!(
                    "active pump or retired source must not be probed here"
                ))
                .is_ok()
            );
        }
    }
    #[test]
    fn audio_project_time_uses_device_frequency_and_checks_overflow() {
        assert_eq!(
            project_time(2_000_000_000, 24_000, 48_000).unwrap(),
            2_500_000_000
        );
        assert!(project_time(0, 1, 0).is_err());
        assert!(project_time(u64::MAX, 1, 1).is_err());
    }
}
