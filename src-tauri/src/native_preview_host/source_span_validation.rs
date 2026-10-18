//! Fixed natural source playback and seek-span experiment; no injected delay.
use super::{source_audio_validation, source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use tauri::AppHandle;

fn fixed_span_duration(duration: u64) -> bool {
    duration == 20_010_666_666
}

fn start_epochs_match(starts: &Value, expected: &[u64]) -> bool {
    starts.as_array().is_some_and(|items| {
        items.len() == expected.len()
            && items
                .iter()
                .zip(expected)
                .all(|(item, epoch)| item["sourceEpoch"].as_u64() == Some(*epoch))
    })
}

fn lifecycle_matches(events: &Value, duration: u64) -> bool {
    let Some(events) = events.as_array() else {
        return false;
    };
    let mut actions = Vec::new();
    for event in events {
        let Some(kind) = event["kind"].as_str() else {
            return false;
        };
        if !matches!(
            kind,
            "prepared" | "retire" | "start" | "paused" | "seek" | "ended" | "observed"
        ) {
            return false;
        }
        match kind {
            "start" => {
                let Some(epoch) = event["sourceEpoch"].as_u64() else {
                    return false;
                };
                actions.push(format!("start:{epoch}"));
            }
            "seek" => {
                let Some(target) = event["targetNs"].as_str() else {
                    return false;
                };
                actions.push(format!("seek:{target}"));
            }
            "paused" | "ended" => {
                actions.push(kind.to_owned());
            }
            _ => {}
        }
    }
    actions
        == [
            "start:1".to_owned(),
            "paused".to_owned(),
            "start:1".to_owned(),
            "ended".to_owned(),
            "seek:10000000000".to_owned(),
            "start:2".to_owned(),
            "paused".to_owned(),
            "seek:1000000000".to_owned(),
            "start:3".to_owned(),
            "paused".to_owned(),
            "seek:".to_owned() + &duration.saturating_sub(200_000_000).to_string(),
            "start:4".to_owned(),
            "ended".to_owned(),
        ]
}

fn sought(state: &Value, token: &str, target: u64, epoch: u64) -> bool {
    let a = &state["active"];
    let d = &a["actor"]["audioDiagnostics"];
    a["token"] == token
        && a["phase"] == "ready"
        && a["positionNs"] == target.to_string()
        && a["actor"]["sourceEpoch"] == epoch
        && a["actor"]["state"]["deviceRunning"] == false
        && d["targetNs"] == target.to_string()
        && d["startCount"] == 0
        && d["complete"] == true
}

pub(super) fn is_scenario(s: &str) -> bool {
    s == "audio-span-stress"
}
fn record(app: &AppHandle, report: &mut Value, label: &str, state: Value) -> Result<(), String> {
    report["steps"]
        .as_array_mut()
        .ok_or("Missing steps")?
        .push(v::record(app, label, state)?);
    Ok(())
}
fn play_to(app: &AppHandle, token: &str, epoch: u64, min: u64) -> Result<Value, String> {
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    v::wait(|s| {
        let a = &s["active"];
        a["token"] == token
            && a["phase"] == "playing"
            && a["actor"]["sourceEpoch"] == epoch
            && a["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n >= min)
    })
}
fn pause(app: &AppHandle, token: &str, epoch: u64) -> Result<Value, String> {
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    v::wait(|s| {
        s["active"]["token"] == token
            && s["active"]["phase"] == "paused"
            && s["active"]["actor"]["sourceEpoch"] == epoch
            && s["active"]["actor"]["state"]["deviceRunning"] == false
    })
}
fn ended(token: &str, epoch: u64, duration: u64) -> Result<Value, String> {
    v::wait(|s| {
        s["active"]["token"] == token
            && s["active"]["phase"] == "ended"
            && s["active"]["actor"]["sourceEpoch"] == epoch
            && s["active"]["positionNs"] == duration.to_string()
            && s["active"]["actor"]["state"]["deviceRunning"] == false
    })
}
pub(super) fn run(app: &AppHandle, files: &[String], report: &mut Value) -> Result<(), String> {
    v::import_fixtures(app, files, report)?;
    report["plan"] = json!({"fixedSequence":true,"fixedFixtureDurationNs":"20010666666",
        "pauseAfterNs":"5000000000","pauseObservationMs":150,"forwardSeekNs":"10000000000",
        "backwardSeekNs":"1000000000","partialPlaybackNs":"1000000000","tailNs":"200000000",
        "artificialDelay":false,"changesPlaybackParameters":false,"stopOnUnderrun":false});
    report["steps"] = json!([]);
    v::select(app, "native-source-a.mp4")?;
    let initial = v::wait(|s| s["active"]["phase"] == "ready")?;
    let token = initial["active"]["token"]
        .as_str()
        .ok_or("Missing token")?
        .to_owned();
    let media = initial["active"]["mediaId"]
        .as_str()
        .ok_or("Missing media")?
        .to_owned();
    let duration = initial["active"]["durationNs"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("Missing duration")?;
    v::need(
        fixed_span_duration(duration),
        "Span scenario requires the fixed twenty-second fixture",
    )?;
    report["sourceAId"] = media.into();
    report["sourceAToken"] = token.clone().into();
    report["durationNs"] = duration.to_string().into();
    record(app, report, "selected-ready", initial)?;
    let playing = play_to(app, &token, 1, 5_000_000_000)?;
    record(app, report, "head-playing", playing)?;
    let paused = pause(app, &token, 1)?;
    let before = paused.clone();
    record(app, report, "head-paused", paused)?;
    let began = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let after = source_runtime::inspect();
    v::need(
        after["active"]["phase"] == "paused"
            && after["active"]["token"] == token
            && after["active"]["positionNs"] == before["active"]["positionNs"]
            && after["active"]["actor"]["audioDiagnostics"]["pumpCount"]
                == before["active"]["actor"]["audioDiagnostics"]["pumpCount"],
        "Paused state changed without a transport command",
    )?;
    report["pauseObservation"] = json!({"scope":"cached-state-stability-not-fresh-device-clock","requestedMs":150,
        "elapsedMs":began.elapsed().as_secs_f64()*1000.0,"freshDeviceClock":false});
    record(app, report, "head-paused-held", after)?;
    let position = before["active"]["positionNs"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("Missing paused time")?;
    let resumed = play_to(app, &token, 1, position + 300_000_000)?;
    record(app, report, "head-resumed", resumed)?;
    record(app, report, "head-ended", ended(&token, 1, duration)?)?;
    for (target, epoch, ready_label, playing_label, paused_label) in [
        (
            10_000_000_000,
            2,
            "forward-ready",
            "forward-playing",
            "forward-paused",
        ),
        (
            1_000_000_000,
            3,
            "backward-ready",
            "backward-playing",
            "backward-paused",
        ),
    ] {
        v::seek(app, target as f64 / 1e9)?;
        let ready = v::wait(|s| sought(s, &token, target, epoch))?;
        record(app, report, ready_label, ready)?;
        let playing = play_to(app, &token, epoch, target + 1_000_000_000)?;
        record(app, report, playing_label, playing)?;
        let paused = pause(app, &token, epoch)?;
        record(app, report, paused_label, paused)?;
    }
    let target = duration - 200_000_000;
    v::seek(app, target as f64 / 1e9)?;
    let tail = v::wait(|s| sought(s, &token, target, 4))?;
    record(app, report, "tail-ready", tail)?;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    record(app, report, "tail-ended", ended(&token, 4, duration)?)?;
    v::need(
        source_audio_validation::snapshot().is_null(),
        "Span run enabled artificial starvation",
    )?;
    report["nativeSourceCalls"] = v::wait_trace_settled()?;
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    Ok(())
}

pub(super) fn validate(report: &mut Value) -> Result<(), String> {
    let state = &report["runtimeFinal"];
    v::need(
        state["active"].is_null()
            && state["busy"] != true
            && state["closeFailure"].is_null()
            && state["pendingPreparations"] == 0
            && state["lifecycle"]["opening"] == 0
            && state["lifecycle"]["exiting"] == true,
        "Span resources remain",
    )?;
    let cleanup = state["cleanups"].as_array().ok_or("Missing cleanup")?;
    v::need(
        cleanup.len() == 1 && cleanup[0]["cleanupConfirmed"] == true,
        "Span cleanup failed",
    )?;
    let actor = &cleanup[0]["actor"];
    let segments = actor["segments"].as_array().ok_or("Missing segments")?;
    v::need(segments.len() == 4, "Missing span segment")?;
    v::need(
        actor["lifecycleEventsComplete"] == true
            && actor["lifecycleEventsDropped"] == 0
            && actor["lifecycleEventCount"].as_u64().is_some_and(|n| {
                actor["lifecycleEvents"]
                    .as_array()
                    .is_some_and(|v| n == v.len() as u64)
            }),
        "Critical lifecycle history incomplete",
    )?;
    v::need(
        lifecycle_matches(
            &actor["lifecycleEvents"],
            report["durationNs"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or("Missing duration")?,
        ),
        "Critical lifecycle sequence incomplete",
    )?;
    let mut count = 0;
    for (i, s) in segments.iter().enumerate() {
        let d = &s["audioDiagnostics"];
        v::need(
            d["complete"] == true
                && d["droppedStarts"] == 0
                && d["droppedUnderruns"] == 0
                && d["segmentSequence"] == i as u64 + 1
                && d["startCount"] == if i == 0 { 2 } else { 1 },
            "Span audio history incomplete",
        )?;
        let expected = match i {
            0 => &[1, 1][..],
            1 => &[2][..],
            2 => &[3][..],
            3 => &[4][..],
            _ => &[][..],
        };
        v::need(
            start_epochs_match(&d["starts"], expected),
            "Span Start source epoch ledger incomplete",
        )?;
        let n = s["underrunObservations"]
            .as_u64()
            .ok_or("Missing underrun count")?;
        v::need(
            d["underrunCount"].as_u64() == Some(n),
            "Underrun counts disagree",
        )?;
        count += n;
    }
    v::need(
        report["steps"].as_array().is_some_and(|s| s.len() == 14),
        "Span steps incomplete",
    )?;
    v::need(
        report["windowCleanup"]["error"].is_null(),
        "Span window cleanup failed",
    )?;
    report["experimentResult"] = json!({"evidenceComplete":true,"naturalUnderrunCount":count,"zeroUnderruns":count==0,
        "naturalIssueFixed":false,"artificialDelay":false,"fullFrameHistory":false});
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fixed_span_duration_is_exact() {
        assert!(fixed_span_duration(20_010_666_666));
        assert!(!fixed_span_duration(20_000_000_000));
        assert!(!fixed_span_duration(20_010_666_667));
    }

    #[test]
    fn start_epoch_ledger_matches_resume_and_seek_generations() {
        let good = json!([{"sourceEpoch":1},{"sourceEpoch":1}]);
        assert!(start_epochs_match(&good, &[1, 1]));
        assert!(!start_epochs_match(&good, &[1]));
        assert!(!start_epochs_match(
            &json!([{"sourceEpoch":2},{"sourceEpoch":1}]),
            &[1, 1]
        ));
        assert!(!start_epochs_match(&json!([{"sourceEpoch":1}]), &[1, 1]));
        assert!(!start_epochs_match(&json!([{"sourceEpoch":"1"}]), &[1]));
    }

    #[test]
    fn lifecycle_sequence_matches_fixed_span_operations() {
        let events = json!([
            {"kind":"prepared"}, {"kind":"start","sourceEpoch":1},
            {"kind":"retire"}, {"kind":"paused"},
            {"kind":"start","sourceEpoch":1}, {"kind":"ended"},
            {"kind":"retire"}, {"kind":"seek","targetNs":"10000000000"},
            {"kind":"start","sourceEpoch":2}, {"kind":"retire"}, {"kind":"paused"},
            {"kind":"retire"}, {"kind":"seek","targetNs":"1000000000"},
            {"kind":"start","sourceEpoch":3}, {"kind":"retire"}, {"kind":"paused"},
            {"kind":"retire"}, {"kind":"seek","targetNs":"19810666666"},
            {"kind":"start","sourceEpoch":4}, {"kind":"ended"}
        ]);
        assert!(lifecycle_matches(&events, 20_010_666_666));
        let mut bad = events.clone();
        bad[7]["targetNs"] = "9000000000".into();
        assert!(!lifecycle_matches(&bad, 20_010_666_666));
        let mut unknown = events;
        unknown[0]["kind"] = "submit".into();
        assert!(!lifecycle_matches(&unknown, 20_010_666_666));
    }

    #[test]
    fn seek_observation_requires_current_owner_new_epoch_and_exact_target() {
        let good = json!({"active":{"token":"t","phase":"ready","positionNs":"10000000000",
            "actor":{"sourceEpoch":2,"state":{"deviceRunning":false},
            "audioDiagnostics":{"targetNs":"10000000000","startCount":0,"complete":true}}}});
        assert!(sought(&good, "t", 10_000_000_000, 2));
        assert!(!sought(&good, "t", 10_000_000_000, 1));
        assert!(!sought(&good, "other", 10_000_000_000, 2));
        let mut bad = good;
        bad["active"]["phase"] = "ended".into();
        assert!(!sought(&bad, "t", 10_000_000_000, 2));
    }
}
