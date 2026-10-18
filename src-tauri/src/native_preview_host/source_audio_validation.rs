//! Hidden positive control for underrun telemetry. Never a performance result.
use super::{source_runtime, source_validation as v, validation};
use serde_json::{Value, json};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::AppHandle;

static ARMED: AtomicBool = AtomicBool::new(false);
static EVIDENCE: Mutex<Option<Value>> = Mutex::new(None);

fn delay_budget_ms(capacity: u32, rate: u32) -> Result<u64, String> {
    if capacity == 0 || rate == 0 {
        return Err("Missing audio buffer duration".into());
    }
    let milliseconds = (u64::from(capacity) * 3000)
        .div_ceil(u64::from(rate))
        .max(60);
    if milliseconds > 250 {
        return Err("Positive control exceeds its 250ms bound".into());
    }
    Ok(milliseconds)
}

pub(super) fn before_pump(
    capacity: u32,
    rate: u32,
    target_ns: u64,
    source_epoch: u64,
    render_epoch: u64,
) -> Result<(), String> {
    if !ARMED.swap(false, Ordering::AcqRel) {
        return Ok(());
    }
    {
        let evidence = EVIDENCE
            .lock()
            .map_err(|_| "Audio validation evidence unavailable")?;
        let record = evidence.as_ref().ok_or("Audio delay was not armed")?;
        if record["targetNs"] != target_ns.to_string() || source_epoch == 0 || render_epoch == 0 {
            return Err("Audio delay reached a different source segment".into());
        }
    }
    let ms = delay_budget_ms(capacity, rate)?;
    let began = Instant::now();
    std::thread::sleep(Duration::from_millis(ms));
    let elapsed = began.elapsed().as_secs_f64() * 1000.0;
    let mut evidence = EVIDENCE
        .lock()
        .map_err(|_| "Audio validation evidence unavailable")?;
    let record = evidence.as_mut().ok_or("Audio delay was not armed")?;
    record["applied"] = true.into();
    record["capacityFrames"] = capacity.into();
    record["sampleRate"] = rate.into();
    record["requestedMs"] = ms.into();
    record["elapsedMs"] = elapsed.into();
    record["overshootMs"] = (elapsed - ms as f64).max(0.0).into();
    record["sourceEpoch"] = source_epoch.into();
    record["renderEpoch"] = render_epoch.into();
    Ok(())
}

pub(super) fn is_scenario(scenario: &str) -> bool {
    scenario == "underrun-delay"
}

fn arm(token: &str, media_id: &str, target: u64) -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().collect();
    if !cfg!(debug_assertions)
        || token.is_empty()
        || media_id.is_empty()
        || ![
            "--mcp",
            "--mcp-hidden",
            "--mcp-validation",
            "--native-preview-host",
            "--native-source-preview",
            "--native-source-check-dir",
        ]
        .iter()
        .all(|flag| args.iter().any(|a| a == flag))
    {
        return Err("Audio positive control requires isolated hidden debug validation".into());
    }
    let mut evidence = EVIDENCE
        .lock()
        .map_err(|_| "Audio validation unavailable")?;
    if evidence.is_some() {
        return Err("Audio positive control already armed".into());
    }
    *evidence = Some(json!({"intentional":true,"oneShot":true,"applied":false,
        "boundary":"before-first-pump-after-start","token":token,"mediaId":media_id,
        "targetNs":target.to_string(),"requestedMaxMs":250,"elapsedHasHardLimit":false,"naturalUnderrun":false}));
    ARMED.store(true, Ordering::Release);
    Ok(())
}

pub(super) fn snapshot() -> Value {
    EVIDENCE
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .unwrap_or(Value::Null)
}

pub(super) fn run(app: &AppHandle, files: &[String], report: &mut Value) -> Result<(), String> {
    v::import_fixtures(app, files, report)?;
    v::select(app, "native-source-a.mp4")?;
    let ready = v::wait(|s| s["active"]["phase"] == "ready")?;
    let token = ready["active"]["token"]
        .as_str()
        .ok_or("Missing token")?
        .to_owned();
    let media = ready["active"]["mediaId"]
        .as_str()
        .ok_or("Missing source")?
        .to_owned();
    let duration = ready["active"]["durationNs"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("Missing extent")?;
    let target = duration.checked_sub(200_000_000).ok_or("Short fixture")?;
    report["sourceAId"] = media.clone().into();
    report["steps"] = json!([v::record(app, "selected-ready", ready)?]);
    v::seek(app, target as f64 / 1e9)?;
    let near = v::wait(|s| {
        s["active"]["phase"] == "ready" && s["active"]["positionNs"] == target.to_string()
    })?;
    report["steps"]
        .as_array_mut()
        .unwrap()
        .push(v::record(app, "near-end-ready", near)?);
    arm(&token, &media, target)?;
    let result = (|| {
        validation::eval(app, "document.querySelector('.preview-play').click()")?;
        let ended = v::wait(|s| s["active"]["phase"] == "ended")?;
        v::need(
            ended["active"]["actor"]["underrunObservations"]
                .as_u64()
                .is_some_and(|n| n > 0),
            "Positive control did not observe buffer exhaustion",
        )?;
        report["steps"].as_array_mut().unwrap().push(v::record(
            app,
            "natural-end-after-delay",
            ended,
        )?);
        report["nativeSourceCalls"] = v::wait_trace_settled()?;
        report["runtimeBeforeFinalCleanup"] = source_runtime::inspect();
        report["positiveControl"] = snapshot();
        Ok(())
    })();
    ARMED.store(false, Ordering::Release);
    result
}

pub(super) fn validate(report: &Value) -> Result<(), String> {
    let delay = &report["positiveControl"];
    v::need(
        delay["intentional"] == true
            && delay["applied"] == true
            && delay["naturalUnderrun"] == false,
        "Missing intentional starvation evidence",
    )?;
    let requested = delay["requestedMs"]
        .as_u64()
        .ok_or("Missing delay duration")?;
    v::need(
        (60..=250).contains(&requested)
            && delay["elapsedMs"]
                .as_f64()
                .is_some_and(|n| n >= requested as f64),
        "Requested delay bound or observed wait is missing",
    )?;
    let runtime = &report["runtimeFinal"];
    v::need(
        runtime["busy"] != true
            && runtime["active"].is_null()
            && runtime["closeFailure"].is_null()
            && runtime["pendingPreparations"] == 0
            && runtime["lifecycle"]["opening"] == 0
            && runtime["lifecycle"]["exiting"] == true,
        "Positive-control runtime did not retire",
    )?;
    let cleanups = runtime["cleanups"].as_array().ok_or("Missing cleanup")?;
    v::need(
        cleanups.len() == 1 && cleanups[0]["cleanupConfirmed"] == true,
        "Positive-control cleanup failed",
    )?;
    let segments = cleanups[0]["actor"]["segments"]
        .as_array()
        .ok_or("Missing segments")?;
    v::need(
        segments.iter().any(|s| {
            s["targetNs"] == delay["targetNs"]
                && s["underrunObservations"].as_u64().is_some_and(|n| n > 0)
        }),
        "Missing raw positive-control underrun",
    )?;
    v::need(
        report["windowCleanup"]["error"].is_null(),
        "Positive-control window cleanup failed",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positive_control_exceeds_capacity_but_never_sleeps_without_a_bound() {
        assert_eq!(delay_budget_ms(1056, 48000).unwrap(), 66);
        assert_eq!(delay_budget_ms(480, 48000).unwrap(), 60);
        assert!(delay_budget_ms(48000, 48000).is_err());
        assert!(delay_budget_ms(0, 48000).is_err());
        assert!(delay_budget_ms(1056, 0).is_err());
    }
    #[test]
    fn ordinary_process_cannot_arm_audio_starvation() {
        assert!(arm("token", "media", 100).is_err());
        assert!(snapshot().is_null());
    }
}
