//! Opt-in calibration surface inside the real Tauri preview region.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutRequest {
    pub session_id: String,
    pub sequence: u64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale_factor: f64,
    pub blocked: bool,
    pub viewport_width: f64,
    pub viewport_height: f64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceOpenRequest {
    pub project_id: String,
    pub revision: u64,
    pub media_id: String,
    pub request_id: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceControlRequest {
    pub token: String,
    pub action: String,
    pub position_ns: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceCancelRequest {
    pub project_id: String,
    pub revision: u64,
    pub request_id: String,
}

#[tauri::command]
pub async fn native_source_cancel_open(
    window: tauri::WebviewWindow,
    request: SourceCancelRequest,
) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if source_enabled() && window.label() == "main" {
        let observation = source_validation::observe(
            "native_source_cancel_open",
            json!({"projectId":request.project_id,"revision":request.revision,"requestId":request.request_id}),
        );
        let result = source_runtime::cancel_open(window, request).await;
        observation.finish(&result);
        return result;
    }
    let _ = (window, request);
    Err("原生素材预览未启用".into())
}

#[tauri::command]
pub fn native_source_capability() -> Value {
    json!({"enabled":source_enabled()})
}
#[tauri::command]
pub async fn native_source_open(
    window: tauri::WebviewWindow,
    request: SourceOpenRequest,
) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if source_enabled() && window.label() == "main" {
        let observation = source_validation::observe(
            "native_source_open",
            json!({"requestId":request.request_id,"mediaId":request.media_id}),
        );
        let result = source_runtime::open(window, request).await;
        observation.finish(&result);
        return result;
    }
    let _ = (window, request);
    Ok(json!({"supported":false,"reason":"原生素材预览未启用"}))
}
#[tauri::command]
pub async fn native_source_control(
    window: tauri::WebviewWindow,
    request: SourceControlRequest,
) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if source_enabled() && window.label() == "main" {
        let observation = source_validation::observe(
            "native_source_control",
            json!({"token":request.token,"action":request.action,"positionNs":request.position_ns}),
        );
        let result = source_runtime::control(window, request).await;
        observation.finish(&result);
        return result;
    }
    let _ = (window, request);
    Err("原生素材预览未启用".into())
}
#[tauri::command]
pub async fn native_source_status(
    window: tauri::WebviewWindow,
    token: String,
) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if source_enabled() && window.label() == "main" {
        return source_runtime::status(window, token).await;
    }
    let _ = (window, token);
    Err("原生素材预览未启用".into())
}
pub fn source_enabled() -> bool {
    cfg!(all(windows, feature = "native-preview-host"))
        && std::env::args_os().any(|v| v == "--native-source-preview")
        && std::env::args_os().any(|v| v == "--native-preview-host")
}
pub fn source_exit_pending(app: &tauri::AppHandle) -> bool {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if source_enabled() {
        return source_runtime::defer_exit(app);
    }
    let _ = app;
    false
}

fn validate_sequence(
    session: &str,
    revision: u64,
    requested: &str,
    sequence: u64,
) -> Result<(), String> {
    if session != requested || sequence <= revision || sequence > 9_007_199_254_740_991 {
        return Err("Native preview request has a stale session or sequence".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn native_preview_host_status(window: tauri::WebviewWindow) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() {
        return on_main(window, |window| platform::status(&window)).await;
    }
    let _ = window;
    Ok(json!({"enabled":false,"sessionId":null,"revision":0}))
}
#[tauri::command]
pub async fn native_preview_host_layout(
    window: tauri::WebviewWindow,
    request: LayoutRequest,
) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() {
        return on_main(window, move |window| platform::layout(&window, request)).await;
    }
    let _ = (window, request);
    Err("Native preview calibration is disabled".into())
}
#[tauri::command]
pub async fn native_preview_host_release(
    window: tauri::WebviewWindow,
    session_id: String,
    sequence: u64,
) -> Result<Value, String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() {
        return on_main(window, move |window| {
            platform::release(&window, &session_id, sequence)
        })
        .await;
    }
    let _ = (window, session_id, sequence);
    Err("Native preview calibration is disabled".into())
}
#[tauri::command]
pub fn native_preview_validation_capture(
    window: tauri::WebviewWindow,
    request_id: String,
    data: Value,
) -> Result<(), String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() && window.label() == "main" {
        return validation::capture(&request_id, data);
    }
    let _ = (window, request_id, data);
    Err("Preview validation is disabled".into())
}
pub fn initialize(app: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() {
        if source_enabled() {
            source_validation::start(app)?;
        }
        if !media_validation::start(app)? {
            validation::start(app)?;
        }
    }
    let _ = app;
    Ok(())
}
pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() && window.label() == "main" {
        match event {
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
                use tauri::Emitter;
                platform::invalidate();
                let _ = window.emit("kaicut:native-preview-invalidated", ());
            }
            tauri::WindowEvent::Destroyed => platform::close(),
            _ => {}
        }
    }
    let _ = (window, event);
}
pub fn shutdown() {
    #[cfg(all(windows, feature = "native-preview-host"))]
    if enabled() {
        platform::close();
    }
}

#[cfg(all(windows, feature = "native-preview-host"))]
fn enabled() -> bool {
    std::env::args_os().any(|v| v == "--native-preview-host")
}

#[cfg(all(windows, feature = "native-preview-host"))]
async fn on_main<F>(window: tauri::WebviewWindow, operation: F) -> Result<Value, String>
where
    F: FnOnce(tauri::WebviewWindow) -> Result<Value, String> + Send + 'static,
{
    if window.label() != "main" {
        return Err("Native preview requires the main window".into());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let handle = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = tx.send(operation(handle));
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|e| format!("Native preview response unavailable: {e}"))?
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/child.rs"]
mod child;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/composition_presenter.rs"]
mod composition_presenter;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/d3d11_renderer.rs"]
mod d3d11_renderer;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/frame_verification.rs"]
mod frame_verification;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/validation.rs"]
mod validation;

#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_cursor.rs"]
#[allow(dead_code)]
mod audio_cursor;
#[cfg(all(windows, feature = "native-preview-host"))]
mod audio_diagnostics;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_preview_adapter.rs"]
#[allow(dead_code)]
mod audio_preview_adapter;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_scheduling.rs"]
#[allow(dead_code)]
mod audio_scheduling;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_session_driver.rs"]
#[allow(dead_code)]
mod audio_session_driver;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_timeline.rs"]
#[allow(dead_code)]
mod audio_timeline;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_timing_driver.rs"]
#[allow(dead_code)]
mod audio_timing_driver;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/audio_transfer.rs"]
#[allow(dead_code)]
mod audio_transfer;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/media_actor.rs"]
mod media_actor;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/media_sink.rs"]
mod media_sink;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/media_validation.rs"]
mod media_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
mod source_audio_stress_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
mod source_audio_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_fault_validation.rs"]
mod source_fault_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_faults.rs"]
mod source_faults;
#[cfg(all(windows, feature = "native-preview-host"))]
mod source_long_span_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
mod source_span_validation;

#[cfg(all(windows, feature = "native-preview-host"))]
mod source_inflight_validation;

#[cfg(all(windows, feature = "native-preview-host"))]
mod source_inflight_hold;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_profile.rs"]
mod source_profile;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_runtime.rs"]
mod source_runtime;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_shutdown_validation.rs"]
mod source_shutdown_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_stage_validation.rs"]
mod source_stage_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
mod source_unbind_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "native_preview_host/source_validation.rs"]
mod source_validation;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/stream_reference.rs"]
#[allow(dead_code)]
mod stream_reference;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/streaming_decoder.rs"]
#[allow(dead_code)]
mod streaming_decoder;
#[cfg(all(windows, feature = "native-preview-host"))]
#[path = "../examples/support/wasapi_output.rs"]
#[allow(dead_code)]
mod wasapi_output;

#[cfg(all(windows, feature = "native-preview-host"))]
mod platform {
    use super::child::{ChildSurface, PhysicalRect};
    use super::*;
    use std::cell::RefCell;
    struct Session {
        id: String,
        revision: u64,
        child: Option<ChildSurface>,
        last_request: Option<LayoutRequest>,
        last_cleanup: Option<Value>,
        error: Option<String>,
        invalidations: u64,
        history: Vec<Value>,
        media: Option<std::sync::Weak<media_sink::MainThreadSink>>,
    }
    impl Session {
        fn new() -> Self {
            Self {
                id: uuid::Uuid::new_v4().to_string(),
                revision: 0,
                child: None,
                last_request: None,
                last_cleanup: None,
                error: None,
                invalidations: 0,
                history: Vec::new(),
                media: None,
            }
        }
        fn record(&mut self, kind: &str, value: Value) {
            if self.history.len() == 64 {
                self.history.remove(0);
            }
            self.history
                .push(json!({"kind":kind,"revision":self.revision,"value":value}));
        }
        fn snapshot(&self, window: &tauri::WebviewWindow) -> Result<Value, String> {
            let inner = window.inner_size().map_err(|e| e.to_string())?;
            Ok(
                json!({"enabled":true,"calibrationOnly":self.media.is_none(),"mediaSessionActive":self.media.is_some(),"sessionId":self.id,"revision":self.revision,"lastRequest":self.last_request,"child":self.child.as_ref().map(ChildSurface::snapshot).transpose()?,"lastCleanup":self.last_cleanup,"error":self.error,"invalidations":self.invalidations,"history":self.history,"parent":{"visible":window.is_visible().map_err(|e|e.to_string())?,"focused":window.is_focused().map_err(|e|e.to_string())?,"minimized":window.is_minimized().map_err(|e|e.to_string())?,"innerWidth":inner.width,"innerHeight":inner.height,"scaleFactor":window.scale_factor().map_err(|e|e.to_string())?}}),
            )
        }
    }
    thread_local! { static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) }; }
    fn with_session<F>(action: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Session) -> Result<Value, String>,
    {
        SESSION.with(|cell| {
            let mut state = cell
                .try_borrow_mut()
                .map_err(|_| "Native preview owner is busy")?;
            action(state.get_or_insert_with(Session::new))
        })
    }
    pub(super) fn status(window: &tauri::WebviewWindow) -> Result<Value, String> {
        with_session(|s| s.snapshot(window))
    }
    pub(super) fn bind_media(
        window: &tauri::WebviewWindow,
        sink: std::sync::Arc<media_sink::MainThreadSink>,
    ) -> Result<Value, String> {
        with_session(|s| {
            require_unbound_media(&s.media)?;
            s.child
                .as_mut()
                .ok_or("Preview surface has not mounted")?
                .begin_media()?;
            s.media = Some(std::sync::Arc::downgrade(&sink));
            s.snapshot(window)
        })
    }
    pub(super) fn present_media(
        window: &tauri::WebviewWindow,
        session: &str,
        surface: &str,
        frame: &media_actor::MediaFrame,
    ) -> Result<(Value, Option<Vec<u8>>), String> {
        let mut result = None;
        with_session(|s| {
            if s.id != session || s.media.as_ref().and_then(|weak| weak.upgrade()).is_none() {
                return Err("Media surface session has expired".into());
            }
            let child = s.child.as_mut().ok_or("Media child has been released")?;
            let snapshot = child.snapshot()?;
            if snapshot["surfaceId"] != surface {
                return Err("Media frame targets a retired child".into());
            }
            if s.last_request.as_ref().is_none_or(|r| r.blocked)
                || snapshot["suspended"] == true
                || window.is_minimized().map_err(|e| e.to_string())?
            {
                result = Some((
                    json!({"blocked":true,"accepted":false,"presentCalled":false,"hresult":null,"diagnosticRequested":frame.diagnostic,"surfaceId":surface}),
                    None,
                ));
            } else {
                result = Some(child.render_media(
                    &frame.rgb,
                    frame.width,
                    frame.height,
                    frame.diagnostic,
                )?);
            }
            Ok(Value::Null)
        })?;
        result.ok_or_else(|| "Media frame result unavailable".into())
    }
    pub(super) fn layout(
        window: &tauri::WebviewWindow,
        request: LayoutRequest,
    ) -> Result<Value, String> {
        with_session(|s| {
            validate_sequence(&s.id, s.revision, &request.session_id, request.sequence)?;
            let dimensions = window.inner_size().map_err(|e| e.to_string())?;
            let scale = window.scale_factor().map_err(|e| e.to_string())?;
            s.revision = request.sequence;
            let operation = (|| {
                let rect = physical_rect(&request, dimensions.width, dimensions.height, scale)?;
                s.last_request = Some(request.clone());
                if request.blocked
                    || rect.width == 0
                    || rect.height == 0
                    || window.is_minimized().map_err(|e| e.to_string())?
                {
                    if let Some(child) = &mut s.child {
                        let result = child.suspend()?;
                        s.record("suspended", result);
                    }
                } else {
                    if s.child.is_none() {
                        let hwnd = window.hwnd().map_err(|e| e.to_string())?;
                        s.child =
                            Some(ChildSurface::new(windows::Win32::Foundation::HWND(hwnd.0))?);
                        if source_enabled() {
                            s.child.as_mut().unwrap().begin_media()?;
                        }
                    }
                    let result = s
                        .child
                        .as_mut()
                        .unwrap()
                        .apply(rect, window.is_visible().map_err(|e| e.to_string())?)?;
                    s.record("layout", result);
                }
                Ok::<(), String>(())
            })();
            if let Err(error) = operation {
                if let Some(child) = &mut s.child {
                    let _ = child.suspend();
                }
                s.error = Some(error.clone());
                s.record("error", json!({"error":error}));
                return Err(error);
            }
            s.error = None;
            s.snapshot(window)
        })
    }
    pub(super) fn release(
        window: &tauri::WebviewWindow,
        id: &str,
        sequence: u64,
    ) -> Result<Value, String> {
        with_session(|s| {
            validate_sequence(&s.id, s.revision, id, sequence)?;
            s.revision = sequence;
            if let Some(sink) = s.media.take().and_then(|weak| weak.upgrade()) {
                sink.close();
            }
            if let Some(mut child) = s.child.take() {
                let closed = child.close();
                s.last_cleanup =
                    Some(json!({"result":closed.as_ref().ok(),"error":closed.as_ref().err()}));
                s.record("release", s.last_cleanup.clone().unwrap());
                closed?;
            }
            s.snapshot(window)
        })
    }
    pub(super) fn invalidate() {
        SESSION.with(|cell| {
            if let Ok(mut state) = cell.try_borrow_mut() {
                if let Some(s) = state.as_mut() {
                    s.invalidations = s.invalidations.saturating_add(1);
                    if let Some(child) = &mut s.child {
                        if let Err(e) = child.suspend() {
                            s.error = Some(e);
                        }
                    }
                }
            }
        });
    }
    pub(super) fn close() {
        SESSION.with(|cell| {
            if let Ok(mut state) = cell.try_borrow_mut() {
                if let Some(s) = state.as_mut() {
                    if let Some(sink) = s.media.take().and_then(|weak| weak.upgrade()) {
                        sink.close();
                    }
                    if let Some(mut child) = s.child.take() {
                        let result = child.close();
                        s.last_cleanup = Some(
                            json!({"result":result.as_ref().ok(),"error":result.as_ref().err()}),
                        );
                        media_validation::record_closed(s.last_cleanup.clone().unwrap());
                        source_validation::record_closed(s.last_cleanup.clone().unwrap());
                        validation::record_closed(s.last_cleanup.clone().unwrap());
                    }
                }
            }
        });
        media_validation::record_closed(
            json!({"result":null,"error":"No owned child remained at final cleanup"}),
        );
        let previous_cleanup = SESSION.with(|cell| {
            cell.try_borrow()
                .ok()
                .and_then(|state| state.as_ref().and_then(|s| s.last_cleanup.clone()))
        });
        source_validation::record_closed(source_window_already_released(previous_cleanup));
        // The validator may fail before a child is constructed. Its pending
        // report must still be published instead of disappearing on exit.
        validation::record_closed(
            json!({"result":null,"error":"No owned child remained at final cleanup","noChildOwned":true}),
        );
    }
    pub(super) fn unbind_media(
        window: &tauri::WebviewWindow,
        sink: std::sync::Arc<media_sink::MainThreadSink>,
    ) -> Result<Value, String> {
        with_session(|s| {
            let result = unbind_media_owner(
                s,
                &sink,
                |s| &mut s.media,
                |s| {
                    if source_faults::unbind_failure() {
                        let snapshot = s.snapshot(window);
                        source_faults::unbind_blocked(&snapshot);
                        return Err(source_faults::UNBIND_ERROR.into());
                    }
                    if let Some(child) = &mut s.child {
                        child.suspend()?;
                    }
                    Ok(())
                },
                |s| s.snapshot(window),
            );
            if result.is_err() {
                source_faults::unbind_retained(&s.snapshot(window));
            }
            result
        })
    }
    fn physical_rect(
        r: &LayoutRequest,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Result<PhysicalRect, String> {
        super::validate_geometry(r, width, height, scale)?;
        let left = (r.x * scale).floor().clamp(0.0, f64::from(width));
        let top = (r.y * scale).floor().clamp(0.0, f64::from(height));
        let right = ((r.x + r.width) * scale)
            .ceil()
            .clamp(left, f64::from(width));
        let bottom = ((r.y + r.height) * scale)
            .ceil()
            .clamp(top, f64::from(height));
        if right - left > 1920.0 || bottom - top > 1080.0 {
            return Err("Native preview surface exceeds supported dimensions".into());
        }
        Ok(PhysicalRect {
            x: left as i32,
            y: top as i32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        })
    }
}

#[cfg(any(test, all(windows, feature = "native-preview-host")))]
fn source_window_already_released(previous: Option<Value>) -> Value {
    let error = previous.as_ref().and_then(|p| p.get("error")).cloned();
    json!({"result":null,"noChildOwned":true,"previousCleanup":previous,"error":error})
}

#[cfg(any(test, all(windows, feature = "native-preview-host")))]
fn require_unbound_media<T>(binding: &Option<std::sync::Weak<T>>) -> Result<(), String> {
    // A failed detach remains owned even after the last strong sink is dropped.
    // Only successful unbinding or full surface release clears that obligation.
    if binding.is_some() {
        Err("Media sink is bound or its release has not been confirmed".into())
    } else {
        Ok(())
    }
}

#[cfg(any(test, all(windows, feature = "native-preview-host")))]
fn unbind_media_owner<S, T, R>(
    state: &mut S,
    sink: &std::sync::Arc<T>,
    binding: fn(&mut S) -> &mut Option<std::sync::Weak<T>>,
    suspend: impl FnOnce(&mut S) -> Result<(), String>,
    snapshot: impl FnOnce(&S) -> Result<R, String>,
) -> Result<R, String> {
    let matches = binding(state)
        .as_ref()
        .is_some_and(|active| std::sync::Weak::ptr_eq(active, &std::sync::Arc::downgrade(sink)));
    if !matches {
        return snapshot(state);
    }
    // Suspend and observe before committing ownership release. In particular,
    // a failing snapshot must not turn the next detach into a no-op success.
    suspend(state)?;
    let previous = binding(state).take();
    match snapshot(state) {
        Ok(value) => Ok(value),
        Err(error) => {
            *binding(state) = previous;
            Err(error)
        }
    }
}

fn validate_geometry(r: &LayoutRequest, width: u32, height: u32, scale: f64) -> Result<(), String> {
    if ![
        r.x,
        r.y,
        r.width,
        r.height,
        r.viewport_width,
        r.viewport_height,
        r.scale_factor,
        scale,
    ]
    .iter()
    .all(|v| v.is_finite() && *v >= 0.0)
        || !(0.5..=4.0).contains(&r.scale_factor)
        || (r.scale_factor - scale).abs() > 0.01
        || r.viewport_width <= 0.0
        || r.viewport_height <= 0.0
        || (r.viewport_width * scale - f64::from(width)).abs() > 2.0
        || (r.viewport_height * scale - f64::from(height)).abs() > 2.0
    {
        return Err("Native preview layout is invalid or belongs to a stale viewport".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Weak};

    #[test]
    fn no_child_at_exit_preserves_previous_release_success_or_failure() {
        let released = json!({"result":{"closed":true,"childExists":false},"error":null});
        let report = source_window_already_released(Some(released.clone()));
        assert_eq!(report["previousCleanup"], released);
        assert!(report["error"].is_null());
        let failed = json!({"result":null,"error":"Detach failed"});
        let report = source_window_already_released(Some(failed.clone()));
        assert_eq!(report["previousCleanup"], failed);
        assert_eq!(report["error"], "Detach failed");
        assert!(source_window_already_released(None)["previousCleanup"].is_null());
    }

    struct MediaOwner {
        binding: Option<Weak<()>>,
        suspend_calls: usize,
        suspend_error: bool,
        snapshot_error: bool,
    }
    impl MediaOwner {
        fn new(sink: &Arc<()>) -> Self {
            Self {
                binding: Some(Arc::downgrade(sink)),
                suspend_calls: 0,
                suspend_error: false,
                snapshot_error: false,
            }
        }
        fn unbind(&mut self, sink: &Arc<()>) -> Result<bool, String> {
            unbind_media_owner(
                self,
                sink,
                |owner| &mut owner.binding,
                |owner| {
                    owner.suspend_calls += 1;
                    if owner.suspend_error {
                        Err("suspend failed".into())
                    } else {
                        Ok(())
                    }
                },
                |owner| {
                    if owner.snapshot_error {
                        Err("snapshot failed".into())
                    } else {
                        Ok(owner.binding.is_some())
                    }
                },
            )
        }
    }

    #[test]
    fn media_unbind_suspend_failure_retains_owner_for_successful_retry() {
        let sink = Arc::new(());
        let mut owner = MediaOwner::new(&sink);
        owner.suspend_error = true;
        assert_eq!(owner.unbind(&sink).unwrap_err(), "suspend failed");
        assert!(require_unbound_media(&owner.binding).is_err());
        owner.suspend_error = false;
        assert!(!owner.unbind(&sink).unwrap());
        assert_eq!(owner.suspend_calls, 2);
        assert!(require_unbound_media(&owner.binding).is_ok());
        assert!(!owner.unbind(&sink).unwrap());
        assert_eq!(owner.suspend_calls, 2);
    }

    #[test]
    fn media_unbind_snapshot_failure_retains_unconfirmed_owner() {
        let sink = Arc::new(());
        let mut owner = MediaOwner::new(&sink);
        owner.snapshot_error = true;
        assert_eq!(owner.unbind(&sink).unwrap_err(), "snapshot failed");
        assert!(require_unbound_media(&owner.binding).is_err());
        owner.snapshot_error = false;
        assert!(!owner.unbind(&sink).unwrap());
        assert_eq!(owner.suspend_calls, 2);
    }

    #[test]
    fn media_unbind_expired_unconfirmed_owner_still_blocks_new_binding() {
        let sink = Arc::new(());
        let mut owner = MediaOwner::new(&sink);
        owner.suspend_error = true;
        assert!(owner.unbind(&sink).is_err());
        drop(sink);
        assert!(require_unbound_media(&owner.binding).is_err());
        assert!(owner.binding.as_ref().unwrap().upgrade().is_none());
        let replacement = Arc::new(());
        assert!(owner.unbind(&replacement).unwrap());
        assert_eq!(owner.suspend_calls, 1);
    }

    #[test]
    fn media_unbind_old_sink_never_suspends_or_clears_current_owner() {
        let old = Arc::new(());
        let current = Arc::new(());
        let mut owner = MediaOwner::new(&current);
        owner.suspend_error = true;
        assert!(owner.unbind(&old).unwrap());
        assert_eq!(owner.suspend_calls, 0);
        assert!(Weak::ptr_eq(
            owner.binding.as_ref().unwrap(),
            &Arc::downgrade(&current)
        ));
        assert!(require_unbound_media(&owner.binding).is_err());
    }

    #[test]
    fn commands_reject_old_sessions_reordered_and_unsafe_sequences() {
        assert!(validate_sequence("new", 7, "old", 8).is_err());
        assert!(validate_sequence("new", 7, "new", 7).is_err());
        assert!(validate_sequence("new", 7, "new", 6).is_err());
        assert!(validate_sequence("new", 7, "new", 9_007_199_254_740_992).is_err());
        assert!(validate_sequence("new", 7, "new", 8).is_ok());
    }
    #[test]
    fn viewport_validation_rejects_stale_scale_and_nonfinite_values() {
        let mut r = LayoutRequest {
            session_id: "a".into(),
            sequence: 1,
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 100.0,
            scale_factor: 2.0,
            blocked: false,
            viewport_width: 1280.0,
            viewport_height: 820.0,
        };
        assert!(validate_geometry(&r, 2560, 1640, 2.0).is_ok());
        assert!(validate_geometry(&r, 2360, 1520, 2.0).is_err());
        assert!(validate_geometry(&r, 2560, 1640, 1.5).is_err());
        r.width = f64::NAN;
        assert!(validate_geometry(&r, 2560, 1640, 2.0).is_err());
        r.width = -1.0;
        assert!(validate_geometry(&r, 2560, 1640, 2.0).is_err());
    }
}
