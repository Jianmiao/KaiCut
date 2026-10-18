//! Serialized source-preview ownership. All waits and media probing run off UI.
use super::{
    SourceCancelRequest, SourceControlRequest, SourceOpenRequest,
    media_actor::{MediaActor, Sink, SourceOptions, SourcePreparationControl, SourceStartError},
    media_sink::MainThreadSink,
    platform, source_profile, validation,
};
use crate::{commands::ProjectState, mcp_project::validate_context, project::MediaAsset};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::UNIX_EPOCH,
};
use tauri::{AppHandle, Manager};
static OWNER: Mutex<Option<Active>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static LIFECYCLE: Mutex<LifecycleGate> = Mutex::new(LifecycleGate::new());
static CLEANUPS: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static OPEN_ERRORS: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static CLOSE_GATE: Mutex<CloseGate> = Mutex::new(CloseGate { failed: None });
static PREPARATIONS: Mutex<PreparationRegistry> = Mutex::new(PreparationRegistry::new());

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparationKey {
    project_id: String,
    revision: u64,
    request_id: String,
}

#[derive(Default)]
struct PreparationRegistry {
    pending: Vec<PreparationRecord>,
    history: Vec<PreparationRecord>,
    tombstones: Vec<PreparationKey>,
    retired: Vec<PreparationKey>,
    active: Option<(PreparationKey, String)>,
    cancel_requests: Vec<Value>,
}
struct PreparationRecord {
    key: PreparationKey,
    media_id: String,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    status: &'static str,
    stage: Option<String>,
    token: Option<String>,
    error: Option<String>,
    events: Vec<Value>,
}
impl PreparationRecord {
    fn inspect(&self) -> Value {
        json!({"projectId":self.key.project_id,"revision":self.key.revision,
            "requestId":self.key.request_id,"mediaId":self.media_id,"generation":self.generation,
            "status":self.status,"stage":self.stage,"cancelled":self.cancelled.load(Ordering::Acquire),
            "token":self.token,"error":self.error,"events":self.events})
    }
}
fn push_bounded<T>(items: &mut Vec<T>, item: T, bound: usize) {
    if items.len() == bound {
        items.remove(0);
    }
    items.push(item);
}
fn probe_cleanup_error(event: &Value) -> Option<String> {
    if event["event"] != "probe-end" {
        return None;
    }
    if event["reaped"] == true
        && event["readerJoined"] == true
        && event["stderrJoined"] == true
        && event["cleanupErrors"].as_array().is_some_and(Vec::is_empty)
    {
        return None;
    }
    Some(format!(
        "素材探测进程未确认完整释放：{}",
        event["stage"].as_str().unwrap_or("unknown")
    ))
}
impl PreparationRegistry {
    const fn new() -> Self {
        Self {
            pending: Vec::new(),
            history: Vec::new(),
            tombstones: Vec::new(),
            retired: Vec::new(),
            active: None,
            cancel_requests: Vec::new(),
        }
    }
    fn register(
        &mut self,
        key: PreparationKey,
        media_id: &str,
        generation: u64,
    ) -> Result<Arc<AtomicBool>, String> {
        self.check_registration(&key)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(index) = self.tombstones.iter().position(|entry| *entry == key) {
            self.tombstones.remove(index);
            cancelled.store(true, Ordering::Release);
        }
        self.pending.push(PreparationRecord {
            key,
            media_id: media_id.into(),
            generation,
            cancelled: Arc::clone(&cancelled),
            status: "preparing",
            stage: None,
            token: None,
            error: None,
            events: Vec::new(),
        });
        Ok(cancelled)
    }
    fn check_registration(&self, key: &PreparationKey) -> Result<(), String> {
        if self.pending.len() >= 32
            || self.pending.iter().any(|entry| entry.key == *key)
            || self.retired.contains(key)
            || self
                .active
                .as_ref()
                .is_some_and(|(active, _)| active == key)
        {
            return Err("素材预览准备请求过多或重复".into());
        }
        Ok(())
    }
    fn cancel_registered(&mut self, key: &PreparationKey) -> Option<Value> {
        if self.pending.iter().any(|entry| entry.key == *key)
            || self
                .active
                .as_ref()
                .is_some_and(|(active, _)| active == key)
            || self.retired.contains(key)
        {
            return Some(self.cancel(key));
        }
        None
    }
    fn cancel_with_context(
        &mut self,
        key: &PreparationKey,
        current_context: Result<(), String>,
    ) -> Result<Value, String> {
        if let Some(reply) = self.cancel_registered(key) {
            return Ok(reply);
        }
        current_context?;
        Ok(self.cancel(key))
    }
    fn cancel(&mut self, key: &PreparationKey) -> Value {
        let entry = self.pending.iter().find(|entry| entry.key == *key);
        let token = self
            .active
            .as_ref()
            .filter(|(owner, _)| owner == key)
            .map(|(_, token)| token.clone());
        let (cancelled, pending, result) = if let Some(entry) = entry {
            entry.cancelled.store(true, Ordering::Release);
            (true, true, "accepted")
        } else if token.is_some() {
            (true, false, "active")
        } else if self.retired.contains(key) {
            (false, false, "retired")
        } else {
            if !self.tombstones.contains(key) {
                push_bounded(&mut self.tombstones, key.clone(), 32);
            }
            (false, false, "missing")
        };
        let mut reply = json!({"cancelled":cancelled,"pending":pending});
        if let Some(token) = &token {
            reply["token"] = token.clone().into();
        }
        push_bounded(
            &mut self.cancel_requests,
            json!({"projectId":key.project_id,
            "revision":key.revision,"requestId":key.request_id,"result":result,
            "cancelled":cancelled,"pending":pending,"token":token}),
            32,
        );
        reply
    }
    fn current_mut(
        &mut self,
        key: &PreparationKey,
        generation: u64,
    ) -> Option<&mut PreparationRecord> {
        self.pending
            .iter_mut()
            .find(|entry| entry.key == *key && entry.generation == generation)
    }
    fn finish(
        &mut self,
        key: &PreparationKey,
        generation: u64,
        status: &'static str,
        error: Option<String>,
    ) {
        if let Some(index) = self
            .pending
            .iter()
            .position(|entry| entry.key == *key && entry.generation == generation)
        {
            let mut entry = self.pending.remove(index);
            entry.status = status;
            entry.error = error;
            if !self.retired.contains(key) {
                push_bounded(&mut self.retired, key.clone(), 32);
            }
            push_bounded(&mut self.history, entry, 8);
        }
    }
    fn activate(&mut self, key: &PreparationKey, generation: u64, token: &str) {
        if let Some(entry) = self.current_mut(key, generation) {
            entry.token = Some(token.into());
            self.active = Some((key.clone(), token.into()));
        }
    }
    fn closed(&mut self, token: &str) {
        if self
            .active
            .as_ref()
            .is_some_and(|(_, current)| current == token)
        {
            self.active = None;
        }
    }
    fn actor_cleanup(&mut self, token: &str, event: Value) {
        if let Some(entry) = self
            .pending
            .iter_mut()
            .chain(self.history.iter_mut())
            .find(|entry| entry.token.as_deref() == Some(token))
        {
            push_bounded(&mut entry.events, event, 32);
        }
    }
    fn inspect(&self) -> Value {
        let mut records: Vec<_> = self.history.iter().chain(self.pending.iter()).collect();
        records.sort_unstable_by_key(|entry| entry.generation);
        let start = records.len().saturating_sub(8);
        json!({"preparations":records[start..].iter().map(|entry| entry.inspect()).collect::<Vec<_>>(),
            "cancelRequests":self.cancel_requests,"pendingCount":self.pending.len()})
    }
}

struct Preparation<'a> {
    registry: &'a Mutex<PreparationRegistry>,
    key: PreparationKey,
    generation: u64,
    cancelled: Arc<AtomicBool>,
}
impl<'a> Preparation<'a> {
    fn register_open(
        registry: &'a Mutex<PreparationRegistry>,
        gate: &'a Mutex<LifecycleGate>,
        key: PreparationKey,
        media_id: &str,
    ) -> Result<Option<(u64, Opening<'a>, Self)>, String> {
        // A rejected duplicate/full registry must not supersede accepted work.
        let mut state = registry.lock().unwrap_or_else(|error| error.into_inner());
        state.check_registration(&key)?;
        let Some((generation, opening)) = Opening::register(gate) else {
            return Ok(None);
        };
        let cancelled = state.register(key.clone(), media_id, generation)?;
        Ok(Some((
            generation,
            opening,
            Self {
                registry,
                key,
                generation,
                cancelled,
            },
        )))
    }
    #[cfg(test)]
    fn register(
        registry: &'a Mutex<PreparationRegistry>,
        key: PreparationKey,
        media_id: &str,
        generation: u64,
    ) -> Result<Self, String> {
        let cancelled = registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register(key.clone(), media_id, generation)?;
        Ok(Self {
            registry,
            key,
            generation,
            cancelled,
        })
    }
    fn checkpoint(&self, stage: &str) -> Result<(), String> {
        checkpoint_preparation(
            self.registry,
            &self.key,
            self.generation,
            &self.cancelled,
            stage,
        )
    }
    fn observe(&self, event: Value) {
        observe_preparation(self.registry, &self.key, self.generation, event);
    }
    fn finish(&self, status: &'static str, error: Option<String>) {
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .finish(&self.key, self.generation, status, error);
    }
}

fn checkpoint_preparation(
    registry: &Mutex<PreparationRegistry>,
    key: &PreparationKey,
    generation: u64,
    cancelled: &AtomicBool,
    stage: &str,
) -> Result<(), String> {
    {
        let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = registry.current_mut(key, generation) {
            entry.stage = Some(stage.into());
        }
    }
    // The validation hold and every device call run without the registry or
    // lifecycle gate held. Cancellation can therefore invalidate this work.
    let check = || {
        if cancelled.load(Ordering::Acquire) {
            return Err("素材预览准备已取消".into());
        }
        if !lifecycle_is_current(generation) {
            return Err("预览选择已更新".into());
        }
        Ok(())
    };
    check()?;
    super::source_stage_validation::checkpoint(stage, &key.request_id, &check)?;
    check()
}

fn observe_preparation(
    registry: &Mutex<PreparationRegistry>,
    key: &PreparationKey,
    generation: u64,
    event: Value,
) {
    if let Some(error) = probe_cleanup_error(&event) {
        let failed_token = uuid::Uuid::new_v4().to_string();
        let _ = CLOSE_GATE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .finish(&failed_token, Err(error));
    }
    if let Some(entry) = registry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .current_mut(key, generation)
    {
        push_bounded(&mut entry.events, event, 32);
    }
}

/// The actor may retain the callbacks, but an open request owns their lifetime.
/// Later play/seek commands must not inherit this request's generation check.
struct PreparationControlLease(Arc<AtomicBool>);
impl Drop for PreparationControlLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl Preparation<'static> {
    fn actor_control(&self) -> (SourcePreparationControl, PreparationControlLease) {
        let armed = Arc::new(AtomicBool::new(true));
        let checkpoint_armed = Arc::clone(&armed);
        let cancelled = Arc::clone(&self.cancelled);
        let key = self.key.clone();
        let generation = self.generation;
        let registry = self.registry;
        let observe_key = key.clone();
        (
            SourcePreparationControl {
                checkpoint: Arc::new(move |stage| {
                    if !checkpoint_armed.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    checkpoint_preparation(registry, &key, generation, &cancelled, stage)
                }),
                observe: Arc::new(move |event| {
                    observe_preparation(registry, &observe_key, generation, event);
                }),
            },
            PreparationControlLease(armed),
        )
    }
}
impl Drop for Preparation<'_> {
    fn drop(&mut self) {
        self.finish(
            if self.cancelled.load(Ordering::Acquire) {
                "cancelled"
            } else {
                "error"
            },
            Some("素材预览准备未完成".into()),
        );
    }
}

#[derive(Default)]
struct CloseGate {
    failed: Option<(String, String)>,
}
impl CloseGate {
    fn finish(&mut self, token: &str, result: Result<(), String>) -> Result<(), String> {
        if let Some((_, error)) = &self.failed {
            return Err(error.clone());
        }
        if let Err(error) = &result {
            self.failed = Some((token.to_owned(), error.clone()));
        }
        result
    }
    fn repeated_close(&self, token: &str) -> Result<(), String> {
        if let Some((failed_token, error)) = &self.failed {
            if failed_token == token {
                return Err(error.clone());
            }
        }
        Ok(())
    }
    fn blocked_open(&self, media_id: &str) -> Option<Value> {
        self.failed.as_ref().map(|(token, error)| {
            json!({"supported":false,"token":token,"mediaId":media_id,
                "error":"原生预览资源未确认释放，请重启应用后重试","diagnostic":error})
        })
    }
}
fn confirm_cleanup(
    actor: &Result<Value, String>,
    sink: &Result<Value, String>,
    unbound: &Result<Value, String>,
) -> Result<(), String> {
    let actor = actor.as_ref().map_err(Clone::clone)?;
    let sink = sink.as_ref().map_err(Clone::clone)?;
    let unbound = unbound.as_ref().map_err(Clone::clone)?;
    let empty = |value: &Value| value.as_array().is_some_and(Vec::is_empty);
    let adapter = &actor["adapterEvidence"];
    let decoder = &adapter["transferClose"]["decoder"];
    if !empty(&actor["cleanupErrors"]) {
        return Err("原生预览工作线程未确认完整清理，请重启应用".into());
    }
    if actor["initializationFailed"] == true {
        let preparation = &actor["preparation"];
        if preparation["workerJoined"] != true
            || preparation["cleanupConfirmed"] != true
            || preparation["resourcesReleased"] != true
            || !empty(&preparation["cleanupErrors"])
        {
            return Err("原生预览初始化资源未确认完整清理，请重启应用".into());
        }
        match (
            preparation["decoderCreationAttempted"].as_bool(),
            preparation["decoderCreated"].as_bool(),
        ) {
            (Some(false), Some(false)) if preparation["decoderClose"].is_null() => {}
            (Some(true), Some(true)) => {
                let decoder = &preparation["decoderClose"]["Ok"]["decoder"];
                if decoder["joined"] != true
                    || decoder["processesReaped"] != true
                    || !empty(&decoder["cleanupErrors"])
                    || !preparation["decoderClose"]["Err"].is_null()
                {
                    return Err("初始化解码器未确认完整释放，请重启应用".into());
                }
            }
            _ => return Err("初始化解码器缺少完整释放证据，请重启应用".into()),
        }
        match (
            preparation["deviceOpenAttempted"].as_bool(),
            preparation["deviceOpened"].as_bool(),
        ) {
            (Some(false), Some(false)) if actor["output"].is_null() => {}
            (Some(true), Some(true)) => {
                if preparation["deviceReleased"] != true
                    || !actor["output"]["sessionMuted"].is_boolean()
                    || (std::env::args_os().any(|arg| arg == "--mcp-hidden")
                        && actor["output"]["sessionMuted"] != true)
                {
                    return Err("初始化音频设备未确认释放，请重启应用".into());
                }
            }
            _ => return Err("初始化音频设备缺少完整释放证据，请重启应用".into()),
        }
        if preparation["audioStartAttempted"] != false || preparation["audioStarted"] != false {
            return Err("初始化音频设备已启动或状态未知，请重启应用".into());
        }
    } else if !empty(&actor["state"]["state"]["cleanupErrors"])
        || !empty(&adapter["lastCleanup"]["errors"])
        || adapter["lastCleanup"]["deviceRunning"] != false
        || adapter["lastCleanup"]["transferClosed"] != true
        || decoder["joined"] != true
        || decoder["processesReaped"] != true
        || !empty(&decoder["cleanupErrors"])
    {
        return Err("原生预览工作线程未确认完整清理，请重启应用".into());
    }
    if sink["closed"] != true
        || sink["pending"] != 0
        || sink["inFlight"] != false
        || !sink["error"].is_null()
        || unbound["mediaSessionActive"] != false
    {
        return Err("原生预览表面未确认完整释放，请重启应用".into());
    }
    // The view can release its child before the source's unbind arrives. An
    // absent binding does not erase a failed or incomplete earlier release.
    let previous_release = &unbound["lastCleanup"];
    if !previous_release.is_null()
        && (!previous_release["error"].is_null()
            || previous_release["result"]["closed"] != true
            || previous_release["result"]["childExists"] != false)
    {
        return Err("原生预览窗口此前的释放未确认成功，请重启应用".into());
    }
    Ok(())
}
fn blocked_open(media_id: &str) -> Option<Value> {
    CLOSE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .blocked_open(media_id)
}

/// Short-lived gate shared by open registration and application exit. Never
/// hold this lock while waiting for OWNER, the UI thread, or media workers.
#[derive(Debug, Default)]
struct LifecycleGate {
    generation: u64,
    opening: usize,
    exiting: bool,
    retired_opening_count: u64,
    exit_observation: Option<Value>,
}

impl LifecycleGate {
    const fn new() -> Self {
        Self {
            generation: 0,
            opening: 0,
            exiting: false,
            retired_opening_count: 0,
            exit_observation: None,
        }
    }

    fn register_open(&mut self) -> Option<u64> {
        if self.exiting {
            return None;
        }
        let generation = self.generation.checked_add(1)?;
        let opening = self.opening.checked_add(1)?;
        self.generation = generation;
        self.opening = opening;
        Some(generation)
    }

    fn is_current(&self, request: u64) -> bool {
        may_replace(request, self.generation, self.exiting)
    }

    fn begin_exit(&mut self, active: bool) -> bool {
        let deferred = active || self.opening > 0;
        if !self.exiting {
            self.exit_observation = Some(json!({
                "opening": self.opening,
                "active": active,
                "deferred": deferred,
                "firstRequested": true,
                "retiredOpeningCount": self.retired_opening_count,
            }));
            self.exiting = true;
            // Saturation is safe: `exiting` itself invalidates every request.
            self.generation = self.generation.saturating_add(1);
        }
        deferred
    }

    fn finish_open(&mut self) -> bool {
        if self.opening == 0 {
            return false;
        }
        self.opening -= 1;
        self.retired_opening_count = self.retired_opening_count.saturating_add(1);
        true
    }

    fn opening_count(&self) -> usize {
        self.opening
    }

    fn is_exiting(&self) -> bool {
        self.exiting
    }

    fn inspect(&self) -> Value {
        json!({
            "generation": self.generation,
            "opening": self.opening,
            "exiting": self.exiting,
            "retiredOpeningCount": self.retired_opening_count,
            "exitObservation": self.exit_observation,
        })
    }
}

struct Opening<'a> {
    gate: &'a Mutex<LifecycleGate>,
}

impl<'a> Opening<'a> {
    fn register(gate: &'a Mutex<LifecycleGate>) -> Option<(u64, Self)> {
        let generation = {
            let mut state = gate.lock().unwrap_or_else(|error| error.into_inner());
            state.register_open()?
        };
        Some((generation, Self { gate }))
    }
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        let mut gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        let _ = gate.finish_open();
    }
}
fn may_replace(request: u64, current: u64, exiting: bool) -> bool {
    request == current && !exiting
}

fn lifecycle_is_current(generation: u64) -> bool {
    let gate = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    gate.is_current(generation)
}

fn lifecycle_snapshot() -> Value {
    let gate = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    gate.inspect()
}
struct Active {
    token: String,
    project_id: String,
    revision: u64,
    media: MediaAsset,
    duration: u64,
    muted: bool,
    actor: MediaActor,
    sink: Arc<MainThreadSink>,
    profile: Value,
    _source_guard: File,
}

fn context(
    app: &AppHandle,
    project_id: &str,
    revision: u64,
    media_id: &str,
) -> Result<MediaAsset, String> {
    let project = app.state::<ProjectState>();
    let s = project.session.lock().map_err(|_| "项目状态不可用")?;
    validate_context(&s, project_id, revision)?;
    s.project
        .media_refs
        .iter()
        .find(|m| m.id == media_id)
        .cloned()
        .ok_or_else(|| "素材已不在当前项目中".into())
}
fn lock_source(
    media: &MediaAsset,
    fingerprint: &Value,
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    checkpoint("source-lock")?;
    let path = std::path::Path::new(media.source_path.as_deref().ok_or("素材已离线")?);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let modified = metadata
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    if metadata.len() != media.size
        || modified.as_millis() != u128::from(media.last_modified)
        || fingerprint["modifiedNs"] != modified.as_nanos().to_string()
    {
        return Err("素材在准备期间已更改".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        checkpoint("source-lock-hash")?;
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if fingerprint["sha256"] != format!("{:x}", hash.finalize()) {
        return Err("素材内容在准备期间已更改".into());
    }
    Ok(file)
}
fn snapshot(active: &Active) -> Value {
    let mut state = active.actor.status();
    // This function is only called from a background command/validator. Do not
    // turn transient status-lock contention into a fabricated zero playhead.
    for _ in 0..20 {
        if state["busy"] != true {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        state = active.actor.status();
    }
    if state["busy"] == true {
        return json!({"supported":true,"token":active.token,"mediaId":active.media.id,"error":"预览状态暂时不可用"});
    }
    let raw = state["projectTimeNs"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        .min(active.duration);
    let phase = state["playbackPhase"]
        .as_str()
        .or_else(|| state["state"]["phase"].as_str())
        .unwrap_or("ready");
    let phase = if matches!(phase, "ready" | "playing" | "paused" | "ended") {
        phase
    } else {
        "ready"
    };
    json!({"supported":true,"token":active.token,"mediaId":active.media.id,"durationNs":active.duration.to_string(),"positionNs":raw.to_string(),"phase":phase,"muted":active.muted,"error":state["error"],"actor":state})
}
fn close_active(app: &AppHandle, slot: &mut Option<Active>) -> Result<Value, String> {
    let Some(active) = slot.take() else {
        if let Some((_, error)) = &CLOSE_GATE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .failed
        {
            return Err(error.clone());
        }
        return Ok(json!({"supported":false,"phase":"closed"}));
    };
    let report = active.actor.finish().map(|report| report.json);
    active.sink.close();
    let sink = Arc::clone(&active.sink);
    let unbound = validation::main_call(app, move |w| platform::unbind_media(&w, sink));
    let unbound = super::source_faults::cleanup_confirmation(unbound);
    let sink_report = active.sink.report();
    let confirmed = confirm_cleanup(&report, &sink_report, &unbound);
    PREPARATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .actor_cleanup(
            &active.token,
            json!({"event":"actor-cleanup", "workerJoined":report.is_ok(),
            "report":report.as_ref().ok(),"joinError":report.as_ref().err()}),
        );
    let evidence = json!({"token":active.token,"mediaId":active.media.id,"actor":report.as_ref().ok(),"joinError":report.as_ref().err(),"sink":sink_report.as_ref().ok(),"sinkError":sink_report.as_ref().err(),"unbind":unbound.as_ref().ok(),"unbindError":unbound.as_ref().err(),"cleanupConfirmed":confirmed.is_ok(),"cleanupConfirmationError":confirmed.as_ref().err(),"profile":active.profile});
    if let Ok(mut reports) = CLEANUPS.lock() {
        if reports.len() == 8 {
            reports.remove(0);
        }
        reports.push(evidence);
    }
    ACTIVE.store(false, Ordering::Release);
    PREPARATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .closed(&active.token);
    CLOSE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .finish(&active.token, confirmed)?;
    Ok(
        json!({"supported":false,"token":active.token,"mediaId":active.media.id,"phase":"closed","joined":true}),
    )
}

fn cleanup_failed_initialization(
    app: &AppHandle,
    media_id: &str,
    sink: &Arc<MainThreadSink>,
    failure: &SourceStartError,
    preparation: &Preparation<'_>,
) {
    // Even an initialization error may have opened a device or queued a frame.
    // First join the actor (start_source_controlled already did), then close and
    // detach the real sink before deciding whether compatibility playback is safe.
    sink.close();
    let detach = Arc::clone(sink);
    let unbound = validation::main_call(app, move |window| platform::unbind_media(&window, detach));
    let unbound = super::source_faults::cleanup_confirmation(unbound);
    let sink_report = sink.report();
    let actor_report = failure
        .report
        .clone()
        .ok_or_else(|| "初始化工作线程缺少清理报告".to_owned());
    let confirmed = if failure.joined {
        confirm_cleanup(&actor_report, &sink_report, &unbound)
    } else {
        Err("初始化工作线程未确认退出，请重启应用".to_owned())
    };
    let token = uuid::Uuid::new_v4().to_string();
    preparation.observe(json!({"event":"actor-initialization-cleanup",
        "workerJoined":failure.joined,"report":failure.report,"error":failure.message}));
    let evidence = json!({"token":token,"mediaId":media_id,"initializationFailed":true,
        "actor":failure.report,"workerJoined":failure.joined,"error":failure.message,
        "sink":sink_report.as_ref().ok(),"sinkError":sink_report.as_ref().err(),
        "unbind":unbound.as_ref().ok(),"unbindError":unbound.as_ref().err(),
        "cleanupConfirmed":confirmed.is_ok(),"cleanupConfirmationError":confirmed.as_ref().err()});
    if let Ok(mut reports) = CLEANUPS.lock() {
        push_bounded(&mut reports, evidence, 8);
    }
    let _ = CLOSE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .finish(&token, confirmed);
}

pub(super) async fn cancel_open(
    window: tauri::WebviewWindow,
    request: SourceCancelRequest,
) -> Result<Value, String> {
    validate_request_id(&request.request_id)?;
    let key = PreparationKey {
        project_id: request.project_id,
        revision: request.revision,
        request_id: request.request_id,
    };
    // Cancelling already-owned work does not require the document to remain
    // current. It never authorizes a new open or touches a different request.
    if let Some(reply) = PREPARATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .cancel_registered(&key)
    {
        return Ok(reply);
    }
    // Never hold the registry and authoritative project locks together. The
    // second lookup also handles an open registered during context validation.
    let current_context = {
        let project = window.state::<ProjectState>();
        project
            .session
            .lock()
            .map_err(|_| "项目状态不可用".to_owned())
            .and_then(|session| validate_context(&session, &key.project_id, key.revision))
    };
    PREPARATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .cancel_with_context(&key, current_context)
}

fn validate_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 128 || request_id.chars().any(char::is_control) {
        return Err("无效的素材预览请求".into());
    }
    Ok(())
}

pub(super) async fn open(
    window: tauri::WebviewWindow,
    request: SourceOpenRequest,
) -> Result<Value, String> {
    validate_request_id(&request.request_id)?;
    let app = window.app_handle().clone();
    let media = context(
        &app,
        &request.project_id,
        request.revision,
        &request.media_id,
    )?;
    let Some((generation, opening, preparation)) = Preparation::register_open(
        &PREPARATIONS,
        &LIFECYCLE,
        PreparationKey {
            project_id: request.project_id.clone(),
            revision: request.revision,
            request_id: request.request_id.clone(),
        },
        &media.id,
    )?
    else {
        return Ok(json!({"supported":false,"reason":"无法登记新的素材预览请求"}));
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _opening = opening;
        let mut slot = OWNER.lock().map_err(|_| "素材预览状态不可用")?;
        if let Some(blocked) = blocked_open(&media.id) {
            preparation.finish("error", blocked["diagnostic"].as_str().map(str::to_owned));
            return Ok(blocked);
        }
        // A cancelled or superseded queued request must not close a newer owner.
        if let Err(error) = preparation.checkpoint("owner-acquired") {
            preparation.finish("cancelled", Some(error.clone()));
            return Ok(json!({"supported":false,"mediaId":media.id,"reason":error}));
        }
        if let Err(error) = close_active(&app, &mut slot) {
            preparation.finish("error", Some(error.clone()));
            return blocked_open(&media.id).map(Ok).unwrap_or(Err(error));
        }
        ACTIVE.store(true, Ordering::Release);
        let result = (|| {
            let checkpoint = |stage: &str| preparation.checkpoint(stage);
            checkpoint("preparing")?;
            let profile = source_profile::prepare_controlled(
                &app, &media, &checkpoint, &|event| preparation.observe(event),
            )?;
            checkpoint("profile-ready")?;
            let current = context(&app, &request.project_id, request.revision, &request.media_id)?;
            if current != media { return Err("预览选择已更新".into()); }
            let source_guard = lock_source(&media, &profile.source_fingerprint, &checkpoint)?;
            checkpoint("host-status")?;
            let host = validation::main_call(&app, |w| platform::status(&w))?;
            let session = host["sessionId"].as_str().ok_or("预览区域尚未准备好")?.to_owned();
            let surface = host["child"]["surfaceId"].as_str().ok_or("预览区域尚未挂载")?.to_owned();
            checkpoint("host-bind")?;
            let sink = MainThreadSink::new_live(app.clone(), session, surface);
            let bind = Arc::clone(&sink);
            validation::main_call(&app, move |w| platform::bind_media(&w, bind))?;
            let muted = std::env::args_os().any(|v| v == "--mcp-hidden");
            let actor_sink: Arc<dyn Sink> = sink.clone();
            let (actor_control, control_lease) = preparation.actor_control();
            let actor = match MediaActor::start_source_controlled(profile.config, profile.timeline, 0,
                actor_sink, SourceOptions { end_ns: profile.end_ns, muted }, actor_control) {
                Ok(actor) => actor,
                Err(failure) => {
                    cleanup_failed_initialization(&app, &media.id, &sink, &failure, &preparation);
                    return Err(failure.message);
                }
            };
            let active = Active {
                token: uuid::Uuid::new_v4().to_string(), project_id: request.project_id,
                revision: request.revision, media: media.clone(), duration: profile.end_ns,
                muted, actor, sink,
                profile: json!({"metadata":profile.metadata,"sourceFingerprint":profile.source_fingerprint}),
                _source_guard: source_guard,
            };
            PREPARATIONS.lock().unwrap_or_else(|error| error.into_inner())
                .activate(&preparation.key, generation, &active.token);
            *slot = Some(active);
            checkpoint("actor-prepare")?;
            slot.as_ref().unwrap().actor.prepare()?;
            checkpoint("ready")?;
            drop(control_lease);
            Ok(snapshot(slot.as_ref().unwrap()))
        })();
        match result {
            Ok(value) => {
                preparation.finish("completed", None);
                Ok(value)
            }
            Err(error) => {
                if let Ok(mut errors) = OPEN_ERRORS.lock() {
                    push_bounded(&mut errors, json!({"mediaId":media.id,"error":error}), 8);
                }
                let cleanup = close_active(&app, &mut slot);
                ACTIVE.store(false, Ordering::Release);
                let cancelled = preparation.cancelled.load(Ordering::Acquire) || !lifecycle_is_current(generation);
                preparation.finish(if cancelled { "cancelled" } else { "error" }, Some(error.clone()));
                if let Some(blocked) = blocked_open(&media.id) { return Ok(blocked); }
                cleanup?;
                Ok(json!({"supported":false,"mediaId":media.id,"reason":"当前素材使用兼容预览","diagnostic":error}))
            }
        }
    }).await.map_err(|error| error.to_string())?
}

pub(super) async fn control(
    window: tauri::WebviewWindow,
    request: SourceControlRequest,
) -> Result<Value, String> {
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut slot = OWNER.lock().map_err(|_| "素材预览状态不可用")?;
        if request.action == "close" {
            CLOSE_GATE
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .repeated_close(&request.token)?;
            if slot.as_ref().is_some_and(|s| s.token == request.token) {
                return close_active(&app, &mut slot);
            }
            return Ok(json!({"supported":false,"phase":"closed","joined":true}));
        }
        let active = slot
            .as_ref()
            .filter(|s| s.token == request.token)
            .ok_or("素材预览会话已更新")?;
        if context(&app, &active.project_id, active.revision, &active.media.id)? != active.media {
            return Err("素材引用已更新".into());
        }
        match request.action.as_str() {
            "play" => {
                active.actor.play()?;
            }
            "pause" => {
                active.actor.pause()?;
            }
            "seek" => {
                let ns = parse_position(request.position_ns.as_deref(), active.duration)?;
                active.actor.seek(ns)?;
                active.actor.prepare()?;
            }
            _ => return Err("不支持的素材预览操作".into()),
        }
        Ok(snapshot(active))
    })
    .await
    .map_err(|e| e.to_string())?
}
pub(super) async fn status(window: tauri::WebviewWindow, token: String) -> Result<Value, String> {
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut slot = OWNER.lock().map_err(|_| "素材预览状态不可用")?;
        let active = slot
            .as_ref()
            .filter(|s| s.token == token)
            .ok_or("素材预览会话已更新")?;
        if context(&app, &active.project_id, active.revision, &active.media.id).as_ref()
            != Ok(&active.media)
        {
            let _ = close_active(&app, &mut slot);
            return Err("项目或素材已更改，预览已停止".into());
        }
        Ok(snapshot(active))
    })
    .await
    .map_err(|e| e.to_string())?
}
fn parse_position(value: Option<&str>, duration: u64) -> Result<u64, String> {
    let value = value
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|c| c.is_ascii_digit()))
        .ok_or("定位时间无效")?;
    let position = value.parse::<u64>().map_err(|_| "定位时间超出范围")?;
    if position > duration {
        return Err("定位时间超过素材长度".into());
    }
    Ok(position)
}
pub(super) fn inspect() -> Value {
    let lifecycle = lifecycle_snapshot();
    let preparations = PREPARATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .inspect();
    let close_failure = CLOSE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .failed
        .as_ref()
        .map(|(token, error)| json!({"token":token,"error":error}));
    match OWNER.try_lock() {
        Ok(slot) => {
            json!({"active":slot.as_ref().map(snapshot),"sink":slot.as_ref().and_then(|s|s.sink.report().ok()),"profile":slot.as_ref().map(|s|s.profile.clone()),"openErrors":OPEN_ERRORS.lock().ok().map(|v|v.clone()),"cleanups":CLEANUPS.lock().ok().map(|v|v.clone()),"closeFailure":close_failure,"lifecycle":lifecycle,"preparations":preparations["preparations"],"cancelRequests":preparations["cancelRequests"],"pendingPreparations":preparations["pendingCount"]})
        }
        Err(_) => {
            json!({"busy":true,"closeFailure":close_failure,"lifecycle":lifecycle,"preparations":preparations["preparations"],"cancelRequests":preparations["cancelRequests"],"pendingPreparations":preparations["pendingCount"]})
        }
    }
}
pub(super) fn defer_exit(app: &AppHandle) -> bool {
    let (should_defer, start_shutdown) = {
        let mut gate = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
        let first_exit = !gate.is_exiting();
        let pending = gate.begin_exit(ACTIVE.load(Ordering::Acquire));
        (pending, first_exit && pending)
    };
    if start_shutdown {
        let app = app.clone();
        std::thread::spawn(move || {
            if let Ok(mut slot) = OWNER.lock() {
                let _ = close_active(&app, &mut slot);
            }
            loop {
                let opening = {
                    let gate = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
                    gate.opening_count()
                };
                if opening == 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            ACTIVE.store(false, Ordering::Release);
            app.exit(0);
        });
    }
    should_defer
}
#[cfg(test)]
mod tests {
    use super::*;

    fn preparation_key(project: &str, revision: u64, request: &str) -> PreparationKey {
        PreparationKey {
            project_id: project.into(),
            revision,
            request_id: request.into(),
        }
    }

    #[test]
    fn preparation_cancel_targets_only_exact_context_and_request() {
        let mut registry = PreparationRegistry::default();
        let first = preparation_key("project", 7, "first");
        let second = preparation_key("project", 7, "second");
        let first_cancelled = registry.register(first.clone(), "a", 11).unwrap();
        let second_cancelled = registry.register(second, "b", 12).unwrap();
        registry.cancel(&preparation_key("other", 7, "first"));
        registry.cancel(&preparation_key("project", 8, "first"));
        assert!(!first_cancelled.load(Ordering::Acquire));
        assert!(!second_cancelled.load(Ordering::Acquire));
        let reply = registry.cancel(&first);
        assert_eq!(reply["cancelled"], true);
        assert_eq!(reply["pending"], true);
        assert!(first_cancelled.load(Ordering::Acquire));
        assert!(!second_cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn tracked_old_context_can_cancel_without_touching_new_context() {
        let mut registry = PreparationRegistry::default();
        let old = preparation_key("old-project", 1, "old");
        let new = preparation_key("new-project", 2, "new");
        let old_cancelled = registry.register(old.clone(), "a", 1).unwrap();
        let new_cancelled = registry.register(new, "b", 2).unwrap();
        let reply = registry
            .cancel_with_context(&old, Err("项目已切换".into()))
            .unwrap();
        assert_eq!(reply["cancelled"], true);
        assert_eq!(reply["pending"], true);
        assert!(old_cancelled.load(Ordering::Acquire));
        assert!(!new_cancelled.load(Ordering::Acquire));
        registry.activate(&old, 1, "old-token");
        registry.finish(&old, 1, "completed", None);
        assert_eq!(
            registry
                .cancel_with_context(&old, Err("修订已更新".into()))
                .unwrap()["token"],
            "old-token"
        );
        registry.closed("old-token");
        let retired = registry
            .cancel_with_context(&old, Err("修订已更新".into()))
            .unwrap();
        assert_eq!(retired["cancelled"], false);
        assert!(registry.tombstones.is_empty());
    }

    #[test]
    fn unknown_stale_cancel_cannot_create_tombstone_or_cancel_current_preparation() {
        let mut registry = PreparationRegistry::default();
        let current = preparation_key("p", 2, "current");
        let flag = registry.register(current, "b", 2).unwrap();
        let unknown = preparation_key("p", 1, "unknown");
        assert!(
            registry
                .cancel_with_context(&unknown, Err("修订已更新".into()))
                .is_err()
        );
        assert!(registry.tombstones.is_empty());
        assert!(registry.cancel_requests.is_empty());
        assert!(!flag.load(Ordering::Acquire));
        let current_unknown = preparation_key("p", 2, "before-open");
        assert_eq!(
            registry
                .cancel_with_context(&current_unknown, Ok(()))
                .unwrap()["cancelled"],
            false
        );
        assert_eq!(registry.tombstones, vec![current_unknown]);
    }

    #[test]
    fn completed_request_id_cannot_be_reused_or_cancel_a_future_owner() {
        let mut registry = PreparationRegistry::default();
        let old = preparation_key("p", 1, "old");
        registry.register(old.clone(), "a", 1).unwrap();
        registry.finish(&old, 1, "cancelled", None);
        assert!(registry.register(old.clone(), "b", 2).is_err());
        let next = preparation_key("p", 1, "next");
        let flag = registry.register(next, "b", 2).unwrap();
        assert_eq!(registry.cancel(&old)["cancelled"], false);
        assert!(!flag.load(Ordering::Acquire));
        assert!(registry.tombstones.is_empty());
    }

    #[test]
    fn bounded_preparation_registry_rejects_duplicate_and_consumes_early_cancel_once() {
        let mut registry = PreparationRegistry::default();
        let early = preparation_key("p", 1, "early");
        registry.cancel(&early);
        assert!(
            registry
                .register(early.clone(), "a", 1)
                .unwrap()
                .load(Ordering::Acquire)
        );
        assert!(registry.tombstones.is_empty());
        assert!(registry.register(early.clone(), "b", 2).is_err());
        for generation in 2..=32 {
            registry
                .register(
                    preparation_key("p", 1, &generation.to_string()),
                    "a",
                    generation,
                )
                .unwrap();
        }
        assert!(
            registry
                .register(preparation_key("p", 1, "overflow"), "a", 33)
                .is_err()
        );
        assert_eq!(registry.pending.len(), 32);
        assert_eq!(
            registry.inspect()["preparations"].as_array().unwrap().len(),
            8
        );
    }

    #[test]
    fn preparation_guard_drop_retires_only_its_exact_generation() {
        let registry = Mutex::new(PreparationRegistry::default());
        let key = preparation_key("p", 1, "id");
        let guard = Preparation::register(&registry, key.clone(), "a", 1).unwrap();
        registry.lock().unwrap().finish(&key, 99, "error", None);
        assert_eq!(registry.lock().unwrap().pending.len(), 1);
        drop(guard);
        let registry = registry.lock().unwrap();
        assert!(registry.pending.is_empty());
        assert_eq!(registry.history[0].status, "error");
    }

    #[test]
    fn actor_control_disarms_without_cancelling_later_live_commands() {
        static REGISTRY: Mutex<PreparationRegistry> = Mutex::new(PreparationRegistry::new());
        let key = preparation_key("local-test", 1, "actor-control");
        let guard = Preparation::register(&REGISTRY, key.clone(), "a", u64::MAX).unwrap();
        let (control, lease) = guard.actor_control();
        guard.cancelled.store(true, Ordering::Release);
        assert!((control.checkpoint)("device-opened").is_err());
        assert_eq!(
            REGISTRY.lock().unwrap().pending[0].stage.as_deref(),
            Some("device-opened")
        );
        drop(lease);
        assert!((control.checkpoint)("later-seek").is_ok());
        assert_eq!(
            REGISTRY.lock().unwrap().pending[0].stage.as_deref(),
            Some("device-opened")
        );
        (control.observe)(json!({"event":"actor-worker-joined","joined":true}));
        assert_eq!(
            REGISTRY.lock().unwrap().pending[0].events[0]["joined"],
            true
        );
    }

    #[test]
    fn actor_cleanup_retains_raw_join_report_for_only_its_preparation() {
        let mut registry = PreparationRegistry::default();
        let key = preparation_key("p", 1, "one");
        registry.register(key.clone(), "a", 1).unwrap();
        registry.activate(&key, 1, "token-a");
        registry.finish(&key, 1, "cancelled", None);
        let second = preparation_key("p", 1, "two");
        registry.register(second, "b", 2).unwrap();
        let evidence = json!({"event":"actor-cleanup","workerJoined":true,
            "report":{"error":"preparation cancelled","cleanupErrors":[]}});
        registry.actor_cleanup("other-token", evidence.clone());
        assert!(registry.history[0].events.is_empty());
        registry.actor_cleanup("token-a", evidence.clone());
        assert_eq!(registry.history[0].events, vec![evidence]);
        assert!(registry.pending[0].events.is_empty());
    }

    #[test]
    fn preparation_history_and_audit_remain_bounded() {
        let registry = Mutex::new(PreparationRegistry::default());
        for generation in 1..=64 {
            let key = preparation_key("p", 1, &generation.to_string());
            let guard = Preparation::register(&registry, key.clone(), "a", generation).unwrap();
            for value in 0..40 {
                guard.observe(json!({"value":value}));
            }
            registry.lock().unwrap().cancel(&key);
            guard.finish("cancelled", None);
            drop(guard);
        }
        let mut registry = registry.lock().unwrap();
        assert!(registry.pending.is_empty());
        assert_eq!(registry.history.len(), 8);
        assert_eq!(registry.history[0].events.len(), 32);
        assert_eq!(registry.history[0].events[0]["value"], 8);
        assert_eq!(registry.retired.len(), 32);
        assert_eq!(registry.cancel_requests.len(), 32);
        for generation in 65..=128 {
            registry.cancel(&preparation_key("p", 1, &generation.to_string()));
        }
        assert_eq!(registry.tombstones.len(), 32);
        assert_eq!(registry.cancel_requests.len(), 32);
    }

    #[test]
    fn cancelling_completed_owner_returns_only_matching_token_without_owner_lock() {
        let mut registry = PreparationRegistry::default();
        let key = preparation_key("p", 1, "id");
        registry.register(key.clone(), "a", 1).unwrap();
        registry.activate(&key, 99, "stale");
        assert!(registry.active.is_none());
        registry.activate(&key, 1, "token");
        registry.finish(&key, 1, "completed", None);
        let reply = registry.cancel(&key);
        assert_eq!(reply["token"], "token");
        assert_eq!(reply["pending"], false);
        assert!(registry.cancel(&preparation_key("p", 2, "id"))["token"].is_null());
        registry.closed("unrelated");
        assert_eq!(registry.cancel(&key)["token"], "token");
        registry.closed("token");
        assert!(registry.cancel(&key)["token"].is_null());
    }

    #[test]
    fn probe_cleanup_gate_distinguishes_decode_failure_from_unconfirmed_cleanup() {
        let event = json!({"event":"probe-end","stage":"metadata-probe","error":"bad source",
            "readerJoined":true,"stderrJoined":true,"reaped":true,"cleanupErrors":[]});
        assert!(probe_cleanup_error(&event).is_none());
        assert!(probe_cleanup_error(&json!({"event":"probe-start"})).is_none());
        for field in ["readerJoined", "stderrJoined", "reaped", "cleanupErrors"] {
            let mut broken = event.clone();
            broken[field] = if field == "cleanupErrors" {
                json!(["join failed"])
            } else {
                false.into()
            };
            assert!(probe_cleanup_error(&broken).is_some(), "{field}");
        }
    }

    #[test]
    fn rejected_preparation_registration_does_not_supersede_accepted_request() {
        let registry = Mutex::new(PreparationRegistry::default());
        let gate = Mutex::new(LifecycleGate::new());
        let key = preparation_key("p", 1, "request");
        let (generation, opening, preparation) =
            Preparation::register_open(&registry, &gate, key.clone(), "a")
                .unwrap()
                .unwrap();
        assert!(Preparation::register_open(&registry, &gate, key, "b").is_err());
        assert!(gate.lock().unwrap().is_current(generation));
        assert_eq!(gate.lock().unwrap().opening_count(), 1);
        drop(preparation);
        drop(opening);
        assert!(registry.lock().unwrap().pending.is_empty());
        assert_eq!(gate.lock().unwrap().opening_count(), 0);
        gate.lock().unwrap().begin_exit(false);
        assert!(
            Preparation::register_open(&registry, &gate, preparation_key("p", 1, "next"), "b")
                .unwrap()
                .is_none()
        );
        assert!(registry.lock().unwrap().pending.is_empty());
    }

    fn clean_actor() -> Value {
        json!({"ok":false,"error":"stream-rgb failed","cleanupErrors":[],
            "state":{"state":{"deviceRunning":false,"cleanupErrors":[]}},
            "adapterEvidence":{"lastCleanup":{"deviceRunning":false,"transferClosed":true,"errors":[]},
                "transferClose":{"decoder":{"joined":true,"processesReaped":true,"cleanupErrors":[],"error":"source decode failed"}}}})
    }
    fn clean_sink() -> Result<Value, String> {
        Ok(json!({"closed":true,"pending":0,"inFlight":false,"error":null}))
    }
    fn clean_unbind() -> Result<Value, String> {
        Ok(json!({"mediaSessionActive":false}))
    }
    #[test]
    fn close_confirmation_rejects_release_error_after_binding_was_cleared() {
        let unbound = Ok(json!({"mediaSessionActive":false,"child":null,
            "lastCleanup":{"error":"DestroyWindow failed","result":null}}));
        assert!(confirm_cleanup(&Ok(clean_actor()), &clean_sink(), &unbound).is_err());
    }
    #[test]
    fn close_confirmation_requires_complete_existing_release_evidence() {
        for result in [
            json!({"closed":false,"childExists":false}),
            json!({"closed":true,"childExists":true}),
            json!({"childExists":false}),
            json!({"closed":true}),
            Value::Null,
        ] {
            let unbound = Ok(json!({"mediaSessionActive":false,"child":null,
                "lastCleanup":{"error":null,"result":result}}));
            assert!(
                confirm_cleanup(&Ok(clean_actor()), &clean_sink(), &unbound).is_err(),
                "{result}"
            );
        }
    }
    #[test]
    fn close_confirmation_accepts_suspended_child_without_a_release_or_after_clean_release() {
        for previous in [
            Value::Null,
            json!({"error":null,"result":{"closed":true,"childExists":false}}),
        ] {
            let unbound = Ok(json!({"mediaSessionActive":false,
                "child":{"childExists":true,"suspended":true},"lastCleanup":previous}));
            assert!(confirm_cleanup(&Ok(clean_actor()), &clean_sink(), &unbound).is_ok());
        }
        assert!(confirm_cleanup(&Ok(clean_actor()), &clean_sink(), &clean_unbind()).is_ok());
    }
    #[test]
    fn initialization_cleanup_requires_worker_and_resource_release_evidence() {
        let actor = json!({"initializationFailed":true,"cleanupErrors":[],
            "preparation":{"workerJoined":true,"cleanupConfirmed":true,"cleanupErrors":[],
                "resourcesReleased":true,"decoderCreationAttempted":true,"decoderCreated":true,
                "decoderClose":{"Ok":{"decoder":{"joined":true,"processesReaped":true,"cleanupErrors":[]}}},
                "deviceOpenAttempted":true,"deviceOpened":true,"deviceReleased":true,
                "audioStartAttempted":false,"audioStarted":false},"output":{"sessionMuted":true}});
        assert!(confirm_cleanup(&Ok(actor.clone()), &clean_sink(), &clean_unbind()).is_ok());
        for field in ["workerJoined", "cleanupConfirmed", "cleanupErrors"] {
            let mut broken = actor.clone();
            broken["preparation"][field] = if field == "cleanupErrors" {
                json!(["device release not confirmed"])
            } else {
                false.into()
            };
            assert!(
                confirm_cleanup(&Ok(broken), &clean_sink(), &clean_unbind()).is_err(),
                "{field}"
            );
        }
        for field in ["joined", "processesReaped", "cleanupErrors"] {
            let mut broken = actor.clone();
            broken["preparation"]["decoderClose"]["Ok"]["decoder"][field] =
                if field == "cleanupErrors" {
                    json!(["reader not joined"])
                } else {
                    false.into()
                };
            assert!(
                confirm_cleanup(&Ok(broken), &clean_sink(), &clean_unbind()).is_err(),
                "{field}"
            );
        }
        for field in ["deviceReleased", "decoderCreated", "resourcesReleased"] {
            let mut broken = actor.clone();
            broken["preparation"][field] = false.into();
            assert!(
                confirm_cleanup(&Ok(broken), &clean_sink(), &clean_unbind()).is_err(),
                "{field}"
            );
        }
        let mut missing_output = actor.clone();
        missing_output["output"] = Value::Null;
        assert!(confirm_cleanup(&Ok(missing_output), &clean_sink(), &clean_unbind()).is_err());
        assert!(
            confirm_cleanup(
                &Ok(actor.clone()),
                &Err("sink busy".into()),
                &clean_unbind()
            )
            .is_err()
        );
        assert!(confirm_cleanup(&Ok(actor), &clean_sink(), &Err("unbind failed".into())).is_err());
    }

    #[test]
    fn initialization_without_a_worker_requires_explicit_empty_resource_state() {
        let mut actor = json!({"initializationFailed":true,"cleanupErrors":[],
            "preparation":{"workerJoined":true,"workerNeverCreated":true,"cleanupConfirmed":true,
                "cleanupErrors":[],"resourcesReleased":true,"decoderCreationAttempted":false,
                "decoderCreated":false,"decoderClose":null,"deviceOpenAttempted":false,
                "deviceOpened":false,"deviceReleased":false,"audioStartAttempted":false,"audioStarted":false},
            "output":null});
        assert!(confirm_cleanup(&Ok(actor.clone()), &clean_sink(), &clean_unbind()).is_ok());
        actor["preparation"]["decoderCreationAttempted"] = true.into();
        assert!(confirm_cleanup(&Ok(actor), &clean_sink(), &clean_unbind()).is_err());
    }
    #[test]
    fn close_confirmation_allows_source_failure_but_rejects_cleanup_failure() {
        assert!(confirm_cleanup(&Ok(clean_actor()), &clean_sink(), &clean_unbind()).is_ok());
        for kind in [
            "actor", "adapter", "decoder", "joined", "reaped", "running", "transfer",
        ] {
            let mut actor = clean_actor();
            match kind {
                "actor" => actor["cleanupErrors"] = json!(["reset failed"]),
                "adapter" => actor["state"]["state"]["cleanupErrors"] = json!(["stop failed"]),
                "decoder" => {
                    actor["adapterEvidence"]["transferClose"]["decoder"]["cleanupErrors"] =
                        json!(["join failed"])
                }
                "joined" => {
                    actor["adapterEvidence"]["transferClose"]["decoder"]["joined"] = false.into()
                }
                "reaped" => {
                    actor["adapterEvidence"]["transferClose"]["decoder"]["processesReaped"] =
                        false.into()
                }
                "running" => actor["adapterEvidence"]["lastCleanup"]["deviceRunning"] = true.into(),
                _ => actor["adapterEvidence"]["lastCleanup"]["transferClosed"] = false.into(),
            }
            assert!(
                confirm_cleanup(&Ok(actor), &clean_sink(), &clean_unbind()).is_err(),
                "{kind}"
            );
        }
        assert!(
            confirm_cleanup(
                &Err("actor panicked".into()),
                &clean_sink(),
                &clean_unbind()
            )
            .is_err()
        );
        assert!(
            confirm_cleanup(
                &Ok(clean_actor()),
                &Err("sink poisoned".into()),
                &clean_unbind()
            )
            .is_err()
        );
        assert!(
            confirm_cleanup(
                &Ok(clean_actor()),
                &clean_sink(),
                &Err("unbind failed".into())
            )
            .is_err()
        );
        assert!(
            confirm_cleanup(
                &Ok(clean_actor()),
                &clean_sink(),
                &Ok(json!({"mediaSessionActive":true}))
            )
            .is_err()
        );
    }
    #[test]
    fn failed_close_is_sticky_and_blocked_open_retains_the_cleanup_token() {
        let mut gate = CloseGate::default();
        assert!(gate.blocked_open("b").is_none());
        assert!(
            gate.finish("old", Err("decoder join failed".into()))
                .is_err()
        );
        assert!(gate.repeated_close("old").is_err());
        assert!(gate.repeated_close("unrelated").is_ok());
        let blocked = gate
            .blocked_open("b")
            .expect("new source must retain failed ownership");
        assert_eq!(blocked["token"], "old");
        assert_eq!(blocked["mediaId"], "b");
        assert_eq!(blocked["supported"], false);
        assert!(blocked["error"].is_string());
        assert!(gate.finish("different", Ok(())).is_err());
        assert_eq!(gate.blocked_open("b").unwrap(), blocked);
    }

    #[test]
    fn open_registration_and_exit_share_one_gate() {
        let mut gate = LifecycleGate::new();
        let request = gate
            .register_open()
            .expect("open before exit is registered");
        assert!(gate.begin_exit(false));
        assert!(!gate.is_current(request));
        assert_eq!(gate.opening_count(), 1);
        gate.finish_open();
        assert_eq!(gate.opening_count(), 0);
    }

    #[test]
    fn exit_gate_rejects_open_registered_after_exit() {
        let mut gate = LifecycleGate::new();
        assert!(!gate.begin_exit(false));
        assert!(gate.register_open().is_none());
        assert_eq!(gate.opening_count(), 0);
    }

    #[test]
    fn new_registration_invalidates_older_request_without_losing_pending_count() {
        let mut gate = LifecycleGate::new();
        let first = gate.register_open().unwrap();
        let second = gate.register_open().unwrap();
        assert!(!gate.is_current(first));
        assert!(gate.is_current(second));
        assert_eq!(gate.opening_count(), 2);
        assert!(gate.finish_open());
        assert_eq!(gate.opening_count(), 1);
        assert!(gate.is_current(second));
    }

    #[test]
    fn duplicate_exit_keeps_gate_closed_until_all_work_has_drained() {
        let mut gate = LifecycleGate::new();
        gate.register_open().unwrap();
        assert!(gate.begin_exit(false));
        let exit_generation = gate.generation;
        assert!(gate.begin_exit(false));
        assert_eq!(gate.generation, exit_generation);
        gate.finish_open();
        assert!(gate.begin_exit(true));
        assert!(!gate.begin_exit(false));
        assert!(gate.register_open().is_none());
    }

    #[test]
    fn exit_observation_preserves_first_request_and_tracks_retirement_separately() {
        let mut gate = LifecycleGate::new();
        gate.register_open().unwrap();
        gate.finish_open();
        gate.register_open().unwrap();
        assert!(gate.begin_exit(true));
        let first = gate.inspect();
        assert_eq!(first["exitObservation"]["opening"], 1);
        assert_eq!(first["exitObservation"]["active"], true);
        assert_eq!(first["exitObservation"]["deferred"], true);
        assert_eq!(first["exitObservation"]["firstRequested"], true);
        assert_eq!(first["exitObservation"]["retiredOpeningCount"], 1);
        gate.finish_open();
        assert!(!gate.begin_exit(false));
        let final_state = gate.inspect();
        assert_eq!(final_state["exitObservation"], first["exitObservation"]);
        assert_eq!(final_state["retiredOpeningCount"], 2);
    }

    #[test]
    fn idle_exit_observation_is_preserved_when_later_exit_is_requested() {
        let mut gate = LifecycleGate::new();
        assert!(gate.inspect()["exitObservation"].is_null());
        assert!(!gate.begin_exit(false));
        assert!(gate.begin_exit(true));
        let state = gate.inspect();
        assert_eq!(state["exitObservation"]["opening"], 0);
        assert_eq!(state["exitObservation"]["active"], false);
        assert_eq!(state["exitObservation"]["deferred"], false);
    }

    #[test]
    fn retirement_observation_counter_does_not_wrap() {
        let mut gate = LifecycleGate {
            opening: 1,
            retired_opening_count: u64::MAX,
            ..LifecycleGate::new()
        };
        assert!(gate.finish_open());
        assert_eq!(gate.opening_count(), 0);
        assert_eq!(gate.inspect()["retiredOpeningCount"], u64::MAX);
    }

    #[test]
    fn registration_overflow_never_wraps_or_partially_updates_the_gate() {
        let mut gate = LifecycleGate {
            generation: u64::MAX,
            opening: 1,
            exiting: false,
            ..LifecycleGate::new()
        };
        assert!(gate.register_open().is_none());
        assert_eq!(gate.opening_count(), 1);
        assert_eq!(gate.generation, u64::MAX);
        assert!(gate.begin_exit(false));
        assert_eq!(gate.generation, u64::MAX);
        assert!(!gate.is_current(u64::MAX));

        let mut gate = LifecycleGate {
            generation: 41,
            opening: usize::MAX,
            exiting: false,
            ..LifecycleGate::new()
        };
        assert!(gate.register_open().is_none());
        assert_eq!(gate.generation, 41);
        assert_eq!(gate.opening_count(), usize::MAX);
    }

    #[test]
    fn opening_guard_drains_the_registered_request_even_after_exit() {
        let gate = Mutex::new(LifecycleGate::new());
        let (generation, opening) = Opening::register(&gate).unwrap();
        {
            let mut state = gate.lock().unwrap();
            assert!(state.begin_exit(false));
            assert!(!state.is_current(generation));
        }
        drop(opening);
        let mut state = gate.lock().unwrap();
        assert_eq!(state.opening_count(), 0);
        assert!(!state.finish_open());
        assert_eq!(state.opening_count(), 0);
        assert!(!state.begin_exit(false));
    }

    #[test]
    fn concurrent_open_and_exit_cannot_admit_untracked_work() {
        // Both orders are legal. If exit wins, open must be rejected; if open
        // wins, exit must defer and the outstanding guard must be counted.
        for _ in 0..100 {
            let gate = Mutex::new(LifecycleGate::new());
            let barrier = std::sync::Barrier::new(2);
            let (opening, deferred) = std::thread::scope(|scope| {
                let open = scope.spawn(|| {
                    barrier.wait();
                    Opening::register(&gate)
                });
                let exit = scope.spawn(|| {
                    barrier.wait();
                    gate.lock().unwrap().begin_exit(false)
                });
                (open.join().unwrap(), exit.join().unwrap())
            });
            {
                let state = gate.lock().unwrap();
                assert!(state.is_exiting());
                assert_eq!(deferred, opening.is_some());
                assert_eq!(state.opening_count(), usize::from(opening.is_some()));
                if let Some((generation, _)) = &opening {
                    assert!(!state.is_current(*generation));
                }
            }
            drop(opening);
            assert_eq!(gate.lock().unwrap().opening_count(), 0);
        }
    }

    #[test]
    fn inspection_retains_lifecycle_state_while_owner_is_busy() {
        let _owner = OWNER.lock().unwrap();
        let state = inspect();
        assert_eq!(state["busy"], true);
        assert!(state["lifecycle"]["generation"].is_u64());
        assert!(state["lifecycle"]["opening"].is_u64());
        assert!(state["lifecycle"]["exiting"].is_boolean());
    }

    #[test]
    fn positions_are_integer_bounded_and_allow_exact_eof() {
        assert_eq!(parse_position(Some("100"), 100).unwrap(), 100);
        for s in ["-1", "1.5", "NaN", "101", "18446744073709551616", ""] {
            assert!(parse_position(Some(s), 100).is_err());
        }
    }
    #[test]
    fn stale_open_never_replaces_newer_owner() {
        assert!(!may_replace(1, 2, false));
        assert!(may_replace(2, 2, false));
        assert!(!may_replace(2, 2, true));
    }
}
