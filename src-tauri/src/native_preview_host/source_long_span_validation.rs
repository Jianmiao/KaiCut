//! Long single-source playback and multi-span seek validation; no injected delay.
use super::{source_audio_validation, source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};
use tauri::AppHandle;

#[cfg(test)]
fn expected_epochs() -> &'static [u64] {
    &[1, 2, 3, 4]
}

fn resume_reached_pause_offset(paused: u64, current: u64) -> bool {
    paused
        .checked_add(300_000_000)
        .is_some_and(|target| current >= target)
}

fn long_segment_ledgers_match(segments: &Value) -> bool {
    let Some(segments) = segments.as_array() else {
        return false;
    };
    if segments.len() != 4 {
        return false;
    }
    let expected: [&[u64]; 4] = [&[1, 1], &[2], &[3], &[4]];
    segments
        .iter()
        .zip(expected)
        .enumerate()
        .all(|(index, (segment, epochs))| {
            let d = &segment["audioDiagnostics"];
            d["complete"] == true
                && d["droppedStarts"] == 0
                && d["droppedUnderruns"] == 0
                && d["segmentSequence"].as_u64() == Some(index as u64 + 1)
                && d["targetNs"] == segment["targetNs"]
                && d["startCount"].as_u64() == Some(epochs.len() as u64)
                && d["starts"].as_array().is_some_and(|starts| {
                    starts.len() == epochs.len()
                        && starts
                            .iter()
                            .zip(epochs)
                            .all(|(start, epoch)| start["sourceEpoch"].as_u64() == Some(*epoch))
                })
        })
}

fn long_lifecycle_matches(events: &Value, duration: u64) -> bool {
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
            "start" => actions.push(format!(
                "start:{}",
                event["sourceEpoch"].as_u64().unwrap_or(0)
            )),
            "seek" => actions.push(format!("seek:{}", event["targetNs"].as_str().unwrap_or(""))),
            "paused" | "ended" => actions.push(kind.to_owned()),
            _ => {}
        }
    }
    actions
        == [
            "start:1".to_owned(),
            "paused".to_owned(),
            "start:1".to_owned(),
            "ended".to_owned(),
            "seek:7500000000".to_owned(),
            "start:2".to_owned(),
            "paused".to_owned(),
            "seek:30000000000".to_owned(),
            "start:3".to_owned(),
            "paused".to_owned(),
            format!("seek:{}", duration.saturating_sub(200_000_000)),
            "start:4".to_owned(),
            "ended".to_owned(),
        ]
}

fn fixed_long_duration(duration: u64) -> bool {
    (58_000_000_000..=60_000_000_000).contains(&duration)
}

fn long_wait<F>(condition: F) -> Result<Value, String>
where
    F: Fn(&Value) -> bool,
{
    let began = Instant::now();
    loop {
        let state = source_runtime::inspect();
        if condition(&state) {
            return Ok(state);
        }
        v::need(
            began.elapsed() < Duration::from_secs(75),
            "Long source wait timed out",
        )?;
        thread::sleep(Duration::from_millis(40));
    }
}

fn sought(state: &Value, token: &str, target: u64, epoch: u64) -> bool {
    let a = &state["active"];
    let d = &a["actor"]["audioDiagnostics"];
    a["token"] == token
        && a["phase"] == "ready"
        && a["positionNs"] == target.to_string()
        && a["actor"]["sourceEpoch"] == epoch
        && d["targetNs"] == target.to_string()
        && d["startCount"] == 0
        && d["complete"] == true
}

fn record(app: &AppHandle, report: &mut Value, label: &str, state: Value) -> Result<(), String> {
    report["steps"]
        .as_array_mut()
        .ok_or("Missing steps")?
        .push(v::record(app, label, state)?);
    Ok(())
}

pub(super) fn is_scenario(s: &str) -> bool {
    s == "audio-long-span-stress"
}

pub(super) fn run(app: &AppHandle, files: &[String], report: &mut Value) -> Result<(), String> {
    v::import_fixtures(app, files, report)?;
    report["plan"] = json!({"fixedSequence":true,"durationRangeNs":["58000000000","60000000000"],
        "continuousPlaybackTarget":"duration/2","pauseObservationMs":150,
        "seekTargetsSeconds":["7.5","30","tail-200ms"],"artificialDelay":false,
        "changesPlaybackParameters":false,"stopOnUnderrun":false});
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
        fixed_long_duration(duration),
        "Long scenario requires a 58-60 second fixture",
    )?;
    v::need(
        initial["active"]["actor"]["sourceEpoch"] == 1,
        "Initial long source epoch must be 1",
    )?;
    report["sourceAId"] = media.into();
    report["sourceAToken"] = token.clone().into();
    report["durationNs"] = duration.to_string().into();
    record(app, report, "selected-ready", initial)?;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let midpoint = duration / 2;
    report["plan"]["continuousPlaybackTargetNs"] = midpoint.to_string().into();
    let playing = long_wait(|s| {
        s["active"]["token"] == token
            && s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n >= midpoint)
    })?;
    record(app, report, "continuous-playing", playing)?;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let paused = v::wait(|s| s["active"]["token"] == token && s["active"]["phase"] == "paused")?;
    let before = paused.clone();
    record(app, report, "continuous-paused", paused)?;
    let began = Instant::now();
    thread::sleep(Duration::from_millis(150));
    let held = source_runtime::inspect();
    v::need(
        held["active"]["positionNs"] == before["active"]["positionNs"]
            && held["active"]["actor"]["audioDiagnostics"]["pumpCount"]
                == before["active"]["actor"]["audioDiagnostics"]["pumpCount"],
        "Long pause state changed",
    )?;
    report["pauseObservation"] = json!({"scope":"cached-state-stability-not-fresh-device-clock","requestedMs":150,"elapsedMs":began.elapsed().as_secs_f64()*1000.0,"freshDeviceClock":false});
    record(app, report, "continuous-paused-held", held)?;
    let paused_position = before["active"]["positionNs"]
        .as_str()
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or("Missing long paused position")?;
    v::need(
        paused_position.checked_add(300_000_000).is_some(),
        "Long resume target overflow",
    )?;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    let resumed = long_wait(|s| {
        s["active"]["token"] == token
            && s["active"]["phase"] == "playing"
            && s["active"]["positionNs"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| resume_reached_pause_offset(paused_position, n))
    })?;
    record(app, report, "continuous-resumed", resumed)?;
    let ended = long_wait(|s| {
        s["active"]["token"] == token
            && s["active"]["phase"] == "ended"
            && s["active"]["positionNs"] == duration.to_string()
    })?;
    record(app, report, "continuous-ended", ended)?;
    for (target, epoch, name) in [
        (7_500_000_000, 2, "seek-7-5s"),
        (30_000_000_000, 3, "seek-30s"),
    ] {
        v::seek(app, target as f64 / 1e9)?;
        let ready = v::wait(|s| sought(s, &token, target, epoch))?;
        record(app, report, &format!("{name}-ready"), ready)?;
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        let played = v::wait(|s| {
            s["active"]["token"] == token
                && s["active"]["phase"] == "playing"
                && s["active"]["actor"]["sourceEpoch"] == epoch
                && s["active"]["positionNs"]
                    .as_str()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= target + 1_000_000_000)
        })?;
        record(app, report, &format!("{name}-playing"), played)?;
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        record(
            app,
            report,
            &format!("{name}-paused"),
            v::wait(|s| s["active"]["token"] == token && s["active"]["phase"] == "paused")?,
        )?;
    }
    let tail = duration
        .checked_sub(200_000_000)
        .ok_or("Long fixture shorter than tail")?;
    v::seek(app, tail as f64 / 1e9)?;
    record(
        app,
        report,
        "tail-ready",
        v::wait(|s| sought(s, &token, tail, 4))?,
    )?;
    validation::eval(app, "document.querySelector('.preview-play').click()")?;
    record(
        app,
        report,
        "tail-ended",
        long_wait(|s| {
            s["active"]["token"] == token
                && s["active"]["phase"] == "ended"
                && s["active"]["positionNs"] == duration.to_string()
        })?,
    )?;
    v::need(
        source_audio_validation::snapshot().is_null(),
        "Long scenario enabled artificial starvation",
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
            && state["pendingPreparations"] == 0
            && state["lifecycle"]["opening"] == 0
            && state["lifecycle"]["exiting"] == true,
        "Long resources remain",
    )?;
    let cleanup = state["cleanups"].as_array().ok_or("Missing cleanup")?;
    v::need(
        cleanup.len() == 1 && cleanup[0]["cleanupConfirmed"] == true,
        "Long cleanup failed",
    )?;
    let actor = &cleanup[0]["actor"];
    let events = actor["lifecycleEvents"]
        .as_array()
        .ok_or("Missing lifecycle events")?;
    v::need(
        actor["lifecycleEventsComplete"] == true
            && actor["lifecycleEventsDropped"] == 0
            && events.len() <= 64
            && actor["lifecycleEventCount"] == events.len() as u64,
        "Long lifecycle ledger incomplete",
    )?;
    let segments = actor["segments"].as_array().ok_or("Missing segments")?;
    v::need(segments.len() == 4, "Long segment retention incomplete")?;
    v::need(
        long_segment_ledgers_match(&Value::Array(segments.clone())),
        "Long segment Start ledger incomplete",
    )?;
    let duration = report["durationNs"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or("Missing duration")?;
    v::need(
        fixed_long_duration(duration),
        "Long duration exceeded source profile",
    )?;
    v::need(
        long_lifecycle_matches(&actor["lifecycleEvents"], duration),
        "Long lifecycle sequence incomplete",
    )?;
    let expected_targets = [
        "0".to_owned(),
        "7500000000".to_owned(),
        "30000000000".to_owned(),
        duration.saturating_sub(200_000_000).to_string(),
    ];
    let mut natural_underruns = 0_u64;
    for (index, segment) in segments.iter().enumerate() {
        let d = &segment["audioDiagnostics"];
        let observed = segment["underrunObservations"]
            .as_u64()
            .ok_or("Missing segment underrun count")?;
        v::need(
            segment["targetNs"] == expected_targets[index]
                && d["targetNs"] == expected_targets[index],
            "Long segment target mismatch",
        )?;
        v::need(
            d["underrunCount"].as_u64() == Some(observed),
            "Long underrun ledgers disagree",
        )?;
        natural_underruns = natural_underruns
            .checked_add(observed)
            .ok_or("Long underrun count overflow")?;
    }
    v::need(
        report["steps"].as_array().is_some_and(|s| s.len() == 14),
        "Long steps incomplete",
    )?;
    v::need(
        report["windowCleanup"]["error"].is_null() && source_audio_validation::snapshot().is_null(),
        "Long cleanup or delay evidence failed",
    )?;
    report["experimentResult"] = json!({"evidenceComplete":true,"naturalUnderrunCount":natural_underruns,"zeroUnderruns":natural_underruns==0,"naturalIssueFixed":false,"artificialDelay":false,"fullFrameHistory":false});
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duration_boundary_is_bounded() {
        assert!(fixed_long_duration(58_000_000_000));
        assert!(fixed_long_duration(60_000_000_000));
        assert!(!fixed_long_duration(57_999_999_999));
        assert!(!fixed_long_duration(60_000_000_001));
    }
    #[test]
    fn expected_epochs_are_seek_generations() {
        assert_eq!(expected_epochs(), &[1, 2, 3, 4]);
    }
    #[test]
    fn lifecycle_requires_resumed_natural_eof_before_multi_span_seeks() {
        let duration = 59_008_000_000u64;
        let events = json!([
            {"kind":"start","sourceEpoch":1},{"kind":"paused"},
            {"kind":"start","sourceEpoch":1},{"kind":"ended"},
            {"kind":"seek","targetNs":"7500000000"},{"kind":"start","sourceEpoch":2},{"kind":"paused"},
            {"kind":"seek","targetNs":"30000000000"},{"kind":"start","sourceEpoch":3},{"kind":"paused"},
            {"kind":"seek","targetNs":"58808000000"},{"kind":"start","sourceEpoch":4},{"kind":"ended"}
        ]);
        assert!(long_lifecycle_matches(&events, duration));
        let mut missing_eof = events.clone();
        missing_eof.as_array_mut().unwrap().remove(3);
        assert!(!long_lifecycle_matches(&missing_eof, duration));
        let mut wrong_seek = events;
        wrong_seek[4]["targetNs"] = "8000000000".into();
        assert!(!long_lifecycle_matches(&wrong_seek, duration));
    }
    #[test]
    fn segment_ledgers_require_resume_start_and_each_seek_epoch() {
        let good = json!([
            {"targetNs":"0","audioDiagnostics":{"segmentSequence":1,"targetNs":"0","complete":true,"droppedStarts":0,"droppedUnderruns":0,"startCount":2,"starts":[{"sourceEpoch":1},{"sourceEpoch":1}]}},
            {"targetNs":"7500000000","audioDiagnostics":{"segmentSequence":2,"targetNs":"7500000000","complete":true,"droppedStarts":0,"droppedUnderruns":0,"startCount":1,"starts":[{"sourceEpoch":2}]}},
            {"targetNs":"30000000000","audioDiagnostics":{"segmentSequence":3,"targetNs":"30000000000","complete":true,"droppedStarts":0,"droppedUnderruns":0,"startCount":1,"starts":[{"sourceEpoch":3}]}},
            {"targetNs":"58808000000","audioDiagnostics":{"segmentSequence":4,"targetNs":"58808000000","complete":true,"droppedStarts":0,"droppedUnderruns":0,"startCount":1,"starts":[{"sourceEpoch":4}]}}
        ]);
        assert!(long_segment_ledgers_match(&good));
        let mut bad = good.clone();
        bad[0]["audioDiagnostics"]["starts"][1]["sourceEpoch"] = 2.into();
        assert!(!long_segment_ledgers_match(&bad));
        let mut truncated = good;
        truncated[3]["audioDiagnostics"]["starts"] = json!([]);
        assert!(!long_segment_ledgers_match(&truncated));
    }
    #[test]
    fn resume_advancement_is_measured_from_the_paused_position() {
        assert!(resume_reached_pause_offset(29_560_000_000, 29_860_000_000));
        assert!(!resume_reached_pause_offset(29_560_000_000, 29_810_354_166));
        assert!(!resume_reached_pause_offset(u64::MAX - 10, u64::MAX));
    }
}
