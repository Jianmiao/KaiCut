//! Explicitly controlled scheduling at real initialization boundaries. This
//! never interrupts a driver call and is not a cancellation-latency benchmark.
use super::{source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use std::{
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tauri::AppHandle;
struct StageHold {
    target: String,
    claimed: bool,
    evidence: Value,
}
impl StageHold {
    fn claim(&mut self, stage: &str, request: &str) -> bool {
        if self.claimed || self.target != stage {
            return false;
        }
        self.claimed = true;
        self.evidence["claimed"] = true.into();
        self.evidence["holding"] = true.into();
        self.evidence["requestId"] = request.into();
        self.evidence["stage"] = stage.into();
        true
    }
}
static HOLD: Mutex<Option<StageHold>> = Mutex::new(None);
pub(super) fn is_scenario(s: &str) -> bool {
    matches!(s, "device-cancel" | "frame-cancel" | "frame-exit")
}
fn arm(scenario: &str) -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().collect();
    if !cfg!(debug_assertions)
        || !is_scenario(scenario)
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
        return Err("Stage scheduling requires isolated hidden debug validation".into());
    }
    let target = if scenario == "device-cancel" {
        "device-opened"
    } else {
        "first-prepare-tick"
    };
    let mut hold = HOLD.lock().map_err(|_| "Stage evidence unavailable")?;
    if hold.is_some() {
        return Err("Stage schedule already armed".into());
    }
    *hold = Some(StageHold {
        target: target.into(),
        claimed: false,
        evidence: json!({"scenario":scenario,"target":target,"controlledScheduling":true,"maxWaitMs":10000,"holding":false,"claimed":false}),
    });
    Ok(())
}
pub(super) fn snapshot() -> Value {
    HOLD.lock()
        .ok()
        .and_then(|h| h.as_ref().map(|h| h.evidence.clone()))
        .unwrap_or(Value::Null)
}
pub(super) fn checkpoint(
    stage: &str,
    request: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    let claimed = HOLD
        .lock()
        .ok()
        .and_then(|mut h| h.as_mut().map(|h| h.claim(stage, request)))
        .unwrap_or(false);
    if !claimed {
        return check();
    }
    let began = Instant::now();
    loop {
        let result = check();
        let timed_out = began.elapsed() >= Duration::from_secs(10);
        if result.is_err() || timed_out {
            if let Ok(mut h) = HOLD.lock() {
                if let Some(h) = h.as_mut() {
                    h.evidence["holding"] = false.into();
                    h.evidence["cancelObserved"] = result.is_err().into();
                    h.evidence["timedOut"] = timed_out.into();
                    h.evidence["elapsedMs"] = json!(began.elapsed().as_secs_f64() * 1000.0);
                    h.evidence["reason"] = json!(result.as_ref().err());
                }
            }
            return if timed_out {
                Err("Stage validation exceeded its bounded scheduling window".into())
            } else {
                result
            };
        }
        thread::sleep(Duration::from_millis(2));
    }
}
fn need(v: bool, s: &str) -> Result<(), String> {
    if v { Ok(()) } else { Err(s.into()) }
}
fn stage_finished(s: &Value) -> bool {
    s["controlledScheduling"] == true
        && s["claimed"] == true
        && s["holding"] == false
        && s["cancelObserved"] == true
        && s["timedOut"] == false
        && s["elapsedMs"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n < 10000.0)
        && s["maxWaitMs"] == 10000
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_claim_matches_exactly_once() {
        let mut h = StageHold {
            target: "device-opened".into(),
            claimed: false,
            evidence: json!({}),
        };
        assert!(!h.claim("device-before-open", "a"));
        assert!(h.claim("device-opened", "a"));
        assert!(!h.claim("device-opened", "b"));
        assert_eq!(h.evidence["requestId"], "a");
    }
    #[test]
    fn timeout_or_unobserved_cancellation_never_qualifies() {
        let mut s = json!({"controlledScheduling":true,"claimed":true,"holding":false,"cancelObserved":true,"timedOut":false,"elapsedMs":20.0,"maxWaitMs":10000});
        assert!(stage_finished(&s));
        s["cancelObserved"] = false.into();
        assert!(!stage_finished(&s));
        s["cancelObserved"] = true.into();
        s["timedOut"] = true.into();
        assert!(!stage_finished(&s));
    }
}
fn media(report: &Value, name: &str) -> Result<String, String> {
    report["mediaRefs"]
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["name"] == name))
        .and_then(|m| m["id"].as_str())
        .map(str::to_owned)
        .ok_or("Missing source identity".into())
}
fn step(app: &AppHandle, label: &str) -> Result<Value, String> {
    Ok(
        json!({"label":label,"runtime":source_runtime::inspect(),"dom":validation::dom(app)?,"host":validation::status(app)?}),
    )
}
pub(super) fn run(
    app: &AppHandle,
    files: &[String],
    scenario: &str,
    report: &mut Value,
) -> Result<(), String> {
    v::import_fixtures(app, files, report)?;
    let a = media(report, "native-source-a.mp4")?;
    let b = media(report, "native-source-b.mp4")?;
    report["sourceAId"] = a.clone().into();
    report["sourceBId"] = b.clone().into();
    report["steps"] = json!([]);
    let initial = step(app, "before-selection")?;
    need(
        initial["host"]["parent"]["visible"] == false
            && initial["host"]["parent"]["focused"] == false,
        "Validation window became visible/focused",
    )?;
    report["steps"].as_array_mut().unwrap().push(initial);
    arm(scenario)?;
    v::select(app, "native-source-a.mp4")?;
    let began = Instant::now();
    loop {
        let hold = snapshot();
        if hold["holding"] == true {
            report["checkpointAtCancel"] =
                json!({"hold":hold,"observation":step(app,"checkpoint-held")?});
            break;
        }
        need(
            began.elapsed() < Duration::from_secs(20),
            "Real target checkpoint was never reached",
        )?;
        thread::sleep(Duration::from_millis(2));
    }
    if scenario == "frame-exit" {
        report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
        return Ok(());
    }
    validation::eval(
        app,
        "Array.from(document.querySelectorAll('.preview-toolbar button')).find(b=>b.textContent==='时间线').click()",
    )?;
    let settled = v::wait(|s| {
        s["lifecycle"]["opening"] == 0 && s["active"].is_null() && s["pendingPreparations"] == 0
    })?;
    need(
        stage_finished(&snapshot()),
        "Target stage did not observe real cancellation",
    )?;
    report["cancelledRuntime"] = settled;
    let began = Instant::now();
    loop {
        let s = step(app, "cancelled-to-timeline")?;
        if s["dom"]["mode"] == "timeline"
            && s["dom"]["mediaNodeCount"] == 0
            && s["host"]["child"].is_null()
        {
            report["steps"].as_array_mut().unwrap().push(s);
            break;
        }
        need(
            began.elapsed() < Duration::from_secs(5),
            "Cancelled stage retained playback",
        )?;
        thread::sleep(Duration::from_millis(25));
    }
    v::select(app, "native-source-b.mp4")?;
    let ready = v::wait(|s| s["active"]["phase"] == "ready" && s["active"]["mediaId"] == b)?;
    need(
        ready["active"]["positionNs"] == "0",
        "New source inherited old time",
    )?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(v::record(app, "recovered-ready", ready)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let playing = v::wait(|s| {
        s["active"]["phase"] == "playing"
            && s["active"]["mediaId"] == b
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n >= 300_000_000)
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(v::record(app, "recovered-playing", playing)?);
    report["nativeSourceCalls"] = v::wait_trace_settled()?;
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    Ok(())
}
pub(super) fn validate(report: &Value) -> Result<(), String> {
    need(
        stage_finished(&report["stageSchedule"]),
        "Target checkpoint cancellation evidence incomplete",
    )?;
    let final_state = &report["runtimeFinal"];
    need(
        final_state["busy"] != true
            && final_state["active"].is_null()
            && final_state["closeFailure"].is_null()
            && final_state["pendingPreparations"] == 0
            && final_state["lifecycle"]["opening"] == 0
            && final_state["lifecycle"]["exiting"] == true,
        "Cancelled stage retained source resources",
    )?;
    let req = &report["stageSchedule"]["requestId"];
    let prepared = final_state["preparations"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["requestId"] == *req))
        .ok_or("Missing cancelled preparation")?;
    need(
        prepared["status"] == "cancelled",
        "First source preparation was not cancelled",
    )?;
    if report["scenario"] == "frame-exit" {
        need(
            final_state["lifecycle"]["exitObservation"]["opening"]
                .as_u64()
                .is_some_and(|n| n > 0),
            "Exit did not actually interrupt preparation",
        )?;
    } else {
        need(
            report["steps"].as_array().is_some_and(|steps| {
                steps.iter().any(|s| {
                    s["label"] == "recovered-playing"
                        && s["runtime"]["active"]["mediaId"] == report["sourceBId"]
                        && s["dom"]["mediaNodeCount"] == 0
                })
            }),
            "Native recovery missing",
        )?;
    }
    need(
        report["windowCleanup"]["error"].is_null(),
        "Window cleanup failed",
    )?;
    Ok(())
}
