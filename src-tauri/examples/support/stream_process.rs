//! Owned, hidden pipe processes for the streaming diagnostic only.
use std::{
    io::{BufRead, Read},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub label: String,
    pub pid: u32,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

pub struct PipeProcess {
    child: Mutex<Child>,
    label: String,
    pid: u32,
    stderr: Mutex<Option<JoinHandle<Vec<u8>>>>,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}

pub fn command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(windows)]
fn protect(child: &Child) -> Result<std::os::windows::io::OwnedHandle, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::{
        Win32::{Foundation::HANDLE, System::JobObjects::*},
        core::PCWSTR,
    };
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).map_err(|e| e.to_string())?;
        let owned = OwnedHandle::from_raw_handle(job.0);
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
        .map_err(|e| e.to_string())?;
        AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).map_err(|e| e.to_string())?;
        Ok(owned)
    }
}

fn drain_tail(mut reader: impl Read) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.extend_from_slice(&buffer[..n]);
                if tail.len() > STDERR_LIMIT {
                    tail.drain(..tail.len() - STDERR_LIMIT);
                }
            }
        }
    }
    tail
}

impl PipeProcess {
    pub fn spawn(mut command: Command, label: &str) -> Result<(Arc<Self>, ChildStdout), String> {
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot start {label}: {e}"))?;
        #[cfg(windows)]
        let job = match protect(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Cannot protect {label}: {error}"));
            }
        };
        let pid = child.id();
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Missing decoder stdout".into());
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Missing decoder stderr".into());
        };
        let log = match thread::Builder::new()
            .name(format!("{label}-stderr"))
            .spawn(move || drain_tail(stderr))
        {
            Ok(log) => log,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Cannot drain decoder stderr: {e}"));
            }
        };
        Ok((
            Arc::new(Self {
                child: Mutex::new(child),
                label: label.into(),
                pid,
                stderr: Mutex::new(Some(log)),
                #[cfg(windows)]
                _job: job,
            }),
            stdout,
        ))
    }
    pub fn info(&self) -> Result<ProcessInfo, String> {
        let mut child = match self.child.try_lock() {
            Ok(child) => child,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Ok(ProcessInfo {
                    label: self.label.clone(),
                    pid: self.pid,
                    exited: false,
                    exit_code: None,
                });
            }
            Err(_) => return Err("Decoder process state poisoned".into()),
        };
        let status = child.try_wait().map_err(|e| e.to_string())?;
        Ok(ProcessInfo {
            label: self.label.clone(),
            pid: self.pid,
            exited: status.is_some(),
            exit_code: status.and_then(|value| value.code()),
        })
    }
    pub fn terminate(&self) -> Result<(), String> {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows::Win32::{Foundation::HANDLE, System::JobObjects::TerminateJobObject};
                // Terminate only this instance's private job, including any
                // inherited pipe handles, before joining pipe readers.
                if let Err(error) =
                    unsafe { TerminateJobObject(HANDLE(self._job.as_raw_handle()), 1) }
                {
                    if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                        return Err(format!("Cannot terminate {} job: {error}", self.label));
                    }
                }
            }
            #[cfg(not(windows))]
            if let Err(error) = child.kill() {
                if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                    return Err(format!("Cannot kill {}: {error}", self.label));
                }
            }
        }
        child
            .wait()
            .map_err(|e| format!("Cannot reap {} ({}): {e}", self.label, self.pid))?;
        Ok(())
    }
    pub fn wait_success(&self) -> Result<(), String> {
        let started = Instant::now();
        loop {
            let status = self
                .child
                .lock()
                .map_err(|_| "Decoder process state poisoned")?
                .try_wait()
                .map_err(|e| e.to_string())?;
            if let Some(status) = status {
                if status.success() {
                    return Ok(());
                }
                return Err(format!(
                    "{} exited with {status}: {}",
                    self.label,
                    self.join_stderr()?
                ));
            }
            if started.elapsed() > Duration::from_secs(10) {
                self.terminate()?;
                return Err(format!("{} did not exit after pipe EOF", self.label));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn join_stderr(&self) -> Result<String, String> {
        let handle = self
            .stderr
            .lock()
            .map_err(|_| "Decoder stderr state poisoned")?
            .take();
        match handle {
            Some(handle) => handle
                .join()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|_| "Decoder stderr reader panicked".into()),
            None => Ok(String::new()),
        }
    }
}
impl Drop for PipeProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
        let _ = self.join_stderr();
    }
}

/// Read only bounded metadata; frame timestamps use a separate streaming pipe.
pub fn small_output(command: Command) -> Result<Vec<u8>, String> {
    bounded_output(command, 65536)
}

/// Explicit larger limit for a bounded decoded-frame index. This remains an
/// owned process with the same deadline and stderr drain as small metadata.
pub fn bounded_output(command: Command, limit: usize) -> Result<Vec<u8>, String> {
    if limit == 0 || limit > 8 * 1024 * 1024 {
        return Err("Probe output limit must be within 1..=8 MiB".into());
    }
    let (process, stdout) = PipeProcess::spawn(command, "stream-timebase")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .map_err(|e| e.to_string())
    });
    let waited = process.wait_success();
    if waited.is_err() {
        let _ = process.terminate();
    }
    let bytes = reader.join().map_err(|_| "Metadata reader panicked")??;
    waited?;
    process.join_stderr()?;
    if bytes.len() > limit {
        return Err(format!(
            "Probe metadata exceeds explicit {limit}-byte limit"
        ));
    }
    Ok(bytes)
}

pub fn bounded_output_controlled(
    command: Command,
    limit: usize,
    checkpoint: &dyn Fn() -> Result<(), String>,
    observe: &dyn Fn(serde_json::Value),
) -> Result<Vec<u8>, String> {
    if limit == 0 || limit > 8 * 1024 * 1024 {
        return Err("Probe output limit must be within 1..=8 MiB".into());
    }
    checkpoint()?;
    let started = Instant::now();
    let (process, stdout) = PipeProcess::spawn(command, "source-profile")?;
    observe(serde_json::json!({
        "event":"probe-start", "process":process.info().ok(), "pid":process.pid,
    }));
    let reader = thread::Builder::new()
        .name("source-profile-stdout".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .map_err(|e| e.to_string())
        });
    let mut cancelled = false;
    let mut cancel_observed_process = None;
    let mut cancel_observation_error = None;
    let mut timed_out = false;
    let mut error = reader
        .as_ref()
        .err()
        .map(|error| format!("Cannot read source metadata: {error}"));
    if reader.is_ok() {
        loop {
            if let Err(reason) = checkpoint() {
                cancelled = true;
                match process.info() {
                    Ok(info) => cancel_observed_process = Some(info),
                    Err(error) => cancel_observation_error = Some(error),
                }
                error = Some(reason);
                break;
            }
            let status = process
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .try_wait();
            match status {
                Ok(Some(status)) => {
                    if !status.success() {
                        error = Some(format!("source-profile exited with {status}"));
                    }
                    break;
                }
                Ok(None) => {}
                Err(reason) => {
                    error = Some(format!("Cannot inspect source-profile: {reason}"));
                    break;
                }
            }
            if started.elapsed() >= Duration::from_secs(10) {
                timed_out = true;
                error = Some("source-profile exceeded its 10-second deadline".into());
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    // Every path retains this exact process handle until it has been reaped.
    // Termination cannot target another request or a reused PID. On a normal
    // exit this just waits/reaps; on cancellation it kills the private job.
    let mut cleanup_errors = Vec::new();
    let reaped = match process.terminate() {
        Ok(()) => true,
        Err(reason) => {
            cleanup_errors.push(reason);
            false
        }
    };
    let mut reader_joined = false;
    let mut bytes = Vec::new();
    if let Ok(reader) = reader {
        match reader.join() {
            Ok(result) => {
                reader_joined = true;
                match result {
                    Ok(output) => bytes = output,
                    Err(reason) => {
                        error.get_or_insert(format!("Cannot read source metadata: {reason}"));
                    }
                }
            }
            Err(_) => cleanup_errors.push("Source metadata reader panicked".into()),
        }
    }
    let stderr_joined = match process.join_stderr() {
        Ok(stderr) => {
            if !cancelled && !stderr.is_empty() {
                if let Some(error) = &mut error {
                    error.push_str(": ");
                    error.push_str(&stderr);
                }
            }
            true
        }
        Err(reason) => {
            cleanup_errors.push(reason);
            false
        }
    };
    let info = match process.info() {
        Ok(info) => Some(info),
        Err(reason) => {
            cleanup_errors.push(reason);
            None
        }
    };
    let reaped = reaped && info.as_ref().is_some_and(|info| info.exited);
    if bytes.len() > limit && error.is_none() {
        error = Some(format!(
            "Probe metadata exceeds explicit {limit}-byte limit"
        ));
    }
    observe(serde_json::json!({
        "event":"probe-end", "process":info, "pid":process.pid,
        "readerJoined":reader_joined, "stderrJoined":stderr_joined,
        "reaped":reaped, "cancelled":cancelled, "timedOut":timed_out,
        "cancelObservedRunning":cancel_observed_process.as_ref().is_some_and(|info| info.pid != 0 && !info.exited),
        "cancelObservedProcess":cancel_observed_process, "cancelObservationError":cancel_observation_error,
        "cleanupErrors":cleanup_errors, "error":error,
        "outputBytes":bytes.len(), "elapsedMs":started.elapsed().as_secs_f64()*1000.0,
    }));
    if !cleanup_errors.is_empty() {
        let cleanup = cleanup_errors.join("; ");
        return Err(match error {
            Some(error) => format!("{error}; cleanup: {cleanup}"),
            None => format!("Source probe cleanup failed: {cleanup}"),
        });
    }
    match error {
        Some(error) => Err(error),
        None => Ok(bytes),
    }
}

/// A line allocation can never grow with a malicious or broken PTS pipe.
pub fn bounded_line(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    let read = reader
        .take(4097)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| format!("Cannot read timestamp pipe: {e}"))?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > 4096 {
        return Err("Timestamp line exceeds 4096 bytes".into());
    }
    if bytes.last() != Some(&b'\n') {
        return Err("Truncated timestamp line".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "Timestamp pipe contains invalid UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    // Spawn only this test entry point in a private hidden child. It has no
    // app, media, device, or user-file dependency and normally returns at once.
    #[test]
    fn cancellable_probe_child() {
        if let Ok(mode) = std::env::var("KAICUT_PROBE_CHILD_MODE") {
            use std::io::Write;
            std::io::stdout().write_all(b"probe stdout\n").unwrap();
            std::io::stderr().write_all(b"probe stderr\n").unwrap();
            if mode == "wait" {
                thread::sleep(Duration::from_secs(3));
            } else if mode == "failure" {
                std::process::exit(9);
            } else if mode == "large" {
                std::io::stdout().write_all(&vec![42; 8192]).unwrap();
            }
        }
    }

    fn probe_test_command(mode: &str) -> Command {
        let child_test = format!(
            "{}::cancellable_probe_child",
            module_path!().split_once("::").unwrap().1
        );
        let mut child = command(&std::env::current_exe().unwrap());
        child.args(["--exact", &child_test, "--nocapture"]);
        child.env("KAICUT_PROBE_CHILD_MODE", mode);
        child
    }

    #[test]
    fn cancellable_probe_already_cancelled_does_not_spawn() {
        use std::cell::RefCell;
        let events = RefCell::new(Vec::new());
        let result = bounded_output_controlled(
            probe_test_command("wait"),
            65_536,
            &|| Err("already retired".into()),
            &|value| events.borrow_mut().push(value),
        );
        assert_eq!(result.unwrap_err(), "already retired");
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn cancellable_probe_preserves_output_and_cleans_natural_exit() {
        use std::cell::RefCell;
        let events = RefCell::new(Vec::new());
        let output =
            bounded_output_controlled(probe_test_command("emit"), 65_536, &|| Ok(()), &|value| {
                events.borrow_mut().push(value)
            })
            .unwrap();
        assert!(String::from_utf8(output).unwrap().contains("probe stdout"));
        let events = events.borrow();
        assert_eq!(events[1]["process"]["exitCode"], 0);
        for name in ["readerJoined", "stderrJoined", "reaped"] {
            assert_eq!(events[1][name], true);
        }
        assert_eq!(events[1]["cancelled"], false);
        assert_eq!(events[1]["cancelObservedRunning"], false);
        assert!(events[1]["cancelObservedProcess"].is_null());
        assert_eq!(events[1]["cleanupErrors"], serde_json::json!([]));
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_after_natural_exit_does_not_claim_live_interruption() {
        use std::cell::{Cell, RefCell};
        use windows::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        let cancelled = Cell::new(false);
        let events = RefCell::new(Vec::new());
        let result = bounded_output_controlled(
            probe_test_command("emit"),
            65_536,
            &|| {
                if cancelled.get() {
                    Err("cancel arrived after exit".into())
                } else {
                    Ok(())
                }
            },
            &|value| {
                if value["event"] == "probe-start" {
                    let pid = value["pid"].as_u64().unwrap() as u32;
                    // Wait on this live child, so the cancellation deliberately
                    // arrives after its natural exit, before wrapper polling.
                    unsafe {
                        let handle = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).unwrap();
                        let waited = WaitForSingleObject(handle, 5_000);
                        CloseHandle(handle).unwrap();
                        assert_eq!(waited, WAIT_OBJECT_0);
                    }
                    cancelled.set(true);
                }
                events.borrow_mut().push(value);
            },
        );
        assert_eq!(result.unwrap_err(), "cancel arrived after exit");
        let events = events.borrow();
        assert_eq!(events[1]["cancelled"], true);
        assert_eq!(events[1]["cancelObservedRunning"], false);
        assert_eq!(
            events[1]["cancelObservedProcess"]["pid"],
            events[0]["process"]["pid"]
        );
        assert_eq!(events[1]["cancelObservedProcess"]["exited"], true);
        assert_eq!(events[1]["process"]["exitCode"], 0);
        assert_eq!(events[1]["readerJoined"], true);
        assert_eq!(events[1]["stderrJoined"], true);
        assert_eq!(events[1]["reaped"], true);
    }

    #[test]
    fn cancellable_probe_retains_process_failure_and_output_limit() {
        for (mode, limit) in [("failure", 65_536), ("large", 256)] {
            use std::cell::RefCell;
            let events = RefCell::new(Vec::new());
            let result =
                bounded_output_controlled(probe_test_command(mode), limit, &|| Ok(()), &|value| {
                    events.borrow_mut().push(value)
                });
            let error = result.unwrap_err();
            if mode == "failure" {
                assert!(error.contains("exited"));
                assert!(error.contains("probe stderr"));
            }
            let events = events.borrow();
            assert!(events[1]["outputBytes"].as_u64().unwrap() <= limit as u64 + 1);
            for name in ["readerJoined", "stderrJoined", "reaped"] {
                assert_eq!(events[1][name], true);
            }
            assert_eq!(events[1]["cancelled"], false);
            assert_eq!(events[1]["cleanupErrors"], serde_json::json!([]));
        }
    }

    #[test]
    fn cancellable_probe_stops_owned_child_and_joins_both_readers() {
        use std::cell::{Cell, RefCell};
        let calls = Cell::new(0);
        let events = RefCell::new(Vec::new());
        let started = Instant::now();
        let result = bounded_output_controlled(
            probe_test_command("wait"),
            65_536,
            &|| {
                calls.set(calls.get() + 1);
                if calls.get() >= 4 {
                    Err("superseded while probe was running".into())
                } else {
                    Ok(())
                }
            },
            &|value| events.borrow_mut().push(value),
        );
        assert!(
            result
                .unwrap_err()
                .contains("superseded while probe was running")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let events = events.borrow();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"], "probe-start");
        assert_eq!(events[0]["process"]["exited"], false);
        assert_eq!(events[1]["event"], "probe-end");
        assert_eq!(events[1]["process"]["pid"], events[0]["process"]["pid"]);
        assert_eq!(events[1]["process"]["exited"], true);
        assert_eq!(events[1]["readerJoined"], true);
        assert_eq!(events[1]["stderrJoined"], true);
        assert_eq!(events[1]["reaped"], true);
        assert_eq!(events[1]["cancelled"], true);
        assert_eq!(events[1]["cancelObservedRunning"], true);
        assert_eq!(
            events[1]["cancelObservedProcess"]["pid"],
            events[0]["process"]["pid"]
        );
        assert_eq!(events[1]["cancelObservedProcess"]["exited"], false);
        assert!(events[1]["cancelObservedProcess"]["exitCode"].is_null());
        #[cfg(windows)]
        assert!(
            events[1]["process"]["exitCode"]
                .as_i64()
                .is_some_and(|code| code != 0)
        );
        assert_eq!(events[1]["cleanupErrors"], serde_json::json!([]));
    }
    #[test]
    fn stderr_and_timestamp_reads_are_bounded() {
        assert_eq!(drain_tail(&vec![7; 100000][..]).len(), 65536);
        assert_eq!(
            bounded_line(&mut BufReader::new(&b"pts=1\n"[..]))
                .unwrap()
                .as_deref(),
            Some("pts=1\n")
        );
        assert!(bounded_line(&mut BufReader::new(&b"pts=1"[..])).is_err());
        assert!(bounded_line(&mut BufReader::new(&vec![b'x'; 5000][..])).is_err());
    }
}
