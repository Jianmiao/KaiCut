//! One queued native frame plus one main-thread callback. No pixel IPC.
use super::{
    media_actor::{MediaAck, MediaFrame, Sink, Submission},
    platform, source_inflight_hold,
};
use crate::preview_queue::{Completion, Job, PreviewQueue, Submission as QueueSubmission};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, TryLockError};
use tauri::{AppHandle, Manager};

pub struct DiagnosticFrame {
    pub source: Arc<[u8]>,
    pub rgba: Vec<u8>,
    pub evidence: Value,
}
struct State {
    queue: PreviewQueue<MediaFrame, MediaAck>,
    scheduled: bool,
    in_flight: bool,
    error: Option<String>,
    events: Vec<Value>,
    diagnostics: Vec<DiagnosticFrame>,
    callbacks: u64,
    present_calls: u64,
    blocked: u64,
    skipped_before_present: u64,
    skipped_closed_before_present: u64,
    skipped_stale_before_present: u64,
    skipped_error_before_present: u64,
    skipped_events: Vec<Value>,
}
struct Shared {
    live: bool,
    state: Mutex<State>,
    app: AppHandle,
    session: String,
    surface: String,
}
pub struct MainThreadSink {
    shared: Arc<Shared>,
}
fn completion_name<R>(completion: &Completion<R>) -> &'static str {
    match completion {
        Completion::Published { .. } => "Published",
        Completion::Stale { .. } => "Stale",
        Completion::Closed { .. } => "Closed",
    }
}
fn state_report(s: &State) -> Value {
    let c = s.queue.counters();
    json!({"epoch":s.queue.epoch(),"closed":s.queue.is_closed(),"pending":s.queue.pending_len(),"inFlight":s.in_flight,"scheduled":s.scheduled,"highWater":c.high_water,"submitted":c.submitted,"evicted":c.evicted,"taken":c.taken,"completed":c.completed,"staleCompletions":c.stale_completions,"closedCompletions":c.closed_completions,"invalidatedPending":c.invalidated_pending,"invalidatedResults":c.invalidated_results,"overwrittenResults":c.overwritten_results,"callbacks":s.callbacks,"presentCalls":s.present_calls,"blocked":s.blocked,"skippedBeforePresent":s.skipped_before_present,"skippedClosedBeforePresent":s.skipped_closed_before_present,"skippedStaleBeforePresent":s.skipped_stale_before_present,"skippedErrorBeforePresent":s.skipped_error_before_present,"skippedEvents":s.skipped_events,"error":s.error,"events":s.events})
}
fn shared_report(shared: &Shared) -> Result<Value, String> {
    let s = shared
        .state
        .lock()
        .map_err(|_| "Media sink state unavailable")?;
    Ok(state_report(&s))
}
enum PresentDispatch<T> {
    Called(T),
    Skipped {
        evidence: Value,
        schedule_next: bool,
    },
}
fn present_if_current<T>(
    state: &Mutex<State>,
    job: &Job<MediaFrame>,
    live: bool,
    render: impl FnOnce() -> T,
) -> Result<PresentDispatch<T>, String> {
    let mut s = state.lock().map_err(|_| "Media sink state unavailable")?;
    let reason = if s.error.is_some() {
        // A sink error is terminal even if its origin forgot to close the queue.
        s.queue.close();
        Some("sink-error")
    } else if s.queue.is_closed() {
        Some("closed")
    } else if s.queue.epoch() != job.epoch || job.epoch != job.item.epoch {
        Some("stale-epoch")
    } else {
        None
    };
    if let Some(reason) = reason {
        s.in_flight = false;
        s.skipped_before_present = s.skipped_before_present.saturating_add(1);
        match reason {
            "closed" => {
                s.skipped_closed_before_present = s.skipped_closed_before_present.saturating_add(1)
            }
            "stale-epoch" => {
                s.skipped_stale_before_present = s.skipped_stale_before_present.saturating_add(1)
            }
            _ => s.skipped_error_before_present = s.skipped_error_before_present.saturating_add(1),
        }
        let frame = &job.item;
        // This is a skipped taken job, not a Present result or an acknowledgement.
        let evidence = json!({
            "skipReason":reason,"jobEpoch":job.epoch,"queueEpoch":s.queue.epoch(),
            "renderEpoch":frame.epoch,"sourceEpoch":frame.source_epoch,"index":frame.index,
            "ptsNs":frame.pts_ns.to_string(),"projectTimeNs":frame.project_ns.to_string(),
            "diagnostic":frame.diagnostic,
        });
        if live && s.skipped_events.len() >= 128 {
            s.skipped_events.remove(0);
        }
        if s.skipped_events.len() < 1024 {
            s.skipped_events.push(evidence.clone());
        } else {
            s.error = Some("Media skipped-frame trace bound exceeded".into());
            s.queue.close();
        }
        let schedule_next = !s.queue.is_closed() && s.queue.pending_len() > 0;
        s.scheduled = schedule_next;
        return Ok(PresentDispatch::Skipped {
            evidence,
            schedule_next,
        });
    }
    // Last queue check before dispatch. Retirement racing after this unlock is
    // still classified by the existing post-call publish gate; a driver call
    // cannot be made atomic with retirement without holding this mutex over it.
    drop(s);
    Ok(PresentDispatch::Called(render()))
}
impl MainThreadSink {
    pub fn new(app: AppHandle, session: String, surface: String) -> Arc<Self> {
        Self::with_mode(app, session, surface, false)
    }
    pub fn new_live(app: AppHandle, session: String, surface: String) -> Arc<Self> {
        Self::with_mode(app, session, surface, true)
    }
    fn with_mode(app: AppHandle, session: String, surface: String, live: bool) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(Shared {
                live,
                state: Mutex::new(State {
                    queue: PreviewQueue::new(1).unwrap(),
                    scheduled: false,
                    in_flight: false,
                    error: None,
                    events: Vec::new(),
                    diagnostics: Vec::new(),
                    callbacks: 0,
                    present_calls: 0,
                    blocked: 0,
                    skipped_before_present: 0,
                    skipped_closed_before_present: 0,
                    skipped_stale_before_present: 0,
                    skipped_error_before_present: 0,
                    skipped_events: Vec::new(),
                }),
                app,
                session,
                surface,
            }),
        })
    }
    fn schedule(shared: Arc<Shared>) -> Result<(), String> {
        let next = Arc::clone(&shared);
        shared
            .app
            .run_on_main_thread(move || Self::consume(next))
            .map_err(|e| e.to_string())
    }
    fn consume(shared: Arc<Shared>) {
        let job = {
            let Ok(mut s) = shared.state.lock() else {
                return;
            };
            if s.queue.is_closed() {
                s.scheduled = false;
                return;
            }
            let Some(job) = s.queue.take() else {
                s.scheduled = false;
                return;
            };
            s.in_flight = true;
            s.callbacks += 1;
            job
        };
        let frame = &job.item;
        let held_before_present =
            source_inflight_hold::before_present(&shared.session, &shared.surface, frame, || {
                shared_report(&shared)
            });
        let dispatch = present_if_current(&shared.state, &job, shared.live, || {
            shared
                .app
                .get_webview_window("main")
                .ok_or("Media window is unavailable".to_owned())
                .and_then(|w| platform::present_media(&w, &shared.session, &shared.surface, frame))
        });
        let result = match dispatch {
            Ok(PresentDispatch::Called(result)) => result,
            Ok(PresentDispatch::Skipped {
                evidence,
                schedule_next,
            }) => {
                if held_before_present {
                    source_inflight_hold::after_skip(
                        &shared.session,
                        &shared.surface,
                        frame,
                        &evidence,
                        shared_report(&shared),
                    );
                }
                Self::schedule_next(&shared, schedule_next);
                return;
            }
            Err(_) => return, // The poisoned state cannot be safely inspected or rendered.
        };
        // The real platform call has returned. The validation hold owns no sink
        // lock, so worker-side close/retire can run before this completion.
        let held_for_validation = source_inflight_hold::after_present(
            &shared.session,
            &shared.surface,
            frame,
            &result,
            || shared_report(&shared),
        );
        let mut schedule_next = false;
        let mut completion_evidence = None;
        if let Ok(mut s) = shared.state.lock() {
            s.in_flight = false;
            match result {
                Ok((mut evidence, rgba)) => {
                    evidence["renderEpoch"] = frame.epoch.into();
                    evidence["sourceEpoch"] = frame.source_epoch.into();
                    evidence["index"] = frame.index.into();
                    evidence["ptsNs"] = frame.pts_ns.to_string().into();
                    evidence["projectTimeNs"] = frame.project_ns.to_string().into();
                    evidence["staleAfterCall"] =
                        (s.queue.epoch() != frame.epoch || s.queue.is_closed()).into();
                    if evidence["presentCalled"] == true {
                        s.present_calls += 1;
                    }
                    if evidence["blocked"] == true {
                        s.blocked += 1;
                    }
                    if let Some(rgba) = rgba {
                        if s.diagnostics.len() >= 16 {
                            s.error = Some("Media diagnostic frame bound exceeded".into());
                            s.queue.close();
                        } else {
                            s.diagnostics.push(DiagnosticFrame {
                                source: Arc::clone(&frame.rgb),
                                rgba,
                                evidence: evidence.clone(),
                            });
                        }
                    }
                    if shared.live && s.events.len() >= 128 {
                        s.events.remove(0);
                    }
                    let record_event = s.events.len() < 1024;
                    if !record_event {
                        s.error = Some("Media presentation trace bound exceeded".into());
                        s.queue.close();
                    } else {
                        s.events.push(evidence.clone());
                    }
                    let accepted = evidence["accepted"] == true;
                    let completion = s.queue.publish(
                        job.epoch,
                        MediaAck {
                            epoch: frame.epoch,
                            source_epoch: frame.source_epoch,
                            index: frame.index,
                            accepted,
                            evidence,
                        },
                    );
                    let kind = completion_name(&completion);
                    if record_event {
                        if let Some(event) = s.events.last_mut() {
                            event["queueCompletion"] = kind.into();
                        }
                    }
                    completion_evidence = Some(kind);
                }
                Err(error) => {
                    s.error = Some(error);
                    s.queue.close();
                }
            }
            if !s.queue.is_closed() && s.queue.pending_len() > 0 {
                schedule_next = true
            } else {
                s.scheduled = false;
            }
        }
        if let Some(kind) = completion_evidence.filter(|_| held_for_validation) {
            source_inflight_hold::after_publish(
                &shared.session,
                &shared.surface,
                frame,
                kind,
                shared_report(&shared),
            );
        }
        Self::schedule_next(&shared, schedule_next);
    }
    fn schedule_next(shared: &Arc<Shared>, schedule_next: bool) {
        if schedule_next {
            if let Err(error) = Self::schedule(Arc::clone(shared)) {
                if let Ok(mut s) = shared.state.lock() {
                    s.error = Some(error);
                    s.queue.close();
                    s.scheduled = false;
                }
            }
        }
    }
    pub fn close(&self) {
        if let Ok(mut s) = self.shared.state.lock() {
            s.queue.close();
        }
        source_inflight_hold::observe_sink(
            &self.shared.session,
            &self.shared.surface,
            "close",
            || shared_report(&self.shared),
        );
    }
    pub fn report(&self) -> Result<Value, String> {
        shared_report(&self.shared)
    }
    pub fn take_diagnostics(&self) -> Result<Vec<DiagnosticFrame>, String> {
        Ok(std::mem::take(
            &mut self
                .shared
                .state
                .lock()
                .map_err(|_| "Media sink unavailable")?
                .diagnostics,
        ))
    }
}
impl Sink for MainThreadSink {
    fn submit(&self, frame: MediaFrame) -> Result<Submission, String> {
        if frame.width == 0
            || frame.height == 0
            || frame.width > 1920
            || frame.height > 1080
            || frame.rgb.len() != frame.width as usize * frame.height as usize * 3
        {
            return Err("Invalid bounded media frame".into());
        }
        let mut s = match self.shared.state.try_lock() {
            Ok(s) => s,
            Err(TryLockError::WouldBlock) => return Ok(Submission::Busy),
            Err(_) => return Err("Media sink mutex poisoned".into()),
        };
        if let Some(error) = &s.error {
            return Err(error.clone());
        }
        if s.queue.is_closed() {
            return Err("Media surface has been closed".into());
        }
        let result = match s.queue.submit(frame.epoch, frame) {
            QueueSubmission::Accepted { evicted: None } => Submission::Queued,
            QueueSubmission::Accepted { evicted: Some(_) } => Submission::Evicted,
            _ => Submission::Stale,
        };
        let schedule = !s.scheduled && matches!(result, Submission::Queued | Submission::Evicted);
        if schedule {
            s.scheduled = true;
        }
        drop(s);
        if schedule {
            Self::schedule(Arc::clone(&self.shared))?;
        }
        Ok(result)
    }
    fn poll(&self) -> Result<Option<MediaAck>, String> {
        let mut s = match self.shared.state.try_lock() {
            Ok(s) => s,
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(_) => return Err("Media sink mutex poisoned".into()),
        };
        if let Some(error) = &s.error {
            return Err(error.clone());
        }
        if s.queue.is_closed() {
            return Err("Media surface has been closed".into());
        }
        Ok(s.queue.take_completed().map(|c| c.item))
    }
    fn retire(&self, new_epoch: u64) -> Result<(), String> {
        let mut s = self
            .shared
            .state
            .lock()
            .map_err(|_| "Media sink unavailable")?;
        if s.queue.is_closed() {
            return Ok(());
        }
        if new_epoch == s.queue.epoch() {
            return Ok(());
        }
        if new_epoch
            != s.queue
                .epoch()
                .checked_add(1)
                .ok_or("Media generation overflow")?
        {
            return Err("Media epoch must advance exactly once".into());
        }
        s.queue.advance_epoch().map_err(|e| e.to_string())?;
        drop(s);
        source_inflight_hold::observe_sink(
            &self.shared.session,
            &self.shared.surface,
            "retire",
            || shared_report(&self.shared),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview_queue::{Completion, PreviewQueue, Submission};
    use std::cell::Cell;

    fn taken_frame() -> (Mutex<State>, Job<MediaFrame>) {
        let mut state = State {
            queue: PreviewQueue::new(1).unwrap(),
            scheduled: true,
            in_flight: true,
            error: None,
            events: Vec::new(),
            diagnostics: Vec::new(),
            callbacks: 1,
            present_calls: 0,
            blocked: 0,
            skipped_before_present: 0,
            skipped_closed_before_present: 0,
            skipped_stale_before_present: 0,
            skipped_error_before_present: 0,
            skipped_events: Vec::new(),
        };
        state.queue.submit(
            1,
            MediaFrame {
                epoch: 1,
                source_epoch: 7,
                index: 3,
                pts_ns: 100,
                project_ns: 200,
                width: 1,
                height: 1,
                rgb: Arc::from([0, 0, 0]),
                diagnostic: false,
            },
        );
        let job = state.queue.take().unwrap();
        (Mutex::new(state), job)
    }

    #[test]
    fn before_present_stale_frame_never_calls_renderer_or_publishes_ack() {
        let (state, job) = taken_frame();
        state.lock().unwrap().queue.advance_epoch().unwrap();
        let calls = Cell::new(0);
        let result = present_if_current(&state, &job, true, || calls.set(calls.get() + 1)).unwrap();
        assert_eq!(calls.get(), 0);
        let PresentDispatch::Skipped {
            evidence,
            schedule_next,
        } = result
        else {
            panic!("Stale frame was rendered")
        };
        assert!(!schedule_next);
        assert_eq!(evidence["skipReason"], "stale-epoch");
        assert_eq!(evidence["renderEpoch"], 1);
        assert_eq!(evidence["sourceEpoch"], 7);
        assert_eq!(evidence["index"], 3);
        assert_eq!(evidence["ptsNs"], "100");
        assert_eq!(evidence["projectTimeNs"], "200");
        assert!(evidence.get("presentResult").is_none());
        assert!(evidence.get("accepted").is_none());
        assert!(evidence.get("presentCalled").is_none());
        assert!(evidence.get("blocked").is_none());
        let state = state.lock().unwrap();
        assert!(!state.in_flight && !state.scheduled);
        assert!(state.events.is_empty());
        assert_eq!(state.present_calls, 0);
        assert_eq!(state.blocked, 0);
        assert_eq!(state.queue.counters().taken, state.skipped_before_present);
        assert!(!state.queue.has_completed());
        assert_eq!(state.queue.counters().completed, 0);
        assert_eq!(state.queue.counters().stale_completions, 0);
        assert_eq!(state_report(&state)["skippedBeforePresent"], 1);
        assert_eq!(state_report(&state)["skippedStaleBeforePresent"], 1);
    }

    #[test]
    fn before_present_closed_frame_never_calls_renderer() {
        let (state, job) = taken_frame();
        state.lock().unwrap().queue.close();
        let calls = Cell::new(0);
        let result = present_if_current(&state, &job, true, || calls.set(calls.get() + 1)).unwrap();
        assert_eq!(calls.get(), 0);
        let PresentDispatch::Skipped {
            evidence,
            schedule_next,
        } = result
        else {
            panic!("Closed frame was rendered")
        };
        assert_eq!(evidence["skipReason"], "closed");
        assert!(!schedule_next);
        let state = state.lock().unwrap();
        assert!(!state.in_flight && !state.scheduled);
        assert!(!state.queue.has_completed());
        assert_eq!(state.queue.counters().closed_completions, 0);
        assert_eq!(state_report(&state)["skippedClosedBeforePresent"], 1);
    }

    #[test]
    fn before_present_error_frame_never_calls_renderer_or_keeps_pending_work() {
        let (state, job) = taken_frame();
        {
            let mut state = state.lock().unwrap();
            state.error = Some("Prior sink failure".into());
            state.queue.submit(1, job.item.clone());
        }
        let calls = Cell::new(0);
        let result = present_if_current(&state, &job, true, || calls.set(calls.get() + 1)).unwrap();
        assert_eq!(calls.get(), 0);
        let PresentDispatch::Skipped {
            evidence,
            schedule_next,
        } = result
        else {
            panic!("Failed sink was rendered")
        };
        assert_eq!(evidence["skipReason"], "sink-error");
        assert!(!schedule_next);
        let state = state.lock().unwrap();
        assert!(state.queue.is_closed());
        assert!(!state.in_flight && !state.scheduled);
        assert_eq!(state.queue.pending_len(), 0);
        assert_eq!(state.error.as_deref(), Some("Prior sink failure"));
        assert_eq!(state_report(&state)["skippedErrorBeforePresent"], 1);
    }

    #[test]
    fn before_present_stale_skip_reschedules_new_epoch_pending_frame() {
        let (state, job) = taken_frame();
        {
            let mut state = state.lock().unwrap();
            state.queue.advance_epoch().unwrap();
            let mut fresh = job.item.clone();
            fresh.epoch = 2;
            state.queue.submit(2, fresh);
        }
        let result = present_if_current(&state, &job, true, || panic!("Stale render")).unwrap();
        assert!(matches!(
            result,
            PresentDispatch::Skipped {
                schedule_next: true,
                ..
            }
        ));
        let state = state.lock().unwrap();
        assert!(state.scheduled && !state.in_flight);
        assert_eq!(state.queue.pending_len(), 1);
    }

    #[test]
    fn before_present_current_frame_calls_renderer_once_without_sink_lock() {
        let (state, job) = taken_frame();
        let calls = Cell::new(0);
        let result = present_if_current(&state, &job, true, || {
            assert!(state.try_lock().is_ok(), "Renderer must not hold sink lock");
            calls.set(calls.get() + 1);
            17
        })
        .unwrap();
        assert!(matches!(result, PresentDispatch::Called(17)));
        assert_eq!(calls.get(), 1);
        let state = state.lock().unwrap();
        assert!(state.in_flight);
        assert_eq!(state.skipped_before_present, 0);
        assert!(state.skipped_events.is_empty());
    }

    #[test]
    fn before_present_rejects_inconsistent_job_and_frame_epoch() {
        let (state, mut job) = taken_frame();
        // Even a frame matching the new epoch must not launder an old taken job.
        state.lock().unwrap().queue.advance_epoch().unwrap();
        job.item.epoch = 2;
        let calls = Cell::new(0);
        let result = present_if_current(&state, &job, true, || calls.set(calls.get() + 1)).unwrap();
        assert_eq!(calls.get(), 0);
        assert!(matches!(result, PresentDispatch::Skipped { .. }));
    }
    #[test]
    fn completion_evidence_reports_real_queue_rejection_after_seek_and_close() {
        let mut queue = PreviewQueue::<u32, u32>::new(1).unwrap();
        queue.submit(1, 10);
        let running = queue.take().unwrap();
        queue.advance_epoch().unwrap();
        let stale = queue.publish(running.epoch, running.item);
        assert_eq!(completion_name(&stale), "Stale");
        assert_eq!(queue.counters().stale_completions, 1);
        assert!(!queue.has_completed());
        assert_eq!(completion_name(&queue.publish(2, 20)), "Published");
        queue.close();
        let closed = queue.publish(2, 30);
        assert_eq!(completion_name(&closed), "Closed");
        assert_eq!(queue.counters().closed_completions, 1);
        assert!(!queue.has_completed());
    }
    #[test]
    fn surface_queue_is_bounded_and_old_results_cannot_cross_seek() {
        let mut queue = PreviewQueue::<u32, u32>::new(1).unwrap();
        queue.submit(1, 10);
        let running = queue.take().unwrap();
        queue.submit(1, 11);
        assert!(matches!(
            queue.submit(1, 12),
            Submission::Accepted { evicted: Some(_) }
        ));
        assert_eq!(queue.pending_len(), 1);
        queue.advance_epoch().unwrap();
        assert!(matches!(
            queue.publish(running.epoch, 10),
            Completion::Stale { .. }
        ));
        assert_eq!(queue.pending_len(), 0);
        assert!(!queue.has_completed());
        queue.submit(2, 20);
        assert_eq!(queue.take().unwrap().item, 20);
        queue.close();
        assert!(matches!(queue.publish(2, 20), Completion::Closed { .. }));
    }
}
