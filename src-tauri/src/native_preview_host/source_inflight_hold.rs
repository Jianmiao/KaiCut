//! Debug-only controlled scheduling immediately before or after real Present.
use super::media_actor::MediaFrame;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_WAIT_MS: u64 = 10_000;
static ARMED: AtomicBool = AtomicBool::new(false);
static HOLD: Mutex<Option<InflightHold>> = Mutex::new(None);

struct InflightHold {
    session: String,
    surface: String,
    boundary: &'static str,
    claimed: bool,
    abandoned: bool,
    evidence: Value,
}
impl InflightHold {
    fn new(scenario: &str, session: &str, surface: &str) -> Self {
        let boundary = if matches!(scenario, "prepresent-cancel" | "prepresent-seek") {
            "before-present"
        } else {
            "after-present"
        };
        Self {
            session: session.into(),
            surface: surface.into(),
            boundary,
            claimed: false,
            abandoned: false,
            evidence: json!({"scenario":scenario,"boundary":boundary,"sessionId":session,"surfaceId":surface,"controlledScheduling":true,"maxWaitMs":MAX_WAIT_MS,"claimed":false,"holding":false,"releaseRequested":false,"released":false,"timedOut":false,"failed":false,"observations":[]}),
        }
    }
    fn matches(&self, session: &str, surface: &str) -> bool {
        self.session == session && self.surface == surface
    }
    fn can_claim(&self, session: &str, surface: &str, boundary: &str) -> bool {
        !self.claimed
            && !self.abandoned
            && self.boundary == boundary
            && self.matches(session, surface)
    }
    fn claim(&mut self, session: &str, surface: &str, diagnostic: bool, boundary: &str) -> bool {
        if diagnostic || !self.can_claim(session, surface, boundary) {
            return false;
        }
        self.claimed = true;
        self.evidence["claimed"] = true.into();
        self.evidence["holding"] = true.into();
        true
    }
    fn abandon(&mut self) {
        // RAII drops also run on the successful path. Preserve completed evidence.
        if self.evidence["released"] == true && self.evidence["holding"] == false {
            return;
        }
        self.abandoned = true;
        self.evidence["abandoned"] = true.into();
        self.evidence["failed"] = true.into();
        if self.evidence["error"].is_null() {
            self.evidence["error"] = "Controlled Present validation was abandoned".into();
        }
        if self.evidence["holding"] == true {
            self.evidence["releaseRequested"] = true.into();
        }
    }
    fn request_release(&mut self) -> Result<(), String> {
        if !self.claimed
            || self.evidence["holding"] != true
            || self.evidence["releaseRequested"] == true
        {
            return Err("No held source callback is awaiting release".into());
        }
        self.evidence["releaseRequested"] = true.into();
        Ok(())
    }
    fn finish_wait(&mut self, elapsed_ms: u64) -> bool {
        let timed_out = elapsed_ms >= MAX_WAIT_MS;
        let released = self.evidence["releaseRequested"] == true;
        if !released && !timed_out {
            return false;
        }
        self.evidence["elapsedMs"] = elapsed_ms.into();
        self.evidence["holding"] = false.into();
        self.evidence["released"] = released.into();
        self.evidence["timedOut"] = timed_out.into();
        if timed_out {
            self.evidence["failed"] = true.into();
            self.evidence["error"] =
                "In-flight validation exceeded its bounded scheduling window".into();
        }
        true
    }
}

pub(super) fn arm(scenario: &str, session: &str, surface: &str) -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().collect();
    if !cfg!(debug_assertions)
        || !matches!(
            scenario,
            "inflight-cancel" | "inflight-seek" | "prepresent-cancel" | "prepresent-seek"
        )
        || session.is_empty()
        || surface.is_empty()
        || ![
            "--mcp",
            "--mcp-hidden",
            "--mcp-validation",
            "--native-preview-host",
            "--native-source-preview",
            "--native-source-check-dir",
        ]
        .iter()
        .all(|f| args.iter().any(|a| a == f))
    {
        return Err("In-flight scheduling requires isolated hidden debug validation".into());
    }
    let mut hold = HOLD.lock().map_err(|_| "In-flight evidence unavailable")?;
    if hold.is_some() {
        return Err("In-flight schedule already armed".into());
    }
    *hold = Some(InflightHold::new(scenario, session, surface));
    ARMED.store(true, Ordering::Release);
    Ok(())
}

pub(super) fn snapshot() -> Value {
    HOLD.lock()
        .ok()
        .and_then(|h| h.as_ref().map(|h| h.evidence.clone()))
        .unwrap_or(Value::Null)
}

pub(super) fn release() -> Result<(), String> {
    HOLD.lock()
        .map_err(|_| "In-flight evidence unavailable")?
        .as_mut()
        .ok_or("No in-flight schedule is armed")?
        .request_release()
}

pub(super) fn abandon() {
    if let Ok(mut hold) = HOLD.lock() {
        if let Some(h) = hold.as_mut() {
            h.abandon();
        }
    }
}

pub(super) fn before_present(
    session: &str,
    surface: &str,
    frame: &MediaFrame,
    sink: impl Fn() -> Result<Value, String>,
) -> bool {
    hold_callback(
        session,
        surface,
        frame,
        "before-present",
        || Value::Null,
        sink,
    )
}

pub(super) fn after_skip(
    session: &str,
    surface: &str,
    frame: &MediaFrame,
    skip: &Value,
    sink: Result<Value, String>,
) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut hold) = HOLD.lock() {
        if let Some(h) = hold.as_mut() {
            if h.claimed && h.boundary == "before-present" && h.matches(session, surface) {
                let identity = frame_evidence(frame, "before-present");
                if h.evidence["frame"] == identity {
                    h.evidence["completion"] = json!({"kind":"SkippedBeforePresent","frame":identity,"skip":skip,"sinkAfterSkip":sink_value(sink)});
                }
            }
        }
    }
}

fn frame_identity(frame: &MediaFrame) -> Value {
    json!({"renderEpoch":frame.epoch,"sourceEpoch":frame.source_epoch,"index":frame.index,"ptsNs":frame.pts_ns.to_string(),"projectTimeNs":frame.project_ns.to_string(),"diagnostic":frame.diagnostic})
}

fn frame_evidence(frame: &MediaFrame, boundary: &str) -> Value {
    let mut identity = frame_identity(frame);
    if boundary == "before-present" {
        identity["width"] = frame.width.into();
        identity["height"] = frame.height.into();
        identity["rgbBytes"] = frame.rgb.len().into();
        identity["inputRgbSha256"] = format!("{:x}", Sha256::digest(frame.rgb.as_ref())).into();
    }
    identity
}

fn sink_value(sink: Result<Value, String>) -> Value {
    sink.unwrap_or_else(|error| json!({"observationError":error}))
}

pub(super) fn after_present(
    session: &str,
    surface: &str,
    frame: &MediaFrame,
    result: &Result<(Value, Option<Vec<u8>>), String>,
    sink: impl Fn() -> Result<Value, String>,
) -> bool {
    hold_callback(
        session,
        surface,
        frame,
        "after-present",
        || match result {
            Ok((evidence, rgba)) => {
                json!({"Ok":evidence,"diagnosticReadbackBytes":rgba.as_ref().map(Vec::len)})
            }
            Err(error) => json!({"Err":error}),
        },
        sink,
    )
}

fn hold_callback(
    session: &str,
    surface: &str,
    frame: &MediaFrame,
    boundary: &str,
    raw_present: impl FnOnce() -> Value,
    sink: impl Fn() -> Result<Value, String>,
) -> bool {
    if !ARMED.load(Ordering::Acquire) || frame.diagnostic {
        return false;
    }
    let candidate = HOLD
        .lock()
        .ok()
        .and_then(|h| h.as_ref().map(|h| h.can_claim(session, surface, boundary)))
        .unwrap_or(false);
    if !candidate {
        return false;
    }
    // Never acquire the sink mutex while holding HOLD, or vice versa.
    let at_hold = sink_value(sink());
    let claimed = HOLD
        .lock()
        .ok()
        .and_then(|mut h| {
            h.as_mut().map(|h| {
                if !h.claim(session, surface, frame.diagnostic, boundary) {
                    return false;
                }
                h.evidence["frame"] = frame_evidence(frame, boundary);
                h.evidence["rawPresent"] = raw_present();
                h.evidence["sinkAtHold"] = at_hold.clone();
                h.evidence["sinkNow"] = at_hold;
                true
            })
        })
        .unwrap_or(false);
    if !claimed {
        return false;
    }
    let began = Instant::now();
    loop {
        let current = sink_value(sink());
        let finished = HOLD
            .lock()
            .ok()
            .and_then(|mut h| {
                h.as_mut().map(|h| {
                    h.evidence["sinkNow"] = current;
                    h.finish_wait(began.elapsed().as_millis().min(u64::MAX as u128) as u64)
                })
            })
            .unwrap_or(true);
        if finished {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    true
}

pub(super) fn observe_sink(
    session: &str,
    surface: &str,
    kind: &str,
    sink: impl FnOnce() -> Result<Value, String>,
) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let candidate = HOLD
        .lock()
        .ok()
        .and_then(|h| {
            h.as_ref()
                .map(|h| h.matches(session, surface) && h.evidence["holding"] == true)
        })
        .unwrap_or(false);
    if !candidate {
        return;
    }
    let sink = sink_value(sink());
    if let Ok(mut hold) = HOLD.lock() {
        if let Some(h) = hold.as_mut() {
            if h.matches(session, surface) && h.evidence["holding"] == true {
                h.evidence["sinkNow"] = sink.clone();
                if let Some(events) = h.evidence["observations"].as_array_mut() {
                    if events.len() < 16 {
                        events.push(json!({"kind":kind,"sink":sink}));
                    } else {
                        h.evidence["failed"] = true.into();
                        h.evidence["error"] = "In-flight observation bound exceeded".into();
                    }
                }
            }
        }
    }
}

pub(super) fn after_publish(
    session: &str,
    surface: &str,
    frame: &MediaFrame,
    kind: &str,
    sink: Result<Value, String>,
) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut hold) = HOLD.lock() {
        if let Some(h) = hold.as_mut() {
            let identity = frame_identity(frame);
            if h.claimed
                && h.boundary == "after-present"
                && h.matches(session, surface)
                && h.evidence["frame"] == identity
            {
                h.evidence["completion"] =
                    json!({"kind":kind,"frame":identity,"sinkAfterPublish":sink_value(sink)});
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_source_session_can_claim_once_and_never_diagnostic_frames() {
        let mut h = InflightHold::new("inflight-cancel", "source-a", "surface-a");
        assert!(!h.claim("source-b", "surface-a", false, "after-present"));
        assert!(!h.claim("source-a", "surface-b", false, "after-present"));
        assert!(!h.claim("source-a", "surface-a", true, "after-present"));
        assert!(h.claim("source-a", "surface-a", false, "after-present"));
        assert_eq!(h.evidence["holding"], true);
        assert!(!h.claim("source-a", "surface-a", false, "after-present"));
    }
    #[test]
    fn only_explicit_release_finishes_a_claimed_callback_before_deadline() {
        let mut h = InflightHold::new("inflight-seek", "source-a", "surface-a");
        assert!(h.request_release().is_err());
        assert!(h.claim("source-a", "surface-a", false, "after-present"));
        assert!(!h.finish_wait(9_999));
        h.request_release().unwrap();
        assert!(h.finish_wait(9_999));
        assert_eq!(h.evidence["holding"], false);
        assert_eq!(h.evidence["released"], true);
        assert_eq!(h.evidence["timedOut"], false);
        assert_eq!(h.evidence["failed"], false);
    }
    #[test]
    fn deadline_fails_evidence_instead_of_leaving_main_thread_held() {
        let mut h = InflightHold::new("inflight-cancel", "source-a", "surface-a");
        assert!(h.claim("source-a", "surface-a", false, "after-present"));
        assert!(h.finish_wait(10_000));
        assert_eq!(h.evidence["holding"], false);
        assert_eq!(h.evidence["released"], false);
        assert_eq!(h.evidence["timedOut"], true);
        assert_eq!(h.evidence["failed"], true);
        assert!(h.request_release().is_err());
    }
    #[test]
    fn callbacks_cannot_claim_the_wrong_present_boundary() {
        for (scenario, correct, wrong) in [
            ("inflight-cancel", "after-present", "before-present"),
            ("inflight-seek", "after-present", "before-present"),
            ("prepresent-cancel", "before-present", "after-present"),
            ("prepresent-seek", "before-present", "after-present"),
        ] {
            let mut h = InflightHold::new(scenario, "source-a", "surface-a");
            assert!(!h.claim("source-a", "surface-a", false, wrong));
            assert!(h.claim("source-a", "surface-a", false, correct));
            assert_eq!(h.evidence["boundary"], correct);
        }
    }
    #[test]
    fn abandoning_before_claim_prevents_a_late_frame_from_waiting() {
        let mut h = InflightHold::new("prepresent-cancel", "source-a", "surface-a");
        h.abandon();
        assert!(!h.claim("source-a", "surface-a", false, "before-present"));
        assert_eq!(h.evidence["claimed"], false);
        assert_eq!(h.evidence["holding"], false);
        assert_eq!(h.evidence["failed"], true);
    }
    #[test]
    fn abandoning_a_held_callback_releases_without_waiting_for_deadline() {
        let mut h = InflightHold::new("prepresent-seek", "source-a", "surface-a");
        assert!(h.claim("source-a", "surface-a", false, "before-present"));
        h.abandon();
        assert!(h.finish_wait(1));
        assert_eq!(h.evidence["releaseRequested"], true);
        assert_eq!(h.evidence["released"], true);
        assert_eq!(h.evidence["failed"], true);
        assert_eq!(h.evidence["timedOut"], false);
    }
    #[test]
    fn abandoning_after_normal_release_does_not_rewrite_successful_evidence() {
        let mut h = InflightHold::new("prepresent-seek", "source-a", "surface-a");
        assert!(h.claim("source-a", "surface-a", false, "before-present"));
        h.request_release().unwrap();
        assert!(h.finish_wait(1));
        let before = h.evidence.clone();
        h.abandon();
        assert_eq!(h.evidence, before);
    }
}
