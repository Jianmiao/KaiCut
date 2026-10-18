//! Scoped MMCSS participation for the current audio-pump thread only.
//! This never changes process priority, timer resolution, registry or devices.
use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::Threading::{
            AVRT_PRIORITY_NORMAL, AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
            AvSetMmThreadPriority,
        },
    },
    core::w,
};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingInfo {
    pub enabled: bool,
    /// Historical evidence: registration succeeded, even after reverting it.
    pub registered: bool,
    pub active: bool,
    pub reverted: bool,
    pub task_index: Option<u32>,
    pub task_name: Option<&'static str>,
    pub priority: Option<&'static str>,
    pub revert_error: Option<String>,
}

trait SchedulingApi {
    type Handle: Copy;
    fn register(&self) -> Result<(Self::Handle, u32), String>;
    fn normal_priority(&self, handle: Self::Handle) -> Result<(), String>;
    fn revert(&self, handle: Self::Handle) -> Result<(), String>;
}

struct WindowsAvrt;
impl SchedulingApi for WindowsAvrt {
    type Handle = HANDLE;
    fn register(&self) -> Result<(HANDLE, u32), String> {
        let mut index = 0;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut index) }
            .map_err(|error| format!("MMCSS Audio registration failed: {error}"))?;
        Ok((handle, index))
    }
    fn normal_priority(&self, handle: HANDLE) -> Result<(), String> {
        unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_NORMAL) }
            .map_err(|error| format!("MMCSS normal relative priority failed: {error}"))
    }
    fn revert(&self, handle: HANDLE) -> Result<(), String> {
        unsafe { AvRevertMmThreadCharacteristics(handle) }
            .map_err(|error| format!("MMCSS restoration failed: {error}"))
    }
}

struct Registration<A: SchedulingApi> {
    api: A,
    handle: Option<A::Handle>,
    info: SchedulingInfo,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl<A: SchedulingApi> Registration<A> {
    fn enter(enabled: bool, api: A) -> Result<Self, String> {
        let mut scope = Self {
            api,
            handle: None,
            _thread_affinity: PhantomData,
            info: SchedulingInfo {
                enabled,
                registered: false,
                active: false,
                reverted: false,
                task_index: None,
                task_name: None,
                priority: None,
                revert_error: None,
            },
        };
        if !enabled {
            return Ok(scope);
        }
        let (handle, index) = scope.api.register()?;
        // Acquire RAII ownership before any further fallible operation.
        // This is an AVRT registration token, not a CloseHandle-owned object.
        scope.handle = Some(handle);
        scope.info.registered = true;
        scope.info.active = true;
        scope.info.task_index = Some(index);
        scope.info.task_name = Some("Audio");
        if let Err(error) = scope.api.normal_priority(handle) {
            let rollback = scope.finish().err();
            return Err(match rollback {
                Some(rollback) => format!("{error}; cleanup also failed: {rollback}"),
                None => error,
            });
        }
        scope.info.priority = Some("normal");
        Ok(scope)
    }
    fn finish(&mut self) -> Result<(), String> {
        let Some(handle) = self.handle else {
            return Ok(());
        };
        match self.api.revert(handle) {
            Ok(()) => {
                self.handle = None;
                self.info.active = false;
                self.info.reverted = true;
                self.info.revert_error = None;
                Ok(())
            }
            Err(error) => {
                // Preserve the active token so an explicit retry or Drop can
                // try again. Never claim success based merely on an attempt.
                self.info.revert_error = Some(error.clone());
                Err(error)
            }
        }
    }
}

impl<A: SchedulingApi> Drop for Registration<A> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

pub struct ThreadScheduling(Registration<WindowsAvrt>);
impl ThreadScheduling {
    pub fn enter(enabled: bool) -> Result<Self, String> {
        Registration::enter(enabled, WindowsAvrt).map(Self)
    }
    pub fn info(&self) -> &SchedulingInfo {
        &self.0.info
    }
    /// Call explicitly before leaving the audio phase to record any failure.
    /// Drop repeats a failed restoration as a best-effort safety net.
    pub fn finish(&mut self) -> Result<(), String> {
        self.0.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Calls {
        events: Vec<&'static str>,
        register_fails: bool,
        priority_fails: bool,
        revert_failures: usize,
    }
    struct FakeApi(Rc<RefCell<Calls>>);
    impl SchedulingApi for FakeApi {
        type Handle = u32;
        fn register(&self) -> Result<(u32, u32), String> {
            let mut state = self.0.borrow_mut();
            state.events.push("register");
            if state.register_fails {
                Err("registration failed".into())
            } else {
                Ok((17, 91))
            }
        }
        fn normal_priority(&self, handle: u32) -> Result<(), String> {
            assert_eq!(handle, 17);
            let mut state = self.0.borrow_mut();
            state.events.push("normal");
            if state.priority_fails {
                Err("priority failed".into())
            } else {
                Ok(())
            }
        }
        fn revert(&self, handle: u32) -> Result<(), String> {
            assert_eq!(handle, 17);
            let mut state = self.0.borrow_mut();
            state.events.push("revert");
            if state.revert_failures > 0 {
                state.revert_failures -= 1;
                Err("revert failed".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn disabled_scheduling_calls_no_os_api_and_claims_no_registration_or_reversion() {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut scope = Registration::enter(false, FakeApi(calls.clone())).unwrap();
        scope.finish().unwrap();
        assert!(!scope.info.registered && !scope.info.reverted && !scope.info.active);
        assert!(scope.info.task_index.is_none());
        drop(scope);
        assert!(calls.borrow().events.is_empty());
    }
    #[test]
    fn registered_scope_reverts_once_and_retains_success_evidence() {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut scope = Registration::enter(true, FakeApi(calls.clone())).unwrap();
        assert!(scope.info.registered && scope.info.active && !scope.info.reverted);
        assert_eq!(scope.info.task_index, Some(91));
        assert_eq!(scope.info.priority, Some("normal"));
        scope.finish().unwrap();
        scope.finish().unwrap();
        assert!(scope.info.reverted && !scope.info.active);
        drop(scope);
        assert_eq!(calls.borrow().events, ["register", "normal", "revert"]);
    }
    #[test]
    fn failed_reversion_remains_active_and_drop_retries_without_claiming_success() {
        let calls = Rc::new(RefCell::new(Calls {
            revert_failures: 1,
            ..Default::default()
        }));
        let mut scope = Registration::enter(true, FakeApi(calls.clone())).unwrap();
        assert!(scope.finish().is_err());
        assert!(scope.info.active && !scope.info.reverted);
        assert!(
            scope
                .info
                .revert_error
                .as_deref()
                .unwrap()
                .contains("revert failed")
        );
        drop(scope);
        assert_eq!(
            calls.borrow().events,
            ["register", "normal", "revert", "revert"]
        );
    }
    #[test]
    fn registration_and_priority_failures_do_not_leak_an_untracked_mmcss_scope() {
        let calls = Rc::new(RefCell::new(Calls {
            register_fails: true,
            ..Default::default()
        }));
        assert!(Registration::enter(true, FakeApi(calls.clone())).is_err());
        assert_eq!(calls.borrow().events, ["register"]);
        let calls = Rc::new(RefCell::new(Calls {
            priority_fails: true,
            revert_failures: 1,
            ..Default::default()
        }));
        let error = Registration::enter(true, FakeApi(calls.clone()))
            .err()
            .unwrap();
        assert!(error.contains("priority failed") && error.contains("revert failed"));
        assert_eq!(
            calls.borrow().events,
            ["register", "normal", "revert", "revert"]
        );
    }
}
