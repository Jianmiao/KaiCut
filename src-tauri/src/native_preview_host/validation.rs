//! Explicit hidden, isolated Tauri integration verification. Never controls a
//! user's window or takes desktop input. The frontend is the actual built app.
use super::{LayoutRequest, platform};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

struct Capture {
    id: String,
    tx: mpsc::SyncSender<Value>,
}
static CAPTURE: Mutex<Option<Capture>> = Mutex::new(None);
static REPORT: Mutex<Option<(PathBuf, Value)>> = Mutex::new(None);

pub(super) fn capture(id: &str, data: Value) -> Result<(), String> {
    if serde_json::to_vec(&data).map_err(|e| e.to_string())?.len() > 65536 {
        return Err("Validation observation too large".into());
    }
    let mut pending = CAPTURE.lock().map_err(|_| "Capture unavailable")?;
    if pending.as_ref().is_none_or(|p| p.id != id) {
        return Err("Unknown validation request".into());
    }
    pending
        .take()
        .unwrap()
        .tx
        .send(data)
        .map_err(|e| e.to_string())
}
pub(super) fn main_call<F>(app: &AppHandle, operation: F) -> Result<Value, String>
where
    F: FnOnce(tauri::WebviewWindow) -> Result<Value, String> + Send + 'static,
{
    let handle = app.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let value = handle
            .get_webview_window("main")
            .ok_or("Main window unavailable".to_owned())
            .and_then(operation);
        let _ = tx.send(value);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(20))
        .map_err(|e| e.to_string())?
}
pub(super) fn status(app: &AppHandle) -> Result<Value, String> {
    main_call(app, |w| platform::status(&w))
}
pub(super) fn eval(app: &AppHandle, script: &str) -> Result<(), String> {
    let s = script.to_owned();
    main_call(app, move |w| {
        w.eval(s).map_err(|e| e.to_string())?;
        Ok(Value::Null)
    })?;
    Ok(())
}
pub(super) fn wait_for<F>(app: &AppHandle, condition: F) -> Result<Value, String>
where
    F: Fn(&Value) -> bool,
{
    let began = Instant::now();
    loop {
        let state = status(app)?;
        if state["parent"]["visible"] != false || state["parent"]["focused"] != false {
            return Err("Validation parent became visible or focused".into());
        }
        if condition(&state) {
            return Ok(state);
        }
        if began.elapsed() > Duration::from_secs(15) {
            return Err(format!("Native preview observation timed out: {state}"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}
pub(super) fn dom(app: &AppHandle) -> Result<Value, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::sync_channel(1);
    *CAPTURE.lock().map_err(|_| "Capture unavailable")? = Some(Capture { id: id.clone(), tx });
    let code = format!(
        r#"(()=>{{const c=document.querySelector('.preview-canvas'),t=document.querySelector('.preview-controls');const b=e=>{{if(!e)return null;const r=e.getBoundingClientRect();return {{x:r.x,y:r.y,width:r.width,height:r.height}}}};const s=c?getComputedStyle(c):null;window.__TAURI_INTERNALS__.invoke('native_preview_validation_capture',{{requestId:'{id}',data:{{viewport:{{width:innerWidth,height:innerHeight,scaleFactor:devicePixelRatio}},canvas:b(c),controls:b(t),paddingBottom:s?parseFloat(s.paddingBottom):null,hostState:c?.dataset.nativePreviewHost,mediaNodeCount:document.querySelectorAll('video,audio').length,sourceEngine:document.querySelector('.preview-stage')?.dataset.previewEngine,sourcePhase:document.querySelector('.preview-stage')?.dataset.previewPhase,sourcePositionNs:document.querySelector('.preview-stage')?.dataset.previewPositionNs,sourceMediaId:document.querySelector('.preview-stage')?.dataset.previewMediaId,mode:document.querySelector('.preview-stage')?.dataset.previewMode,mediaCardNames:Array.from(document.querySelectorAll('.media-card')).map(e=>e.getAttribute('aria-label')),selectedMediaLabel:document.querySelector('.media-card[aria-pressed=true]')?.getAttribute('aria-label'),playDisabled:document.querySelector('.preview-play')?.disabled,sourceAlerts:Array.from(document.querySelectorAll('.preview-stage [role=alert]')).slice(0,2).map(e=>e.textContent.slice(0,512)),blocked:!!document.querySelector('[data-native-preview-blocker]')}}}})}})()"#
    );
    eval(app, &code)?;
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string());
    *CAPTURE.lock().map_err(|_| "Capture unavailable")? = None;
    result
}
pub(super) fn snapshot(app: &AppHandle, label: &str, mut state: Value) -> Result<Value, String> {
    let started = Instant::now();
    loop {
        let observed = dom(app)?;
        let current = status(app)?;
        let expected = if state["lastRequest"]["blocked"] == true {
            "blocked"
        } else {
            "attached"
        };
        if observed["hostState"] == expected && current["revision"] == state["revision"] {
            return Ok(json!({"label":label,"native":state,"dom":observed}));
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(format!(
                "Frontend did not acknowledge the current native layout: {observed}"
            ));
        }
        state = current;
        thread::sleep(Duration::from_millis(25));
    }
}
pub(super) fn record_closed(cleanup: Value) {
    if let Ok(mut pending) = REPORT.lock() {
        if let Some((path, mut report)) = pending.take() {
            if !cleanup["error"].is_null() {
                report["ok"] = false.into();
            }
            report["automaticClose"] = cleanup;
            report["closedAt"] = chrono::Utc::now().to_rfc3339().into();
            if let Err(error) = save(&path, &report) {
                eprintln!("Cannot save preview integration report: {error}; {report}");
            }
        }
    }
}
pub(super) fn save(path: &std::path::Path, report: &Value) -> Result<(), String> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing report parent")?)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut temp, report).map_err(|e| e.to_string())?;
    temp.write_all(b"\n")
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temp.persist_noclobber(path).map_err(|e| e.to_string())?;
    Ok(())
}
pub(super) fn start(app: &AppHandle) -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().collect();
    let Some(i) = args.iter().position(|v| v == "--native-preview-check-dir") else {
        return Ok(());
    };
    if !cfg!(debug_assertions)
        || !["--mcp", "--mcp-hidden", "--mcp-validation"]
            .iter()
            .all(|flag| args.iter().any(|v| v == flag))
    {
        return Err("Preview check requires an isolated hidden debug process".into());
    }
    let output = PathBuf::from(
        args.get(i + 1)
            .ok_or("Missing preview evidence directory")?,
    );
    let lexical = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    if !output.is_absolute()
        || !output.starts_with(&lexical)
        || output
            .components()
            .any(|v| matches!(v, std::path::Component::ParentDir))
    {
        return Err("Preview evidence must stay inside checkout target".into());
    }
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    if !ancestor
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(&target)
    {
        return Err("Preview output escaped via link".into());
    }
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let app = app.clone();
    thread::spawn(move || {
        let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"processId":std::process::id(),"scope":{"actualTauriWebview":true,"hiddenParent":true,"calibrationOnly":true,"screenDisplayed":false,"mediaPlaybackIntegrated":false,"syntheticDomChanges":true},"steps":[]});
        let work = (|| {
            let initial = wait_for(&app, |s| !s["child"].is_null() && s["error"].is_null())?;
            report["steps"].as_array_mut().unwrap().push(snapshot(
                &app,
                "initial",
                initial.clone(),
            )?);
            let revision = initial["revision"]
                .as_u64()
                .ok_or("Missing initial revision")?;
            eval(
                &app,
                "document.querySelector('.preview-canvas').style.width='300px'",
            )?;
            let shrink = wait_for(&app, |s| {
                s["revision"].as_u64().unwrap_or(0) > revision
                    && s["lastRequest"]["width"].as_f64().unwrap_or(9999.0) <= 300.0
                    && s["error"].is_null()
            })?;
            report["steps"].as_array_mut().unwrap().push(snapshot(
                &app,
                "content-resize",
                shrink.clone(),
            )?);
            let revision = shrink["revision"].as_u64().unwrap();
            eval(
                &app,
                "(()=>{const d=document.createElement('div');d.id='native-preview-validation-overlay';d.setAttribute('role','dialog');d.setAttribute('data-native-preview-blocker','true');d.style.cssText='position:fixed;inset:100px;';document.body.append(d)})()",
            )?;
            let blocked = wait_for(&app, |s| {
                s["revision"].as_u64().unwrap_or(0) > revision
                    && s["lastRequest"]["blocked"] == true
            })?;
            report["steps"].as_array_mut().unwrap().push(snapshot(
                &app,
                "overlay-blocked",
                blocked.clone(),
            )?);
            let revision = blocked["revision"].as_u64().unwrap();
            eval(
                &app,
                "document.querySelector('#native-preview-validation-overlay').remove()",
            )?;
            let resumed = wait_for(&app, |s| {
                s["revision"].as_u64().unwrap_or(0) > revision
                    && s["lastRequest"]["blocked"] == false
                    && s["error"].is_null()
            })?;
            report["steps"].as_array_mut().unwrap().push(snapshot(
                &app,
                "overlay-removed",
                resumed.clone(),
            )?);
            let old: LayoutRequest = serde_json::from_value(resumed["lastRequest"].clone())
                .map_err(|e| e.to_string())?;
            let stale = main_call(&app, move |w| {
                let before = platform::status(&w)?;
                let old_error = platform::layout(&w, old.clone()).err();
                let mut foreign = old;
                foreign.session_id = "obsolete-session".into();
                foreign.sequence += 1;
                let foreign_error = platform::layout(&w, foreign).err();
                let after = platform::status(&w)?;
                Ok(
                    json!({"oldError":old_error,"foreignError":foreign_error,"unchanged":before==after}),
                )
            })?;
            if stale["oldError"].is_null()
                || stale["foreignError"].is_null()
                || stale["unchanged"] != true
            {
                return Err("Stale host commands changed native state".into());
            }
            report["staleRequests"] = stale;
            report["requestedWindowSize"] = json!({"logicalWidth":1180,"logicalHeight":760});
            let invalidations = resumed["invalidations"].as_u64().unwrap_or(0);
            main_call(&app, |w| {
                w.set_size(tauri::LogicalSize::new(1180.0, 760.0))
                    .map_err(|e| e.to_string())?;
                Ok(Value::Null)
            })?;
            let resized = wait_for(&app, |s| {
                s["invalidations"].as_u64().unwrap_or(0) > invalidations
                    && s["lastRequest"]["viewportWidth"] == 1180.0
                    && s["lastRequest"]["blocked"] == false
                    && s["error"].is_null()
            })?;
            report["steps"].as_array_mut().unwrap().push(snapshot(
                &app,
                "tauri-resize",
                resized.clone(),
            )?);
            let revision = resized["revision"].as_u64().unwrap();
            eval(
                &app,
                "document.querySelector('.preview-toolbar button').click()",
            )?;
            let remount = wait_for(&app, |s| {
                s["revision"].as_u64().unwrap_or(0) > revision
                    && !s["child"].is_null()
                    && !s["lastCleanup"].is_null()
                    && s["error"].is_null()
            })?;
            report["steps"].as_array_mut().unwrap().push(snapshot(
                &app,
                "preview-remount",
                remount,
            )?);
            Ok::<(), String>(())
        })();
        report["ok"] = work.is_ok().into();
        if let Err(error) = &work {
            report["error"] = error.clone().into();
        }
        let path = run.join("native-tauri-preview-validation.json");
        *REPORT.lock().unwrap() = Some((path.clone(), report));
        // Exercise parent destruction while the child is still attached. Its
        // WM_DESTROY owns graphics cleanup; the Tauri event saves that evidence.
        let closed = main_call(&app, |w| {
            w.destroy().map_err(|e| e.to_string())?;
            Ok(Value::Null)
        });
        if let Err(error) = closed {
            record_closed(json!({"error":error}));
            app.exit(1);
        }
    });
    Ok(())
}
