//! Opt-in local control bridge for the external MCP stdio adapter.

use crate::{
    commands, export_job,
    mcp_project::{self, validate_context},
    media_engine,
    project::ProjectSession,
    timeline,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

const REQUEST_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
const CLIENT_LIMIT: usize = 4;
const UI_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct BridgeState {
    stopping: AtomicBool,
    connection: Mutex<Option<(PathBuf, ConnectionInfo)>>,
    pending: PendingReplies,
}

#[derive(Default)]
struct PendingReplies(Mutex<HashMap<String, mpsc::SyncSender<Value>>>);

impl PendingReplies {
    fn register(&self) -> Result<(String, mpsc::Receiver<Value>), String> {
        let mut pending = self.0.lock().map_err(|_| "MCP UI state unavailable")?;
        if pending.len() >= CLIENT_LIMIT {
            return Err("MCP UI request limit reached".into());
        }
        let id = Uuid::new_v4().to_string();
        let (sender, receiver) = mpsc::sync_channel(1);
        pending.insert(id.clone(), sender);
        Ok((id, receiver))
    }

    fn remove(&self, id: &str) {
        if let Ok(mut pending) = self.0.lock() {
            pending.remove(id);
        }
    }

    fn wait(
        &self,
        id: &str,
        receiver: mpsc::Receiver<Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let result = receiver.recv_timeout(timeout);
        self.remove(id);
        let value = result.map_err(|_| "KaiCut UI did not respond before the deadline")?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(error.to_owned());
        }
        Ok(value)
    }

    fn acknowledge(&self, id: &str, result: Value) -> Result<(), String> {
        if !result.is_object()
            || serde_json::to_vec(&result)
                .map_err(|_| "Invalid UI result")?
                .len()
                > REQUEST_LIMIT
        {
            return Err("MCP UI result must be an object of at most 1 MiB".into());
        }
        let sender = self
            .0
            .lock()
            .map_err(|_| "MCP UI state unavailable")?
            .remove(id)
            .ok_or("Unknown or expired MCP UI request")?;
        sender
            .try_send(result)
            .map_err(|_| "MCP UI request has expired".into())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionInfo {
    version: u32,
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    token: String,
    command: String,
    args: Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", content = "args")]
enum BridgeCommand {
    #[serde(rename = "get_state")]
    GetState(EmptyArgs),
    #[serde(rename = "project_open_path")]
    ProjectOpenPath(mcp_project::ProjectPathArgs),
    #[serde(rename = "project_import_paths")]
    ProjectImportPaths(mcp_project::ImportPathsArgs),
    #[serde(rename = "project_save_as")]
    ProjectSaveAs(mcp_project::ProjectPathArgs),
    #[serde(rename = "media_check_links")]
    MediaCheckLinks(EmptyArgs),
    #[serde(rename = "media_probe")]
    MediaProbe(EmptyArgs),
    #[serde(rename = "export_job_list")]
    ExportJobList(EmptyArgs),
    #[serde(rename = "export_job_status")]
    ExportJobStatus(JobArgs),
    #[serde(rename = "export_preflight")]
    ExportPreflight(ExportArgs),
    #[serde(rename = "export_prepare")]
    ExportPrepare(ExportArgs),
    #[serde(rename = "export_job_start")]
    ExportJobStart(JobArgs),
    #[serde(rename = "export_job_cancel")]
    ExportJobCancel(JobArgs),
    #[serde(rename = "timeline_edit")]
    TimelineEdit(EditArgs),
    #[serde(rename = "timeline_undo")]
    TimelineUndo(SessionArgs),
    #[serde(rename = "timeline_redo")]
    TimelineRedo(SessionArgs),
    #[serde(rename = "ui_inspect")]
    UiInspect(EmptyArgs),
    #[serde(rename = "ui_seek")]
    UiSeek(SeekArgs),
    #[serde(rename = "ui_play")]
    UiPlay(SessionArgs),
    #[serde(rename = "ui_pause")]
    UiPause(SessionArgs),
    #[serde(rename = "ui_select")]
    UiSelect(SelectArgs),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobArgs {
    #[serde(deserialize_with = "deserialize_job_id")]
    job_id: String,
}

fn deserialize_job_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let id = String::deserialize(deserializer)?;
    export_job::validate_job_id(&id).map_err(serde::de::Error::custom)?;
    Ok(id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExportArgs {
    project_id: String,
    #[serde(deserialize_with = "deserialize_revision")]
    revision: u64,
    output_path: String,
}

fn deserialize_revision<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    let revision = u64::deserialize(deserializer)?;
    if revision > crate::project::MAX_SAFE_INTEGER {
        return Err(serde::de::Error::custom(
            "Project revision must be a safe integer",
        ));
    }
    Ok(revision)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionArgs {
    project_id: String,
    revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditArgs {
    project_id: String,
    revision: u64,
    edit: timeline::TimelineEdit,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeekArgs {
    project_id: String,
    revision: u64,
    frame: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectArgs {
    project_id: String,
    revision: u64,
    clip_ids: Vec<String>,
}

fn from_json<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|_| "Invalid MCP command or arguments".into())
}

fn to_json<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "MCP result serialization failed".into())
}

fn tokens_match(actual: &str, expected: &str) -> bool {
    if actual.len() != 64 || expected.len() != 64 {
        return false;
    }
    actual
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn decode_request(line: &[u8], token: &str) -> Result<BridgeCommand, String> {
    if line.len() > REQUEST_LIMIT {
        return Err("MCP request exceeds 1 MiB".into());
    }
    let request: WireRequest = serde_json::from_slice(line).map_err(|_| "Invalid MCP request")?;
    if !tokens_match(&request.token, token) {
        return Err("MCP authentication failed".into());
    }
    from_json(json!({"command":request.command, "args": request.args}))
}

fn validate_seek(session: &ProjectSession, args: &SeekArgs) -> Result<(), String> {
    validate_context(session, &args.project_id, args.revision)?;
    let duration = session
        .project
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.start_frame + clip.duration_frames)
        .max()
        .unwrap_or(0);
    if args.frame > duration {
        return Err("Seek frame is outside the current timeline".into());
    }
    Ok(())
}

fn validate_selection(session: &ProjectSession, args: &SelectArgs) -> Result<(), String> {
    validate_context(session, &args.project_id, args.revision)?;
    if args.clip_ids.len() > 10000 {
        return Err("Too many selected clips".into());
    }
    let clips: HashSet<&str> = session
        .project
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.id.as_str())
        .collect();
    let mut unique = HashSet::new();
    for id in &args.clip_ids {
        if !clips.contains(id.as_str()) || !unique.insert(id) {
            return Err("Selection contains an unknown or duplicate clip".into());
        }
    }
    Ok(())
}

fn ui_request(app: &AppHandle, action: &str, args: Value) -> Result<Value, String> {
    let state = app.state::<BridgeState>();
    let (id, receiver) = state.pending.register()?;
    if app
        .emit_to(
            "main",
            "kaicut:mcp-command",
            json!({
                "requestId": id, "action": action, "args": args
            }),
        )
        .is_err()
    {
        state.pending.remove(&id);
        return Err("KaiCut UI is unavailable".into());
    }
    let mut result = state.pending.wait(&id, receiver, UI_TIMEOUT)?;
    let window = app
        .get_webview_window("main")
        .ok_or("KaiCut window is unavailable")?;
    // DOM visibility can differ from native visibility (including hidden WebViews).
    // Read the native window instead of inferring it from the page or launch flags.
    result["nativeWindow"] = json!({
        "visible": window.is_visible().map_err(|_| "Cannot read window visibility")?,
        "focused": window.is_focused().map_err(|_| "Cannot read window focus")?,
        "minimized": window.is_minimized().map_err(|_| "Cannot read window state")?,
    });
    result["nativePreviewHost"] = tauri::async_runtime::block_on(
        crate::native_preview_host::native_preview_host_status(window),
    )?;
    Ok(result)
}

/// Only a running, opted-in bridge can create a pending request to acknowledge.
#[tauri::command]
pub fn mcp_ui_ack(
    window: tauri::WebviewWindow,
    request_id: String,
    result: Value,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("MCP acknowledgments require the main window".into());
    }
    window
        .state::<BridgeState>()
        .pending
        .acknowledge(&request_id, result)
}

fn publish_session(app: &AppHandle, session: ProjectSession) -> Result<Value, String> {
    // The edit already committed. An absent UI must not turn it into a retryable failure.
    let _ = app.emit_to("main", "kaicut:mcp-session", &session);
    to_json(session)
}

async fn prepare_export(
    app: AppHandle,
    args: ExportArgs,
) -> Result<export_job::ExportJobRecord, String> {
    // The MCP client cannot supply a forged preflight/snapshot or choose encoders.
    // Snapshot creation rechecks this result and the revision before registering
    // its immutable input. Preparing a job never starts the worker.
    let preflight = commands::export_preflight(
        app.clone(),
        args.project_id.clone(),
        args.revision,
        args.output_path.clone(),
        Some(false),
    )
    .await?;
    commands::export_snapshot_create(
        app,
        args.project_id,
        args.revision,
        args.output_path,
        Some(false),
        preflight,
    )
    .await
}

fn dispatch(app: &AppHandle, command: BridgeCommand) -> Result<Value, String> {
    match command {
        BridgeCommand::ProjectOpenPath(args) => {
            let session =
                tauri::async_runtime::block_on(mcp_project::open_path(app.clone(), args))?;
            let mut result = to_json(session)?;
            result["resetEditingView"] = true.into();
            let _ = app.emit_to("main", "kaicut:mcp-session", &result);
            Ok(result)
        }
        BridgeCommand::ProjectImportPaths(args) => publish_session(
            app,
            tauri::async_runtime::block_on(mcp_project::import_paths(app.clone(), args))?,
        ),
        BridgeCommand::ProjectSaveAs(args) => publish_session(
            app,
            tauri::async_runtime::block_on(mcp_project::save_as(app.clone(), args))?,
        ),
        BridgeCommand::GetState(_) => to_json(tauri::async_runtime::block_on(
            commands::project_get(app.clone()),
        )?),
        BridgeCommand::MediaCheckLinks(_) => to_json(tauri::async_runtime::block_on(
            commands::media_check_links(app.clone()),
        )?),
        BridgeCommand::MediaProbe(_) => to_json(tauri::async_runtime::block_on(
            media_engine::media_probe(app.clone()),
        )?),
        BridgeCommand::ExportJobList(_) => to_json(export_job::export_job_list(app.clone())?),
        BridgeCommand::ExportJobStatus(args) => {
            to_json(export_job::export_job_status(app.clone(), args.job_id)?)
        }
        BridgeCommand::ExportPreflight(args) => {
            to_json(tauri::async_runtime::block_on(commands::export_preflight(
                app.clone(),
                args.project_id,
                args.revision,
                args.output_path,
                Some(false),
            ))?)
        }
        BridgeCommand::ExportPrepare(args) => to_json(tauri::async_runtime::block_on(
            prepare_export(app.clone(), args),
        )?),
        // These operate on the committed job, not the currently edited project.
        BridgeCommand::ExportJobStart(args) => {
            to_json(export_job::export_job_start(app.clone(), args.job_id)?)
        }
        BridgeCommand::ExportJobCancel(args) => {
            to_json(export_job::export_job_cancel(app.clone(), args.job_id)?)
        }
        BridgeCommand::TimelineEdit(args) => publish_session(
            app,
            tauri::async_runtime::block_on(timeline::timeline_edit(
                app.clone(),
                args.project_id,
                args.revision,
                args.edit,
            ))?,
        ),
        BridgeCommand::TimelineUndo(args) => publish_session(
            app,
            tauri::async_runtime::block_on(timeline::timeline_undo(
                app.clone(),
                args.project_id,
                args.revision,
            ))?,
        ),
        BridgeCommand::TimelineRedo(args) => publish_session(
            app,
            tauri::async_runtime::block_on(timeline::timeline_redo(
                app.clone(),
                args.project_id,
                args.revision,
            ))?,
        ),
        BridgeCommand::UiInspect(_) => ui_request(app, "inspect", json!({})),
        BridgeCommand::UiSeek(args) => {
            let session = tauri::async_runtime::block_on(commands::project_get(app.clone()))?;
            validate_seek(&session, &args)?;
            ui_request(app, "seek", to_json(args)?)
        }
        BridgeCommand::UiSelect(args) => {
            let session = tauri::async_runtime::block_on(commands::project_get(app.clone()))?;
            validate_selection(&session, &args)?;
            ui_request(app, "select", to_json(args)?)
        }
        BridgeCommand::UiPlay(args) => {
            let session = tauri::async_runtime::block_on(commands::project_get(app.clone()))?;
            validate_context(&session, &args.project_id, args.revision)?;
            ui_request(app, "play", to_json(args)?)
        }
        BridgeCommand::UiPause(args) => {
            let session = tauri::async_runtime::block_on(commands::project_get(app.clone()))?;
            validate_context(&session, &args.project_id, args.revision)?;
            ui_request(app, "pause", to_json(args)?)
        }
    }
}

fn read_request(reader: impl BufRead) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((REQUEST_LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "MCP request read failed or timed out")?;
    if bytes.len() > REQUEST_LIMIT {
        return Err("MCP request exceeds 1 MiB".into());
    }
    if bytes.last() != Some(&b'\n') {
        return Err("MCP request requires a newline terminator".into());
    }
    Ok(bytes)
}

fn write_response(writer: &mut impl Write, response: Result<Value, String>) -> Result<(), String> {
    let envelope = match response {
        Ok(result) => json!({"ok":true, "result": result}),
        Err(error) => json!({"ok":false, "error": error}),
    };
    let mut bytes = serde_json::to_vec(&envelope).map_err(|_| "MCP response encoding failed")?;
    if bytes.len() >= RESPONSE_LIMIT {
        bytes = br#"{"ok":false,"error":"MCP response exceeds 16 MiB"}"#.to_vec();
    }
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .map_err(|_| "MCP response write failed".into())
}

fn handle_client(
    mut stream: TcpStream,
    token: &str,
    execute: impl FnOnce(BridgeCommand) -> Result<Value, String>,
) {
    let reader = DeadlineStream {
        stream: &mut stream,
        deadline: Instant::now() + Duration::from_secs(3),
    };
    let response = read_request(BufReader::new(reader))
        .and_then(|line| decode_request(&line, token))
        .and_then(execute);
    let mut writer = DeadlineStream {
        stream: &mut stream,
        deadline: Instant::now() + Duration::from_secs(5),
    };
    let _ = write_response(&mut writer, response);
}

struct DeadlineStream<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl DeadlineStream<'_> {
    fn remaining(&self) -> std::io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "MCP transport deadline expired",
                )
            })
    }
}

impl Read for DeadlineStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn new_token() -> String {
    // UUIDv4 fixes version/variant bits; omit those bytes to retain 256 random bits.
    let mut random = Vec::with_capacity(32);
    while random.len() < 32 {
        let uuid = Uuid::new_v4();
        for (index, byte) in uuid.as_bytes().iter().enumerate() {
            if index != 6 && index != 8 && random.len() < 32 {
                random.push(*byte);
            }
        }
    }
    random.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_connection(path: &Path, info: &ConnectionInfo) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("Invalid MCP connection path")?;
    #[cfg(not(unix))]
    fs::create_dir_all(parent).map_err(|_| "Cannot create MCP connection directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // Restrict newly created directories without chmod on caller-owned parents.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|_| "Cannot create MCP connection directory")?;
    }
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "Cannot create MCP connection file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "Cannot restrict MCP connection file")?;
    }
    serde_json::to_writer(&mut temp, info).map_err(|_| "Cannot encode MCP connection file")?;
    temp.as_file()
        .sync_all()
        .map_err(|_| "Cannot sync MCP connection file")?;
    temp.persist(path)
        .map_err(|_| "Cannot publish MCP connection file")?;
    Ok(())
}

fn cleanup_connection(path: &Path, token: &str) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let mut bytes = Vec::new();
    if file.take(4097).read_to_end(&mut bytes).is_err() || bytes.len() > 4096 {
        return;
    }
    let Ok(info) = serde_json::from_slice::<ConnectionInfo>(&bytes) else {
        return;
    };
    if tokens_match(&info.token, token) {
        let _ = fs::remove_file(path);
    }
}

/// The TCP listener never exists unless the desktop process was started with --mcp.
pub fn initialize(app: &AppHandle) -> Result<(), String> {
    if !std::env::args_os().any(|argument| argument == "--mcp") {
        return Ok(());
    }
    // For unattended integration checks; this does not activate remote desktop control.
    if std::env::args_os().any(|argument| argument == "--mcp-hidden") {
        if let Some(window) = app.get_webview_window("main") {
            window
                .hide()
                .map_err(|_| "Cannot hide MCP validation window")?;
        }
    }
    let path = match std::env::var_os("KAICUT_MCP_CONNECTION_FILE") {
        Some(path) => PathBuf::from(path),
        None => app
            .path()
            .app_local_data_dir()
            .map_err(|_| "MCP data directory unavailable")?
            .join("mcp/connection.json"),
    };
    if !path.is_absolute() {
        return Err("MCP connection path must be absolute".into());
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "Cannot bind MCP loopback bridge")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "Cannot configure MCP loopback bridge")?;
    let info = ConnectionInfo {
        version: 1,
        port: listener
            .local_addr()
            .map_err(|_| "Cannot read MCP bridge address")?
            .port(),
        token: new_token(),
        pid: std::process::id(),
    };
    write_connection(&path, &info)?;
    *app.state::<BridgeState>()
        .connection
        .lock()
        .map_err(|_| "MCP bridge state unavailable")? = Some((path, info.clone()));
    let bridge_app = app.clone();
    let active = Arc::new(AtomicUsize::new(0));
    let spawn = thread::Builder::new()
        .name("kaicut-mcp-listener".into())
        .spawn(move || {
            while !bridge_app
                .state::<BridgeState>()
                .stopping
                .load(Ordering::Acquire)
            {
                match listener.accept() {
                    Ok((mut stream, address)) => {
                        if !address.ip().is_loopback() {
                            continue;
                        }
                        if active.load(Ordering::Acquire) >= CLIENT_LIMIT {
                            let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                            let _ = write_response(&mut stream, Err("MCP bridge is busy".into()));
                            continue;
                        }
                        active.fetch_add(1, Ordering::AcqRel);
                        let client_app = bridge_app.clone();
                        let client_token = info.token.clone();
                        let count = active.clone();
                        if thread::Builder::new()
                            .name("kaicut-mcp-client".into())
                            .spawn(move || {
                                struct ClientGuard(Arc<AtomicUsize>);
                                impl Drop for ClientGuard {
                                    fn drop(&mut self) {
                                        self.0.fetch_sub(1, Ordering::AcqRel);
                                    }
                                }
                                let _guard = ClientGuard(count);
                                handle_client(stream, &client_token, |command| {
                                    dispatch(&client_app, command)
                                });
                            })
                            .is_err()
                        {
                            active.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(40))
                    }
                    Err(_) => break,
                }
            }
        });
    if spawn.is_err() {
        shutdown(app);
        return Err("Cannot start MCP bridge worker".into());
    }
    Ok(())
}

pub fn shutdown(app: &AppHandle) {
    let state = app.state::<BridgeState>();
    state.stopping.store(true, Ordering::Release);
    if let Ok(mut pending) = state.pending.0.lock() {
        pending.clear();
    }
    if let Ok(mut connection) = state.connection.lock() {
        if let Some((path, info)) = connection.take() {
            cleanup_connection(&path, &info.token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn authenticated_request_decodes_only_the_allowlisted_command() {
        let line = serde_json::to_vec(&json!({
            "token": "a".repeat(64), "command": "get_state", "args": {}
        }))
        .unwrap();
        assert!(matches!(
            decode_request(&line, &"a".repeat(64)),
            Ok(BridgeCommand::GetState(_))
        ));
    }

    #[test]
    fn export_commands_accept_only_the_current_fixed_arguments() {
        let token = "a".repeat(64);
        let directory = tempfile::tempdir().unwrap();
        let output_path = directory
            .path()
            .join("成片.mp4")
            .to_string_lossy()
            .into_owned();
        for (command, args) in [
            (
                "export_preflight",
                json!({"projectId":"project","revision":0,"outputPath":output_path}),
            ),
            (
                "export_prepare",
                json!({"projectId":"project","revision":0,"outputPath":output_path}),
            ),
            ("export_job_start", json!({"jobId":"job-123"})),
            ("export_job_cancel", json!({"jobId":"job-123"})),
        ] {
            assert!(
                decode_request(
                    &serde_json::to_vec(&json!({"token":token,"command":command,"args":args}))
                        .unwrap(),
                    &token
                )
                .is_ok(),
                "{command} should decode"
            );
        }
    }

    #[test]
    fn export_prepare_rejects_overwrite_snapshots_and_encoder_arguments() {
        let token = "a".repeat(64);
        for command in ["export_preflight", "export_prepare"] {
            for (key, value) in [
                ("overwrite", json!(true)),
                ("preflight", json!({"ready":true})),
                ("snapshot", json!({"schemaVersion":2})),
                ("ffmpegArgs", json!(["-y"])),
                ("encoder", json!("libx264")),
                ("shell", json!("anything")),
            ] {
                let mut args =
                    json!({"projectId":"project","revision":0,"outputPath":"output.mp4"});
                args[key] = value;
                assert!(
                    decode_request(
                        &serde_json::to_vec(&json!({"token":token,"command":command,"args":args}))
                            .unwrap(),
                        &token
                    )
                    .is_err(),
                    "{command} must reject {key}"
                );
            }
        }
    }

    #[test]
    fn export_requests_require_exact_project_context_and_safe_integer_revision() {
        let token = "a".repeat(64);
        for command in ["export_preflight", "export_prepare"] {
            for revision in [
                json!(-1),
                json!(0.5),
                json!("0"),
                json!(crate::project::MAX_SAFE_INTEGER + 1),
                json!(null),
            ] {
                assert!(decode_request(&serde_json::to_vec(&json!({"token":token,"command":command,
                    "args":{"projectId":"project","revision":revision,"outputPath":"output.mp4"}
                })).unwrap(),&token).is_err());
            }
            for missing in ["projectId", "revision", "outputPath"] {
                let mut args =
                    json!({"projectId":"project","revision":0,"outputPath":"output.mp4"});
                args.as_object_mut().unwrap().remove(missing);
                assert!(
                    decode_request(
                        &serde_json::to_vec(&json!({"token":token,"command":command,"args":args}))
                            .unwrap(),
                        &token
                    )
                    .is_err()
                );
            }
        }
        let command = decode_request(&serde_json::to_vec(&json!({"token":token,"command":"export_preflight",
            "args":{"projectId":"project","revision":crate::project::MAX_SAFE_INTEGER,"outputPath":"output.mp4"}
        })).unwrap(),&token).unwrap();
        assert!(matches!(
            command,
            BridgeCommand::ExportPreflight(ExportArgs {
                revision: crate::project::MAX_SAFE_INTEGER,
                ..
            })
        ));
    }

    #[test]
    fn job_control_rejects_paths_missing_ids_and_live_project_overrides() {
        let token = "a".repeat(64);
        for command in ["export_job_start", "export_job_cancel", "export_job_status"] {
            for id in [
                String::new(),
                "../job-123".into(),
                "C:/job-123".into(),
                "job;echo".into(),
                "x".repeat(81),
            ] {
                assert!(
                    decode_request(
                        &serde_json::to_vec(
                            &json!({"token":token,"command":command,"args":{"jobId":id}})
                        )
                        .unwrap(),
                        &token
                    )
                    .is_err()
                );
            }
            for args in [
                json!({}),
                json!({"jobId":"job-123","revision":0}),
                json!({"jobId":"job-123","projectId":"project"}),
                json!({"jobId":"job-123","outputPath":"other.mp4"}),
            ] {
                assert!(
                    decode_request(
                        &serde_json::to_vec(&json!({"token":token,"command":command,"args":args}))
                            .unwrap(),
                        &token
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn export_output_path_remains_literal_for_existing_native_preflight_validation() {
        let token = "a".repeat(64);
        let directory = tempfile::tempdir().unwrap();
        let output_path = directory
            .path()
            .join("成片 空格;[literal]%03d.mp4")
            .to_string_lossy()
            .into_owned();
        for command in ["export_preflight", "export_prepare"] {
            let decoded = decode_request(
                &serde_json::to_vec(&json!({"token":token,"command":command,
                    "args":{"projectId":"project","revision":2,"outputPath":output_path}
                }))
                .unwrap(),
                &token,
            )
            .unwrap();
            let args = match decoded {
                BridgeCommand::ExportPreflight(args) | BridgeCommand::ExportPrepare(args) => args,
                _ => panic!("Expected typed export command"),
            };
            assert_eq!(args.output_path, output_path);
            assert_eq!(args.project_id, "project");
            assert_eq!(args.revision, 2);
        }
        assert!(!Path::new(&output_path).exists());
    }

    #[test]
    fn authentication_and_unknown_fields_do_not_echo_credentials() {
        let secret = "a".repeat(64);
        for request in [
            json!({"token": "wrong", "command": "get_state", "args": {}}),
            json!({"token": secret, "command": "eval", "args": {"code":"anything"}}),
            json!({"token": secret, "command": "get_state", "args": {"path":"anything"}}),
        ] {
            let error =
                decode_request(&serde_json::to_vec(&request).unwrap(), &secret).unwrap_err();
            assert!(!error.contains(&secret));
        }
    }

    #[test]
    fn input_framing_rejects_incomplete_oversized_and_non_json_requests() {
        assert!(read_request(&b"{}"[..]).unwrap_err().contains("newline"));
        assert!(
            read_request(vec![b'x'; REQUEST_LIMIT + 1].as_slice())
                .unwrap_err()
                .contains("1 MiB")
        );
        assert!(decode_request(b"GET / HTTP/1.1", &"a".repeat(64)).is_err());
        assert_eq!(read_request(&b"{}\nignored\n"[..]).unwrap(), b"{}\n");
    }

    #[test]
    fn typed_mutations_reject_negative_fractional_and_unknown_fields() {
        for args in [
            json!({"projectId":"project","revision":0,"frame":-1}),
            json!({"projectId":"project","revision":0,"frame":0.5}),
            json!({"projectId":"project","revision":0,"frame":0,"path":"x"}),
        ] {
            assert!(
                decode_request(
                    &serde_json::to_vec(&json!({
                        "token":"a".repeat(64),"command":"ui_seek","args":args
                    }))
                    .unwrap(),
                    &"a".repeat(64)
                )
                .is_err()
            );
        }
        assert!(decode_request(&serde_json::to_vec(&json!({
            "token":"a".repeat(64),"command":"timeline_edit",
            "args":{"projectId":"project","revision":0,"edit":{"type":"shell","command":"x"}}
        })).unwrap(), &"a".repeat(64)).is_err());
    }

    #[test]
    fn project_commands_require_context_and_reject_implicit_discard_or_overwrite() {
        let token = "a".repeat(64);
        for (command, args) in [
            ("project_open_path", json!({"path":"C:/project.kaicut"})),
            (
                "project_open_path",
                json!({"projectId":"p","revision":0,"path":"C:/project.kaicut","discard":true}),
            ),
            (
                "project_save_as",
                json!({"projectId":"p","revision":0,"path":"C:/project.kaicut","overwrite":true}),
            ),
            (
                "project_import_paths",
                json!({"projectId":"p","revision":0,"paths":"C:/clip.mp4"}),
            ),
        ] {
            assert!(
                decode_request(
                    &serde_json::to_vec(&json!({"token":token,"command":command,"args":args}))
                        .unwrap(),
                    &token
                )
                .is_err()
            );
        }
        for (command, args) in [
            (
                "project_open_path",
                json!({"projectId":"p","revision":0,"path":"C:/project.kaicut"}),
            ),
            (
                "project_save_as",
                json!({"projectId":"p","revision":0,"path":"C:/project.kaicut"}),
            ),
            (
                "project_import_paths",
                json!({"projectId":"p","revision":0,"paths":["C:/clip.mp4"]}),
            ),
        ] {
            assert!(
                decode_request(
                    &serde_json::to_vec(&json!({"token":token,"command":command,"args":args}))
                        .unwrap(),
                    &token
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn ui_actions_validate_live_project_revision_bounds_and_clip_membership() {
        let mut session = ProjectSession::default();
        let project_id = session.project.project_id.clone();
        assert!(validate_context(&session, &project_id, 0).is_ok());
        assert!(validate_context(&session, "different-project", 0).is_err());
        assert!(validate_context(&session, &project_id, 1).is_err());
        assert!(
            validate_seek(
                &session,
                &SeekArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    frame: 0
                }
            )
            .is_ok()
        );
        assert!(
            validate_seek(
                &session,
                &SeekArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    frame: 1
                }
            )
            .is_err()
        );
        assert!(
            validate_selection(
                &session,
                &SelectArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    clip_ids: vec![]
                }
            )
            .is_ok()
        );
        assert!(
            validate_selection(
                &session,
                &SelectArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    clip_ids: vec!["missing".into()]
                }
            )
            .is_err()
        );
        session.project.tracks[0]
            .clips
            .push(crate::project::TimelineClip {
                id: "clip".into(),
                media_id: "media".into(),
                track_id: "video-1".into(),
                start_frame: 0,
                duration_frames: 30,
                source_in_frame: 0,
                source_duration_frames: 30,
                fade_in_frames: 0,
                fade_out_frames: 0,
            });
        assert!(
            validate_seek(
                &session,
                &SeekArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    frame: 30
                }
            )
            .is_ok()
        );
        assert!(
            validate_seek(
                &session,
                &SeekArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    frame: 31
                }
            )
            .is_err()
        );
        assert!(
            validate_selection(
                &session,
                &SelectArgs {
                    project_id: project_id.clone(),
                    revision: 0,
                    clip_ids: vec!["clip".into()]
                }
            )
            .is_ok()
        );
        assert!(
            validate_selection(
                &session,
                &SelectArgs {
                    project_id,
                    revision: 0,
                    clip_ids: vec!["clip".into(), "clip".into()]
                }
            )
            .is_err()
        );
    }

    #[test]
    fn pending_ui_requests_complete_once_and_timeout_without_leaking_slots() {
        let pending = PendingReplies::default();
        let (id, receiver) = pending.register().unwrap();
        pending.acknowledge(&id, json!({"playing":false})).unwrap();
        assert!(pending.acknowledge(&id, json!({})).is_err());
        assert_eq!(
            pending
                .wait(&id, receiver, Duration::from_millis(1))
                .unwrap(),
            json!({"playing":false})
        );
        let (id, receiver) = pending.register().unwrap();
        assert!(
            pending
                .wait(&id, receiver, Duration::from_millis(1))
                .unwrap_err()
                .contains("deadline")
        );
        assert!(pending.acknowledge(&id, json!({})).is_err());
        assert!(pending.0.lock().unwrap().is_empty());
        let (id, receiver) = pending.register().unwrap();
        pending
            .acknowledge(&id, json!({"error":"Playback unavailable"}))
            .unwrap();
        assert_eq!(
            pending
                .wait(&id, receiver, Duration::from_millis(1))
                .unwrap_err(),
            "Playback unavailable"
        );
    }

    #[test]
    fn pending_ui_requests_enforce_capacity_and_object_result_limit() {
        let pending = PendingReplies::default();
        let requests: Vec<_> = (0..CLIENT_LIMIT)
            .map(|_| pending.register().unwrap())
            .collect();
        assert!(pending.register().is_err());
        assert!(pending.acknowledge(&requests[0].0, json!(null)).is_err());
        assert!(
            pending
                .acknowledge(&requests[0].0, json!({"text":"x".repeat(REQUEST_LIMIT)}))
                .is_err()
        );
        pending.remove(&requests[0].0);
        assert!(pending.register().is_ok());
    }

    #[test]
    fn rendezvous_cleanup_removes_only_its_own_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp/connection.json");
        let first = ConnectionInfo {
            version: 1,
            port: 1234,
            token: new_token(),
            pid: std::process::id(),
        };
        assert_eq!(first.token.len(), 64);
        assert!(first.token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        write_connection(&path, &first).unwrap();
        let second = ConnectionInfo {
            token: new_token(),
            ..first.clone()
        };
        write_connection(&path, &second).unwrap();
        cleanup_connection(&path, &first.token);
        assert!(path.exists());
        cleanup_connection(&path, &second.token);
        assert!(!path.exists());
    }

    #[test]
    fn tcp_bridge_authenticates_before_dispatching_and_returns_ndjson() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let token = new_token();
        let server_token = token.clone();
        let dispatched = Arc::new(AtomicUsize::new(0));
        let count = dispatched.clone();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                handle_client(stream, &server_token, |command| {
                    count.fetch_add(1, Ordering::Relaxed);
                    assert!(matches!(command, BridgeCommand::GetState(_)));
                    Ok(json!({"revision":3}))
                });
            }
        });
        for (candidate, accepted) in [("wrong".to_string(), false), (token, true)] {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            writeln!(
                client,
                "{}",
                json!({"token":candidate,"command":"get_state","args":{}})
            )
            .unwrap();
            let mut response = String::new();
            BufReader::new(client).read_line(&mut response).unwrap();
            let value: Value = serde_json::from_str(&response).unwrap();
            assert_eq!(value["ok"], accepted);
            if accepted {
                assert_eq!(value["result"]["revision"], 3);
            }
        }
        server.join().unwrap();
        assert_eq!(dispatched.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn output_limit_returns_small_error_instead_of_large_payload() {
        let mut output = Vec::new();
        write_response(&mut output, Ok(json!({"data":"x".repeat(RESPONSE_LIMIT)}))).unwrap();
        assert!(output.len() < 1024);
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["ok"], false);
    }
}
