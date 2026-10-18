//! An owned child HWND for the opt-in native-preview diagnostic. The borrowed
//! Tauri parent and all graphics operations must remain on its event-loop thread.
//! This draws a calibration frame on geometry changes, not video playback.

#[path = "geometry.rs"]
mod geometry;

use super::{
    composition_presenter::{CompositionPresenter, PresentDisposition},
    frame_verification,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::MapWindowPoints,
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

fn child_styles() -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    (
        WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
        WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
    )
}

/// Heap address remains stable until after DestroyWindow/WM_NCDESTROY. The
/// window procedure never borrows the root's thread-local host container.
#[derive(Default)]
struct WindowState {
    presenter: RefCell<Option<CompositionPresenter>>,
    destroying: Cell<bool>,
    nc_destroyed: Cell<bool>,
    auto_cleanup_triggered: Cell<bool>,
    cleanup_before_nc_destroy: Cell<bool>,
    cleanup_deferred: Cell<bool>,
    release_in_progress: Cell<bool>,
    detach_result: RefCell<Option<Value>>,
    detach_error: RefCell<Option<String>>,
}

impl WindowState {
    fn release_graphics(&self, automatic: bool) -> Result<(), String> {
        // Take before calling COM: synchronous destruction must never borrow
        // the presenter recursively, and there can only be one dropping owner.
        let mut presenter = match self.presenter.try_borrow_mut() {
            Ok(mut slot) => slot.take(),
            Err(_) => {
                self.cleanup_deferred.set(true);
                return Err("Native preview graphics cleanup deferred during reentry".into());
            }
        };
        let Some(mut presenter) = presenter.take() else {
            return Ok(());
        };
        if automatic {
            self.auto_cleanup_triggered.set(true);
        }
        self.release_in_progress.set(true);
        let detached = presenter.detach_from_window();
        let result = match detached {
            Ok(evidence) => {
                if let Ok(mut slot) = self.detach_result.try_borrow_mut() {
                    *slot = Some(json!(evidence));
                }
                Ok(())
            }
            Err(error) => {
                if let Ok(mut slot) = self.detach_error.try_borrow_mut() {
                    *slot = Some(error.clone());
                }
                Err(error)
            }
        };
        // Drop still releases owned COM references if detach reports failure.
        drop(presenter);
        self.release_in_progress.set(false);
        self.cleanup_deferred.set(false);
        result
    }
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let creation = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, creation.lpCreateParams as isize) };
    }
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
    match message {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_CLOSE => LRESULT(0),
        WM_DESTROY => {
            if let Some(state) = unsafe { state_ptr.as_ref() } {
                state.destroying.set(true);
                let _ = state.release_graphics(true);
                state.cleanup_before_nc_destroy.set(
                    state
                        .presenter
                        .try_borrow()
                        .is_ok_and(|slot| slot.is_none())
                        && !state.release_in_progress.get(),
                );
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_NCDESTROY => {
            if let Some(state) = unsafe { state_ptr.as_ref() } {
                let _ = state.release_graphics(true);
                state.nc_destroyed.set(true);
            }
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

pub struct ChildSurface {
    media_mode: bool,
    parent: HWND,
    child: HWND,
    instance: HINSTANCE,
    class: Vec<u16>,
    registered: bool,
    owner_thread: u32,
    state: Box<WindowState>,
    last_rect: Option<PhysicalRect>,
    suspended: bool,
    closed: bool,
    present_calls: u64,
    resize_calls: u64,
    last_frame: Option<Value>,
    last_apply: Option<Value>,
    attach: Option<Value>,
    _not_send: PhantomData<Rc<()>>,
}

impl ChildSurface {
    pub fn new(parent: HWND) -> Result<Self, String> {
        let owner_thread = unsafe { GetCurrentThreadId() };
        let mut process = 0;
        if !unsafe { IsWindow(Some(parent)) }.as_bool()
            || unsafe { GetWindowThreadProcessId(parent, Some(&mut process)) } != owner_thread
            || process != std::process::id()
        {
            return Err(
                "Native preview parent must be a live window on the current process and thread"
                    .into(),
            );
        }
        let instance = HINSTANCE(
            unsafe { GetModuleHandleW(None) }
                .map_err(|e| e.to_string())?
                .0,
        );
        let class: Vec<u16> = format!("KaiCut.NativePreviewChild.{}", uuid::Uuid::new_v4())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let definition = WNDCLASSW {
            lpfnWndProc: Some(child_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&definition) } == 0 {
            return Err(format!(
                "Register native preview class: {}",
                windows::core::Error::from_thread()
            ));
        }
        let state = Box::<WindowState>::default();
        let (style, ex_style) = child_styles();
        let created = unsafe {
            CreateWindowExW(
                ex_style,
                PCWSTR(class.as_ptr()),
                w!("KaiCut native preview diagnostic"),
                style,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(instance),
                Some(state.as_ref() as *const WindowState as *const _),
            )
        };
        let child = match created {
            Ok(child) => child,
            Err(error) => {
                let cleanup = unsafe { UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance)) };
                return Err(format!(
                    "Create native preview child: {error}; unregister: {:?}",
                    cleanup.err()
                ));
            }
        };
        let mut surface = Self {
            media_mode: false,
            parent,
            child,
            instance,
            class,
            registered: true,
            owner_thread,
            state,
            last_rect: None,
            suspended: false,
            closed: false,
            present_calls: 0,
            resize_calls: 0,
            last_frame: None,
            last_apply: None,
            attach: None,
            _not_send: PhantomData,
        };
        *surface.state.presenter.borrow_mut() = Some(CompositionPresenter::new(1, 1)?);
        let attached = {
            let mut guard = surface.state.presenter.borrow_mut();
            guard
                .as_mut()
                .ok_or("Native preview presenter has been released")?
                .attach_to_window(child)
        };
        if surface.state.destroying.get() || surface.state.nc_destroyed.get() {
            let cleanup = surface.state.release_graphics(true);
            return Err(format!(
                "Native preview window was destroyed during attachment; cleanup: {:?}",
                cleanup.err()
            ));
        }
        surface.attach = Some(json!(attached?));
        surface.require_live()?;
        Ok(surface)
    }

    fn check_thread(&self) -> Result<(), String> {
        if unsafe { GetCurrentThreadId() } != self.owner_thread {
            Err("Native preview operations must run on the creating Tauri event-loop thread".into())
        } else {
            Ok(())
        }
    }

    fn child_exists(&self) -> bool {
        !self.state.nc_destroyed.get()
            && unsafe { IsWindow(Some(self.child)) }.as_bool()
            && unsafe { GetWindowLongPtrW(self.child, GWLP_USERDATA) }
                == self.state.as_ref() as *const WindowState as isize
    }

    fn require_live(&self) -> Result<(), String> {
        self.check_thread()?;
        if self.closed
            || self.state.destroying.get()
            || !self.child_exists()
            || !unsafe { IsWindow(Some(self.parent)) }.as_bool()
            || unsafe { GetParent(self.child) }.ok() != Some(self.parent)
        {
            return Err("Native preview child or its borrowed parent has been closed".into());
        }
        Ok(())
    }

    pub fn apply(&mut self, requested: PhysicalRect, show: bool) -> Result<Value, String> {
        self.require_live()?;
        let mut parent_rect = RECT::default();
        unsafe { GetClientRect(self.parent, &mut parent_rect) }.map_err(|e| e.to_string())?;
        let (x, y, width, height) = geometry::clipped_rect(
            requested.x,
            requested.y,
            requested.width,
            requested.height,
            parent_rect.right.max(0) as u32,
            parent_rect.bottom.max(0) as u32,
        )?;
        let clipped = PhysicalRect {
            x,
            y,
            width,
            height,
        };
        if width == 0 || height == 0 {
            self.last_apply = Some(json!({"requested":requested,"clipped":clipped,
                "showRequested":show,"showAllowed":false,"calibrationDrawn":false}));
            return self.suspend();
        }
        let geometry_changed = self.last_rect != Some(clipped);
        let must_draw = geometry_changed || self.suspended || self.last_frame.is_none();
        // Hide before resize/draw so an uninitialized surface is never exposed.
        if must_draw || !show {
            unsafe {
                let _ = ShowWindow(self.child, SW_HIDE);
            }
        }
        unsafe {
            SetWindowPos(
                self.child,
                Some(HWND_TOP),
                x,
                y,
                width as i32,
                height as i32,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        }
        .map_err(|e| format!("Position native preview child: {e}"))?;
        if must_draw && self.media_mode {
            let mut guard = self
                .state
                .presenter
                .try_borrow_mut()
                .map_err(|_| "Native preview presenter is busy")?;
            let presenter = guard
                .as_mut()
                .ok_or("Native preview presenter has been released")?;
            let result = presenter.resize(width, height)?;
            self.resize_calls += u64::from(result.resize_buffers_called);
            self.last_rect = Some(clipped);
            self.suspended = false;
        }
        if must_draw && !self.media_mode {
            let rgb = frame_verification::calibration_rgb(width, height)?;
            let rendered = (|| {
                let mut guard = self
                    .state
                    .presenter
                    .try_borrow_mut()
                    .map_err(|_| "Native preview presenter is busy")?;
                let presenter = guard
                    .as_mut()
                    .ok_or("Native preview presenter has been released")?;
                let current = presenter.composition_info();
                if current.width != width || current.height != height || self.suspended {
                    let result = presenter.resize(width, height)?;
                    self.resize_calls += u64::from(result.resize_buffers_called);
                }
                presenter.render_and_present(&rgb, true)
            })();
            // A COM call may dispatch destruction while the presenter is
            // borrowed. Release it immediately after that borrow ends, report
            // failure, and retain cleanupBeforeNcDestroy=false as evidence.
            if self.state.destroying.get() || self.state.nc_destroyed.get() {
                let cleanup = self.state.release_graphics(true);
                return Err(format!(
                    "Native preview window was destroyed during graphics work; cleanup: {:?}",
                    cleanup.err()
                ));
            }
            let result = rendered?;
            self.present_calls += u64::from(result.present_called);
            let readback = result
                .diagnostic
                .as_ref()
                .ok_or("Calibration readback is missing")?;
            let verified =
                frame_verification::verify_readback(&rgb, &readback.rgba, width, height)?;
            self.last_frame = Some(json!({
                "kind":"geometryCalibration", "width":width,"height":height,
                "presentCalled":result.present_called,"hresult":result.hresult,
                "disposition":result.disposition,"uploadDrawMs":result.upload_draw_ms,
                "presentCallMs":result.present_call_ms,"rowPitch":readback.row_pitch,
                "inputRgbSha256":verified.input_rgb_sha256,
                "expectedRgbaSha256":verified.expected_rgba_sha256,
                "actualRgbaSha256":verified.actual_rgba_sha256,
                "mismatchedPixels":verified.mismatched_pixels,
                "maxChannelError":verified.max_channel_error,
                "alphaErrors":verified.alpha_errors,"exact":verified.exact,
                "displayedPixelsProven":false
            }));
            if !verified.exact {
                return Err("Native preview calibration pixels do not match".into());
            }
            if !result.present_called || result.disposition != PresentDisposition::Accepted {
                return Err(format!(
                    "Native preview calibration was not accepted for presentation: {:?}, HRESULT {:?}",
                    result.disposition, result.hresult
                ));
            }
            self.last_rect = Some(clipped);
            self.suspended = false;
        }
        self.require_live()?;
        let show_allowed = geometry::may_show(
            show,
            unsafe { IsWindowVisible(self.parent) }.as_bool(),
            unsafe { IsIconic(self.parent) }.as_bool(),
            !self.media_mode,
        );
        unsafe {
            let _ = ShowWindow(
                self.child,
                if show_allowed {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_HIDE
                },
            );
        }
        self.last_apply = Some(json!({"requested":requested,"clipped":clipped,
            "showRequested":show,"showAllowed":show_allowed,"geometryChanged":geometry_changed,
            "calibrationDrawn":must_draw && !self.media_mode}));
        self.snapshot()
    }

    pub fn suspend(&mut self) -> Result<Value, String> {
        self.require_live()?;
        unsafe {
            let _ = ShowWindow(self.child, SW_HIDE);
        }
        if !self.suspended {
            let mut guard = self
                .state
                .presenter
                .try_borrow_mut()
                .map_err(|_| "Native preview presenter is busy")?;
            if let Some(presenter) = guard.as_mut() {
                presenter.resize(0, 0)?;
            }
            self.suspended = true;
        }
        self.snapshot()
    }

    pub fn begin_media(&mut self) -> Result<(), String> {
        self.require_live()?;
        self.media_mode = true;
        Ok(())
    }

    pub fn render_media(
        &mut self,
        rgb: &[u8],
        source_width: u32,
        source_height: u32,
        diagnostic: bool,
    ) -> Result<(Value, Option<Vec<u8>>), String> {
        use super::composition_presenter::{fit_rect, nearest_letterbox_rgba};
        use sha2::{Digest, Sha256};
        self.require_live()?;
        if !self.media_mode || self.suspended {
            return Err("Native media surface is not ready".into());
        }
        let rect = self
            .last_rect
            .ok_or("Native media surface has no confirmed layout")?;
        let rendered = {
            let mut slot = self
                .state
                .presenter
                .try_borrow_mut()
                .map_err(|_| "Native presenter is busy")?;
            slot.as_mut()
                .ok_or("Native presenter has been released")?
                .render_source_and_present(rgb, source_width, source_height, diagnostic)
        };
        if self.state.destroying.get() || self.state.nc_destroyed.get() {
            let cleanup = self.state.release_graphics(true);
            return Err(format!(
                "Media surface destroyed during rendering: {:?}",
                cleanup.err()
            ));
        }
        let result = rendered?;
        self.present_calls += u64::from(result.present_called);
        let fit = fit_rect(source_width, source_height, rect.width, rect.height)?;
        let mut evidence = json!({"kind":"decodedMedia","sourceWidth":source_width,"sourceHeight":source_height,"width":rect.width,"height":rect.height,"fit":{"x":fit.x,"y":fit.y,"width":fit.width,"height":fit.height},"diagnosticRequested":diagnostic,"presentCalled":result.present_called,"hresult":result.hresult,"disposition":result.disposition,"accepted":result.disposition==PresentDisposition::Accepted,"inputRgbSha256":format!("{:x}",Sha256::digest(rgb)),"uploadDrawMs":result.upload_draw_ms,"presentCallMs":result.present_call_ms,"surfaceId":String::from_utf16_lossy(&self.class[..self.class.len()-1]),"actualRect":rect,"screenDisplayed":false});
        let rgba = if let Some(readback) = result.diagnostic {
            let expected =
                nearest_letterbox_rgba(rgb, source_width, source_height, rect.width, rect.height)?;
            let exact = expected == readback.rgba;
            evidence["readback"] = json!({"exact":exact,"expectedRgbaSha256":format!("{:x}",Sha256::digest(&expected)),"actualRgbaSha256":format!("{:x}",Sha256::digest(&readback.rgba)),"rowPitch":readback.row_pitch,"bytes":readback.rgba.len(),"readbackWaitMs":readback.readback_wait_ms});
            self.last_frame = Some(evidence.clone());
            if !exact {
                return Err("Decoded media backbuffer differs from fitted source".into());
            }
            Some(readback.rgba)
        } else {
            None
        };
        self.last_frame = Some(evidence.clone());
        if result.disposition == PresentDisposition::Accepted
            && unsafe { IsWindowVisible(self.parent) }.as_bool()
            && !unsafe { IsIconic(self.parent) }.as_bool()
        {
            unsafe {
                let _ = ShowWindow(self.child, SW_SHOWNOACTIVATE);
            }
        }
        Ok((evidence, rgba))
    }

    pub fn snapshot(&self) -> Result<Value, String> {
        self.check_thread()?;
        let child_exists = self.child_exists();
        let parent_exists = unsafe { IsWindow(Some(self.parent)) }.as_bool();
        let mut process = 0;
        let child_thread = if child_exists {
            unsafe { GetWindowThreadProcessId(self.child, Some(&mut process)) }
        } else {
            0
        };
        let actual_parent = if child_exists {
            unsafe { GetParent(self.child) }.ok()
        } else {
            None
        };
        let mut actual_rect = None;
        if child_exists && parent_exists {
            let mut rect = RECT::default();
            unsafe { GetWindowRect(self.child, &mut rect) }.map_err(|e| e.to_string())?;
            let mut points = [
                POINT {
                    x: rect.left,
                    y: rect.top,
                },
                POINT {
                    x: rect.right,
                    y: rect.bottom,
                },
            ];
            // A zero return is also valid when the origins coincide.
            unsafe {
                MapWindowPoints(None, Some(self.parent), &mut points);
            }
            actual_rect = Some(PhysicalRect {
                x: points[0].x,
                y: points[0].y,
                width: (points[1].x - points[0].x).max(0) as u32,
                height: (points[1].y - points[0].y).max(0) as u32,
            });
        }
        let previous = if child_exists {
            unsafe { GetWindow(self.child, GW_HWNDPREV) }.ok()
        } else {
            None
        };
        let next = if child_exists {
            unsafe { GetWindow(self.child, GW_HWNDNEXT) }.ok()
        } else {
            None
        };
        let next_thread = next.map(|window| unsafe { GetWindowThreadProcessId(window, None) });
        let (composition, binding) = match self.state.presenter.try_borrow() {
            Ok(guard) => match guard.as_ref() {
                Some(presenter) => (
                    Some(json!(presenter.composition_info())),
                    Some(json!(presenter.binding_info())),
                ),
                None => (None, None),
            },
            Err(_) => {
                return Err("Native preview snapshot requested during graphics mutation".into());
            }
        };
        let mut snapshot = json!({
            "surfaceId":String::from_utf16_lossy(&self.class[..self.class.len()-1]),
            "childHwnd":self.child.0 as usize,"parentHwnd":self.parent.0 as usize,
            "getParentHwnd":actual_parent.map(|h|h.0 as usize),
            "childExists":child_exists,"parentExists":parent_exists,
            "parentVisible":parent_exists && unsafe { IsWindowVisible(self.parent) }.as_bool(),
            "parentIconic":parent_exists && unsafe { IsIconic(self.parent) }.as_bool(),
            "childVisible":child_exists && unsafe { IsWindowVisible(self.child) }.as_bool(),
            "childProcessId":process,"childThreadId":child_thread,
            "currentThreadId":unsafe { GetCurrentThreadId() },
            "processSame":child_exists && process==std::process::id(),
            "threadSame":child_exists && child_thread==self.owner_thread,
            "parentMatches":actual_parent==Some(self.parent),
            "style":if child_exists {(unsafe {GetWindowLongW(self.child,GWL_STYLE)}) as u32} else {0},
            "exStyle":if child_exists {(unsafe {GetWindowLongW(self.child,GWL_EXSTYLE)}) as u32} else {0},
            "dpi":if child_exists {unsafe {GetDpiForWindow(self.child)}} else {0},
            "actualRect":actual_rect,"lastAppliedRect":self.last_rect,
        });
        let surface = json!({
            "previousSiblingHwnd":previous.map(|h|h.0 as usize),
            "nextSiblingHwnd":next.map(|h|h.0 as usize),
            "nextSiblingThreadId":next_thread,
            "nextSiblingSameThread":next_thread==Some(self.owner_thread),
            "isTopSibling":child_exists && previous.is_none(),
            "hitTestResult":if child_exists {Some(unsafe {SendMessageW(self.child,WM_NCHITTEST,None,None)}.0)} else {None},
            "mouseActivateResult":if child_exists {Some(unsafe {SendMessageW(self.child,WM_MOUSEACTIVATE,None,None)}.0)} else {None},
            "inputProof":"owned-window procedure responses only; no user input dispatched",
            "presentCalls":self.present_calls,"resizeBuffersCalls":self.resize_calls,
            "suspended":self.suspended,"closed":self.closed,
            "composition":composition,"binding":binding,"attach":self.attach,
            "lastFrame":self.last_frame,"lastApply":self.last_apply,
            "lifecycle":{
                "destroying":self.state.destroying.get(),"ncDestroyed":self.state.nc_destroyed.get(),
                "autoCleanupTriggered":self.state.auto_cleanup_triggered.get(),
                "cleanupBeforeNcDestroy":self.state.cleanup_before_nc_destroy.get(),
                "cleanupDeferred":self.state.cleanup_deferred.get(),
                "releaseInProgress":self.state.release_in_progress.get(),
                "detach":self.state.detach_result.try_borrow().ok().and_then(|v|v.clone()),
                "detachError":self.state.detach_error.try_borrow().ok().and_then(|v|v.clone()),
                "classUnregistered":!self.registered
            },
            "displayedPixelsProven":false,"videoPlayback":false
        });
        snapshot
            .as_object_mut()
            .ok_or("Native preview window snapshot is not an object")?
            .extend(
                surface
                    .as_object()
                    .ok_or("Native preview surface snapshot is not an object")?
                    .clone(),
            );
        Ok(snapshot)
    }

    pub fn close(&mut self) -> Result<Value, String> {
        self.check_thread()?;
        let graphics = self.state.release_graphics(false);
        if self.child_exists() {
            unsafe {
                let _ = ShowWindow(self.child, SW_HIDE);
            }
            unsafe { DestroyWindow(self.child) }
                .map_err(|e| format!("Destroy native preview child: {e}"))?;
        }
        if self.registered {
            unsafe { UnregisterClassW(PCWSTR(self.class.as_ptr()), Some(self.instance)) }
                .map_err(|e| format!("Unregister native preview class: {e}"))?;
            self.registered = false;
        }
        self.closed = true;
        self.suspended = true;
        graphics?;
        self.snapshot()
    }
}

impl Drop for ChildSurface {
    fn drop(&mut self) {
        let _ = self.close();
        // If window destruction unexpectedly fails, prevent a dangling pointer
        // in its future window procedure. Graphics have already been released.
        if self.child_exists() {
            unsafe {
                SetWindowLongPtrW(self.child, GWLP_USERDATA, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_has_no_initial_visibility_activation_topmost_or_tab_stop() {
        let (style, ex_style) = child_styles();
        assert_eq!(style.0 & (WS_VISIBLE.0 | WS_POPUP.0 | WS_TABSTOP.0), 0);
        assert_eq!(
            style.0 & (WS_CHILD.0 | WS_CLIPSIBLINGS.0 | WS_CLIPCHILDREN.0),
            WS_CHILD.0 | WS_CLIPSIBLINGS.0 | WS_CLIPCHILDREN.0
        );
        assert_ne!(ex_style.0 & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(ex_style.0 & WS_EX_NOREDIRECTIONBITMAP.0, 0);
        assert_eq!(ex_style.0 & (WS_EX_TOPMOST.0 | WS_EX_APPWINDOW.0), 0);
    }
}
