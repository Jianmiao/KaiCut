//! Fixed-count natural tail-start experiment. No artificial audio delay.
use super::{source_audio_validation, source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use tauri::AppHandle;
const CYCLES: u64 = 6;

fn ready_for_cycle(
    state: &Value,
    token: &str,
    target: u64,
    segment: u64,
    previous_epoch: u64,
) -> bool {
    let a = &state["active"];
    let actor = &a["actor"];
    let d = &actor["audioDiagnostics"];
    a["token"] == token
        && a["phase"] == "ready"
        && a["positionNs"] == target.to_string()
        && a["error"].is_null()
        && actor["state"]["deviceRunning"] == false
        && actor["segmentCount"] == segment
        && previous_epoch
            .checked_add(1)
            .is_some_and(|next| actor["sourceEpoch"].as_u64() == Some(next))
        && d["segmentSequence"] == segment
        && d["targetNs"] == target.to_string()
        && d["startCount"] == 0
        && d["underrunCount"] == 0
        && d["complete"] == true
}

pub(super) fn is_scenario(scenario: &str) -> bool {
    scenario == "audio-tail-stress"
}

fn complete_video_history(actor: &Value) -> bool {
    actor["state"]["eventCount"].as_u64().is_some_and(|count| {
        actor["videoEvents"]
            .as_array()
            .is_some_and(|e| count == e.len() as u64)
    })
}

pub(super) fn run(app: &AppHandle, files: &[String], report: &mut Value) -> Result<(), String> {
    v::import_fixtures(app, files, report)?;
    report["plan"] = json!({"cycles":CYCLES,"tailNs":"200000000","fixedCount":true,
        "intentionalDelay":false,"stopOnUnderrun":false,"changesPlaybackParameters":false});
    report["cycles"] = json!([]);
    v::select(app, "native-source-a.mp4")?;
    let initial = v::wait(|s| s["active"]["phase"] == "ready")?;
    let token = initial["active"]["token"]
        .as_str()
        .ok_or("Missing source token")?
        .to_owned();
    let media = initial["active"]["mediaId"]
        .as_str()
        .ok_or("Missing source identity")?
        .to_owned();
    let duration = initial["active"]["durationNs"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("Missing duration")?;
    let target = duration
        .checked_sub(200_000_000)
        .ok_or("Fixture shorter than tail")?;
    let mut epoch = initial["active"]["actor"]["sourceEpoch"]
        .as_u64()
        .ok_or("Missing epoch")?;
    report["sourceAId"] = media.clone().into();
    report["sourceAToken"] = token.clone().into();
    report["durationNs"] = duration.to_string().into();
    report["targetNs"] = target.to_string().into();
    report["initial"] = v::record(app, "selected-ready", initial)?;
    for index in 1..=CYCLES {
        v::seek(app, target as f64 / 1e9)?;
        let ready = v::wait(|s| ready_for_cycle(s, &token, target, index + 1, epoch))?;
        let next_epoch = ready["active"]["actor"]["sourceEpoch"]
            .as_u64()
            .ok_or("Missing sought epoch")?;
        let started = std::time::Instant::now();
        let recorded = v::record(app, "tail-ready", ready)?;
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        let ended = v::wait(|s| {
            let a = &s["active"];
            a["token"] == token
                && a["phase"] == "ended"
                && a["positionNs"] == duration.to_string()
                && a["actor"]["sourceEpoch"] == next_epoch
                && a["actor"]["audioDiagnostics"]["segmentSequence"] == index + 1
                && a["actor"]["audioDiagnostics"]["startCount"] == 1
                && a["actor"]["audioDiagnostics"]["targetNs"] == target.to_string()
                && a["actor"]["audioDiagnostics"]["complete"] == true
                && a["actor"]["state"]["deviceRunning"] == false
        })?;
        v::need(
            ended["active"]["error"].is_null() && ended["active"]["actor"]["error"].is_null(),
            "Tail playback failed",
        )?;
        report["cycles"].as_array_mut().unwrap().push(
            json!({"index":index,"previousSourceEpoch":epoch,
            "sourceEpoch":next_epoch,"observationElapsedMs":started.elapsed().as_secs_f64()*1000.0,
            "ready":recorded,"ended":v::record(app,"tail-ended",ended)?}),
        );
        epoch = next_epoch;
    }
    v::need(
        source_audio_validation::snapshot().is_null(),
        "Natural stress accidentally enabled audio delay",
    )?;
    report["nativeSourceCalls"] = v::wait_trace_settled()?;
    report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
    Ok(())
}

pub(super) fn validate(report: &mut Value) -> Result<(), String> {
    let target = report["targetNs"]
        .as_str()
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or("Missing numeric tail target")?;
    let cycles = report["cycles"].as_array().ok_or("Missing tail cycles")?;
    v::need(
        cycles.len() == CYCLES as usize,
        "Incomplete fixed-count tail experiment",
    )?;
    let state = &report["runtimeFinal"];
    v::need(
        state["active"].is_null()
            && state["busy"] != true
            && state["closeFailure"].is_null()
            && state["lifecycle"]["opening"] == 0
            && state["pendingPreparations"] == 0
            && state["lifecycle"]["exiting"] == true,
        "Tail runtime retained resources",
    )?;
    let cleanups = state["cleanups"].as_array().ok_or("Missing cleanup")?;
    v::need(
        cleanups.len() == 1 && cleanups[0]["cleanupConfirmed"] == true,
        "Tail cleanup failed",
    )?;
    let actor = &cleanups[0]["actor"];
    let segments = actor["segments"].as_array().ok_or("Missing segments")?;
    v::need(
        segments.len() == CYCLES as usize + 1 && complete_video_history(actor),
        "Tail history truncated",
    )?;
    let mut underruns = 0;
    for (index, segment) in segments.iter().enumerate() {
        let d = &segment["audioDiagnostics"];
        v::need(
            d["complete"] == true
                && d["droppedStarts"] == 0
                && d["droppedUnderruns"] == 0
                && d["segmentSequence"] == index as u64 + 1,
            "Incomplete tail audio history",
        )?;
        if index > 0 {
            v::need(
                segment["targetNs"] == target.to_string()
                    && d["targetNs"] == target.to_string()
                    && d["startCount"] == 1,
                "Tail seek/start identity missing",
            )?;
        }
        let count = segment["underrunObservations"]
            .as_u64()
            .ok_or("Missing underrun count")?;
        v::need(
            d["underrunCount"].as_u64() == Some(count),
            "Tail underrun ledgers disagree",
        )?;
        underruns += count;
    }
    v::need(
        source_audio_validation::snapshot().is_null(),
        "Artificial delay enabled",
    )?;
    v::need(
        report["windowCleanup"]["error"].is_null(),
        "Tail window cleanup failed",
    )?;
    report["experimentResult"] = json!({"evidenceComplete":true,"completedCycles":CYCLES,
        "naturalUnderrunCount":underruns,"zeroUnderruns":underruns==0,"artificialDelay":false,
        "naturalIssueFixed":false});
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn repeated_target_requires_new_ready_generation_not_old_end() {
        let good = json!({"active":{"token":"a","phase":"ready","positionNs":"3810666666",
            "error":null,"actor":{"sourceEpoch":3,"segmentCount":3,"state":{"deviceRunning":false},
            "audioDiagnostics":{"segmentSequence":3,"targetNs":"3810666666","startCount":0,"underrunCount":0,"complete":true}}}});
        assert!(ready_for_cycle(&good, "a", 3_810_666_666, 3, 2));
        for (pointer, value) in [
            ("/active/phase", json!("ended")),
            ("/active/actor/sourceEpoch", json!(2)),
            ("/active/actor/segmentCount", json!(2)),
            ("/active/actor/audioDiagnostics/startCount", json!(1)),
            ("/active/token", json!("b")),
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                !ready_for_cycle(&bad, "a", 3_810_666_666, 3, 2),
                "{pointer}"
            );
        }
    }
    #[test]
    fn missing_history_and_overflow_cannot_mimic_valid_evidence() {
        assert!(!complete_video_history(&json!({})));
        assert!(!complete_video_history(
            &json!({"state":{"eventCount":4},"videoEvents":[1,2]})
        ));
        assert!(complete_video_history(
            &json!({"state":{"eventCount":2},"videoEvents":[1,2]})
        ));
        let absent_epoch = json!({"active":{"token":"a","phase":"ready","positionNs":"1",
            "actor":{"segmentCount":2,"state":{"deviceRunning":false},"audioDiagnostics":{
                "segmentSequence":2,"targetNs":"1","startCount":0,"underrunCount":0,"complete":true}}}});
        assert!(!ready_for_cycle(&absent_epoch, "a", 1, 2, u64::MAX));
    }
}
