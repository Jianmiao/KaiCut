//! Hidden validation of a one-shot error before native surface suspension.
//! This is a controlled application boundary failure, not a COM/driver failure.
use super::{source_faults, source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

pub(super) fn is_scenario(value: &str) -> bool {
    value == "unbind-failure"
}

fn released_child(cleanup: &Value) -> bool {
    cleanup["error"].is_null()
        && cleanup["result"]["closed"] == true
        && cleanup["result"]["childExists"] == false
        && cleanup["result"]["lifecycle"]["classUnregistered"] == true
        && cleanup["result"]["lifecycle"]["ncDestroyed"] == true
        && cleanup["result"]["lifecycle"]["detachError"].is_null()
}
fn retained_binding(step: &Value, surface: &Value) -> bool {
    let host = &step["host"];
    let binding_retained =
        host["mediaSessionActive"] == true && host["child"]["childExists"] == true;
    let released_by_view = host["mediaSessionActive"] == false
        && host["child"].is_null()
        && released_child(&host["lastCleanup"]);
    surface.is_string()
        && host["sessionId"] == *surface
        && (binding_retained || released_by_view)
        && host["parent"]["visible"] == false
        && host["parent"]["focused"] == false
        && step["runtime"]["busy"] != true
        && step["runtime"]["active"].is_null()
        && step["runtime"]["closeFailure"]["token"].is_string()
        && step["dom"]["mediaNodeCount"] == 0
        && step["dom"]["playDisabled"] == true
        && step["dom"]["sourceAlerts"]
            .as_array()
            .is_some_and(|alerts| !alerts.is_empty())
}
fn retired_actor(cleanup: &Value) -> bool {
    let actor = &cleanup["actor"];
    let decoder = &actor["adapterEvidence"]["transferClose"]["decoder"];
    let device = &actor["adapterEvidence"]["lastCleanup"];
    cleanup["joinError"].is_null()
        && cleanup["sinkError"].is_null()
        && actor["cleanupErrors"].as_array().is_some_and(Vec::is_empty)
        && actor["preparation"]["workerJoined"] == true
        && actor["preparation"]["resourcesReleased"] == true
        && actor["preparation"]["deviceReleased"] == true
        && decoder["joined"] == true
        && decoder["processesReaped"] == true
        && decoder["cleanupErrors"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && device["deviceRunning"] == false
        && device["transferClosed"] == true
        && device["errors"].as_array().is_some_and(Vec::is_empty)
        && cleanup["sink"]["closed"] == true
        && cleanup["sink"]["pending"] == 0
        && cleanup["sink"]["inFlight"] == false
        && cleanup["sink"]["error"].is_null()
}
fn step(app: &AppHandle, label: &str) -> Result<Value, String> {
    Ok(json!({"label":label,"runtime":source_runtime::inspect(),
        "dom":validation::dom(app)?,"host":validation::status(app)?}))
}
fn wait_step(
    app: &AppHandle,
    label: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<Value, String> {
    let began = Instant::now();
    loop {
        let state = step(app, label)?;
        if check(&state) {
            return Ok(state);
        }
        v::need(
            began.elapsed() < Duration::from_secs(20),
            &format!("Unbind state timeout: {state}"),
        )?;
        thread::sleep(Duration::from_millis(20));
    }
}
fn media(report: &Value, name: &str) -> Result<String, String> {
    report["mediaRefs"]
        .as_array()
        .and_then(|refs| refs.iter().find(|m| m["name"] == name))
        .and_then(|m| m["id"].as_str())
        .map(str::to_owned)
        .ok_or("Missing fixture media".into())
}
pub(super) fn run(app: &AppHandle, files: &[String], report: &mut Value) -> Result<(), String> {
    report["scope"]["actualDomControls"] = false.into();
    report["scope"]["initialDomControls"] = true.into();
    report["scope"]["controlOrigin"] = "native-command-wrapper-background".into();
    v::import_fixtures(app, files, report)?;
    let a = media(report, "native-source-a.mp4")?;
    let b = media(report, "native-source-b.mp4")?;
    report["sourceAId"] = a.clone().into();
    report["sourceBId"] = b.clone().into();
    let initial = step(app, "before-selection")?;
    v::need(
        initial["host"]["parent"]["visible"] == false
            && initial["host"]["parent"]["focused"] == false,
        "Instance must remain hidden and unfocused",
    )?;
    report["steps"] = json!([initial]);
    v::select(app, "native-source-a.mp4")?;
    let ready = v::wait(|s| s["active"]["mediaId"] == a && s["active"]["phase"] == "ready")?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(v::record(app, "original-ready", ready)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let playing = v::wait(|s| {
        s["active"]["mediaId"] == a
            && s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|n| n >= 300_000_000)
    })?;
    report["playingBeforeFailure"] = v::record(app, "original-playing", playing)?;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let paused = v::wait(|s| s["active"]["mediaId"] == a && s["active"]["phase"] == "paused")?;
    let token = paused["active"]["token"]
        .as_str()
        .ok_or("Missing source token")?
        .to_owned();
    report["sourceAToken"] = token.clone().into();
    let paused = v::record(app, "original-paused", paused)?;
    let surface = paused["host"]["sessionId"].clone();
    report["steps"].as_array_mut().unwrap().push(paused);
    source_faults::arm("unbind-failure")?;
    let close = || {
        tauri::async_runtime::block_on(super::native_source_control(
            app.get_webview_window("main").ok_or("Missing main")?,
            super::SourceControlRequest {
                token: token.clone(),
                action: "close".into(),
                position_ns: None,
            },
        ))
    };
    let first = close();
    v::need(first.is_err(), "Injected unbind did not reject close")?;
    report["firstCloseError"] = json!(first.err());
    report["afterFirstClose"] = step(app, "immediately-after-close")?;
    let blocked = wait_step(app, "original-blocked", |s| retained_binding(s, &surface))?;
    report["steps"].as_array_mut().unwrap().push(blocked);
    let repeated = close();
    v::need(repeated.is_err(), "Repeated close lost sticky failure")?;
    report["repeatedCloseError"] = json!(repeated.err());
    let blocked = wait_step(app, "repeated-close-blocked", |s| {
        retained_binding(s, &surface)
    })?;
    report["steps"].as_array_mut().unwrap().push(blocked);
    v::select(app, "native-source-b.mp4")?;
    let blocked = wait_step(app, "replacement-blocked", |s| {
        retained_binding(s, &surface) && s["dom"]["sourceMediaId"] == b
    })?;
    report["steps"].as_array_mut().unwrap().push(blocked);
    report["nativeSourceCalls"] = v::wait_trace_settled()?;
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    report["faultEvidence"] = source_faults::snapshot();
    Ok(())
}
pub(super) fn validate(report: &Value) -> Result<(), String> {
    let fault = &report["faultEvidence"];
    let unbind = &fault["unbind"];
    let before = &unbind["bindingBeforeFailure"];
    let after = &unbind["bindingAfterFailure"];
    let surface = &before["sessionId"];
    v::need(
        fault["scenario"] == "unbind-failure"
            && fault["oneShot"] == true
            && fault["injected"] == true
            && unbind["injectionBoundary"] == "before-suspend"
            && unbind["actualSuspendAttempted"] == false
            && unbind["error"] == source_faults::UNBIND_ERROR
            && unbind["snapshotError"].is_null()
            && unbind["afterSnapshotError"].is_null(),
        "Missing pre-suspend application failure evidence",
    )?;
    v::need(
        surface.is_string()
            && after["sessionId"] == *surface
            && before["mediaSessionActive"] == true
            && after["mediaSessionActive"] == true
            && before["child"]["childExists"] == true
            && after["child"]["childExists"] == true
            && before["child"]["surfaceId"].is_string()
            && before["child"]["surfaceId"] == after["child"]["surfaceId"]
            && before["child"]["childHwnd"] == after["child"]["childHwnd"],
        "Failed transaction did not preserve the same bound surface",
    )?;
    v::need(
        report["firstCloseError"] == source_faults::UNBIND_ERROR
            && report["repeatedCloseError"] == report["firstCloseError"],
        "Close error was hidden or changed",
    )?;
    let playing = &report["playingBeforeFailure"];
    v::need(
        playing["runtime"]["active"]["mediaId"] == report["sourceAId"]
            && playing["runtime"]["active"]["phase"] == "playing"
            && playing["runtime"]["active"]["positionNs"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|n| n >= 300_000_000)
            && playing["dom"]["mediaNodeCount"] == 0,
        "A did not play before failure",
    )?;
    let steps = report["steps"].as_array().ok_or("Missing unbind steps")?;
    for label in [
        "original-blocked",
        "repeated-close-blocked",
        "replacement-blocked",
    ] {
        let step = steps
            .iter()
            .find(|s| s["label"] == label)
            .ok_or("Missing blocked step")?;
        v::need(
            retained_binding(step, surface)
                && step["runtime"]["closeFailure"]["token"] == report["sourceAToken"]
                && step["runtime"]["closeFailure"]["error"] == report["firstCloseError"],
            "Replacement escaped sticky ownership",
        )?;
        if label == "replacement-blocked" {
            v::need(
                step["dom"]["sourceMediaId"] == report["sourceBId"],
                "B selection was not exercised",
            )?;
        }
    }
    let state = &report["runtimeFinal"];
    v::need(
        state["active"].is_null()
            && state["busy"] != true
            && state["pendingPreparations"] == 0
            && state["lifecycle"]["opening"] == 0
            && state["lifecycle"]["exiting"] == true
            && state["closeFailure"]["token"] == report["sourceAToken"]
            && state["closeFailure"]["error"] == report["firstCloseError"],
        "Final sticky runtime is not retired",
    )?;
    let cleanups = state["cleanups"]
        .as_array()
        .ok_or("Missing actor cleanup")?;
    v::need(cleanups.len() == 1, "Unexpected replacement actor cleanup")?;
    let cleanup = &cleanups[0];
    v::need(
        retired_actor(cleanup)
            && cleanup["token"] == report["sourceAToken"]
            && cleanup["mediaId"] == report["sourceAId"]
            && cleanup["unbind"].is_null()
            && cleanup["unbindError"] == report["firstCloseError"]
            && cleanup["cleanupConfirmed"] == false,
        "Raw actor cleanup or unbind rejection evidence missing",
    )?;
    let trace = &report["nativeSourceCallsFinal"];
    v::need(
        trace["error"].is_null() && trace["pendingOpen"] == 0 && trace["pendingControl"] == 0,
        "Command trace did not settle",
    )?;
    let calls = trace["calls"].as_array().ok_or("Missing command trace")?;
    v::need(
        calls
            .iter()
            .filter(|c| {
                c["command"] == "native_source_control"
                    && c["request"]["token"] == report["sourceAToken"]
                    && c["request"]["action"] == "close"
                    && c["error"] == report["firstCloseError"]
                    && c["settledMs"].is_number()
            })
            .count()
            >= 2,
        "Actual repeated close commands were not rejected",
    )?;
    v::need(
        !calls.iter().any(|c| {
            c["command"] == "native_source_open"
                && c["request"]["mediaId"] == report["sourceBId"]
                && c["result"]["supported"] == true
        }),
        "B unexpectedly obtained a source owner",
    )?;
    let final_close = &report["windowCleanup"];
    let closed_before_exit = final_close["noChildOwned"] == true
        && final_close["error"].is_null()
        && (released_child(&final_close["previousCleanup"])
            || steps.iter().any(|s| {
                s["label"] == "replacement-blocked" && released_child(&s["host"]["lastCleanup"])
            }));
    v::need(
        released_child(final_close) || closed_before_exit,
        "Owned child lacks actual release evidence",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blocked() -> Value {
        json!({"runtime":{"active":null,"closeFailure":{"token":"old","error":"unbind failed"}},
            "dom":{"mediaNodeCount":0,"playDisabled":true,"sourceAlerts":["preview stopped"]},
            "host":{"sessionId":"surface-a","mediaSessionActive":true,"child":{"childExists":true},
                "parent":{"visible":false,"focused":false}}})
    }
    fn cleanup() -> Value {
        json!({"joinError":null,"actor":{"cleanupErrors":[],
            "preparation":{"workerJoined":true,"resourcesReleased":true,"deviceReleased":true},
            "adapterEvidence":{"lastCleanup":{"deviceRunning":false,"transferClosed":true,"errors":[]},
                "transferClose":{"decoder":{"joined":true,"processesReaped":true,"cleanupErrors":[]}}}},
            "sink":{"closed":true,"pending":0,"inFlight":false,"error":null},"sinkError":null})
    }
    #[test]
    fn blocked_replacement_retains_same_surface_and_disables_both_players() {
        let surface = json!("surface-a");
        let good = blocked();
        assert!(retained_binding(&good, &surface));
        for pointer in ["/host/mediaSessionActive", "/dom/playDisabled"] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = false.into();
            assert!(!retained_binding(&bad, &surface), "{pointer}");
        }
        let mut bad = good.clone();
        bad["host"]["sessionId"] = "surface-b".into();
        assert!(!retained_binding(&bad, &surface));
        let mut bad = good.clone();
        bad["dom"]["mediaNodeCount"] = 1.into();
        assert!(!retained_binding(&bad, &surface));
        let mut bad = good;
        bad["runtime"]["active"] = json!({"token":"replacement"});
        assert!(!retained_binding(&bad, &surface));
    }
    #[test]
    fn actor_retirement_requires_raw_join_release_and_queue_evidence() {
        let good = cleanup();
        assert!(retired_actor(&good));
        for pointer in [
            "/actor/preparation/workerJoined",
            "/actor/preparation/resourcesReleased",
            "/actor/preparation/deviceReleased",
            "/actor/adapterEvidence/transferClose/decoder/joined",
            "/actor/adapterEvidence/transferClose/decoder/processesReaped",
            "/sink/closed",
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = false.into();
            assert!(!retired_actor(&bad), "{pointer}");
        }
        let mut bad = good.clone();
        bad["sink"]["inFlight"] = true.into();
        assert!(!retired_actor(&bad));
        let mut bad = good;
        bad["actor"]["adapterEvidence"]["lastCleanup"]["errors"] = json!(["stop failed"]);
        assert!(!retired_actor(&bad));
    }
}
