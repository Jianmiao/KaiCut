//! One-shot failures for the explicitly gated hidden validation instance.
//! No user-controlled PID, device setting or media path is accepted here.
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Device,
    Decoder,
    Cleanup,
    Unbind,
}
struct Fault {
    kind: Kind,
    consumed: bool,
    evidence: Value,
}
impl Fault {
    fn take(&mut self, kind: Kind) -> bool {
        if self.kind != kind || self.consumed {
            return false;
        }
        self.consumed = true;
        self.evidence["injected"] = true.into();
        true
    }
    fn record_unbind(&mut self, snapshot: &Result<Value, String>) {
        if self.kind == Kind::Unbind && self.consumed && self.evidence["unbind"].is_null() {
            self.evidence["unbind"] = json!({"injected":true,"error":UNBIND_ERROR,
                "injectionBoundary":"before-suspend","actualSuspendAttempted":false,
                "bindingBeforeFailure":snapshot.as_ref().ok(),"snapshotError":snapshot.as_ref().err()});
        }
    }
}
static FAULT: Mutex<Option<Fault>> = Mutex::new(None);
pub(super) const UNBIND_ERROR: &str = "Injected native surface unbind failure before suspend";
pub(super) fn unbind_failure() -> bool {
    take(Kind::Unbind)
}
pub(super) fn unbind_blocked(snapshot: &Result<Value, String>) {
    if let Ok(mut state) = FAULT.lock() {
        if let Some(fault) = state.as_mut() {
            fault.record_unbind(snapshot);
        }
    }
}
pub(super) fn unbind_retained(snapshot: &Result<Value, String>) {
    if let Ok(mut state) = FAULT.lock() {
        if let Some(fault) = state.as_mut().filter(|f| {
            f.kind == Kind::Unbind
                && f.consumed
                && f.evidence["unbind"].is_object()
                && f.evidence["unbind"]["bindingAfterFailure"].is_null()
                && f.evidence["unbind"]["afterSnapshotError"].is_null()
        }) {
            fault.evidence["unbind"]["bindingAfterFailure"] = json!(snapshot.as_ref().ok());
            fault.evidence["unbind"]["afterSnapshotError"] = json!(snapshot.as_ref().err());
        }
    }
}

pub(super) fn arm(scenario: &str) -> Result<(), String> {
    let kind = match scenario {
        "device-init-failure" => Kind::Device,
        "decoder-failure" => Kind::Decoder,
        "cleanup-failure" => Kind::Cleanup,
        "unbind-failure" => Kind::Unbind,
        "seek-switch" => return Ok(()),
        _ => return Err("Unknown source failure scenario".into()),
    };
    let args: Vec<_> = std::env::args_os().collect();
    if !cfg!(debug_assertions)
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
        return Err("Fault injection requires an isolated hidden debug instance".into());
    }
    let mut slot = FAULT.lock().map_err(|_| "Fault observation unavailable")?;
    if slot.is_some() {
        return Err("Fault already armed".into());
    }
    *slot = Some(Fault {
        kind,
        consumed: false,
        evidence: json!({"scenario":scenario,"oneShot":true,"injected":false}),
    });
    Ok(())
}
fn take(kind: Kind) -> bool {
    FAULT
        .lock()
        .ok()
        .and_then(|mut v| v.as_mut().map(|f| f.take(kind)))
        .unwrap_or(false)
}
pub(super) fn device_failure() -> bool {
    take(Kind::Device)
}
pub(super) fn decoder_failure(position_ns: u64) -> bool {
    position_ns >= 250_000_000 && take(Kind::Decoder)
}
pub(super) fn device_cleanup(error: &str, cleanup: &Result<Value, String>) {
    if let Ok(mut state) = FAULT.lock() {
        if let Some(f) = state
            .as_mut()
            .filter(|f| f.kind == Kind::Device && f.consumed)
        {
            f.evidence["device"] = json!({"injected":true,"error":error,"transferClose":cleanup.as_ref().ok(),"cleanupError":cleanup.as_ref().err(),"workerJoined":false,"actualDeviceOpenAttempted":false});
        }
    }
}
pub(super) fn device_worker_joined(joined: bool) {
    if let Ok(mut state) = FAULT.lock() {
        if let Some(f) = state
            .as_mut()
            .filter(|f| f.kind == Kind::Device && f.consumed)
        {
            f.evidence["device"]["workerJoined"] = joined.into();
        }
    }
}
pub(super) fn decoder_terminated(position_ns: u64, result: &Result<Value, String>) {
    if let Ok(mut state) = FAULT.lock() {
        if let Some(f) = state
            .as_mut()
            .filter(|f| f.kind == Kind::Decoder && f.consumed)
        {
            f.evidence["decoder"] = json!({"injected":true,"injectionPhase":"paused","actorPositionNs":position_ns.to_string(),"termination":result.as_ref().ok(),"error":result.as_ref().err()});
        }
    }
}
pub(super) fn snapshot() -> Value {
    FAULT
        .lock()
        .ok()
        .and_then(|f| f.as_ref().map(|f| f.evidence.clone()))
        .unwrap_or(Value::Null)
}

/// Inject only the cleanup confirmation failure. Actual unbinding still runs
/// first, so validation never deliberately leaves a live native surface behind.
pub(super) fn cleanup_confirmation(result: Result<Value, String>) -> Result<Value, String> {
    if !take(Kind::Cleanup) {
        return result;
    }
    let error = "Injected native surface cleanup confirmation failure";
    if let Ok(mut state) = FAULT.lock() {
        if let Some(f) = state.as_mut() {
            f.evidence["cleanup"] = json!({"injected":true,"error":error,"actualUnbind":result.as_ref().ok(),"actualError":result.as_ref().err(),"injectedAfterActualUnbind":true});
        }
    }
    Err(error.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_process_cannot_arm_device_or_decoder_faults() {
        assert!(arm("device-init-failure").is_err());
        assert!(arm("decoder-failure").is_err());
        assert!(arm("arbitrary-process").is_err());
        assert!(snapshot().is_null());
    }
    #[test]
    fn fault_fires_only_once_at_the_matching_boundary() {
        let mut fault = Fault {
            kind: Kind::Device,
            consumed: false,
            evidence: json!({}),
        };
        assert!(!fault.take(Kind::Decoder));
        assert!(fault.take(Kind::Device));
        assert!(!fault.take(Kind::Device));
        assert!(!fault.take(Kind::Decoder));
    }
    #[test]
    fn unbind_fault_is_one_shot_and_records_the_actual_retained_session() {
        let mut fault = Fault {
            kind: Kind::Unbind,
            consumed: false,
            evidence: json!({}),
        };
        let snapshot = Ok(json!({"sessionId":"surface-a","mediaSessionActive":true}));
        fault.record_unbind(&snapshot);
        assert!(fault.evidence["unbind"].is_null());
        assert!(!fault.take(Kind::Cleanup));
        assert!(fault.take(Kind::Unbind));
        assert!(!fault.take(Kind::Unbind));
        fault.record_unbind(&snapshot);
        assert_eq!(
            fault.evidence["unbind"]["bindingBeforeFailure"],
            snapshot.unwrap()
        );
        assert_eq!(fault.evidence["unbind"]["actualSuspendAttempted"], false);
        assert_eq!(
            fault.evidence["unbind"]["injectionBoundary"],
            "before-suspend"
        );
        assert!(fault.evidence["unbind"]["snapshotError"].is_null());
    }
    #[test]
    fn unbind_fault_preserves_failed_snapshot_instead_of_inventing_binding() {
        let mut fault = Fault {
            kind: Kind::Unbind,
            consumed: true,
            evidence: json!({}),
        };
        fault.record_unbind(&Err("snapshot unavailable".into()));
        assert!(fault.evidence["unbind"]["bindingBeforeFailure"].is_null());
        assert_eq!(
            fault.evidence["unbind"]["snapshotError"],
            "snapshot unavailable"
        );
    }
}
