//! Owned hidden top-level HWND, never a desktop automation surface.
use serde::Serialize;
use std::{
    marker::PhantomData,
    rc::Rc,
    thread::{self, ThreadId},
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

fn hidden_styles() -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    (
        WS_POPUP,
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSnapshot {
    pub handle: usize,
    pub exists: bool,
    pub visible: bool,
    pub iconic: bool,
    pub foreground_is_owned: bool,
    pub style: u32,
    pub ex_style: u32,
    pub owner_process: u32,
    pub window_thread: u32,
    pub current_thread: u32,
    pub owner_thread_matches: bool,
    pub client_width: i32,
    pub client_height: i32,
    pub dpi: u32,
}

struct DpiGuard {
    previous: Option<DPI_AWARENESS_CONTEXT>,
}
impl DpiGuard {
    fn enter() -> Result<Self, String> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            return Err(format!(
                "Set thread DPI context: {}",
                windows::core::Error::from_thread()
            ));
        }
        Ok(Self {
            previous: Some(previous),
        })
    }
    fn restore(&mut self) -> Result<(), String> {
        if let Some(previous) = self.previous {
            let result = unsafe { SetThreadDpiAwarenessContext(previous) };
            if result.0.is_null() {
                return Err(format!(
                    "Restore thread DPI context: {}",
                    windows::core::Error::from_thread()
                ));
            }
            self.previous = None;
        }
        Ok(())
    }
}
impl Drop for DpiGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub struct HiddenSurfaceHost {
    hwnd: Option<HWND>,
    original_handle: HWND,
    class: Vec<u16>,
    instance: HINSTANCE,
    registered: bool,
    owner: ThreadId,
    dpi: DpiGuard,
    _not_send: PhantomData<Rc<()>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseResult {
    pub handle: usize,
    pub destroyed: bool,
    pub is_window_after: bool,
    pub class_unregistered: bool,
    pub dpi_restored: bool,
    pub owner_thread_matches: bool,
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Only the owner may destroy this host after graphics are detached.
        WM_CLOSE => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

impl HiddenSurfaceHost {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        valid_extent(width, height)?;
        let dpi = DpiGuard::enter()?;
        let instance = HINSTANCE(
            unsafe { GetModuleHandleW(None) }
                .map_err(|e| e.to_string())?
                .0,
        );
        let class: Vec<u16> = format!("KaiCut.HiddenSurface.{}", uuid::Uuid::new_v4())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let definition = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&definition) } == 0 {
            return Err(format!(
                "Register hidden host class: {}",
                windows::core::Error::from_thread()
            ));
        }
        let (style, ex_style) = hidden_styles();
        let created = unsafe {
            CreateWindowExW(
                ex_style,
                PCWSTR(class.as_ptr()),
                w!("KaiCut surface validation"),
                style,
                0,
                0,
                width as i32,
                height as i32,
                None,
                None,
                Some(instance),
                None,
            )
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(error) => {
                let cleanup = unsafe { UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance)) };
                return Err(format!(
                    "Create hidden host: {error}; unregister: {:?}",
                    cleanup.err()
                ));
            }
        };
        let host = Self {
            hwnd: Some(hwnd),
            original_handle: hwnd,
            class,
            instance,
            registered: true,
            owner: thread::current().id(),
            dpi,
            _not_send: PhantomData,
        };
        host.inspect()?;
        Ok(host)
    }

    pub fn handle(&self) -> Result<HWND, String> {
        self.check_owner()?;
        self.hwnd
            .ok_or_else(|| "Hidden surface host is closed".into())
    }

    fn check_owner(&self) -> Result<(), String> {
        if thread::current().id() != self.owner {
            return Err("Window operations must stay on the creating thread".into());
        }
        Ok(())
    }

    pub fn inspect(&self) -> Result<WindowSnapshot, String> {
        let hwnd = self.handle()?;
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) }.map_err(|e| e.to_string())?;
        let mut process = 0;
        let window_thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process)) };
        let state = WindowSnapshot {
            handle: hwnd.0 as usize,
            exists: unsafe { IsWindow(Some(hwnd)) }.as_bool(),
            visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
            iconic: unsafe { IsIconic(hwnd) }.as_bool(),
            foreground_is_owned: unsafe { GetForegroundWindow() } == hwnd,
            style: unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32,
            ex_style: unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32,
            owner_process: process,
            window_thread,
            current_thread: unsafe { GetCurrentThreadId() },
            owner_thread_matches: thread::current().id() == self.owner
                && window_thread == unsafe { GetCurrentThreadId() },
            client_width: rect.right - rect.left,
            client_height: rect.bottom - rect.top,
            dpi: unsafe { GetDpiForWindow(hwnd) },
        };
        if !state.exists
            || state.visible
            || state.foreground_is_owned
            || state.style & WS_VISIBLE.0 != 0
            || state.ex_style & WS_EX_NOACTIVATE.0 == 0
            || state.owner_process != std::process::id()
            || !state.owner_thread_matches
        {
            return Err(format!("Hidden host invariant failed: {state:?}"));
        }
        Ok(state)
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<WindowSnapshot, String> {
        valid_extent(width, height)?;
        let hwnd = self.handle()?;
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE | SWP_NOOWNERZORDER,
            )
        }
        .map_err(|e| e.to_string())?;
        self.pump()?;
        let state = self.inspect()?;
        if state.client_width != width as i32 || state.client_height != height as i32 {
            return Err("Hidden host client extent differs from physical request".into());
        }
        Ok(state)
    }

    pub fn pump(&self) -> Result<(), String> {
        let hwnd = self.handle()?;
        let mut message = MSG::default();
        for _ in 0..256 {
            if !unsafe { PeekMessageW(&mut message, Some(hwnd), 0, 0, PM_REMOVE) }.as_bool() {
                return Ok(());
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Err("Owned window message queue exceeded diagnostic bound".into())
    }

    /// Caller must detach and drop all graphics references before calling.
    pub fn close(&mut self) -> Result<CloseResult, String> {
        self.check_owner()?;
        if let Some(hwnd) = self.hwnd {
            unsafe { DestroyWindow(hwnd) }.map_err(|e| format!("Destroy owned host: {e}"))?;
            self.hwnd = None;
        }
        let is_window_after = unsafe { IsWindow(Some(self.original_handle)) }.as_bool();
        if is_window_after {
            return Err("Destroyed host still exists".into());
        }
        if self.registered {
            unsafe { UnregisterClassW(PCWSTR(self.class.as_ptr()), Some(self.instance)) }
                .map_err(|e| format!("Unregister owned host class: {e}"))?;
            self.registered = false;
        }
        self.dpi.restore()?;
        Ok(CloseResult {
            handle: self.original_handle.0 as usize,
            destroyed: true,
            is_window_after,
            class_unregistered: !self.registered,
            dpi_restored: self.dpi.previous.is_none(),
            owner_thread_matches: true,
        })
    }
}
impl Drop for HiddenSurfaceHost {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn valid_extent(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > 1920 || height > 1080 {
        return Err("Hidden host extent must be nonzero and at most 1920x1080".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_window_policy_cannot_activate_or_create_a_visible_taskbar_window() {
        let (style, ex) = hidden_styles();
        assert_eq!(style.0 & WS_VISIBLE.0, 0);
        assert_ne!(style.0 & WS_POPUP.0, 0);
        assert_eq!(ex.0 & WS_EX_APPWINDOW.0, 0);
        assert_eq!(ex.0 & WS_EX_TOPMOST.0, 0);
        assert_ne!(ex.0 & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(ex.0 & WS_EX_TOOLWINDOW.0, 0);
        assert_ne!(ex.0 & WS_EX_NOREDIRECTIONBITMAP.0, 0);
    }
}
