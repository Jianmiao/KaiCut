//! Hidden tests for cancelling real probe work and failing closed on an
//! injected confirmation error after actual native unbinding.
use super::{source_faults, source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

pub(super) fn is_scenario(value: &str) -> bool {
    matches!(
        value,
        "prepare-cancel" | "prepare-supersede" | "cleanup-failure"
    )
}
fn need(ok: bool, message: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn cancelled_probe(entry: &Value) -> bool {
    if entry["status"] != "cancelled" {
        return false;
    }
    let Some(events) = entry["events"].as_array() else {
        return false;
    };
    events.iter().any(|end| {
        end["event"] == "probe-end"
            && end["cancelled"] == true
            && end["cancelObservedRunning"] == true
            && end["cancelObservedProcess"]["exited"] == false
            && end["cancelObservedProcess"]["pid"] == end["process"]["pid"]
            && end["process"]["exitCode"]
                .as_i64()
                .is_some_and(|code| code != 0)
            && end["readerJoined"] == true
            && end["stderrJoined"] == true
            && end["reaped"] == true
            && end["process"]["exited"] == true
            && end["cleanupErrors"].as_array().is_some_and(Vec::is_empty)
            && events.iter().any(|start| {
                start["event"] == "probe-start"
                    && start["process"]["exited"] == false
                    && start["stage"] == end["stage"]
                    && start["process"]["pid"] == end["process"]["pid"]
            })
    })
}
fn blocked_player(step: &Value) -> bool {
    step["runtime"]["busy"] != true
        && step["runtime"]["active"].is_null()
        && step["runtime"]["closeFailure"]["token"].is_string()
        && step["dom"]["mediaNodeCount"] == 0
        && step["dom"]["playDisabled"] == true
        && step["dom"]["sourceAlerts"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        && step["host"]["child"].is_null()
        && step["host"]["mediaSessionActive"] == false
        && step["host"]["parent"]["visible"] == false
        && step["host"]["parent"]["focused"] == false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_requires_running_probe_and_complete_retirement() {
        let mut entry = json!({"status":"cancelled","events":[{"event":"probe-start","stage":"metadata-probe","process":{"pid":123,"exited":false}}, {"event":"probe-end","stage":"metadata-probe","process":{"pid":123,"exited":true,"exitCode":1},"cancelObservedRunning":true,"cancelObservedProcess":{"pid":123,"exited":false},"cancelled":true,"readerJoined":true,"stderrJoined":true,"reaped":true,"cleanupErrors":[]}]});
        assert!(cancelled_probe(&entry));
        entry["events"][1]["cancelObservedRunning"] = false.into();
        assert!(!cancelled_probe(&entry));
        entry["events"][1]["cancelObservedRunning"] = true.into();
        entry["events"][1]["process"]["exitCode"] = 0.into();
        assert!(!cancelled_probe(&entry));
        entry["events"][1]["process"]["exitCode"] = 1.into();
        entry["events"][1]["readerJoined"] = false.into();
        assert!(!cancelled_probe(&entry));
        entry["events"][1]["readerJoined"] = true.into();
        entry["events"][1]["process"]["pid"] = 124.into();
        assert!(!cancelled_probe(&entry));
        entry["events"][1]["process"]["pid"] = 123.into();
        entry["events"][1]["cleanupErrors"] = json!(["join failed"]);
        assert!(!cancelled_probe(&entry));
    }
    #[test]
    fn failed_close_must_block_html_and_native_replacement() {
        let mut step = json!({"runtime":{"active":null,"closeFailure":{"token":"old","error":"failed"}},"dom":{"mediaNodeCount":0,"playDisabled":true,"sourceAlerts":["preview stopped"]},"host":{"child":null,"mediaSessionActive":false,"parent":{"visible":false,"focused":false}}});
        assert!(blocked_player(&step));
        step["dom"]["mediaNodeCount"] = 1.into();
        assert!(!blocked_player(&step));
        step["dom"]["mediaNodeCount"] = 0.into();
        step["runtime"]["active"] = json!({"token":"new"});
        assert!(!blocked_player(&step));
    }
}

fn step(app: &AppHandle, label: &str) -> Result<Value, String> {
    Ok(
        json!({"label":label,"runtime":source_runtime::inspect(),"dom":validation::dom(app)?,"host":validation::status(app)?}),
    )
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
        need(
            began.elapsed() < Duration::from_secs(20),
            &format!("Shutdown state timeout: {state}"),
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
        .ok_or("Missing media".into())
}
fn prep<'a>(state: &'a Value, id: &str) -> Option<&'a Value> {
    state["preparations"]
        .as_array()?
        .iter()
        .rev()
        .find(|p| p["mediaId"] == id)
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
        "Instance must be hidden/unfocused",
    )?;
    report["steps"].as_array_mut().unwrap().push(initial);
    v::select(app, "native-source-a.mp4")?;
    if scenario == "cleanup-failure" {
        let ready = v::wait(|s| s["active"]["phase"] == "ready")?;
        report["steps"]
            .as_array_mut()
            .unwrap()
            .push(v::record(app, "original-ready", ready)?);
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        v::wait(|s| {
            s["active"]["phase"] == "playing"
                && s["active"]["positionNs"]
                    .as_str()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= 300_000_000)
        })?;
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        let paused = v::wait(|s| s["active"]["phase"] == "paused")?;
        let token = paused["active"]["token"]
            .as_str()
            .ok_or("Missing source token")?
            .to_owned();
        report["sourceAToken"] = token.clone().into();
        report["steps"]
            .as_array_mut()
            .unwrap()
            .push(v::record(app, "original-paused", paused)?);
        source_faults::arm(scenario)?;
        let close = tauri::async_runtime::block_on(super::native_source_control(
            app.get_webview_window("main").ok_or("Missing main")?,
            super::SourceControlRequest {
                token: token.clone(),
                action: "close".into(),
                position_ns: None,
            },
        ));
        need(close.is_err(), "Injected close did not reject")?;
        report["firstCloseError"] = json!(close.err());
        let blocked = wait_step(app, "original-blocked", blocked_player)?;
        report["steps"].as_array_mut().unwrap().push(blocked);
        v::select(app, "native-source-b.mp4")?;
        let blocked = wait_step(app, "replacement-blocked", |s| {
            blocked_player(s) && s["dom"]["sourceMediaId"] == b
        })?;
        report["steps"].as_array_mut().unwrap().push(blocked);
    } else {
        let began = Instant::now();
        loop {
            let runtime = source_runtime::inspect();
            if let Some(p) = prep(&runtime, &a) {
                let events = p["events"].as_array();
                let live = events.is_some_and(|es| {
                    es.iter().any(|e| {
                        e["event"] == "probe-start"
                            && !es.iter().any(|end| {
                                end["event"] == "probe-end"
                                    && end["process"]["pid"] == e["process"]["pid"]
                            })
                    })
                });
                if p["status"] == "preparing" && live {
                    report["preparationAtCancel"] = runtime;
                    break;
                }
            }
            need(
                began.elapsed() < Duration::from_secs(10),
                "No actual running probe observed",
            )?;
            thread::sleep(Duration::from_millis(1));
        }
        if scenario == "prepare-cancel" {
            validation::eval(
                app,
                "Array.from(document.querySelectorAll('.preview-toolbar button')).find(b=>b.textContent==='时间线').click()",
            )?;
        } else {
            v::select(app, "native-source-b.mp4")?;
        }
        let settled = v::wait(|s| {
            prep(s, &a).is_some_and(|p| p["status"] == "cancelled")
                && s["lifecycle"]["opening"] == 0
        })?;
        need(
            prep(&settled, &a).is_some_and(cancelled_probe),
            "Cancellation did not stop and join an actual running probe",
        )?;
        report["cancelledPreparation"] = prep(&settled, &a).unwrap().clone();
        if scenario == "prepare-cancel" {
            let cancelled = wait_step(app, "cancelled-to-timeline", |s| {
                s["runtime"]["active"].is_null()
                    && s["dom"]["mode"] == "timeline"
                    && s["dom"]["mediaNodeCount"] == 0
            })?;
            report["steps"].as_array_mut().unwrap().push(cancelled);
            v::select(app, "native-source-b.mp4")?;
        }
        let ready = v::wait(|s| s["active"]["mediaId"] == b && s["active"]["phase"] == "ready")?;
        report["steps"]
            .as_array_mut()
            .unwrap()
            .push(v::record(app, "recovered-ready", ready)?);
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        let playing = v::wait(|s| {
            s["active"]["mediaId"] == b
                && s["active"]["phase"] == "playing"
                && s["active"]["positionNs"]
                    .as_str()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= 300_000_000)
        })?;
        report["steps"]
            .as_array_mut()
            .unwrap()
            .push(v::record(app, "recovered-playing", playing)?);
    }
    report["nativeSourceCalls"] = v::wait_trace_settled()?;
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    report["faultEvidence"] = source_faults::snapshot();
    Ok(())
}

pub(super) fn validate(report: &Value) -> Result<(), String> {
    let state = &report["runtimeFinal"];
    need(
        state["active"].is_null()
            && state["busy"] != true
            && state["lifecycle"]["opening"] == 0
            && state["lifecycle"]["exiting"] == true,
        "Final runtime not retired",
    )?;
    need(
        report["nativeSourceCallsFinal"]["error"].is_null(),
        "Trace overflow",
    )?;
    let steps = report["steps"].as_array().ok_or("Missing steps")?;
    if report["scenario"] == "cleanup-failure" {
        need(
            steps
                .iter()
                .any(|s| s["label"] == "replacement-blocked" && blocked_player(s)),
            "Replacement was not blocked",
        )?;
        let fault = &report["faultEvidence"]["cleanup"];
        need(
            fault["injected"] == true
                && fault["injectedAfterActualUnbind"] == true
                && fault["actualError"].is_null()
                && fault["actualUnbind"]["mediaSessionActive"] == false,
            "Missing actual unbind and injected confirmation error",
        )?;
        need(
            state["closeFailure"]["token"] == report["sourceAToken"],
            "Sticky close failure lost",
        )?;
    } else {
        need(
            state["closeFailure"].is_null() && cancelled_probe(&report["cancelledPreparation"]),
            "Cancellation not fully retired",
        )?;
        need(
            steps.iter().any(|s| {
                s["label"] == "recovered-playing"
                    && s["runtime"]["active"]["phase"] == "playing"
                    && s["dom"]["mediaNodeCount"] == 0
            }),
            "Native recovery missing",
        )?;
    }
    Ok(())
}
