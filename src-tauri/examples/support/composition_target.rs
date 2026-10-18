//! Borrowed-window DirectComposition attachment. Commit only submits a visual
//! tree transaction; it is not a scanout or screen-visibility measurement.

use serde::Serialize;
use std::{
    marker::PhantomData,
    rc::Rc,
    thread::{self, ThreadId},
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::{
            DirectComposition::{
                DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget,
                IDCompositionVisual,
            },
            Dxgi::{IDXGIDevice, IDXGISwapChain1},
        },
        UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow},
    },
    core::{HRESULT, IUnknown, Interface},
};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingInfo {
    /// True only after SetContent, SetRoot and Commit returned success. If a
    /// later detach fails it stays true to avoid claiming proven detachment.
    /// It says nothing about whether this window is visible or scanned out.
    pub attached: bool,
    pub window_handle: Option<String>,
    pub attach_commit_submitted: bool,
    pub attach_commit_hresult: Option<i32>,
    pub detach_attempted: bool,
    pub detach_commit_hresult: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachResult {
    pub binding: BindingInfo,
    pub commit_submitted: bool,
    pub commit_hresult: i32,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachResult {
    pub was_attached: bool,
    pub commit_submitted: bool,
    pub commit_hresult: Option<i32>,
    pub elapsed_ms: f64,
}

impl DetachResult {
    pub fn already_detached() -> Self {
        Self {
            was_attached: false,
            commit_submitted: false,
            commit_hresult: None,
            elapsed_ms: 0.0,
        }
    }
}

/// The HWND is borrowed. The caller must keep it alive through explicit detach
/// and destruction of this object. No window creation, display, focus, message
/// pump or DestroyWindow call is performed here. All COM references stay on
/// the creating thread, including release and best-effort Drop cleanup.
pub(super) struct CompositionTarget {
    // Release the root-owning target and its content-owning visual before the
    // DirectComposition device. The presenter then releases swap chain/D3D.
    target: IDCompositionTarget,
    visual: IDCompositionVisual,
    device: IDCompositionDevice,
    window: HWND,
    owner: ThreadId,
    state: BindingState,
    requires_detach: bool,
    _not_send: PhantomData<Rc<()>>,
}

impl CompositionTarget {
    pub(super) fn attach(
        dxgi: &IDXGIDevice,
        chain: &IDXGISwapChain1,
        window: HWND,
    ) -> Result<(Self, AttachResult), String> {
        let started = Instant::now();
        let mut process = 0;
        let exists = !window.0.is_null() && unsafe { IsWindow(Some(window)).as_bool() };
        if exists {
            unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
        }
        validate_window_identity(!window.0.is_null(), exists, process, std::process::id())?;

        // This is the very same DXGI device queried from the renderer's D3D11
        // device, not a second adapter/device or a software fallback.
        let device: IDCompositionDevice = unsafe { DCompositionCreateDevice(dxgi) }
            .map_err(|error| format!("DCompositionCreateDevice: {error}"))?;
        let target = unsafe { device.CreateTargetForHwnd(window, false) }
            .map_err(|error| format!("CreateTargetForHwnd: {error}"))?;
        let visual =
            unsafe { device.CreateVisual() }.map_err(|error| format!("CreateVisual: {error}"))?;
        let mut binding = Self {
            target,
            visual,
            device,
            window,
            owner: thread::current().id(),
            state: BindingState::default(),
            requires_detach: true,
            _not_send: PhantomData,
        };
        let result: Result<i32, String> = (|| {
            unsafe { binding.visual.SetContent(chain) }
                .map_err(|error| format!("SetContent swap chain: {error}"))?;
            unsafe { binding.target.SetRoot(&binding.visual) }
                .map_err(|error| format!("SetRoot visual: {error}"))?;
            let status = commit(&binding.device);
            binding.state.attach_commit_hresult = Some(status.0);
            require_commit_accepted(status, "attachment")?;
            Ok(status.0)
        })();
        let commit_hresult = match result {
            Ok(status) => {
                binding.state.finish_attach(Ok(()))?;
                status
            }
            Err(primary) => {
                let _ = binding.state.finish_attach(Err(primary.clone()));
                let cleanup = binding.detach().map(|_| ());
                return Err(preserve_primary_error(primary, cleanup));
            }
        };
        let result = AttachResult {
            binding: binding.info(),
            commit_submitted: true,
            commit_hresult,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        Ok((binding, result))
    }

    pub(super) fn info(&self) -> BindingInfo {
        BindingInfo {
            attached: self.state.attached,
            window_handle: Some(format!("0x{:X}", self.window.0 as usize)),
            attach_commit_submitted: self.state.attach_commit_submitted,
            attach_commit_hresult: self.state.attach_commit_hresult,
            detach_attempted: self.state.detach_attempted,
            detach_commit_hresult: self.state.detach_commit_hresult,
            error: self.state.error.clone(),
        }
    }

    pub(super) fn detach(&mut self) -> Result<DetachResult, String> {
        if thread::current().id() != self.owner {
            return Err("DirectComposition target must stay on its creating thread".into());
        }
        if !self.requires_detach {
            return Ok(DetachResult::already_detached());
        }
        let started = Instant::now();
        let was_attached = self.state.attached;
        // Do not short-circuit: even a root-detach failure must still attempt
        // to clear swap-chain content and submit the cleanup transaction.
        let mut errors = Vec::new();
        if let Err(error) = unsafe { self.target.SetRoot(None::<&IDCompositionVisual>) } {
            errors.push(format!("SetRoot(None): {error}"));
        }
        if let Err(error) = unsafe { self.visual.SetContent(None::<&IUnknown>) } {
            errors.push(format!("SetContent(None): {error}"));
        }
        let status = commit(&self.device);
        self.state.detach_commit_hresult = Some(status.0);
        if let Err(error) = require_commit_accepted(status, "detachment") {
            errors.push(error);
        }
        let result = if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        };
        self.state.finish_detach(result)?;
        self.requires_detach = false;
        Ok(DetachResult {
            was_attached,
            commit_submitted: true,
            commit_hresult: Some(status.0),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

fn commit(device: &IDCompositionDevice) -> HRESULT {
    // The windows-rs Result<()> projection discards nonzero success statuses.
    // Preserve the actual status; Commit acceptance still is not scanout.
    unsafe { (Interface::vtable(device).Commit)(Interface::as_raw(device)) }
}

fn require_commit_accepted(status: HRESULT, operation: &str) -> Result<(), String> {
    if status.is_err() {
        Err(format!(
            "Commit {operation} failed with HRESULT 0x{:08X}",
            status.0 as u32
        ))
    } else {
        Ok(())
    }
}

impl Drop for CompositionTarget {
    fn drop(&mut self) {
        // Explicit detach is required when diagnostics matter. Drop cannot
        // report failure and is not evidence of COM/VRAM leak freedom.
        let _ = self.detach();
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct BindingState {
    attached: bool,
    attach_commit_submitted: bool,
    attach_commit_hresult: Option<i32>,
    detach_attempted: bool,
    detach_commit_hresult: Option<i32>,
    error: Option<String>,
}

impl BindingState {
    fn finish_attach(&mut self, result: Result<(), String>) -> Result<(), String> {
        self.error = result.as_ref().err().cloned();
        if result.is_ok() {
            self.attached = true;
            self.attach_commit_submitted = true;
        }
        result
    }

    fn finish_detach(&mut self, result: Result<(), String>) -> Result<(), String> {
        self.detach_attempted = true;
        self.error = result.as_ref().err().cloned();
        if result.is_ok() {
            self.attached = false;
        }
        result
    }
}

fn validate_window_identity(
    non_null: bool,
    exists: bool,
    window_process: u32,
    current_process: u32,
) -> Result<(), String> {
    if !non_null || !exists {
        return Err("Composition target requires a non-null live HWND".into());
    }
    if window_process == 0 || window_process != current_process {
        return Err("Composition target HWND must belong to the current process".into());
    }
    Ok(())
}

pub(crate) fn allow_attach(has_target: bool) -> Result<(), String> {
    if has_target {
        return Err("Composition presenter already owns a window target; detach it first".into());
    }
    Ok(())
}

fn preserve_primary_error(primary: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => primary,
        Err(error) => format!("{primary}; attachment rollback also failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_is_reported_only_after_commit_is_accepted() {
        let mut state = BindingState::default();
        assert!(state.finish_attach(Err("commit failed".into())).is_err());
        assert!(!state.attached);
        assert!(!state.attach_commit_submitted);
        assert_eq!(state.error.as_deref(), Some("commit failed"));
        state.finish_attach(Ok(())).unwrap();
        assert!(state.attached);
        assert!(state.attach_commit_submitted);
        assert!(state.error.is_none());
    }

    #[test]
    fn failed_detach_keeps_attachment_uncertain_until_retry_succeeds() {
        let mut state = BindingState::default();
        state.finish_attach(Ok(())).unwrap();
        assert!(
            state
                .finish_detach(Err("detach commit failed".into()))
                .is_err()
        );
        assert!(state.attached);
        assert!(state.detach_attempted);
        assert_eq!(state.error.as_deref(), Some("detach commit failed"));
        state.finish_detach(Ok(())).unwrap();
        assert!(!state.attached);
        assert!(state.error.is_none());
    }

    #[test]
    fn only_valid_non_null_same_process_windows_are_accepted() {
        assert!(validate_window_identity(true, true, 42, 42).is_ok());
        assert!(validate_window_identity(false, true, 42, 42).is_err());
        assert!(validate_window_identity(true, false, 42, 42).is_err());
        assert!(validate_window_identity(true, true, 0, 42).is_err());
        assert!(validate_window_identity(true, true, 43, 42).is_err());
    }

    #[test]
    fn duplicate_attachment_is_rejected_without_replacing_a_live_target() {
        assert!(allow_attach(false).is_ok());
        assert!(allow_attach(true).is_err());
    }

    #[test]
    fn rollback_failure_never_overwrites_primary_attach_failure() {
        assert_eq!(
            preserve_primary_error("attach failed".into(), Ok(())),
            "attach failed"
        );
        let error = preserve_primary_error("attach failed".into(), Err("cleanup failed".into()));
        assert!(error.starts_with("attach failed"));
        assert!(error.contains("cleanup failed"));
    }
}
