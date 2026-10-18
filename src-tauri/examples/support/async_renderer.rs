//! A bounded CPU-frame mailbox with a single thread-owned D3D11 renderer.
//! The producer never waits on GPU work. This diagnostic has no presentation
//! window and does not pretend a Rust thread can preempt a hung driver call.

use super::{
    d3d11_renderer::{D3d11Renderer, RenderResult, RendererInfo},
    frame_verification::{FrameVerification, verify_readback},
};
use kaicut_lib::preview_queue::{Completion, PreviewQueue, Submission};
use std::{
    sync::{
        Arc, Condvar, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct Frame {
    pub index: usize,
    pub pts_ns: u64,
    pub rgb: Arc<[u8]>,
}

pub struct CompletedFrame {
    pub epoch: u64,
    pub index: usize,
    pub pts_ns: u64,
    pub result: RenderResult,
    pub verification: FrameVerification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmitStatus {
    Queued,
    Evicted,
    Stale,
    Busy,
    Closed,
    Failed,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStats {
    pub epoch: u64,
    pub pending_depth: usize,
    pub completion_pending: bool,
    pub in_flight: bool,
    pub in_flight_epoch: Option<u64>,
    pub submitted: u64,
    pub evicted: u64,
    pub stale_submissions: u64,
    pub stale_completions: u64,
    pub high_water: usize,
    pub taken: u64,
    pub completed: u64,
    pub overwritten_results: u64,
    pub closed_submissions: u64,
    pub closed_completions: u64,
    pub invalidated_pending: u64,
    pub invalidated_results: u64,
    pub rendered: u64,
    pub stale_before_render: u64,
    pub submit_busy: u64,
    pub failures: u64,
    pub closed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownReport {
    pub joined: bool,
    pub elapsed_ms: f64,
    pub exceeded_expected_wait: bool,
    /// Backend destructor ran on its owning worker; this is not proof of COM
    /// leak freedom or a driver deadline guarantee.
    pub renderer_drop_ms: Option<f64>,
    pub renderer_drop_panicked: bool,
    pub loop_exit_after_close_ms: Option<f64>,
    /// Timestamp immediately before the worker closure returns, not an OS
    /// thread-termination timestamp. join_wait_ms includes that final overhead.
    pub worker_exit_after_close_ms: Option<f64>,
    pub worker_finished_before_close: bool,
    pub close_requested_while_in_flight: Option<bool>,
    pub join_wait_ms: f64,
    pub shutdown_overhead_ms: f64,
    pub stats: WorkerStats,
    pub error: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetireReport {
    pub instance_id: String,
    pub old_epoch: u64,
    pub new_epoch: u64,
    pub elapsed_ms: f64,
    pub stats: WorkerStats,
}

struct State {
    queue: PreviewQueue<Frame, CompletedFrame>,
    in_flight_epoch: Option<u64>,
    rendered: u64,
    stale_before_render: u64,
    failures: u64,
    error: Option<String>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stopping: AtomicBool,
    failed: AtomicBool,
    submit_busy: AtomicU64,
    exit: Mutex<ExitTelemetry>,
}

#[derive(Clone, Default)]
struct ExitTelemetry {
    close_requested: Option<Instant>,
    close_while_in_flight: Option<bool>,
    loop_exited: Option<Instant>,
    renderer_drop_duration: Option<Duration>,
    renderer_drop_panicked: bool,
    worker_finished: Option<Instant>,
}

trait RendererBackend {
    fn info(&self) -> &RendererInfo;
    fn render_rgb(&mut self, rgb: &[u8]) -> Result<RenderResult, String>;
}
impl RendererBackend for D3d11Renderer {
    fn info(&self) -> &RendererInfo {
        self.info()
    }
    fn render_rgb(&mut self, rgb: &[u8]) -> Result<RenderResult, String> {
        self.render_rgb(rgb)
    }
}

pub struct AsyncRenderer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    info: RendererInfo,
    instance_id: String,
}

impl AsyncRenderer {
    pub fn start(width: u32, height: u32, delay_ms: u64) -> Result<Self, String> {
        Self::start_with_factory(delay_ms, move || D3d11Renderer::new(width, height))
    }

    fn start_with_factory<B, F>(delay_ms: u64, factory: F) -> Result<Self, String>
    where
        B: RendererBackend + 'static,
        F: FnOnce() -> Result<B, String> + Send + 'static,
    {
        if delay_ms > 2000 {
            return Err("Injected render delay is limited to 2000 ms".into());
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: PreviewQueue::new(2).map_err(|error| error.to_string())?,
                in_flight_epoch: None,
                rendered: 0,
                stale_before_render: 0,
                failures: 0,
                error: None,
            }),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            submit_busy: AtomicU64::new(0),
            exit: Mutex::new(ExitTelemetry::default()),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("kaicut-offscreen-render".into())
            .spawn(move || {
                // B need not implement Send: it is constructed, used and dropped
                // inside this worker. Only the Send factory crosses the boundary.
                let created = std::panic::catch_unwind(std::panic::AssertUnwindSafe(factory))
                    .unwrap_or_else(|_| {
                        Err("Renderer factory panicked before returning a backend".into())
                    });
                match created {
                    Ok(mut backend) => {
                        // The outer worker retains ownership across a render
                        // panic so destruction has its own unwind boundary.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            ready_tx
                                .send(Ok(backend.info().clone()))
                                .map_err(|_| "Renderer startup listener disconnected".to_owned())?;
                            render_loop(
                                &worker_shared,
                                &mut backend,
                                Duration::from_millis(delay_ms),
                            )
                        }))
                        .unwrap_or_else(|_| {
                            Err("Renderer worker panicked during its work loop".into())
                        });
                        worker_shared
                            .exit
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .loop_exited = Some(Instant::now());
                        if let Err(error) = result {
                            record_failure(&worker_shared, &error);
                            let _ = ready_tx.try_send(Err(error));
                        }
                        let drop_started = Instant::now();
                        let dropped =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                drop(backend)
                            }));
                        let drop_duration = drop_started.elapsed();
                        {
                            let mut exit = worker_shared
                                .exit
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            exit.renderer_drop_duration = Some(drop_duration);
                            exit.renderer_drop_panicked = dropped.is_err();
                        }
                        if dropped.is_err() {
                            let error = "Renderer backend destructor panicked on its owning worker";
                            record_failure(&worker_shared, error);
                            let _ = ready_tx.try_send(Err(error.to_owned()));
                        }
                    }
                    Err(error) => {
                        record_failure(&worker_shared, &error);
                        let _ = ready_tx.try_send(Err(error));
                    }
                }
                worker_shared
                    .exit
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .worker_finished = Some(Instant::now());
            })
            .map_err(|error| format!("Cannot start renderer worker: {error}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(info)) => Ok(Self {
                shared,
                thread: Some(handle),
                info,
                instance_id: uuid::Uuid::new_v4().to_string(),
            }),
            result => {
                close_shared(&shared);
                // Never detach an owned GPU worker. Rust cannot interrupt an
                // OS/driver call that exceeds its own deadline; joining here
                // preserves ownership even if that exceptional call is slow.
                let joined = handle.join();
                let error = match result {
                    Ok(Err(error)) => error,
                    Err(error) => {
                        format!("Renderer startup did not complete within 15 seconds: {error}")
                    }
                    Ok(Ok(_)) => unreachable!(),
                };
                Err(if joined.is_err() {
                    format!("{error}; renderer thread panicked during shutdown")
                } else {
                    error
                })
            }
        }
    }

    pub fn info(&self) -> &RendererInfo {
        &self.info
    }
    /// Identity of this owner and its single successfully constructed backend,
    /// not a globally unique identity of a physical adapter or COM object.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// End one producer session without destroying or reconstructing the
    /// worker-owned backend. Call only from the single control owner after
    /// stopping audio; no new producer may submit until this call returns.
    /// Epoch advancement invalidates queued/completed old work immediately,
    /// then the condvar wait releases the state lock while in-flight GPU work
    /// finishes. A timeout is an error, not an idle guarantee: the new epoch is
    /// retained, the worker remains owned, and a stuck driver is not killed.
    pub fn retire_epoch(&self, timeout: Duration) -> Result<RetireReport, String> {
        if !(Duration::from_millis(1)..=Duration::from_millis(3000)).contains(&timeout) {
            return Err("Renderer retirement timeout must be within 1..=3000 ms".into());
        }
        let started = Instant::now();
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| "Renderer queue mutex is poisoned")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        if self.shared.stopping.load(Ordering::Acquire) || state.queue.is_closed() {
            return Err("Cannot retire a closed renderer".into());
        }
        let old_epoch = state.queue.epoch();
        let new_epoch = state
            .queue
            .advance_epoch()
            .map_err(|error| error.to_string())?;
        self.shared.wake.notify_all();
        loop {
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            if self.shared.stopping.load(Ordering::Acquire) || state.queue.is_closed() {
                return Err(
                    "Renderer closed during epoch retirement; idle was not confirmed".into(),
                );
            }
            if state.queue.epoch() != new_epoch {
                return Err("Renderer epoch changed concurrently during retirement".into());
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "Renderer retirement timed out after {} ms; epoch {new_epoch} remains active, old GPU work may still be owned, and idle was not confirmed",
                    timeout.as_millis()
                ));
            }
            if state.in_flight_epoch.is_none()
                && state.queue.pending_len() == 0
                && !state.queue.has_completed()
            {
                return Ok(RetireReport {
                    instance_id: self.instance_id.clone(),
                    old_epoch,
                    new_epoch,
                    elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                    stats: self.stats_locked(&state),
                });
            }
            state = self
                .shared
                .wake
                .wait_timeout(state, timeout.saturating_sub(started.elapsed()))
                .map_err(|_| "Renderer retirement wait is poisoned")?
                .0;
        }
    }
    pub fn advance_epoch(&self) -> Result<u64, String> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| "Renderer queue mutex is poisoned")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        let epoch = state
            .queue
            .advance_epoch()
            .map_err(|error| error.to_string())?;
        drop(state);
        self.shared.wake.notify_all();
        Ok(epoch)
    }
    pub fn submit(&self, epoch: u64, frame: Frame) -> SubmitStatus {
        if self.shared.failed.load(Ordering::Acquire) {
            return SubmitStatus::Failed;
        }
        // Capacity is also bounded in bytes: only one exact, declared RGB
        // frame is accepted per Arc payload, never an arbitrary large blob.
        if frame.rgb.len() != self.info.width as usize * self.info.height as usize * 3 {
            return SubmitStatus::Failed;
        }
        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => {
                let _ = self.shared.submit_busy.fetch_update(
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                    |value| Some(value.saturating_add(1)),
                );
                return SubmitStatus::Busy;
            }
            Err(TryLockError::Poisoned(_)) => return SubmitStatus::Failed,
        };
        if state.error.is_some() {
            return SubmitStatus::Failed;
        }
        let result = state.queue.submit(epoch, frame);
        drop(state);
        // Returned rejected/evicted CPU payloads drop after releasing the lock.
        let status = match result {
            Submission::Accepted { evicted: None } => SubmitStatus::Queued,
            Submission::Accepted { evicted: Some(_) } => SubmitStatus::Evicted,
            Submission::Stale { .. } => SubmitStatus::Stale,
            Submission::Closed { .. } => SubmitStatus::Closed,
        };
        if matches!(status, SubmitStatus::Queued | SubmitStatus::Evicted) {
            self.shared.wake.notify_one();
        }
        status
    }
    pub fn poll_latest(&self) -> Option<CompletedFrame> {
        self.shared
            .state
            .try_lock()
            .ok()?
            .queue
            .take_completed()
            .map(|job| job.item)
    }
    pub fn stats(&self) -> Result<WorkerStats, String> {
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| "Renderer queue mutex is poisoned")?;
        Ok(self.stats_locked(&state))
    }
    fn stats_locked(&self, state: &State) -> WorkerStats {
        let counters = state.queue.counters();
        WorkerStats {
            epoch: state.queue.epoch(),
            pending_depth: state.queue.pending_len(),
            completion_pending: state.queue.has_completed(),
            in_flight: state.in_flight_epoch.is_some(),
            in_flight_epoch: state.in_flight_epoch,
            submitted: counters.submitted,
            evicted: counters.evicted,
            stale_submissions: counters.stale_submissions,
            stale_completions: counters.stale_completions,
            high_water: counters.high_water,
            taken: counters.taken,
            completed: counters.completed,
            overwritten_results: counters.overwritten_results,
            closed_submissions: counters.closed_submissions,
            closed_completions: counters.closed_completions,
            invalidated_pending: counters.invalidated_pending,
            invalidated_results: counters.invalidated_results,
            rendered: state.rendered,
            stale_before_render: state.stale_before_render,
            submit_busy: self.shared.submit_busy.load(Ordering::Relaxed),
            failures: state.failures,
            closed: state.queue.is_closed(),
            error: state.error.clone(),
        }
    }
    pub fn health(&self) -> Result<(), String> {
        // The audio producer calls this frequently; it must not acquire a
        // blocking mutex even when a completion is being published.
        if !self.shared.failed.load(Ordering::Acquire) {
            return Ok(());
        }
        let error = self
            .shared
            .state
            .try_lock()
            .ok()
            .and_then(|state| state.error.clone());
        Err(error.unwrap_or_else(|| "Renderer worker failed".into()))
    }
    pub fn shutdown(&mut self) -> Result<ShutdownReport, String> {
        let started = Instant::now();
        close_shared(&self.shared);
        let join_started = Instant::now();
        if let Some(handle) = self.thread.take() {
            if handle.join().is_err() {
                record_failure(
                    &self.shared,
                    "Renderer thread panicked outside its work boundary",
                );
            }
        }
        let join_wait_ms = join_started.elapsed().as_secs_f64() * 1000.0;
        let stats = self.stats()?;
        let exit = self
            .shared
            .exit
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let since_close = |time: Option<Instant>| {
            time.and_then(|end| {
                exit.close_requested
                    .and_then(|start| end.checked_duration_since(start))
            })
            .map(|duration| duration.as_secs_f64() * 1000.0)
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(ShutdownReport {
            joined: true,
            elapsed_ms,
            exceeded_expected_wait: elapsed_ms > 3000.0,
            renderer_drop_ms: exit
                .renderer_drop_duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            renderer_drop_panicked: exit.renderer_drop_panicked,
            loop_exit_after_close_ms: since_close(exit.loop_exited),
            worker_exit_after_close_ms: since_close(exit.worker_finished),
            worker_finished_before_close: matches!((exit.worker_finished, exit.close_requested), (Some(end), Some(start)) if end < start),
            close_requested_while_in_flight: exit.close_while_in_flight,
            join_wait_ms,
            shutdown_overhead_ms: (elapsed_ms - join_wait_ms).max(0.0),
            error: stats.error.clone(),
            stats,
        })
    }
}

impl Drop for AsyncRenderer {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.shutdown();
        }
    }
}

fn close_shared(shared: &Shared) {
    let requested_at = Instant::now();
    {
        let mut exit = shared
            .exit
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        exit.close_requested.get_or_insert(requested_at);
    }
    shared.stopping.store(true, Ordering::Release);
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let in_flight = state.in_flight_epoch.is_some();
    state.queue.close();
    drop(state);
    // Never hold telemetry and queue locks together; a failing worker and a
    // concurrent shutdown cannot acquire them in opposite order.
    shared
        .exit
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .close_while_in_flight
        .get_or_insert(in_flight);
    shared.wake.notify_all();
}

fn record_failure(shared: &Shared, error: &str) {
    shared.failed.store(true, Ordering::Release);
    shared.stopping.store(true, Ordering::Release);
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.error = Some(match state.error.take() {
        Some(previous) => format!("{previous}; {error}").chars().take(4096).collect(),
        None => error.chars().take(4096).collect(),
    });
    state.failures = state.failures.saturating_add(1);
    state.in_flight_epoch = None;
    state.queue.close();
    drop(state);
    shared.wake.notify_all();
}

fn render_loop<B: RendererBackend>(
    shared: &Shared,
    backend: &mut B,
    delay: Duration,
) -> Result<(), String> {
    let width = backend.info().width;
    let height = backend.info().height;
    loop {
        let job = {
            let mut state = shared
                .state
                .lock()
                .map_err(|_| "Renderer queue mutex is poisoned")?;
            loop {
                if shared.stopping.load(Ordering::Acquire) || state.queue.is_closed() {
                    return Ok(());
                }
                if let Some(job) = state.queue.take() {
                    state.in_flight_epoch = Some(job.epoch);
                    break job;
                }
                state = shared
                    .wake
                    .wait_timeout(state, Duration::from_millis(10))
                    .map_err(|_| "Renderer queue wait is poisoned")?
                    .0;
            }
        };
        // Interrupt injected delay on close/seek. Never sleep under a mutex:
        // Condvar releases it, and the wait interval is bounded at five ms.
        let delay_started = Instant::now();
        let valid = {
            let mut state = shared
                .state
                .lock()
                .map_err(|_| "Renderer queue mutex is poisoned")?;
            loop {
                if shared.stopping.load(Ordering::Acquire)
                    || state.queue.is_closed()
                    || state.queue.epoch() != job.epoch
                {
                    state.in_flight_epoch = None;
                    state.stale_before_render = state.stale_before_render.saturating_add(1);
                    break false;
                }
                let remaining = delay.saturating_sub(delay_started.elapsed());
                if remaining.is_zero() {
                    break true;
                }
                state = shared
                    .wake
                    .wait_timeout(state, remaining.min(Duration::from_millis(5)))
                    .map_err(|_| "Renderer delay wait is poisoned")?
                    .0;
            }
        };
        if !valid {
            shared.wake.notify_all();
            continue;
        }
        // Creation, shader upload/draw/readback, verification, and eventual
        // backend destruction all occur here, outside the mailbox lock.
        let result = backend.render_rgb(&job.item.rgb)?;
        let verification = verify_readback(&job.item.rgb, &result.rgba, width, height)?;
        if !verification.exact {
            return Err(format!(
                "GPU readback mismatch: {} pixels",
                verification.mismatched_pixels
            ));
        }
        let completed = CompletedFrame {
            epoch: job.epoch,
            index: job.item.index,
            pts_ns: job.item.pts_ns,
            result,
            verification,
        };
        let mut state = shared
            .state
            .lock()
            .map_err(|_| "Renderer queue mutex is poisoned")?;
        state.in_flight_epoch = None;
        state.rendered = state.rendered.saturating_add(1);
        let publication = state.queue.publish(job.epoch, completed);
        drop(state);
        shared.wake.notify_all();
        match publication {
            Completion::Published { overwritten } => drop(overwritten),
            Completion::Stale { result } | Completion::Closed { result } => drop(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{marker::PhantomData, rc::Rc};

    struct FakeRenderer {
        info: RendererInfo,
        barrier: Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
        panic: bool,
        fail: bool,
        owner: thread::ThreadId,
        drop_delay: Duration,
        drop_panic: bool,
        dropped_on: Option<Arc<Mutex<Option<thread::ThreadId>>>>,
        drop_count: Option<Arc<AtomicU64>>,
        _not_send: PhantomData<Rc<()>>,
    }
    impl FakeRenderer {
        fn new() -> Self {
            Self {
                info: RendererInfo {
                    adapter_description: "unit-test fake, no GPU".into(),
                    vendor_id: 0,
                    device_id: 0,
                    adapter_flags: 0,
                    luid_low: 0,
                    luid_high: 0,
                    feature_level: 0,
                    width: 1,
                    height: 1,
                },
                barrier: None,
                panic: false,
                fail: false,
                owner: thread::current().id(),
                drop_delay: Duration::ZERO,
                drop_panic: false,
                dropped_on: None,
                drop_count: None,
                _not_send: PhantomData,
            }
        }
    }
    impl Drop for FakeRenderer {
        fn drop(&mut self) {
            assert_eq!(self.owner, thread::current().id());
            if let Some(count) = &self.drop_count {
                count.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(thread) = &self.dropped_on {
                *thread.lock().unwrap() = Some(thread::current().id());
            }
            if !self.drop_delay.is_zero() {
                thread::sleep(self.drop_delay);
            }
            assert!(!self.drop_panic, "injected renderer destructor panic");
        }
    }
    impl RendererBackend for FakeRenderer {
        fn info(&self) -> &RendererInfo {
            &self.info
        }
        fn render_rgb(&mut self, rgb: &[u8]) -> Result<RenderResult, String> {
            assert_eq!(self.owner, thread::current().id());
            if let Some((entered, release)) = self.barrier.take() {
                entered.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            assert!(!self.panic, "injected worker panic");
            if self.fail {
                return Err("injected renderer failure".into());
            }
            Ok(RenderResult {
                rgba: vec![rgb[0], rgb[1], rgb[2], 255],
                row_pitch: 4,
                upload_draw_readback_ms: 0.0,
                readback_wait_ms: 0.0,
            })
        }
    }
    fn frame(index: usize) -> Frame {
        Frame {
            index,
            pts_ns: index as u64 * 33_333_333,
            rgb: Arc::from([1, 2, 3]),
        }
    }
    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "worker condition timeout");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn submit_when_unlocked(worker: &AsyncRenderer, epoch: u64, index: usize) -> SubmitStatus {
        let mut status = SubmitStatus::Busy;
        wait_until(|| {
            status = worker.submit(epoch, frame(index));
            status != SubmitStatus::Busy
        });
        status
    }

    #[test]
    fn blocked_renderer_does_not_block_submission_and_only_latest_epoch_can_publish() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut worker = AsyncRenderer::start_with_factory(0, move || {
            let mut fake = FakeRenderer::new();
            fake.barrier = Some((entered_tx, release_rx));
            Ok(fake)
        })
        .unwrap();
        assert_eq!(submit_when_unlocked(&worker, 1, 1), SubmitStatus::Queued);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(worker.submit(1, frame(2)), SubmitStatus::Queued);
        assert_eq!(worker.submit(1, frame(3)), SubmitStatus::Queued);
        assert_eq!(worker.submit(1, frame(4)), SubmitStatus::Evicted);
        let stats = worker.stats().unwrap();
        assert_eq!(stats.pending_depth, 2);
        assert_eq!(stats.high_water, 2);
        assert!(stats.in_flight);
        let epoch = worker.advance_epoch().unwrap();
        assert_eq!(epoch, 2);
        assert_eq!(worker.submit(1, frame(5)), SubmitStatus::Stale);
        release_tx.send(()).unwrap();
        wait_until(|| worker.stats().unwrap().stale_completions == 1);
        assert!(worker.poll_latest().is_none());
        assert_eq!(
            submit_when_unlocked(&worker, epoch, 6),
            SubmitStatus::Queued
        );
        let mut complete = None;
        wait_until(|| {
            complete = worker.poll_latest();
            complete.is_some()
        });
        let complete = complete.unwrap();
        assert_eq!(complete.epoch, epoch);
        assert_eq!(complete.index, 6);
        assert!(complete.verification.exact);
        let report = worker.shutdown().unwrap();
        assert!(report.joined);
        assert!(report.error.is_none());
        assert_eq!(report.stats.pending_depth, 0);
    }

    #[test]
    fn producer_try_lock_returns_busy_without_waiting() {
        let mut worker = AsyncRenderer::start_with_factory(0, || Ok(FakeRenderer::new())).unwrap();
        let held = worker.shared.state.lock().unwrap();
        assert_eq!(worker.submit(1, frame(1)), SubmitStatus::Busy);
        assert!(worker.poll_latest().is_none());
        drop(held);
        assert_eq!(worker.stats().unwrap().submit_busy, 1);
        worker.shutdown().unwrap();
        assert_eq!(worker.submit(1, frame(1)), SubmitStatus::Closed);
    }

    #[test]
    fn injected_delay_is_interruptible_and_shutdown_joins_its_worker() {
        let mut worker =
            AsyncRenderer::start_with_factory(2000, || Ok(FakeRenderer::new())).unwrap();
        submit_when_unlocked(&worker, 1, 1);
        wait_until(|| worker.stats().unwrap().in_flight);
        let report = worker.shutdown().unwrap();
        assert!(report.joined);
        assert!(report.elapsed_ms < 500.0);
        assert_eq!(report.stats.rendered, 0);
        assert!(!report.stats.in_flight);
    }

    #[test]
    fn renderer_failures_and_panics_close_acceptance_and_are_reported_after_join() {
        for panic in [false, true] {
            let mut worker = AsyncRenderer::start_with_factory(0, move || {
                let mut fake = FakeRenderer::new();
                fake.panic = panic;
                fake.fail = !panic;
                Ok(fake)
            })
            .unwrap();
            submit_when_unlocked(&worker, 1, 1);
            wait_until(|| worker.health().is_err());
            assert_eq!(worker.submit(1, frame(2)), SubmitStatus::Failed);
            let report = worker.shutdown().unwrap();
            assert!(report.joined);
            assert!(report.error.is_some());
            assert_eq!(report.stats.failures, 1);
        }
    }

    #[test]
    fn startup_failure_is_returned_without_detaching_a_thread() {
        assert!(
            AsyncRenderer::start_with_factory::<FakeRenderer, _>(0, || Err(
                "factory failure".into()
            ))
            .err()
            .unwrap()
            .contains("factory failure")
        );
    }

    #[test]
    fn completed_storage_retains_only_the_latest_verified_frame() {
        let mut worker = AsyncRenderer::start_with_factory(0, || Ok(FakeRenderer::new())).unwrap();
        for index in 1..=3 {
            assert_eq!(
                submit_when_unlocked(&worker, 1, index),
                SubmitStatus::Queued
            );
            wait_until(|| worker.stats().unwrap().completed == index as u64);
        }
        let stats = worker.stats().unwrap();
        assert!(stats.completion_pending);
        assert_eq!(stats.overwritten_results, 2);
        assert_eq!(stats.pending_depth, 0);
        assert_eq!(worker.poll_latest().unwrap().index, 3);
        assert!(worker.poll_latest().is_none());
        assert!(!worker.stats().unwrap().completion_pending);
        worker.shutdown().unwrap();
    }

    #[test]
    fn shutdown_waits_for_owned_inflight_work_and_rejects_its_late_result() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut worker = AsyncRenderer::start_with_factory(0, move || {
            let mut fake = FakeRenderer::new();
            fake.barrier = Some((entered_tx, release_rx));
            Ok(fake)
        })
        .unwrap();
        submit_when_unlocked(&worker, 1, 1);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let shared = Arc::clone(&worker.shared);
        let shutdown = thread::spawn(move || worker.shutdown().unwrap());
        wait_until(|| shared.stopping.load(Ordering::Acquire));
        assert!(
            !shutdown.is_finished(),
            "shutdown must retain ownership until the renderer returns"
        );
        release_tx.send(()).unwrap();
        let report = shutdown.join().unwrap();
        assert!(report.joined);
        assert_eq!(report.stats.closed_completions, 1);
        assert_eq!(report.stats.rendered, 1);
        assert!(!report.stats.in_flight);
        assert!(!report.stats.completion_pending);
    }

    #[test]
    fn oversized_or_partial_payloads_do_not_enter_the_bounded_queue() {
        let mut worker = AsyncRenderer::start_with_factory(0, || Ok(FakeRenderer::new())).unwrap();
        for rgb in [Arc::from([1, 2]), Arc::from([1, 2, 3, 4])] {
            assert_eq!(
                worker.submit(
                    1,
                    Frame {
                        index: 0,
                        pts_ns: 0,
                        rgb
                    }
                ),
                SubmitStatus::Failed
            );
        }
        assert_eq!(worker.stats().unwrap().submitted, 0);
        worker.shutdown().unwrap();
    }

    #[test]
    fn shutdown_measures_thread_owned_backend_drop_separately_from_render_work() {
        let owner = thread::current().id();
        let dropped_on = Arc::new(Mutex::new(None));
        let observation = Arc::clone(&dropped_on);
        let mut worker = AsyncRenderer::start_with_factory(0, move || {
            let mut fake = FakeRenderer::new();
            fake.drop_delay = Duration::from_millis(10);
            fake.dropped_on = Some(observation);
            Ok(fake)
        })
        .unwrap();
        let report = worker.shutdown().unwrap();
        assert_ne!(*dropped_on.lock().unwrap(), Some(owner));
        assert!(dropped_on.lock().unwrap().is_some());
        assert_eq!(report.close_requested_while_in_flight, Some(false));
        assert!(report.renderer_drop_ms.unwrap() >= 9.0);
        assert!(report.loop_exit_after_close_ms.is_some());
        assert!(report.worker_exit_after_close_ms.unwrap() >= report.renderer_drop_ms.unwrap());
        assert!(report.join_wait_ms <= report.elapsed_ms);
        assert!(!report.renderer_drop_panicked);
        assert_eq!(report.stats.rendered, 0);
        let repeated = worker.shutdown().unwrap();
        assert_eq!(repeated.renderer_drop_ms, report.renderer_drop_ms);
        assert_eq!(
            repeated.worker_exit_after_close_ms,
            report.worker_exit_after_close_ms
        );
    }

    #[test]
    fn destructor_panic_is_caught_on_worker_and_preserves_failure_evidence() {
        let mut worker = AsyncRenderer::start_with_factory(0, || {
            let mut fake = FakeRenderer::new();
            fake.drop_panic = true;
            Ok(fake)
        })
        .unwrap();
        let report = worker.shutdown().unwrap();
        assert!(report.joined);
        assert!(report.renderer_drop_panicked);
        assert!(report.renderer_drop_ms.is_some());
        assert!(report.worker_exit_after_close_ms.is_some());
        assert!(report.error.as_deref().unwrap().contains("destructor"));
        assert!(worker.health().is_err());
        assert!(report.stats.closed);
    }

    #[test]
    fn retiring_sessions_reuses_one_backend_and_keeps_its_instance_identity() {
        let created = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let factory_created = Arc::clone(&created);
        let factory_dropped = Arc::clone(&dropped);
        let mut worker = AsyncRenderer::start_with_factory(0, move || {
            factory_created.fetch_add(1, Ordering::Relaxed);
            let mut fake = FakeRenderer::new();
            fake.drop_count = Some(factory_dropped);
            Ok(fake)
        })
        .unwrap();
        let identity = worker.instance_id().to_owned();
        assert!(uuid::Uuid::parse_str(&identity).is_ok());
        for index in 1..=3 {
            let old_epoch = worker.stats().unwrap().epoch;
            submit_when_unlocked(&worker, old_epoch, index);
            wait_until(|| worker.stats().unwrap().completed == index as u64);
            let retired = worker.retire_epoch(Duration::from_millis(100)).unwrap();
            assert_eq!(retired.instance_id, identity);
            assert_eq!(retired.old_epoch, old_epoch);
            assert_eq!(retired.new_epoch, old_epoch + 1);
            assert_eq!(retired.stats.epoch, retired.new_epoch);
            assert!(!retired.stats.in_flight && !retired.stats.completion_pending);
            assert_eq!(retired.stats.pending_depth, 0);
            assert!(!retired.stats.closed);
            assert!(worker.poll_latest().is_none());
            assert_eq!(created.load(Ordering::Relaxed), 1);
            assert_eq!(dropped.load(Ordering::Relaxed), 0);
        }
        worker.shutdown().unwrap();
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(worker.retire_epoch(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn retire_waits_for_old_inflight_work_and_discards_its_completion() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker = AsyncRenderer::start_with_factory(0, move || {
            let mut fake = FakeRenderer::new();
            fake.barrier = Some((entered_tx, release_rx));
            Ok(fake)
        })
        .unwrap();
        submit_when_unlocked(&worker, 1, 1);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let shared = Arc::clone(&worker.shared);
        let retiring = thread::spawn(move || {
            let result = worker.retire_epoch(Duration::from_millis(1000));
            (worker, result)
        });
        wait_until(|| shared.state.lock().unwrap().queue.epoch() == 2);
        assert!(
            !retiring.is_finished(),
            "Retire must wait until old GPU ownership returns"
        );
        release_tx.send(()).unwrap();
        let (mut worker, result) = retiring.join().unwrap();
        let report = result.unwrap();
        assert_eq!(report.stats.stale_completions, 1);
        assert!(!report.stats.in_flight);
        assert_eq!(worker.submit(1, frame(2)), SubmitStatus::Stale);
        assert_eq!(submit_when_unlocked(&worker, 2, 3), SubmitStatus::Queued);
        worker.shutdown().unwrap();
    }

    #[test]
    fn retire_timeout_preserves_owned_worker_without_claiming_idle_or_closing_it() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut worker = AsyncRenderer::start_with_factory(0, move || {
            let mut fake = FakeRenderer::new();
            fake.barrier = Some((entered_tx, release_rx));
            Ok(fake)
        })
        .unwrap();
        assert!(worker.retire_epoch(Duration::ZERO).is_err());
        assert!(worker.retire_epoch(Duration::from_millis(3001)).is_err());
        assert_eq!(worker.stats().unwrap().epoch, 1);
        submit_when_unlocked(&worker, 1, 1);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let result = worker.retire_epoch(Duration::from_millis(10));
        let stats = worker.stats().unwrap();
        release_tx.send(()).unwrap();
        assert!(result.unwrap_err().contains("timed out"));
        assert!(stats.in_flight && !stats.closed);
        assert_eq!(stats.epoch, 2);
        let final_retire = worker.retire_epoch(Duration::from_millis(1000)).unwrap();
        assert_eq!(final_retire.new_epoch, 3);
        assert!(!final_retire.stats.in_flight);
        worker.shutdown().unwrap();
    }

    #[test]
    fn retire_does_not_clear_backend_failures_or_advance_failed_epochs() {
        let mut worker = AsyncRenderer::start_with_factory(0, || {
            let mut fake = FakeRenderer::new();
            fake.fail = true;
            Ok(fake)
        })
        .unwrap();
        submit_when_unlocked(&worker, 1, 1);
        wait_until(|| worker.health().is_err());
        let failed = worker.stats().unwrap();
        let error = worker.retire_epoch(Duration::from_millis(100)).unwrap_err();
        assert!(error.contains("injected renderer failure"));
        let after = worker.stats().unwrap();
        assert_eq!(after.epoch, failed.epoch);
        assert_eq!(after.error, failed.error);
        worker.shutdown().unwrap();
    }
}
