use super::{source_runtime, validation};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
static REPORT: Mutex<Option<(PathBuf, Value)>> = Mutex::new(None);
static TRACE: Mutex<Option<CommandTrace>> = Mutex::new(None);

struct CommandTrace {
    began: Instant,
    next: u64,
    pending_open: usize,
    pending_control: usize,
    max_pending_open: usize,
    overflowed: bool,
    calls: Vec<Value>,
}
// A single owned ticket retires a counter even if the bounded display history
// has evicted its row. Moving it into finish prevents duplicate retirement.
struct CommandTicket {
    sequence: u64,
    open: bool,
}
impl CommandTrace {
    fn new() -> Self {
        Self {
            began: Instant::now(),
            next: 0,
            pending_open: 0,
            pending_control: 0,
            max_pending_open: 0,
            overflowed: false,
            calls: Vec::new(),
        }
    }
    fn start(&mut self, command: &str, request: Value) -> CommandTicket {
        self.next += 1;
        if command == "native_source_open" {
            self.pending_open += 1;
        } else {
            self.pending_control += 1;
        }
        self.max_pending_open = self.max_pending_open.max(self.pending_open);
        if self.calls.len() == 128 {
            self.overflowed = true;
            self.calls.remove(0);
        }
        self.calls.push(json!({"sequence":self.next,"command":command,"request":request,
            "startedMs":self.began.elapsed().as_secs_f64()*1000.0,"settledMs":null,"pendingOpenAtStart":self.pending_open}));
        CommandTicket {
            sequence: self.next,
            open: command == "native_source_open",
        }
    }
    fn finish(&mut self, ticket: CommandTicket, result: Option<&Result<Value, String>>) {
        if ticket.open {
            self.pending_open -= 1;
        } else {
            self.pending_control -= 1;
        }
        let Some(call) = self
            .calls
            .iter_mut()
            .find(|call| call["sequence"] == ticket.sequence && call["settledMs"].is_null())
        else {
            return;
        };
        call["settledMs"] = (self.began.elapsed().as_secs_f64() * 1000.0).into();
        match result {
            Some(Ok(value)) => {
                call["result"] = json!({"supported":value["supported"],"mediaId":value["mediaId"],"token":value["token"],"phase":value["phase"],"joined":value["joined"],"reason":value["reason"],"cancelled":value["cancelled"],"pending":value["pending"]})
            }
            Some(Err(error)) => call["error"] = error.chars().take(512).collect::<String>().into(),
            None => call["aborted"] = true.into(),
        }
    }
    fn snapshot(&self) -> Value {
        json!({"source":"native-command-entry","calls":self.calls,"next":self.next,"pendingOpen":self.pending_open,"pendingControl":self.pending_control,"maxPendingOpen":self.max_pending_open,"error":self.overflowed.then_some("Source command evidence exceeded its 128-call history bound")})
    }
}

/// Enabled only after the hidden/debug validation gates in `start` succeeded.
pub(super) struct CommandObservation(Option<CommandTicket>);
pub(super) fn observe(command: &str, request: Value) -> CommandObservation {
    CommandObservation(
        TRACE
            .lock()
            .ok()
            .and_then(|mut trace| trace.as_mut().map(|t| t.start(command, request))),
    )
}
impl CommandObservation {
    pub(super) fn finish(mut self, result: &Result<Value, String>) {
        if let Some(sequence) = self.0.take() {
            if let Ok(mut trace) = TRACE.lock() {
                if let Some(trace) = trace.as_mut() {
                    trace.finish(sequence, Some(result));
                }
            }
        }
    }
}
impl Drop for CommandObservation {
    fn drop(&mut self) {
        if let Some(sequence) = self.0.take() {
            if let Ok(mut trace) = TRACE.lock() {
                if let Some(trace) = trace.as_mut() {
                    trace.finish(sequence, None);
                }
            }
        }
    }
}
pub(super) fn trace_snapshot() -> Value {
    TRACE
        .lock()
        .ok()
        .and_then(|trace| trace.as_ref().map(CommandTrace::snapshot))
        .unwrap_or(Value::Null)
}
pub(super) fn wait_trace_settled() -> Result<Value, String> {
    let began = Instant::now();
    loop {
        let trace = trace_snapshot();
        need(
            trace["error"].is_null(),
            "Source command observation history overflowed",
        )?;
        if trace["pendingOpen"] == 0 && trace["pendingControl"] == 0 {
            return Ok(trace);
        }
        need(
            began.elapsed() < Duration::from_secs(30),
            "Native source commands did not settle",
        )?;
        thread::sleep(Duration::from_millis(4));
    }
}

fn validate_stress_cleanup(report: &Value) -> Result<(), String> {
    need(
        report["nativeSourceCallsFinal"]["error"].is_null(),
        "Source command observation history overflowed",
    )?;
    let runtime = &report["runtimeFinal"];
    need(
        runtime["busy"] != true && runtime["active"].is_null(),
        "Source owner remained at exit",
    )?;
    need(
        runtime["lifecycle"]["opening"] == 0,
        "Source preparation remained at exit",
    )?;
    need(
        runtime["lifecycle"]["exiting"] == true,
        "Application exit gate was not exercised",
    )?;
    let cleanup = &report["windowCleanup"];
    need(cleanup["error"].is_null(), "Window cleanup failed")?;
    need(
        cleanup["noChildOwned"] == true
            || (cleanup["result"]["closed"] == true && cleanup["result"]["childExists"] == false),
        "Native child remained at exit",
    )?;
    let cleanups = runtime["cleanups"]
        .as_array()
        .ok_or("Missing actor cleanup evidence")?;
    need(
        report["scenario"] == "close-during-prepare" || !cleanups.is_empty(),
        "No source actor cleanup was recorded",
    )?;
    for entry in cleanups {
        let actor = &entry["actor"];
        let decoder = &actor["adapterEvidence"]["transferClose"]["decoder"];
        need(
            entry["joinError"].is_null()
                && actor["ok"] == true
                && actor["error"].is_null()
                && actor["cleanupErrors"].as_array().is_some_and(Vec::is_empty),
            "Source actor failed to join cleanly",
        )?;
        need(
            decoder["joined"] == true
                && decoder["processesReaped"] == true
                && decoder["error"].is_null(),
            "Source decoder was not joined and reaped",
        )?;
        need(
            entry["sink"]["closed"] == true
                && entry["sink"]["pending"] == 0
                && entry["sink"]["error"].is_null(),
            "Source sink was not retired",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_import_waits_for_every_media_card_to_reach_the_ui() {
        let required = vec![
            "native-source-a.mp4".to_owned(),
            "native-source-b.mp4".to_owned(),
            "native-source-silent.mp4".to_owned(),
        ];
        assert!(imported_media_visible(
            &json!({"mediaCardNames":["native-source-a.mp4，8.1 MB","native-source-b.mp4，8.1 MB","native-source-silent.mp4，6.9 MB"]}),
            &required,
        ));
        assert!(!imported_media_visible(
            &json!({"mediaCardNames":[]}),
            &required,
        ));
        assert!(!imported_media_visible(
            &json!({"mediaCardNames":["native-source-a.mp4","native-source-b.mp4"]}),
            &required,
        ));
    }

    fn finished() -> Value {
        json!({
            "scenario":"exit-during-play",
            "runtimeFinal": {
                "active":null,"lifecycle":{"opening":0,"exiting":true},
                "cleanups":[{"joinError":null,"actor":{"ok":true,"error":null,
                    "cleanupErrors":[],"adapterEvidence":{"transferClose":{"decoder":{
                        "joined":true,"processesReaped":true,"error":null
                    }}}},"sink":{"closed":true,"pending":0,"error":null}}]
            },
            "windowCleanup":{"error":null,"result":{"closed":true,"childExists":false}}
        })
    }

    #[test]
    fn stress_report_rejects_exit_before_opening_finishes() {
        let mut report = finished();
        report["runtimeFinal"]["lifecycle"]["opening"] = 1.into();
        assert!(validate_stress_cleanup(&report).is_err());
    }

    #[test]
    fn stress_report_rejects_busy_or_retained_owner() {
        let mut report = finished();
        report["runtimeFinal"]["busy"] = true.into();
        assert!(validate_stress_cleanup(&report).is_err());
        report["runtimeFinal"]["busy"] = false.into();
        report["runtimeFinal"]["active"] = json!({"token":"retained"});
        assert!(validate_stress_cleanup(&report).is_err());
    }

    #[test]
    fn stress_report_requires_real_exit_and_join_evidence() {
        let mut report = finished();
        report["runtimeFinal"]["lifecycle"]["exiting"] = false.into();
        assert!(validate_stress_cleanup(&report).is_err());
        report["runtimeFinal"]["lifecycle"]["exiting"] = true.into();
        report["runtimeFinal"]["cleanups"][0]["actor"]["adapterEvidence"]["transferClose"]["decoder"]
            ["joined"] = false.into();
        assert!(validate_stress_cleanup(&report).is_err());
    }

    #[test]
    fn stress_report_accepts_joined_playback_and_cancelled_preparation() {
        assert!(validate_stress_cleanup(&finished()).is_ok());
        let mut preparation = finished();
        preparation["scenario"] = "close-during-prepare".into();
        preparation["runtimeFinal"]["cleanups"] = json!([]);
        preparation["windowCleanup"] = json!({"noChildOwned":true});
        assert!(validate_stress_cleanup(&preparation).is_ok());
    }

    #[test]
    fn command_trace_observes_overlapping_requests_and_actual_tokens() {
        let mut trace = CommandTrace::new();
        let a = trace.start(
            "native_source_open",
            json!({"requestId":"request-a","mediaId":"a"}),
        );
        let b = trace.start(
            "native_source_open",
            json!({"requestId":"request-b","mediaId":"b"}),
        );
        assert_eq!(trace.snapshot()["pendingOpen"], 2);
        trace.finish(
            a,
            Some(&Ok(json!({"supported":false,"reason":"selection changed"}))),
        );
        trace.finish(
            b,
            Some(&Ok(
                json!({"supported":true,"mediaId":"b","token":"owner-b","phase":"ready"}),
            )),
        );
        let close = trace.start(
            "native_source_control",
            json!({"token":"owner-b","action":"close"}),
        );
        trace.finish(
            close,
            Some(&Ok(
                json!({"token":"owner-b","phase":"closed","joined":true}),
            )),
        );
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["pendingOpen"], 0);
        assert_eq!(snapshot["pendingControl"], 0);
        assert_eq!(snapshot["maxPendingOpen"], 2);
        assert_eq!(snapshot["calls"][1]["result"]["token"], "owner-b");
        assert_eq!(snapshot["calls"][2]["request"]["token"], "owner-b");
        assert!(
            snapshot["calls"]
                .as_array()
                .unwrap()
                .iter()
                .all(|call| call["settledMs"].as_f64().unwrap()
                    >= call["startedMs"].as_f64().unwrap())
        );
    }

    #[test]
    fn command_trace_bounds_history_and_preserves_aborted_observation() {
        let mut trace = CommandTrace::new();
        for _ in 0..140 {
            let id = trace.start("native_source_open", Value::Null);
            trace.finish(id, None);
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["calls"].as_array().unwrap().len(), 128);
        assert_eq!(snapshot["pendingOpen"], 0);
        assert_eq!(snapshot["calls"][127]["aborted"], true);
    }

    #[test]
    fn command_trace_retires_inflight_request_after_its_row_is_evicted() {
        let mut trace = CommandTrace::new();
        let tickets = (0..129)
            .map(|_| trace.start("native_source_open", Value::Null))
            .collect::<Vec<_>>();
        assert_eq!(trace.snapshot()["pendingOpen"], 129);
        for ticket in tickets {
            trace.finish(ticket, None);
        }
        assert_eq!(trace.snapshot()["pendingOpen"], 0);
        assert!(
            trace.snapshot()["error"]
                .as_str()
                .is_some_and(|error| error.contains("128"))
        );
    }
}
pub(super) fn record_closed(cleanup: Value) {
    if let Ok(mut pending) = REPORT.lock() {
        if let Some((path, mut r)) = pending.take() {
            r["windowCleanup"] = cleanup;
            r["runtimeFinal"] = source_runtime::inspect();
            r["nativeSourceCallsFinal"] = trace_snapshot();
            if super::source_span_validation::is_scenario(r["scenario"].as_str().unwrap_or("")) {
                if let Err(error) = super::source_span_validation::validate(&mut r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_long_span_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                if let Err(error) = super::source_long_span_validation::validate(&mut r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_audio_stress_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                if let Err(error) = super::source_audio_stress_validation::validate(&mut r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_audio_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                r["positiveControl"] = super::source_audio_validation::snapshot();
                if let Err(error) = super::source_audio_validation::validate(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_unbind_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                r["faultEvidence"] = super::source_faults::snapshot();
                if let Err(error) = super::source_unbind_validation::validate(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_inflight_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                r["inflightSchedule"] = super::source_inflight_hold::snapshot();
                if let Err(error) = super::source_inflight_validation::validate(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_stage_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                r["stageSchedule"] = super::source_stage_validation::snapshot();
                if let Err(error) = super::source_stage_validation::validate(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_shutdown_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                r["faultEvidence"] = super::source_faults::snapshot();
                if let Err(error) = super::source_shutdown_validation::validate(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if super::source_fault_validation::is_scenario(
                r["scenario"].as_str().unwrap_or(""),
            ) {
                r["faultEvidence"] = super::source_faults::snapshot();
                if let Err(error) = super::source_fault_validation::validate_cleanup(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
            } else if r["scenario"] != "controls" {
                if let Err(error) = validate_stress_cleanup(&r) {
                    r["ok"] = false.into();
                    r["cleanupValidationError"] = error.into();
                }
                if r["scenario"] == "close-during-prepare"
                    && !r["runtimeFinal"]["lifecycle"]["exitObservation"]["opening"]
                        .as_u64()
                        .is_some_and(|n| n > 0)
                {
                    r["ok"] = false.into();
                    r["exitValidationError"] =
                        "Exit did not observe an in-flight preparation".into();
                }
            }
            if let Err(error) = validation::save(&path, &r) {
                eprintln!("Source validation save failed {error}; {r}");
            }
        }
    }
}
pub(super) fn need(value: bool, message: &str) -> Result<(), String> {
    if value { Ok(()) } else { Err(message.into()) }
}
pub(super) fn wait<F>(condition: F) -> Result<Value, String>
where
    F: Fn(&Value) -> bool,
{
    let began = Instant::now();
    loop {
        let s = source_runtime::inspect();
        if condition(&s) {
            return Ok(s);
        }
        need(
            began.elapsed() < Duration::from_secs(45),
            &format!("Source UI wait timed out: {s}"),
        )?;
        thread::sleep(Duration::from_millis(40));
    }
}
pub(super) fn record(app: &AppHandle, label: &str, state: Value) -> Result<Value, String> {
    let started = Instant::now();
    loop {
        let dom = validation::dom(app)?;
        let active = &state["active"];
        let matched = if active.is_null() {
            dom["sourceEngine"] != "native-source"
        } else {
            dom["sourceEngine"] == "native-source"
                && dom["sourcePhase"] == active["phase"]
                && dom["sourceMediaId"] == active["mediaId"]
        };
        if matched {
            return Ok(
                json!({"label":label,"runtime":state,"dom":dom,"host":validation::status(app)?}),
            );
        }
        need(
            started.elapsed() < Duration::from_secs(5),
            &format!("Source UI did not acknowledge state: {dom}"),
        )?;
        thread::sleep(Duration::from_millis(30));
    }
}
pub(super) fn seek(app: &AppHandle, seconds: f64) -> Result<(), String> {
    validation::eval(
        app,
        r#"(()=>{const e=document.querySelector('input[aria-label="素材定位（秒）"]');e.dispatchEvent(new FocusEvent('focusin',{bubbles:true}))})()"#,
    )?;
    thread::sleep(Duration::from_millis(60));
    validation::eval(
        app,
        &format!(
            r#"(()=>{{const e=document.querySelector('input[aria-label="素材定位（秒）"]');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,'{seconds:.9}');e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));}})()"#
        ),
    )?;
    thread::sleep(Duration::from_millis(60));
    validation::eval(
        app,
        r#"document.querySelector('input[aria-label="素材定位（秒）"]').dispatchEvent(new FocusEvent('focusout',{bubbles:true}))"#,
    )
}
pub(super) fn select(app: &AppHandle, name: &str) -> Result<(), String> {
    let quoted = serde_json::to_string(name).map_err(|e| e.to_string())?;
    validation::eval(
        app,
        &format!(
            "Array.from(document.querySelectorAll('.media-card')).find(e=>e.textContent.includes({quoted})).click()"
        ),
    )
}

fn imported_media_visible(dom: &Value, required_names: &[String]) -> bool {
    let Some(cards) = dom["mediaCardNames"].as_array() else {
        return false;
    };
    cards.len() == required_names.len()
        && required_names.iter().all(|name| {
            let expected_prefix = format!("{name}，");
            cards.iter().any(|card| {
                card.as_str()
                    .is_some_and(|label| label.starts_with(&expected_prefix))
            })
        })
}

pub(super) fn import_fixtures(
    app: &AppHandle,
    files: &[String],
    report: &mut Value,
) -> Result<(), String> {
    let session = tauri::async_runtime::block_on(crate::commands::project_get(app.clone()))?;
    let imported = tauri::async_runtime::block_on(crate::mcp_project::import_paths(
        app.clone(),
        crate::mcp_project::ImportPathsArgs {
            project_id: session.project.project_id,
            revision: session.revision,
            paths: files.to_vec(),
        },
    ))?;
    report["projectId"] = imported.project.project_id.clone().into();
    report["revision"] = imported.revision.into();
    report["mediaRefs"] = json!(imported.project.media_refs);
    let required_names = files
        .iter()
        .map(|file| {
            PathBuf::from(file)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or("Fixture path has no valid filename")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let began = Instant::now();
    loop {
        // The window can finish loading after this validation thread starts.
        // Re-publish the authoritative session until the actual UI acknowledges it.
        app.emit_to("main", "kaicut:mcp-session", &imported)
            .map_err(|e| e.to_string())?;
        let ui_state = match validation::dom(app) {
            Ok(dom) => {
                if imported_media_visible(&dom, &required_names) {
                    return Ok(());
                }
                dom.to_string()
            }
            Err(error) => error,
        };
        need(
            began.elapsed() < Duration::from_secs(15),
            &format!("Imported media did not reach the UI: {ui_state}"),
        )?;
        thread::sleep(Duration::from_millis(200));
    }
}
fn run(app: &AppHandle, files: &[String], report: &mut Value) -> Result<(), String> {
    import_fixtures(app, files, report)?;
    select(app, "native-source-a.mp4")?;
    let ready = wait(|s| s["active"]["phase"] == "ready")?;
    let token = ready["active"]["token"]
        .as_str()
        .ok_or("Missing source token")?
        .to_owned();
    let duration = ready["active"]["durationNs"]
        .as_str()
        .ok_or("Missing duration")?
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    report["steps"] = json!([record(app, "selected-ready", ready)?]);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let playing = wait(|s| {
        s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n > 2_200_000_000)
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "playing-over-two-seconds", playing)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let paused = wait(|s| s["active"]["phase"] == "paused")?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "paused", paused)?);
    seek(app, duration.saturating_sub(200_000_000) as f64 / 1e9)?;
    let sought = wait(|s| {
        s["active"]["phase"] == "ready"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n >= duration.saturating_sub(201_000_000))
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "near-end-seek", sought)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let ended = wait(|s| s["active"]["phase"] == "ended")?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "natural-end", ended)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let replay = wait(|s| {
        s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n > 50_000_000 && n < 1_000_000_000)
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "replay", replay)?);
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    wait(|s| s["active"]["phase"] == "paused")?;
    seek(app, duration as f64 / 1e9)?;
    let exact_end = wait(|s| {
        s["active"]["phase"] == "ended" && s["active"]["positionNs"] == duration.to_string()
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "exact-end-seek", exact_end)?);
    select(app, "native-source-b.mp4")?;
    let other = wait(|s| s["active"]["phase"] == "ready" && s["active"]["token"] != token)?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "changed-source", other)?);
    let old = tauri::async_runtime::block_on(super::native_source_control(
        app.get_webview_window("main").ok_or("Missing main")?,
        super::SourceControlRequest {
            token: token.clone(),
            action: "play".into(),
            position_ns: None,
        },
    ));
    need(old.is_err(), "Old source token was accepted")?;
    report["oldTokenError"] = json!(old.err());
    let before = source_runtime::inspect();
    let closed = tauri::async_runtime::block_on(super::native_source_control(
        app.get_webview_window("main").ok_or("Missing main")?,
        super::SourceControlRequest {
            token,
            action: "close".into(),
            position_ns: None,
        },
    ))?;
    let after = source_runtime::inspect();
    need(
        before["active"]["token"] == after["active"]["token"]
            && after["active"]["phase"] == "ready",
        "Old close replaced current source",
    )?;
    report["oldClose"] = json!({"result":closed,"beforeToken":before["active"]["token"],"afterToken":after["active"]["token"]});
    select(app, "native-source-silent.mp4")?;
    wait(|s| s["active"].is_null() && s["cleanups"].as_array().is_some_and(|c| c.len() >= 2))?;
    thread::sleep(Duration::from_millis(700));
    report["steps"].as_array_mut().unwrap().push(record(
        app,
        "unsupported-fallback",
        source_runtime::inspect(),
    )?);

    Ok(())
}
use tauri::Manager;

fn run_stress(
    app: &AppHandle,
    files: &[String],
    scenario: &str,
    report: &mut Value,
) -> Result<(), String> {
    import_fixtures(app, files, report)?;

    let host = validation::status(app)?;
    need(
        host["parent"]["visible"] == false && host["parent"]["focused"] == false,
        "Stress instance became visible or focused",
    )?;
    report["hostBeforeScenario"] = host;
    report["domBeforeScenario"] = validation::dom(app)?;
    report["steps"] = json!([]);
    if scenario == "close-during-prepare" {
        select(app, "native-source-a.mp4")?;
        let began = Instant::now();
        loop {
            let runtime = source_runtime::inspect();
            if runtime["lifecycle"]["opening"]
                .as_u64()
                .is_some_and(|n| n > 0)
            {
                report["runtimeBeforeFinalCleanup"] = runtime;
                report["exitReason"] = "Observed real in-flight source preparation; requesting application exit without source close".into();
                // No DOM round-trip after observing the real open window.
                return Ok(());
            }
            need(
                began.elapsed() < Duration::from_secs(10),
                "Did not observe an in-flight source preparation",
            )?;
            thread::sleep(Duration::from_millis(4));
        }
    }
    let first = if scenario == "rapid-switch" {
        "native-source-b.mp4"
    } else {
        "native-source-a.mp4"
    };
    select(app, first)?;
    let ready = wait(|s| s["active"]["phase"] == "ready" && s["lifecycle"]["opening"] == 0)?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "initial-ready", ready)?);
    if scenario == "rapid-switch" {
        let before = trace_snapshot()["next"].as_u64().unwrap_or(0);
        for (index, name) in [
            "native-source-a.mp4",
            "native-source-b.mp4",
            "native-source-a.mp4",
        ]
        .iter()
        .enumerate()
        {
            select(app, name)?;
            let began = Instant::now();
            loop {
                let trace = trace_snapshot();
                let count = trace["calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|call| {
                        call["command"] == "native_source_open"
                            && call["sequence"].as_u64().is_some_and(|n| n > before)
                    })
                    .count();
                if count > index {
                    break;
                }
                need(
                    began.elapsed() < Duration::from_secs(10),
                    "Selection did not issue the real source open",
                )?;
                thread::sleep(Duration::from_millis(4));
            }
        }
        let trace = wait_trace_settled()?;
        need(
            trace["maxPendingOpen"].as_u64().is_some_and(|n| n >= 2),
            "Rapid selection did not overlap real open requests",
        )?;
        let media_id = report["mediaRefs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "native-source-a.mp4")
            .and_then(|m| m["id"].as_str())
            .ok_or("Missing imported A identity")?
            .to_owned();
        let final_source = wait(|s| {
            s["active"]["mediaId"] == media_id
                && s["active"]["phase"] == "ready"
                && s["lifecycle"]["opening"] == 0
        })?;
        let step = record(app, "rapid-final-ready", final_source)?;
        need(
            step["dom"]["selectedMediaLabel"]
                .as_str()
                .is_some_and(|s| s.starts_with("native-source-a.mp4，")),
            "Final selected media card did not match A",
        )?;
        need(
            step["dom"]["mediaNodeCount"] == 0
                && step["host"]["mediaSessionActive"] == true
                && step["host"]["child"].is_object(),
            "Native final owner lacked its single child or had duplicate WebView media",
        )?;
        report["steps"].as_array_mut().unwrap().push(step);
    }
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let playing = wait(|s| {
        s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n > 250_000_000)
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(record(app, "playing-before-exit", playing)?);
    report["nativeSourceCalls"] = wait_trace_settled()?;
    report["finalDom"] = validation::dom(app)?;
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    report["exitReason"] = "Requesting application exit while source playback owns its actor; no explicit source close".into();
    Ok(())
}

pub(super) fn start(app: &AppHandle) -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().collect();
    let Some(i) = args.iter().position(|v| v == "--native-source-check-dir") else {
        return Ok(());
    };
    need(
        cfg!(debug_assertions)
            && [
                "--mcp",
                "--mcp-hidden",
                "--mcp-validation",
                "--native-preview-host",
                "--native-source-preview",
            ]
            .iter()
            .all(|flag| args.iter().any(|v| v == flag)),
        "Source check requires isolated hidden debug instance",
    )?;
    let scenario = args
        .iter()
        .position(|v| v == "--native-source-scenario")
        .map(|i| {
            args.get(i + 1)
                .and_then(|v| v.to_str())
                .ok_or("Missing source scenario")
        })
        .transpose()?
        .unwrap_or("controls")
        .to_owned();
    need(
        [
            "controls",
            "rapid-switch",
            "close-during-prepare",
            "exit-during-play",
        ]
        .contains(&scenario.as_str())
            || super::source_fault_validation::is_scenario(&scenario)
            || super::source_shutdown_validation::is_scenario(&scenario)
            || super::source_stage_validation::is_scenario(&scenario)
            || super::source_inflight_validation::is_scenario(&scenario)
            || super::source_unbind_validation::is_scenario(&scenario)
            || super::source_audio_validation::is_scenario(&scenario)
            || super::source_audio_stress_validation::is_scenario(&scenario)
            || super::source_span_validation::is_scenario(&scenario)
            || super::source_long_span_validation::is_scenario(&scenario),
        "Unknown source validation scenario",
    )?;
    let output = PathBuf::from(args.get(i + 1).ok_or("Missing output")?);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let target = root.canonicalize().map_err(|e| e.to_string())?;
    need(
        output.is_absolute()
            && output.starts_with(&root)
            && !output
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Source evidence must remain in target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    need(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Source evidence escaped through link",
    )?;
    let fi = args
        .iter()
        .position(|v| v == "--native-source-fixture-dir")
        .ok_or("Missing source fixture")?;
    let fixture = PathBuf::from(args.get(fi + 1).ok_or("Missing fixture value")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    need(
        fixture.starts_with(&target),
        "Validation fixture must stay inside checkout target",
    )?;
    let files = [
        "native-source-a.mp4",
        "native-source-b.mp4",
        "native-source-silent.mp4",
    ]
    .iter()
    .map(|f| fixture.join(f).to_string_lossy().into_owned())
    .collect::<Vec<_>>();
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let run_dir = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run_dir).map_err(|e| e.to_string())?;
    if scenario != "controls" {
        *TRACE
            .lock()
            .map_err(|_| "Source command observation unavailable")? = Some(CommandTrace::new());
    }
    let app = app.clone();
    thread::spawn(move || {
        let mut r = json!({"schemaVersion":1,"scenario":scenario,"processId":std::process::id(),"createdAt":chrono::Utc::now().to_rfc3339(),"scope":{"actualSourceSelection":true,"actualDomControls":true,"hidden":true,"muted":true,"desktopInput":false,"screenDisplayed":false},"ok":false});
        let result = if scenario == "controls" {
            run(&app, &files, &mut r)
        } else if super::source_span_validation::is_scenario(&scenario) {
            super::source_span_validation::run(&app, &files, &mut r)
        } else if super::source_long_span_validation::is_scenario(&scenario) {
            super::source_long_span_validation::run(&app, &files, &mut r)
        } else if super::source_audio_stress_validation::is_scenario(&scenario) {
            super::source_audio_stress_validation::run(&app, &files, &mut r)
        } else if super::source_audio_validation::is_scenario(&scenario) {
            super::source_audio_validation::run(&app, &files, &mut r)
        } else if super::source_unbind_validation::is_scenario(&scenario) {
            super::source_unbind_validation::run(&app, &files, &mut r)
        } else if super::source_inflight_validation::is_scenario(&scenario) {
            super::source_inflight_validation::run(&app, &files, &scenario, &mut r)
        } else if super::source_stage_validation::is_scenario(&scenario) {
            super::source_stage_validation::run(&app, &files, &scenario, &mut r)
        } else if super::source_shutdown_validation::is_scenario(&scenario) {
            super::source_shutdown_validation::run(&app, &files, &scenario, &mut r)
        } else if super::source_fault_validation::is_scenario(&scenario) {
            super::source_fault_validation::run_fault(&app, &files, &scenario, &mut r)
        } else {
            run_stress(&app, &files, &scenario, &mut r)
        };
        r["ok"] = result.is_ok().into();
        if let Err(error) = result {
            r["error"] = error.into();
        }
        if scenario != "controls" {
            // Store the evidence before ExitRequested. The normal lifecycle
            // path must drain/retire workers; platform::close saves it on Exit.
            *REPORT.lock().unwrap() = Some((run_dir.join("native-source-validation.json"), r));
            app.exit(0);
            return;
        }
        r["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
        if let Some(token) = r["runtimeBeforeFinalCleanup"]["active"]["token"].as_str() {
            if let Some(window) = app.get_webview_window("main") {
                let closed = tauri::async_runtime::block_on(super::native_source_control(
                    window,
                    super::SourceControlRequest {
                        token: token.to_owned(),
                        action: "close".into(),
                        position_ns: None,
                    },
                ));
                if let Err(error) = &closed {
                    r["ok"] = false.into();
                    r["finalCleanupError"] = error.clone().into();
                }
            }
        }
        r["runtimeFinal"] = source_runtime::inspect();
        r["finalDom"] = validation::dom(&app).unwrap_or_else(|error| json!({"error":error}));
        *REPORT.lock().unwrap() = Some((run_dir.join("native-source-validation.json"), r));
        let _ = validation::main_call(&app, |w| {
            w.destroy().map_err(|e| e.to_string())?;
            Ok(Value::Null)
        });
        record_closed(
            json!({"note":"Parent closed after source cleanup; no native child remained"}),
        );
    });
    Ok(())
}
