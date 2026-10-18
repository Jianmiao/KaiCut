//! Controlled concurrency checks for an entered native submission callback.
use super::{source_inflight_hold as hold, source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

fn retired_while_held(scenario: &str, held: &Value, retired: &Value, pending: bool) -> bool {
    if !pending || held["inFlight"] != true || retired["inFlight"] != true {
        return false;
    }
    let Some(epoch) = held["epoch"].as_u64() else {
        return false;
    };
    match scenario {
        "inflight-cancel" | "prepresent-cancel" => retired["closed"] == true,
        "inflight-seek" | "prepresent-seek" => {
            retired["closed"] == false && retired["epoch"].as_u64().is_some_and(|next| next > epoch)
        }
        _ => false,
    }
}

pub(super) fn is_scenario(scenario: &str) -> bool {
    matches!(
        scenario,
        "inflight-cancel" | "inflight-seek" | "prepresent-cancel" | "prepresent-seek"
    )
}

// Disarm an unclaimed schedule or release a claimed callback on error. Completed
// schedules are retained unchanged; this hook only exists in isolated validation.
struct ReleaseOnDrop;
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        hold::abandon();
    }
}

pub(super) fn run(
    app: &AppHandle,
    files: &[String],
    scenario: &str,
    report: &mut Value,
) -> Result<(), String> {
    let cancel = matches!(scenario, "inflight-cancel" | "prepresent-cancel");
    v::import_fixtures(app, files, report)?;
    let media = |name: &str| {
        report["mediaRefs"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["name"] == name))
            .and_then(|row| row["id"].as_str())
            .map(str::to_owned)
            .ok_or("Missing fixture identity".to_owned())
    };
    let a = media("native-source-a.mp4")?;
    let b = media("native-source-b.mp4")?;
    report["sourceAId"] = a.clone().into();
    report["sourceBId"] = b.clone().into();
    report["scope"]["actualDomControls"] = false.into();
    report["scope"]["initialAndRecoveryDomControls"] = true.into();
    report["controlOrigin"] = "native-command-wrapper-background".into();
    report["steps"] = json!([]);
    v::select(app, "native-source-a.mp4")?;
    let ready = v::wait(|s| s["active"]["phase"] == "ready" && s["active"]["mediaId"] == a)?;
    let token = ready["active"]["token"]
        .as_str()
        .ok_or("Missing source token")?
        .to_owned();
    report["sourceAToken"] = token.clone().into();
    let first = v::record(app, "initial-ready", ready)?;
    v::need(
        first["host"]["parent"]["visible"] == false && first["host"]["parent"]["focused"] == false,
        "Window was visible/focused",
    )?;
    let session = first["host"]["sessionId"]
        .as_str()
        .ok_or("Missing host session")?
        .to_owned();
    let surface = first["host"]["child"]["surfaceId"]
        .as_str()
        .ok_or("Missing child surface")?
        .to_owned();
    report["steps"].as_array_mut().unwrap().push(first);
    let window = app
        .get_webview_window("main")
        .ok_or("Missing main window")?;
    hold::arm(scenario, &session, &surface)?;
    let _release = ReleaseOnDrop;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let began = Instant::now();
    let held = loop {
        let h = hold::snapshot();
        if h["holding"] == true {
            break h;
        }
        v::need(
            began.elapsed() < Duration::from_secs(8),
            "No real entered callback observed",
        )?;
        thread::sleep(Duration::from_millis(2));
    };
    report["held"] = held.clone();
    // Do not call DOM/status/main_call while its event thread is held. Exercise
    // the same typed command wrapper in a background task, not pretend UI input.
    let action = if cancel { "close" } else { "seek" };
    let target = "3000000000";
    let (tx, rx) = mpsc::sync_channel(1);
    let request = super::SourceControlRequest {
        token,
        action: action.into(),
        position_ns: (action == "seek").then(|| target.to_owned()),
    };
    report["controlRequest"] =
        json!({"token":request.token,"action":request.action,"positionNs":request.position_ns});
    let worker = thread::spawn(move || {
        let result = tauri::async_runtime::block_on(super::native_source_control(window, request));
        let _ = tx.send(result);
    });
    let began = Instant::now();
    loop {
        let h = hold::snapshot();
        let pending = matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        v::need(
            pending,
            "Control finished before the entered callback was released",
        )?;
        if retired_while_held(scenario, &held["sinkAtHold"], &h["sinkNow"], pending) {
            report["retiring"] = h;
            report["controlPendingWhileHeld"] = true.into();
            report["traceWhileHeld"] = v::trace_snapshot();
            break;
        }
        v::need(
            h["holding"] == true && began.elapsed() < Duration::from_secs(8),
            "Callback was not retired while held",
        )?;
        thread::sleep(Duration::from_millis(2));
    }
    hold::release()?;
    let result = rx
        .recv_timeout(Duration::from_secs(25))
        .map_err(|e| e.to_string())?;
    worker
        .join()
        .map_err(|_| "Concurrent source control worker panicked")?;
    report["controlResult"] = result?;
    let began = Instant::now();
    loop {
        let observed = hold::snapshot();
        if !observed["completion"].is_null() {
            report["afterRelease"] = observed;
            break;
        }
        v::need(
            began.elapsed() < Duration::from_secs(2),
            "Released callback lacks completion evidence",
        )?;
        thread::sleep(Duration::from_millis(2));
    }
    if cancel {
        v::wait(|s| s["active"].is_null() && s["busy"] != true)?;
        validation::eval(
            app,
            "Array.from(document.querySelectorAll('.preview-toolbar button')).find(b=>b.textContent==='时间线').click()",
        )?;
        v::wait(|s| s["active"].is_null() && s["lifecycle"]["opening"] == 0)?;
        v::select(app, "native-source-b.mp4")?;
        let ready = v::wait(|s| s["active"]["phase"] == "ready" && s["active"]["mediaId"] == b)?;
        v::need(
            ready["active"]["positionNs"] == "0",
            "Replacement inherited old time",
        )?;
        report["steps"]
            .as_array_mut()
            .unwrap()
            .push(v::record(app, "recovered-ready", ready)?);
    } else {
        let ready = v::wait(|s| s["active"]["phase"] == "ready" && s["active"]["mediaId"] == a)?;
        v::need(
            ready["active"]["positionNs"] == target,
            "Seek did not prepare exact target",
        )?;
        report["steps"]
            .as_array_mut()
            .unwrap()
            .push(v::record(app, "seek-ready", ready)?);
    }
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let expected = if cancel { b } else { a };
    let minimum = if cancel { 300_000_000 } else { 3_300_000_000 };
    let playing = v::wait(|s| {
        s["active"]["phase"] == "playing"
            && s["active"]["mediaId"] == expected
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|ns| ns >= minimum)
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
    let h = &report["inflightSchedule"];
    let scenario = report["scenario"].as_str().unwrap_or("");
    v::need(
        h["released"] == true && h["timedOut"] == false && h["failed"] == false,
        "Entered callback did not release normally",
    )?;
    v::need(
        retired_while_held(
            scenario,
            &report["held"]["sinkAtHold"],
            &report["retiring"]["sinkNow"],
            report["controlPendingWhileHeld"] == true,
        ),
        "No concurrent retirement evidence",
    )?;
    let expected = if scenario.starts_with("prepresent-") {
        "SkippedBeforePresent"
    } else if scenario == "inflight-cancel" {
        "Closed"
    } else {
        "Stale"
    };
    v::need(
        h["completion"]["kind"] == expected,
        "Retired completion was published",
    )?;
    if scenario.starts_with("prepresent-") {
        let skipped = &h["completion"]["skip"];
        let after = &h["completion"]["sinkAfterSkip"];
        let before = &h["sinkAtHold"];
        let counter = |state: &Value, key: &str| {
            state[key]
                .as_u64()
                .ok_or_else(|| format!("Missing numeric skip evidence: {key}"))
        };
        let present_before = counter(before, "presentCalls")?;
        let present_after = counter(after, "presentCalls")?;
        let completed_before = counter(before, "completed")?;
        let completed_after = counter(after, "completed")?;
        let skipped_before = counter(before, "skippedBeforePresent")?;
        let skipped_after = counter(after, "skippedBeforePresent")?;
        let reason = if scenario == "prepresent-cancel" {
            "closed"
        } else {
            "stale-epoch"
        };
        v::need(
            h["boundary"] == "before-present"
                && h.get("rawPresent") == Some(&Value::Null)
                && skipped["skipReason"] == reason
                && after["inFlight"] == false
                && present_after == present_before
                && completed_after == completed_before
                && skipped_before.checked_add(1) == Some(skipped_after),
            "Obsolete frame was not rejected before platform submission",
        )?;
    }
    let state = &report["runtimeFinal"];
    v::need(
        state["busy"] != true
            && state["active"].is_null()
            && state["closeFailure"].is_null()
            && state["lifecycle"]["exiting"] == true
            && state["lifecycle"]["opening"] == 0
            && state["pendingPreparations"] == 0,
        "Final source resources remain",
    )?;
    let cleanups = state["cleanups"]
        .as_array()
        .ok_or("Missing cleanup ledger")?;
    v::need(
        !cleanups.is_empty() && cleanups.iter().all(|c| c["cleanupConfirmed"] == true),
        "Cleanup not confirmed",
    )?;
    v::need(
        report["windowCleanup"]["error"].is_null(),
        "Window cleanup failed",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entered_callback_requires_retirement_and_an_unfinished_control() {
        let held = json!({"epoch":3,"closed":false,"inFlight":true});
        let closed = json!({"epoch":4,"closed":true,"inFlight":true});
        let sought = json!({"epoch":4,"closed":false,"inFlight":true});
        assert!(retired_while_held("inflight-cancel", &held, &closed, true));
        assert!(retired_while_held("inflight-seek", &held, &sought, true));
        assert!(!retired_while_held("inflight-cancel", &held, &sought, true));
        assert!(!retired_while_held("inflight-seek", &held, &held, true));
        assert!(!retired_while_held("inflight-seek", &held, &sought, false));
        let mut finished = sought.clone();
        finished["inFlight"] = false.into();
        assert!(!retired_while_held("inflight-seek", &held, &finished, true));
        assert!(!retired_while_held("unknown", &held, &closed, true));
    }

    #[test]
    fn pre_submit_retirement_never_accepts_an_unretired_or_finished_control() {
        let held = json!({"epoch":7,"closed":false,"inFlight":true});
        let closed = json!({"epoch":8,"closed":true,"inFlight":true});
        let sought = json!({"epoch":8,"closed":false,"inFlight":true});
        assert!(retired_while_held(
            "prepresent-cancel",
            &held,
            &closed,
            true
        ));
        assert!(retired_while_held("prepresent-seek", &held, &sought, true));
        assert!(!retired_while_held("prepresent-cancel", &held, &held, true));
        assert!(!retired_while_held(
            "prepresent-seek",
            &held,
            &sought,
            false
        ));
    }

    #[test]
    fn pre_submit_validation_rejects_missing_numeric_evidence() {
        let h = json!({"boundary":"before-present","released":true,"timedOut":false,"failed":false,
            "completion":{"kind":"SkippedBeforePresent","skip":{"skipReason":"closed"},
                "sinkAfterSkip":{"inFlight":false}}});
        let mut report = json!({"scenario":"prepresent-cancel","inflightSchedule":h,
            "held":{"sinkAtHold":{"epoch":1,"inFlight":true}},
            "retiring":{"sinkNow":{"epoch":2,"closed":true,"inFlight":true}},
            "controlPendingWhileHeld":true,"runtimeFinal":{"active":null,"closeFailure":null,
                "pendingPreparations":0,"lifecycle":{"opening":0,"exiting":true},
                "cleanups":[{"cleanupConfirmed":true}]},"windowCleanup":{"error":null}});
        assert!(
            validate(&report).is_err(),
            "Absent evidence cannot prove no platform call"
        );
        report["inflightSchedule"]["sinkAtHold"] =
            json!({"presentCalls":4,"completed":4,"skippedBeforePresent":0});
        report["inflightSchedule"]["completion"]["sinkAfterSkip"] =
            json!({"inFlight":false,"presentCalls":4,"completed":4,"skippedBeforePresent":1});
        assert!(
            validate(&report).is_err(),
            "Explicit rawPresent is required"
        );
        report["inflightSchedule"]["rawPresent"] = Value::Null;
        assert!(validate(&report).is_ok());
        for key in ["presentCalls", "completed", "skippedBeforePresent"] {
            let mut broken = report.clone();
            broken["inflightSchedule"]["sinkAtHold"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            broken["inflightSchedule"]["completion"]["sinkAfterSkip"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(validate(&broken).is_err(), "{key}");
        }
    }
}
