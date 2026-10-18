//! Hidden source-preview failure-path verification. Expected failure is never
//! confused with successful playback or successful resource retirement.
use super::{source_faults, source_runtime, source_validation, validation};
use serde_json::{Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};
use tauri::AppHandle;

pub(super) fn is_scenario(scenario: &str) -> bool {
    matches!(
        scenario,
        "seek-switch" | "decoder-failure" | "device-init-failure"
    )
}

fn need(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}
fn joined(decoder: &Value) -> Result<(), String> {
    need(
        decoder["joined"] == true
            && decoder["processesReaped"] == true
            && decoder["cleanupErrors"]
                .as_array()
                .is_some_and(Vec::is_empty),
        "Failure-path decoder was not joined and reaped without cleanup failures",
    )
}
fn hidden(step: &Value) -> Result<(), String> {
    need(
        step["host"]["parent"]["visible"] == false && step["host"]["parent"]["focused"] == false,
        "Failure-path instance became visible or focused",
    )
}
fn seek_overlapped(trace: &Value, token: &str, next_media: &str) -> bool {
    seek_overlap(trace, token, next_media, None)
}
fn seek_overlap(trace: &Value, token: &str, next_media: &str, error: Option<&str>) -> bool {
    let Some(calls) = trace["calls"].as_array() else {
        return false;
    };
    calls.iter().any(|seek| {
        if seek["command"] != "native_source_control"
            || seek["request"]["action"] != "seek"
            || seek["request"]["token"] != token
            || error.is_some_and(|error| seek["error"] != error)
        {
            return false;
        }
        let (Some(start), Some(end)) = (seek["startedMs"].as_f64(), seek["settledMs"].as_f64())
        else {
            return false;
        };
        calls.iter().any(|open| {
            open["command"] == "native_source_open"
                && open["request"]["mediaId"] == next_media
                && open["startedMs"]
                    .as_f64()
                    .is_some_and(|time| time >= start && time < end)
        })
    })
}

pub(super) fn validate_cleanup(report: &Value) -> Result<(), String> {
    let scenario = report["scenario"]
        .as_str()
        .ok_or("Missing failure scenario")?;
    need(is_scenario(scenario), "Unknown failure scenario")?;
    let trace = &report["nativeSourceCallsFinal"];
    need(
        trace["error"].is_null() && trace["pendingOpen"] == 0 && trace["pendingControl"] == 0,
        "Failure-path native commands did not settle",
    )?;
    need(
        trace["calls"].as_array().is_some_and(|calls| {
            calls
                .iter()
                .all(|call| call["settledMs"].is_number() && call["aborted"] != true)
        }),
        "Missing completed native command evidence",
    )?;
    let runtime = &report["runtimeFinal"];
    need(
        runtime["busy"] != true
            && runtime["active"].is_null()
            && runtime["lifecycle"]["opening"] == 0
            && runtime["lifecycle"]["exiting"] == true,
        "Failure-path application exit retained source ownership",
    )?;
    let window = &report["windowCleanup"];
    need(
        window["error"].is_null()
            && (window["noChildOwned"] == true
                || (window["result"]["closed"] == true
                    && window["result"]["childExists"] == false)),
        "Failure-path native child remained at exit",
    )?;
    let a = report["sourceAId"]
        .as_str()
        .ok_or("Missing original media identity")?;
    let b = report["sourceBId"]
        .as_str()
        .ok_or("Missing recovery media identity")?;
    let cleanups = runtime["cleanups"]
        .as_array()
        .ok_or("Missing failure cleanup evidence")?;
    need(
        cleanups.iter().any(|entry| entry["mediaId"] == b),
        "Recovery actor was not joined",
    )?;
    if scenario != "device-init-failure" {
        need(
            cleanups.iter().any(|entry| entry["mediaId"] == a),
            "Original source actor was not joined",
        )?;
    }
    for entry in cleanups {
        let actor = &entry["actor"];
        let decoder = &actor["adapterEvidence"]["transferClose"]["decoder"];
        joined(decoder)?;
        need(
            entry["joinError"].is_null()
                && entry["sink"]["closed"] == true
                && entry["sink"]["pending"] == 0
                && entry["sink"]["error"].is_null(),
            "Failure-path actor or surface retirement failed",
        )?;
        let errors = actor["cleanupErrors"]
            .as_array()
            .ok_or("Missing cleanup error ledger")?;
        need(errors.is_empty(), "Source actor reported cleanup failures")?;
        if scenario == "decoder-failure" && entry["mediaId"] == a {
            need(
                actor["ok"] == false && nonempty(&actor["error"]),
                "Injected decoder failure was not observed by the source actor",
            )?;
            // The injected stream error remains primary evidence; retirement
            // has its own ledger and must succeed without any exemptions.
        } else if scenario == "seek-switch" && entry["mediaId"] == a && !actor["error"].is_null() {
            // Selecting another source can detach the old surface before its
            // seek completes. Admit only that exact cancellation with matching
            // command/owner evidence; never call it a completed seek.
            let token = report["sourceAToken"]
                .as_str()
                .ok_or("Missing original token")?;
            let cancellation = "Media surface has been closed";
            need(
                actor["ok"] == false
                    && actor["error"] == cancellation
                    && entry["token"] == token
                    && entry["cleanupConfirmed"] == true
                    && entry["cleanupConfirmationError"].is_null()
                    && decoder["error"].is_null()
                    && seek_overlap(trace, token, b, Some(cancellation)),
                "Original seek failed without proven replacement cancellation and cleanup",
            )?;
        } else {
            need(
                actor["ok"] == true
                    && actor["error"].is_null()
                    && errors.is_empty()
                    && decoder["error"].is_null(),
                "Recovered or unfaulted actor failed",
            )?;
        }
    }
    let steps = report["steps"]
        .as_array()
        .ok_or("Missing failure-path observations")?;
    let recovery = steps
        .iter()
        .find(|step| step["label"] == "recovered-playing")
        .ok_or("Native recovery playback was not observed")?;
    hidden(recovery)?;
    let active = &recovery["runtime"]["active"];
    need(
        active["mediaId"] == b
            && active["phase"] == "playing"
            && active["muted"] == true
            && active["error"].is_null()
            && active["positionNs"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|n| n >= 250_000_000)
            && recovery["dom"]["sourceEngine"] == "native-source"
            && recovery["dom"]["sourceMediaId"] == b
            && recovery["dom"]["mediaNodeCount"] == 0
            && recovery["host"]["mediaSessionActive"] == true
            && recovery["host"]["child"].is_object(),
        "Recovered source did not own a single working native player",
    )?;
    if scenario == "seek-switch" {
        need(
            seek_overlapped(
                trace,
                report["sourceAToken"]
                    .as_str()
                    .ok_or("Missing original token")?,
                b,
            ),
            "Selection did not overlap the real source seek command",
        )?;
    } else {
        let fallback = steps
            .iter()
            .find(|step| step["label"] == "failure-fallback")
            .ok_or("Compatibility fallback was not observed")?;
        hidden(fallback)?;
        need(
            fallback["runtime"]["busy"] != true
                && fallback["runtime"]["active"].is_null()
                && fallback["runtime"]["lifecycle"]["opening"] == 0
                && fallback["dom"]["mediaNodeCount"] == 1
                && fallback["dom"]["sourceEngine"] != "native-source"
                && fallback["host"]["mediaSessionActive"] == false
                && fallback["host"]["child"].is_null(),
            "Compatibility player started before native retirement",
        )?;
        if scenario == "device-init-failure" {
            let fault = &report["faultEvidence"]["device"];
            need(
                fault["injected"] == true
                    && fault["workerJoined"] == true
                    && nonempty(&fault["error"])
                    && fault["cleanupError"].is_null()
                    && fault["actualDeviceOpenAttempted"] == false,
                "Device initialization injection or worker join was not observed",
            )?;
            joined(&fault["transferClose"]["decoder"])?;
            need(
                fault["transferClose"]["decoder"]["error"].is_null(),
                "Initialization decoder cleanup failed",
            )?;
        } else {
            let fault = &report["faultEvidence"]["decoder"];
            need(
                fault["injected"] == true
                    && fault["termination"]["exited"] == true
                    && fault["termination"]["pid"]
                        .as_u64()
                        .is_some_and(|pid| pid > 0)
                    && fault["termination"]["label"] == "stream-rgb"
                    && fault["error"].is_null()
                    && fault["injectionPhase"] == "paused"
                    && fault["actorPositionNs"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .is_some_and(|n| n >= 250_000_000),
                "Owned decoder termination while paused was not observed",
            )?;
        }
    }
    Ok(())
}

fn push_step(report: &mut Value, step: Value) {
    report["steps"].as_array_mut().unwrap().push(step);
}
fn wait_call<F>(predicate: F) -> Result<Value, String>
where
    F: Fn(&Value) -> bool,
{
    let began = Instant::now();
    loop {
        let trace = source_validation::trace_snapshot();
        need(
            trace["error"].is_null(),
            "Source command evidence overflowed",
        )?;
        if trace["calls"]
            .as_array()
            .is_some_and(|calls| calls.iter().any(&predicate))
        {
            return Ok(trace);
        }
        need(
            began.elapsed() < Duration::from_secs(10),
            "Expected native source command was not observed",
        )?;
        thread::sleep(Duration::from_millis(1));
    }
}
fn wait_fallback(app: &AppHandle) -> Result<Value, String> {
    let began = Instant::now();
    loop {
        let state = source_runtime::inspect();
        if state["busy"] != true && state["active"].is_null() && state["lifecycle"]["opening"] == 0
        {
            let dom = validation::dom(app)?;
            let host = validation::status(app)?;
            let step = json!({"label":"failure-fallback","runtime":state,"dom":dom,"host":host});
            hidden(&step)?;
            if step["dom"]["mediaNodeCount"] == 1
                && step["dom"]["sourceEngine"] != "native-source"
                && step["host"]["mediaSessionActive"] == false
                && step["host"]["child"].is_null()
            {
                return Ok(step);
            }
        }
        need(
            began.elapsed() < Duration::from_secs(20),
            "Source failure did not retire into compatibility playback",
        )?;
        thread::sleep(Duration::from_millis(30));
    }
}

pub(super) fn run_fault(
    app: &AppHandle,
    files: &[String],
    scenario: &str,
    report: &mut Value,
) -> Result<(), String> {
    use source_validation::{import_fixtures, record, seek, select, wait, wait_trace_settled};
    need(is_scenario(scenario), "Unknown source fault scenario")?;
    import_fixtures(app, files, report)?;
    let media_id = |name: &str| {
        report["mediaRefs"]
            .as_array()
            .and_then(|refs| refs.iter().find(|m| m["name"] == name))
            .and_then(|m| m["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("Missing imported source {name}"))
    };
    let a = media_id("native-source-a.mp4")?;
    let b = media_id("native-source-b.mp4")?;
    report["sourceAId"] = a.clone().into();
    report["sourceBId"] = b.clone().into();
    report["steps"] = json!([]);
    let host = validation::status(app)?;
    need(
        host["parent"]["visible"] == false && host["parent"]["focused"] == false,
        "Fault validation instance became visible or focused",
    )?;
    report["hostBeforeScenario"] = host;
    if scenario == "device-init-failure" {
        source_faults::arm(scenario)?;
    }
    select(app, "native-source-a.mp4")?;
    if scenario != "device-init-failure" {
        let ready = wait(|s| {
            s["active"]["phase"] == "ready"
                && s["active"]["mediaId"] == a
                && s["lifecycle"]["opening"] == 0
        })?;
        let token = ready["active"]["token"]
            .as_str()
            .ok_or("Missing original native token")?
            .to_owned();
        report["sourceAToken"] = token.clone().into();
        push_step(report, record(app, "original-ready", ready)?);
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        let playing = wait(|s| {
            s["active"]["phase"] == "playing"
                && s["active"]["positionNs"]
                    .as_str()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= 300_000_000)
        })?;
        push_step(report, record(app, "original-playing", playing)?);
        if scenario == "decoder-failure" {
            validation::eval(app, "document.querySelector('.preview-play').click()")?;
            let paused = wait(|s| s["active"]["phase"] == "paused")?;
            push_step(report, record(app, "original-paused", paused)?);
            source_faults::arm(scenario)?;
        } else {
            seek(app, 1.75)?;
            report["nativeCallsAtSwitch"] = wait_call(|call| {
                call["command"] == "native_source_control"
                    && call["request"]["action"] == "seek"
                    && call["request"]["token"] == token
                    && call["settledMs"].is_null()
            })?;
            // Select immediately after the actual native seek entry, without a
            // round-trip that could let seeking settle before the selection.
            select(app, "native-source-b.mp4")?;
            wait_call(|call| {
                call["command"] == "native_source_open" && call["request"]["mediaId"] == b
            })?;
            let trace = wait_trace_settled()?;
            need(
                seek_overlapped(&trace, &token, &b),
                "Real source seek did not overlap source replacement",
            )?;
            report["nativeCallsAfterSwitch"] = trace;
        }
    }
    if scenario != "seek-switch" {
        push_step(report, wait_fallback(app)?);
        report["faultEvidenceBeforeRecovery"] = source_faults::snapshot();
        select(app, "native-source-b.mp4")?;
    }
    let ready = wait(|s| {
        s["active"]["phase"] == "ready"
            && s["active"]["mediaId"] == b
            && s["lifecycle"]["opening"] == 0
    })?;
    need(
        ready["active"]["positionNs"] == "0",
        "Replacement source inherited the old playhead",
    )?;
    push_step(report, record(app, "recovered-ready", ready)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let recovered = wait(|s| {
        s["active"]["mediaId"] == b
            && s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n >= 300_000_000)
    })?;
    push_step(report, record(app, "recovered-playing", recovered)?);
    report["nativeSourceCalls"] = wait_trace_settled()?;
    report["faultEvidence"] = source_faults::snapshot();
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    report["finalDom"] = validation::dom(app)?;
    report["exitReason"] =
        "Exit after native recovery; normal lifecycle must join the remaining source actor".into();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cleaned(media: &str, failed: bool) -> Value {
        json!({"mediaId":media,"joinError":null,"actor":{
            "ok":!failed,"error":if failed {json!("Decoder stream-rgb exited unexpectedly")} else {Value::Null},
            "cleanupErrors":[],"adapterEvidence":{"transferClose":{"decoder":{
                "joined":true,"processesReaped":true,"error":null,"cleanupErrors":[]
            }}}},"sink":{"closed":true,"pending":0,"error":null}})
    }
    fn report(scenario: &str) -> Value {
        let mut cleanups = vec![cleaned("b", false)];
        if scenario != "device-init-failure" {
            cleanups.insert(0, cleaned("a", scenario == "decoder-failure"));
        }
        json!({
            "scenario":scenario,"sourceAId":"a","sourceBId":"b","sourceAToken":"token-a",
            "runtimeFinal":{"active":null,"lifecycle":{"opening":0,"exiting":true},"cleanups":cleanups},
            "windowCleanup":{"noChildOwned":true},
            "nativeSourceCallsFinal":{"error":null,"pendingOpen":0,"pendingControl":0,"calls":[
                {"command":"native_source_control","request":{"action":"seek","token":"token-a"},"startedMs":1.0,"settledMs":3.0},
                {"command":"native_source_open","request":{"mediaId":"b"},"startedMs":2.0,"settledMs":4.0}
            ]},
            "faultEvidence":{
                "device":{"injected":true,"workerJoined":true,"error":"Injected audio initialization failure","actualDeviceOpenAttempted":false,
                    "transferClose":{"decoder":{"joined":true,"processesReaped":true,"error":null,"cleanupErrors":[]}}},
                "decoder":{"injected":true,"injectionPhase":"paused","actorPositionNs":"300000000","termination":{"pid":4321,"label":"stream-rgb","exited":true}}
            },
            "steps":[
                {"label":"failure-fallback","runtime":{"active":null,"lifecycle":{"opening":0}},
                    "dom":{"mediaNodeCount":1},"host":{"mediaSessionActive":false,"child":null,"parent":{"visible":false,"focused":false}}},
                {"label":"recovered-playing","runtime":{"active":{"mediaId":"b","token":"token-b","phase":"playing","positionNs":"300000000","muted":true,"error":null}},
                    "dom":{"sourceEngine":"native-source","sourceMediaId":"b","mediaNodeCount":0},
                    "host":{"mediaSessionActive":true,"child":{},"parent":{"visible":false,"focused":false}}}
            ]
        })
    }

    #[test]
    fn accepts_expected_decoder_failure_with_retirement_and_recovery() {
        assert!(validate_cleanup(&report("decoder-failure")).is_ok());
    }
    #[test]
    fn accepts_device_initialization_failure_only_after_worker_join() {
        let mut value = report("device-init-failure");
        assert!(validate_cleanup(&value).is_ok());
        value["faultEvidence"]["device"]["workerJoined"] = false.into();
        assert!(validate_cleanup(&value).is_err());
    }
    #[test]
    fn accepts_seek_switch_only_when_native_commands_really_overlap() {
        let mut value = report("seek-switch");
        assert!(validate_cleanup(&value).is_ok());
        value["nativeSourceCallsFinal"]["calls"][1]["startedMs"] = 4.0.into();
        assert!(validate_cleanup(&value).is_err());
    }
    #[test]
    fn rejects_decoder_failure_without_injection_or_observed_actor_error() {
        let mut value = report("decoder-failure");
        value["faultEvidence"]["decoder"]["injected"] = false.into();
        assert!(validate_cleanup(&value).is_err());
        value = report("decoder-failure");
        value["runtimeFinal"]["cleanups"][0]["actor"]["error"] = Value::Null;
        assert!(validate_cleanup(&value).is_err());
    }
    #[test]
    fn rejects_retained_workers_children_and_unsettled_commands() {
        for path in ["worker", "child", "commands", "opening"] {
            let mut value = report("decoder-failure");
            match path {
                "worker" => {
                    value["runtimeFinal"]["cleanups"][0]["actor"]["adapterEvidence"]["transferClose"]
                        ["decoder"]["joined"] = false.into()
                }
                "child" => value["windowCleanup"]["noChildOwned"] = false.into(),
                "commands" => value["nativeSourceCallsFinal"]["pendingControl"] = 1.into(),
                _ => value["runtimeFinal"]["lifecycle"]["opening"] = 1.into(),
            }
            assert!(validate_cleanup(&value).is_err(), "{path}");
        }
    }
    #[test]
    fn rejects_fallback_before_child_release_or_without_html_player() {
        for child in [false, true] {
            let mut value = report("decoder-failure");
            if child {
                value["steps"][0]["host"]["child"] = json!({"surfaceId":"old"});
            } else {
                value["steps"][0]["dom"]["mediaNodeCount"] = 0.into();
            }
            assert!(validate_cleanup(&value).is_err());
        }
    }
    #[test]
    fn rejects_failed_recovery_actor_or_duplicate_html_player() {
        let mut value = report("decoder-failure");
        value["runtimeFinal"]["cleanups"][1]["actor"]["error"] =
            "unexpected recovery failure".into();
        assert!(validate_cleanup(&value).is_err());
        value = report("decoder-failure");
        value["steps"][1]["dom"]["mediaNodeCount"] = 1.into();
        assert!(validate_cleanup(&value).is_err());
    }
    #[test]
    fn rejects_failure_in_an_unproven_phase_or_unrelated_cleanup_error() {
        let mut value = report("decoder-failure");
        value["faultEvidence"]["decoder"]["injectionPhase"] = "playing".into();
        assert!(validate_cleanup(&value).is_err());
        value = report("decoder-failure");
        value["runtimeFinal"]["cleanups"][0]["actor"]["cleanupErrors"] =
            json!(["device stop failed"]);
        assert!(validate_cleanup(&value).is_err());
    }
    #[test]
    fn rejects_decoder_failure_repeated_in_cleanup_errors() {
        let mut value = report("decoder-failure");
        value["runtimeFinal"]["cleanups"][0]["actor"]["adapterEvidence"]["transferClose"]["decoder"]
            ["error"] = "stream-rgb exited unexpectedly".into();
        value["runtimeFinal"]["cleanups"][0]["actor"]["cleanupErrors"] =
            json!(["Audio cancellation cleanup failed: stream-rgb exited unexpectedly"]);
        assert!(validate_cleanup(&value).is_err());
    }
    #[test]
    fn rejects_missing_or_nonempty_decoder_cleanup_error_ledger() {
        for scenario in ["decoder-failure", "device-init-failure", "seek-switch"] {
            for errors in [Value::Null, json!(["decoder reader join failed"])] {
                let mut value = report(scenario);
                let decoder = if scenario == "device-init-failure" {
                    &mut value["faultEvidence"]["device"]["transferClose"]["decoder"]
                } else {
                    &mut value["runtimeFinal"]["cleanups"][0]["actor"]["adapterEvidence"]["transferClose"]
                        ["decoder"]
                };
                decoder["cleanupErrors"] = errors;
                assert!(validate_cleanup(&value).is_err(), "{scenario}");
            }
        }
    }
    fn cancelled_seek() -> Value {
        let mut value = report("seek-switch");
        let old = &mut value["runtimeFinal"]["cleanups"][0];
        old["token"] = "token-a".into();
        old["cleanupConfirmed"] = true.into();
        old["actor"]["ok"] = false.into();
        old["actor"]["error"] = "Media surface has been closed".into();
        value["nativeSourceCallsFinal"]["calls"][0]["error"] =
            "Media surface has been closed".into();
        value
    }
    #[test]
    fn accepts_seek_cancelled_by_replacement_after_confirmed_cleanup() {
        assert!(validate_cleanup(&cancelled_seek()).is_ok());
    }
    #[test]
    fn rejects_seek_cancellation_without_exact_error_overlap_or_cleanup() {
        for reason in [
            "actor-error",
            "command-error",
            "overlap",
            "cleanup",
            "token",
        ] {
            let mut value = cancelled_seek();
            match reason {
                "actor-error" => {
                    value["runtimeFinal"]["cleanups"][0]["actor"]["error"] =
                        "Unexpected decoder failure".into()
                }
                "command-error" => {
                    value["nativeSourceCallsFinal"]["calls"][0]["error"] =
                        "Unexpected decoder failure".into()
                }
                "overlap" => value["nativeSourceCallsFinal"]["calls"][1]["startedMs"] = 4.0.into(),
                "cleanup" => {
                    value["runtimeFinal"]["cleanups"][0]["cleanupConfirmed"] = false.into()
                }
                _ => value["runtimeFinal"]["cleanups"][0]["token"] = "another-token".into(),
            }
            assert!(validate_cleanup(&value).is_err(), "{reason}");
        }
    }
}
