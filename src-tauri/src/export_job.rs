use crate::project::KaicutProject;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::Manager;
use tempfile::NamedTempFile;
use uuid::Uuid;

pub const EXPORT_SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// Cancellation remains Running until the worker has reaped its child and
/// removed its temporary output. A close guard must treat that as active work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportJobState {
    Queued,
    Running,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportJobEvent {
    Start,
    Complete,
    Cancel,
    Fail,
    Interrupt,
}

/// Pure state transition function used by the encoder supervisor and
/// tested independently from Tauri or the filesystem.
pub fn transition_state(
    current: ExportJobState,
    event: ExportJobEvent,
) -> Result<ExportJobState, String> {
    use ExportJobEvent::{Cancel, Complete, Fail, Interrupt, Start};
    use ExportJobState::{Cancelled, Completed, Failed, Interrupted, Queued, Running};

    match (current, event) {
        (Queued | Failed | Cancelled, Start) => Ok(Running),
        (Queued, Cancel) => Ok(Cancelled),
        (Running, Complete) => Ok(Completed),
        (Running, Cancel) => Ok(Cancelled),
        (Running, Fail) => Ok(Failed),
        (Running, Interrupt) => Ok(Interrupted),
        // A repeated cancel is harmless for a queued/cancelled UI action.
        (Cancelled, Cancel) => Ok(Cancelled),
        (state, event) => Err(format!("导出任务不能从 {:?} 通过 {:?} 转换", state, event)),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJobRecord {
    pub job_id: String,
    pub project_id: String,
    pub revision: u64,
    pub snapshot_path: String,
    pub output_path: String,
    pub state: ExportJobState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub retry_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ExportJobRecord {
    pub fn transition(&mut self, event: ExportJobEvent) -> Result<(), String> {
        let next = transition_state(self.state.clone(), event.clone())?;
        self.state = next;
        self.updated_at = timestamp();
        if matches!(event, ExportJobEvent::Fail | ExportJobEvent::Interrupt) {
            self.error = Some(match event {
                ExportJobEvent::Fail => "导出任务失败".into(),
                ExportJobEvent::Interrupt => "应用关闭时导出任务被中断".into(),
                _ => unreachable!(),
            });
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ExportJobsState {
    pub jobs: Mutex<HashMap<String, ExportJobRecord>>,
    // Only snapshots created and validated by this process may execute. Loaded
    // history is deliberately read-only; disk edits never become authority.
    snapshots: Mutex<HashMap<String, ExportSnapshot>>,
    temporary_outputs: Mutex<HashMap<String, String>>,
}

fn cleanup_tracked_temporary(
    record: &ExportJobRecord,
    paths: &mut HashMap<String, String>,
) -> Result<(), String> {
    cleanup_recovered_temporary(record, paths.get(&record.job_id).map(String::as_str))?;
    paths.remove(&record.job_id);
    Ok(())
}

fn finish_unsuccessful_worker(
    job: &mut ExportJobRecord,
    result: Result<(), String>,
    cleanup_error: Option<String>,
) -> Result<(), String> {
    if job.cancel_requested {
        job.transition(ExportJobEvent::Cancel)?;
        job.cancel_requested = false;
        job.error = cleanup_error.map(|error| format!("导出已取消，但临时文件清理失败：{error}；解除文件占用后可重试，或下次启动时继续清理"));
    } else {
        job.transition(ExportJobEvent::Fail)?;
        let mut message = result
            .err()
            .unwrap_or_else(|| "导出未生成可验证结果".into());
        if let Some(error) = cleanup_error {
            message.push_str(&format!(
                "；临时文件清理失败：{error}；解除文件占用后可重试，或下次启动时继续清理"
            ));
        }
        job.error = Some(message);
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiskJob {
    record: ExportJobRecord,
    temporary_output_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportMediaSnapshot {
    pub id: String,
    pub path: Option<String>,
    pub size: u64,
    pub last_modified: u64,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Immutable input to a future export worker. It intentionally contains the
/// persisted v2 project and media fingerprints, but no asset URLs, waveform
/// data, playhead, selection, or Zustand state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSnapshot {
    pub format_version: u32,
    pub job_id: String,
    pub created_at: String,
    pub project_id: String,
    pub revision: u64,
    pub output_path: String,
    pub duration_frames: u64,
    pub project: KaicutProject,
    pub media: Vec<ExportMediaSnapshot>,
}

pub fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn new_job_id() -> String {
    format!("job-{}", Uuid::new_v4())
}

pub fn validate_job_id(job_id: &str) -> Result<(), String> {
    if job_id.len() > 80
        || job_id.is_empty()
        || !job_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("导出任务 ID 无效".into());
    }
    Ok(())
}

pub fn media_snapshot(project: &KaicutProject) -> Vec<ExportMediaSnapshot> {
    let used: std::collections::HashSet<&str> = project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(|clip| clip.media_id.as_str()))
        .collect();
    project
        .media_refs
        .iter()
        .filter(|media| used.contains(media.id.as_str()))
        .map(|media| {
            let status = media
                .source_path
                .as_deref()
                .map(Path::new)
                .and_then(|path| {
                    fs::metadata(path)
                        .ok()
                        .filter(|metadata| metadata.is_file())
                })
                .map(|metadata| {
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|value| value.as_millis() as u64);
                    if metadata.len() == media.size && modified == Some(media.last_modified) {
                        ("online".to_string(), None)
                    } else {
                        (
                            "changed".to_string(),
                            Some("文件大小或修改时间已变化".into()),
                        )
                    }
                })
                .unwrap_or_else(|| ("missing".into(), Some("找不到原始素材".into())));
            ExportMediaSnapshot {
                id: media.id.clone(),
                path: media.source_path.clone(),
                size: media.size,
                last_modified: media.last_modified,
                state: status.0,
                reason: status.1,
            }
        })
        .collect()
}

pub fn write_snapshot_to_dir(
    export_jobs_dir: &Path,
    snapshot: &ExportSnapshot,
) -> Result<PathBuf, String> {
    validate_job_id(&snapshot.job_id)?;
    snapshot.project.validate()?;
    if snapshot.project.project_id != snapshot.project_id || snapshot.project.schema_version != 2 {
        return Err("导出快照的项目身份或 schema 版本无效".into());
    }
    if snapshot.output_path.trim().is_empty() || !Path::new(&snapshot.output_path).is_absolute() {
        return Err("导出快照的输出路径必须是绝对路径".into());
    }
    fs::create_dir_all(export_jobs_dir)
        .map_err(|error| format!("无法创建导出任务目录：{error}"))?;
    let job_dir = export_jobs_dir.join(&snapshot.job_id);
    fs::create_dir(&job_dir).map_err(|error| format!("无法创建导出任务目录：{error}"))?;
    let path = job_dir.join("snapshot.json");
    let bytes = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("无法序列化导出快照：{error}"))?;
    let mut temporary = match NamedTempFile::new_in(&job_dir) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_dir_all(&job_dir);
            return Err(format!("无法创建导出快照临时文件：{error}"));
        }
    };
    if let Err(error) = temporary
        .write_all(&bytes)
        .and_then(|_| temporary.flush())
        .and_then(|_| temporary.as_file().sync_all())
    {
        let _ = fs::remove_dir_all(&job_dir);
        return Err(format!("写入导出快照失败：{error}"));
    }
    if let Err(error) = temporary.persist(&path) {
        let _ = fs::remove_dir_all(&job_dir);
        return Err(format!("保存导出快照失败：{}", error.error));
    }
    Ok(path)
}

pub fn export_jobs_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?
        .join("export-jobs"))
}

fn persist_record(app: &tauri::AppHandle, record: &ExportJobRecord) -> Result<(), String> {
    let directory = export_jobs_dir(app)?.join(&record.job_id);
    let mut temporary =
        NamedTempFile::new_in(&directory).map_err(|error| format!("无法保存导出任务：{error}"))?;
    let state = app.state::<ExportJobsState>();
    let temporary_output_path = state
        .temporary_outputs
        .lock()
        .map_err(|_| "导出临时文件状态不可用")?
        .get(&record.job_id)
        .cloned();
    serde_json::to_writer_pretty(
        &mut temporary,
        &DiskJob {
            record: record.clone(),
            temporary_output_path,
        },
    )
    .map_err(|error| format!("无法保存导出任务：{error}"))?;
    temporary
        .flush()
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("无法保存导出任务：{error}"))?;
    temporary
        .persist(directory.join("job.json"))
        .map_err(|error| format!("无法保存导出任务：{}", error.error))?;
    Ok(())
}

pub fn register_job(
    app: &tauri::AppHandle,
    mut record: ExportJobRecord,
    snapshot: ExportSnapshot,
) -> Result<ExportJobRecord, String> {
    validate_job_id(&record.job_id)?;
    if record.job_id != snapshot.job_id
        || record.project_id != snapshot.project_id
        || record.revision != snapshot.revision
        || record.output_path != snapshot.output_path
    {
        return Err("导出任务与快照身份不一致".into());
    }
    verify_snapshot_file(&record, &snapshot)?;
    let state = app.state::<ExportJobsState>();
    let mut snapshots = state.snapshots.lock().map_err(|_| "导出快照状态不可用")?;
    let mut jobs = state.jobs.lock().map_err(|_| "导出任务状态不可用")?;
    if jobs.contains_key(&record.job_id) {
        return Err("导出任务已存在".into());
    }
    record.retry_available = true;
    persist_record(app, &record)?;
    snapshots.insert(record.job_id.clone(), snapshot);
    jobs.insert(record.job_id.clone(), record.clone());
    Ok(record)
}

/// Recovered records are history only. Do not trust a persisted project as
/// executable authority or infer that an output exists after an interrupted run.
pub fn initialize(app: &tauri::AppHandle) -> Result<(), String> {
    let root = export_jobs_dir(app)?;
    if !root.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(&root).map_err(|error| format!("无法读取导出历史：{error}"))?;
    let state = app.state::<ExportJobsState>();
    let mut jobs = state.jobs.lock().map_err(|_| "导出任务状态不可用")?;
    for entry in entries.take(500).flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if validate_job_id(&id).is_err() {
            continue;
        }
        let path = entry.path().join("job.json");
        let Ok(bytes) = read_bounded(&path, 128 * 1024) else {
            continue;
        };
        let Ok(disk) = serde_json::from_slice::<DiskJob>(&bytes).or_else(|_| {
            serde_json::from_slice::<ExportJobRecord>(&bytes).map(|record| DiskJob {
                record,
                temporary_output_path: None,
            })
        }) else {
            continue;
        };
        let mut record = disk.record;
        if record.job_id != id {
            continue;
        }
        record.snapshot_path = entry
            .path()
            .join("snapshot.json")
            .to_string_lossy()
            .into_owned();
        record.retry_available = false;
        let cleanup_error =
            cleanup_recovered_temporary(&record, disk.temporary_output_path.as_deref()).err();
        if cleanup_error.is_some() {
            if let Some(path) = disk.temporary_output_path {
                state
                    .temporary_outputs
                    .lock()
                    .map_err(|_| "导出临时文件状态不可用")?
                    .insert(record.job_id.clone(), path);
            }
        }
        cleanup_filter_scripts(&entry.path(), &record.job_id);
        if matches!(
            record.state,
            ExportJobState::Queued | ExportJobState::Running
        ) {
            record.state = ExportJobState::Interrupted;
            record.cancel_requested = false;
            record.error =
                Some("上次应用退出时任务未完成。请重新预检创建任务；未确认最终输出。".into());
            if let Some(error) = &cleanup_error {
                record.error = Some(format!(
                    "上次导出被中断，临时文件未能清理：{error}。请重新预检。"
                ));
            }
            record.updated_at = timestamp();
            if let Err(error) = persist_record(app, &record) {
                log::warn!("Unable to save recovered export history: {error}");
            }
        } else if let Some(error) = cleanup_error {
            record.error = Some(format!("历史任务临时文件未能清理：{error}"));
        }
        jobs.insert(id, record);
    }
    Ok(())
}

fn cleanup_recovered_temporary(
    record: &ExportJobRecord,
    temporary: Option<&str>,
) -> Result<(), String> {
    let Some(temporary) = temporary else {
        return Ok(());
    };
    validate_job_id(&record.job_id)?;
    let path = Path::new(temporary);
    let destination = Path::new(&record.output_path);
    if !path.is_absolute() || !destination.is_absolute() || path == destination {
        return Err("临时文件路径不符合任务约束".into());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("临时文件名无效")?;
    let prefix = format!(".kaicut-export-{}-", record.job_id);
    let unique = name
        .strip_prefix(&prefix)
        .and_then(|name| name.strip_suffix(".mp4"));
    if !unique.is_some_and(|value| {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        return Err("临时文件名不属于该任务".into());
    }
    let parent = path
        .parent()
        .ok_or("临时文件目录无效")?
        .canonicalize()
        .map_err(|_| "临时文件目录已离线")?;
    let output_parent = destination
        .parent()
        .ok_or("输出目录无效")?
        .canonicalize()
        .map_err(|_| "输出目录已离线")?;
    if parent != output_parent {
        return Err("临时文件不在输出目录".into());
    }
    if let (Ok(source), Ok(output)) = (path.canonicalize(), destination.canonicalize()) {
        if source == output {
            return Err("临时文件路径指向最终输出".into());
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|error| format!("无法删除临时文件：{error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("临时文件不是普通文件".into()),
    }
}

fn cleanup_filter_scripts(directory: &Path, id: &str) {
    let prefix = format!("filter-{id}-");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.take(500).flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let unique = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".txt"));
        if unique.is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) && entry
            .file_type()
            .is_ok_and(|kind| kind.is_file() && !kind.is_symlink())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub fn has_running_jobs(app: &tauri::AppHandle) -> bool {
    app.state::<ExportJobsState>()
        .jobs
        .lock()
        .map(|jobs| {
            jobs.values()
                .any(|job| job.state == ExportJobState::Running)
        })
        .unwrap_or(true)
}

#[tauri::command]
pub fn export_job_list(app: tauri::AppHandle) -> Result<Vec<ExportJobRecord>, String> {
    let state = app.state::<ExportJobsState>();
    let jobs = state.jobs.lock().map_err(|_| "导出任务状态不可用")?;
    let mut records: Vec<_> = jobs.values().cloned().collect();
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(records)
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| format!("无法读取导出文件：{error}"))?;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取导出文件：{error}"))?;
    if bytes.len() as u64 > maximum {
        return Err("导出文件超过允许大小".into());
    }
    Ok(bytes)
}

fn verify_snapshot_file(record: &ExportJobRecord, snapshot: &ExportSnapshot) -> Result<(), String> {
    let bytes = read_bounded(Path::new(&record.snapshot_path), 20 * 1024 * 1024)?;
    let on_disk: ExportSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| "导出快照已损坏，请重新预检")?;
    if on_disk != *snapshot {
        return Err("导出快照已被修改，请重新预检".into());
    }
    snapshot.project.validate()?;
    if snapshot.format_version != EXPORT_SNAPSHOT_FORMAT_VERSION
        || snapshot.project.schema_version != 2
    {
        return Err("不支持的导出快照版本".into());
    }
    Ok(())
}

fn verify_sources(snapshot: &ExportSnapshot) -> Result<(), String> {
    let current = media_snapshot(&snapshot.project);
    if current != snapshot.media || current.iter().any(|media| media.state != "online") {
        return Err("素材在预检后发生变化或已离线，请重新链接并预检".into());
    }
    Ok(())
}

fn request_cancel(job: &mut ExportJobRecord) -> Result<(), String> {
    match job.state {
        ExportJobState::Running => {
            job.cancel_requested = true;
            job.updated_at = timestamp();
            Ok(())
        }
        ExportJobState::Queued | ExportJobState::Cancelled => {
            job.transition(ExportJobEvent::Cancel)?;
            job.cancel_requested = false;
            Ok(())
        }
        _ => Err("这个导出任务已经结束".into()),
    }
}

fn cancelled(app: &tauri::AppHandle, id: &str) -> bool {
    app.state::<ExportJobsState>()
        .jobs
        .lock()
        .map(|jobs| jobs.get(id).is_none_or(|job| job.cancel_requested))
        .unwrap_or(true)
}

#[tauri::command]
pub fn export_job_start(app: tauri::AppHandle, job_id: String) -> Result<ExportJobRecord, String> {
    validate_job_id(&job_id)?;
    if !cfg!(target_os = "windows") {
        return Err("当前导出仅支持 Windows 的 Media Foundation 编码器".into());
    }
    let ffmpeg =
        crate::media_engine::packaged_binary(&app, "ffmpeg").ok_or("尚未随应用打包 FFmpeg")?;
    let ffprobe =
        crate::media_engine::packaged_binary(&app, "ffprobe").ok_or("尚未随应用打包 ffprobe")?;
    let state = app.state::<ExportJobsState>();
    let snapshot = state
        .snapshots
        .lock()
        .map_err(|_| "导出快照状态不可用")?
        .get(&job_id)
        .cloned()
        .ok_or("这个任务来自上次运行，请重新预检创建任务")?;
    let mut jobs = state.jobs.lock().map_err(|_| "导出任务状态不可用")?;
    if jobs
        .values()
        .any(|job| job.state == ExportJobState::Running)
    {
        return Err("已有导出任务正在运行或取消，请等待它结束".into());
    }
    let record = jobs.get_mut(&job_id).ok_or("找不到导出任务")?;
    // A retry must not overwrite tracking for a leftover file whose deletion
    // was blocked by another Windows process during the previous attempt.
    {
        let mut paths = state
            .temporary_outputs
            .lock()
            .map_err(|_| "导出临时文件状态不可用")?;
        cleanup_tracked_temporary(record, &mut paths).map_err(|error| {
            format!("上次导出的临时文件仍未清理，请解除文件占用后重试：{error}")
        })?;
    }
    if Path::new(&record.output_path).exists() {
        return Err("输出文件已存在，请选择其他文件名并重新预检".into());
    }
    let previous = record.clone();
    record.transition(ExportJobEvent::Start)?;
    record.progress = 0.0;
    record.cancel_requested = false;
    record.error = None;
    if let Err(error) = persist_record(&app, record) {
        *record = previous;
        return Err(error);
    }
    let started = record.clone();
    drop(jobs);
    let worker_app = app.clone();
    let worker_id = job_id.clone();
    if let Err(error) = thread::Builder::new()
        .name(format!("export-{job_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_worker(&worker_app, &worker_id, &snapshot, &ffmpeg, &ffprobe)
            }))
            .unwrap_or_else(|_| Err("导出执行异常".into()));
            let state = worker_app.state::<ExportJobsState>();
            if let Ok(mut jobs) = state.jobs.lock() {
                if let Some(job) = jobs.get_mut(&worker_id) {
                    if job.state == ExportJobState::Running {
                        // TempPath's Drop may fail silently (e.g. a Windows
                        // reader denied DELETE sharing). Verify cleanup now,
                        // retaining the path in job.json whenever it fails.
                        let cleanup_error = match state.temporary_outputs.lock() {
                            Ok(mut paths) => cleanup_tracked_temporary(job, &mut paths).err(),
                            Err(_) => Some("导出临时文件状态不可用".into()),
                        };
                        if let Err(error) = finish_unsuccessful_worker(job, result, cleanup_error) {
                            log::error!("{error}");
                        }
                        if let Err(error) = persist_record(&worker_app, job) {
                            log::error!("{error}");
                        }
                    }
                }
            }
        })
    {
        let mut jobs = state.jobs.lock().map_err(|_| "导出任务状态不可用")?;
        if let Some(job) = jobs.get_mut(&job_id) {
            job.transition(ExportJobEvent::Fail)?;
            job.error = Some(format!("无法启动导出任务：{error}"));
            persist_record(&app, job)?;
        }
        return Err(format!("无法启动导出任务：{error}"));
    }
    Ok(started)
}

fn update_progress(app: &tauri::AppHandle, id: &str, progress: f64) {
    if let Ok(mut jobs) = app.state::<ExportJobsState>().jobs.lock() {
        if let Some(job) = jobs.get_mut(id) {
            job.progress = job.progress.max(progress.clamp(0.0, 99.0));
        }
    }
}

fn duration_seconds(snapshot: &ExportSnapshot) -> f64 {
    snapshot.duration_frames as f64 * snapshot.project.timebase.fps_denominator as f64
        / snapshot.project.timebase.fps_numerator as f64
}

fn run_worker(
    app: &tauri::AppHandle,
    id: &str,
    snapshot: &ExportSnapshot,
    ffmpeg: &Path,
    ffprobe: &Path,
) -> Result<(), String> {
    let record = export_job_status(app.clone(), id.to_owned())?;
    verify_snapshot_file(&record, snapshot)?;
    verify_sources(snapshot)?;
    if snapshot.duration_frames == 0 {
        return Err("时间线为空，无法导出".into());
    }
    let destination = Path::new(&snapshot.output_path);
    if destination.exists() {
        return Err("输出文件已存在，不会覆盖".into());
    }
    let parent = destination.parent().ok_or("输出目录无效")?;
    let temporary = tempfile::Builder::new()
        .prefix(&format!(".kaicut-export-{id}-"))
        .suffix(".mp4")
        .tempfile_in(parent)
        .map_err(|error| format!("无法创建导出临时文件：{error}"))?
        .into_temp_path();
    app.state::<ExportJobsState>()
        .temporary_outputs
        .lock()
        .map_err(|_| "导出临时文件状态不可用")?
        .insert(id.to_string(), temporary.to_string_lossy().into_owned());
    let running_record = export_job_status(app.clone(), id.to_string())?;
    persist_record(app, &running_record)?;
    let job_directory = export_jobs_dir(app)?.join(id);
    let mut filter_script = tempfile::Builder::new()
        .prefix(&format!("filter-{id}-"))
        .suffix(".txt")
        .tempfile_in(&job_directory)
        .map_err(|error| format!("无法创建导出滤镜文件：{error}"))?;
    let plan = crate::export_render::render_plan_cancellable(
        snapshot,
        ffprobe,
        &temporary,
        filter_script.path(),
        || cancelled(app, id),
    )?;
    filter_script
        .write_all(plan.filter_graph.as_bytes())
        .and_then(|_| filter_script.flush())
        .map_err(|error| format!("无法写入导出滤镜文件：{error}"))?;
    if cancelled(app, id) {
        return Err("导出已取消".into());
    }
    let mut command = Command::new(ffmpeg);
    command
        .args(plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::media_engine::hide_console(&mut command);
    let (status, stderr) = supervise_encoder(
        command,
        duration_seconds(snapshot),
        || cancelled(app, id),
        |progress| update_progress(app, id, progress),
    )?;
    if !status.success() {
        return Err(format!(
            "FFmpeg 导出失败：{}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    if cancelled(app, id) {
        return Err("导出已取消".into());
    }
    verify_output(ffprobe, &temporary, snapshot)?;
    verify_snapshot_file(&record, snapshot)?;
    verify_sources(snapshot)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("无法同步导出文件：{error}"))?;
    // One lock covers the final cancellation check and no-clobber publication.
    // A cancel racing here either wins before publication or observes Completed.
    let state = app.state::<ExportJobsState>();
    let mut jobs = state.jobs.lock().map_err(|_| "导出任务状态不可用")?;
    let job = jobs.get_mut(id).ok_or("找不到导出任务")?;
    publish_output(temporary, destination, job)?;
    // persist_noclobber consumed the temporary path successfully. Clear only
    // its tracking, never run failure cleanup against a completed output.
    if let Ok(mut paths) = state.temporary_outputs.lock() {
        paths.remove(id);
    }
    if let Err(error) = persist_record(app, job) {
        // The video is already durably published; never report it as failed or
        // delete a successful user output because a history write failed.
        job.error = Some(format!("视频已导出，但任务历史保存失败：{error}"));
        log::error!("{error}");
    }
    Ok(())
}

/// Caller holds the jobs mutex for the whole publication so cancellation and
/// completion have a single ordering; the helper consumes/cleans its temp path.
fn publish_output(
    temporary: tempfile::TempPath,
    destination: &Path,
    job: &mut ExportJobRecord,
) -> Result<(), String> {
    if job.cancel_requested {
        return Err("导出已取消".into());
    }
    if job.state != ExportJobState::Running {
        return Err("导出任务已不在运行状态".into());
    }
    temporary
        .persist_noclobber(destination)
        .map_err(|error| format!("无法提交导出文件（不会覆盖已有文件）：{}", error.error))?;
    job.transition(ExportJobEvent::Complete)?;
    job.progress = 100.0;
    job.error = None;
    Ok(())
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "windows")]
struct ProcessJob(windows::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl ProcessJob {
    fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows::{
            Win32::{Foundation::HANDLE, System::JobObjects::*},
            core::PCWSTR,
        };
        // The handle is non-inheritable. Closing the app's last handle causes
        // Windows to terminate the encoder even after unexpected app exit.
        unsafe {
            let job = Self(
                CreateJobObjectW(None, PCWSTR::null())
                    .map_err(|error| format!("无法保护编码进程：{error}"))?,
            );
            let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(|error| format!("无法保护编码进程：{error}"))?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle()))
                .map_err(|error| format!("无法保护编码进程：{error}"))?;
            Ok(job)
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn read_stderr(mut reader: impl Read) -> Vec<u8> {
    const LIMIT: usize = 64 * 1024;
    let mut tail = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                tail.extend_from_slice(&buffer[..size]);
                if tail.len() > LIMIT {
                    tail.drain(..tail.len() - LIMIT);
                }
            }
        }
    }
    tail
}

fn progress_line(line: &[u8], latest: &AtomicU64) {
    if let Some(value) = line.strip_prefix(b"out_time_us=") {
        if let Ok(micros) = String::from_utf8_lossy(value).trim().parse::<u64>() {
            latest.fetch_max(micros, Ordering::Relaxed);
        }
    }
}

fn read_progress(mut reader: impl Read, latest: &AtomicU64) {
    let mut chunk = [0; 4096];
    let mut line = Vec::with_capacity(128);
    let mut overflow = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                for byte in &chunk[..size] {
                    if *byte == b'\n' {
                        if !overflow {
                            progress_line(&line, latest);
                        }
                        line.clear();
                        overflow = false;
                    } else if line.len() < 1024 {
                        line.push(*byte);
                    } else {
                        overflow = true;
                    }
                }
            }
        }
    }
}

fn supervise_encoder(
    mut command: Command,
    seconds: f64,
    cancel: impl Fn() -> bool,
    progress: impl Fn(f64),
) -> Result<(ExitStatus, Vec<u8>), String> {
    let mut child = ChildGuard(
        command
            .spawn()
            .map_err(|error| format!("无法启动 FFmpeg：{error}"))?,
    );
    #[cfg(target_os = "windows")]
    let _process_job = ProcessJob::attach(&child.0)?;
    let stdout = child.0.stdout.take().ok_or("无法读取编码进度")?;
    let stderr = child.0.stderr.take().ok_or("无法读取编码错误")?;
    let latest = Arc::new(AtomicU64::new(0));
    let reader_latest = latest.clone();
    let stdout_reader = thread::spawn(move || read_progress(stdout, &reader_latest));
    let stderr_reader = thread::spawn(move || read_stderr(stderr));
    let deadline = Instant::now();
    let result = loop {
        if cancel() {
            break Err("导出已取消".into());
        }
        if deadline.elapsed() > Duration::from_secs(24 * 60 * 60) {
            break Err("编码运行超过 24 小时上限".into());
        }
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                progress(
                    (latest.load(Ordering::Relaxed) as f64 / 1_000_000.0 / seconds * 100.0)
                        .clamp(0.0, 99.0),
                );
                thread::sleep(Duration::from_millis(80));
            }
            Err(error) => break Err(format!("无法读取编码进程状态：{error}")),
        }
    };
    // Kill and wait before joining pipe readers so cancellation cannot block
    // forever on a live child retaining its pipe handles.
    if result.is_err() {
        let _ = child.0.kill();
    }
    let _ = child.0.wait();
    let _ = stdout_reader.join();
    let stderr = stderr_reader.join().map_err(|_| "无法读取编码错误输出")?;
    Ok((result?, stderr))
}

fn verify_output(ffprobe: &Path, path: &Path, snapshot: &ExportSnapshot) -> Result<(), String> {
    let mut command = Command::new(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,sample_rate,channels,duration,nb_frames,r_frame_rate,avg_frame_rate:format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::media_engine::hide_console(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("无法验证导出文件：{error}"))?;
    let (status, bytes, _) =
        crate::media_engine::collect_child_output(child, Duration::from_secs(10))?;
    if !status.success() {
        return Err("导出文件未通过媒体探测".into());
    }
    validate_output_metadata(&bytes, snapshot)
}

pub(crate) fn validate_output_metadata(
    bytes: &[u8],
    snapshot: &ExportSnapshot,
) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "导出媒体探测返回无效数据")?;
    let streams = parsed["streams"].as_array().ok_or("导出文件没有媒体流")?;
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .ok_or("导出文件没有视频流")?;
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .ok_or("导出文件没有音频流")?;
    if video["codec_name"] != "h264"
        || video["width"] != 1920
        || video["height"] != 1080
        || audio["codec_name"] != "aac"
        || audio["sample_rate"] != "48000"
        || audio["channels"] != 2
    {
        return Err("导出文件的编码或画面/音频规格不匹配".into());
    }
    let actual = parsed["format"]["duration"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(f64::NAN);
    let tolerance = (2.0 * snapshot.project.timebase.fps_denominator as f64
        / snapshot.project.timebase.fps_numerator as f64)
        .max(0.25);
    if !actual.is_finite()
        || actual <= 0.0
        || (actual - duration_seconds(snapshot)).abs() > tolerance
    {
        return Err("导出文件时长与时间线不匹配".into());
    }
    let video_duration = video["duration"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(f64::NAN);
    let frames = video["nb_frames"]
        .as_str()
        .and_then(|value| value.parse::<u64>().ok());
    let matches_fps = |field: &str| {
        video[field]
            .as_str()
            .and_then(|value| value.split_once('/'))
            .and_then(|(numerator, denominator)| {
                Some((
                    numerator.parse::<u64>().ok()?,
                    denominator.parse::<u64>().ok()?,
                ))
            })
            .is_some_and(|(numerator, denominator)| {
                denominator != 0
                    && numerator as u128 * snapshot.project.timebase.fps_denominator as u128
                        == denominator as u128 * snapshot.project.timebase.fps_numerator as u128
            })
    };
    if !video_duration.is_finite()
        || (video_duration - duration_seconds(snapshot)).abs() > tolerance
        || frames != Some(snapshot.duration_frames)
        || !matches_fps("r_frame_rate")
        || !matches_fps("avg_frame_rate")
    {
        return Err("导出视频帧数、帧率或视频流时长与时间线不匹配".into());
    }
    Ok(())
}

#[tauri::command]
pub fn export_job_status(app: tauri::AppHandle, job_id: String) -> Result<ExportJobRecord, String> {
    validate_job_id(&job_id)?;
    let state = app.state::<ExportJobsState>();
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| "导出任务状态不可用，请重新启动应用")?;
    jobs.get(&job_id)
        .cloned()
        .ok_or_else(|| "找不到导出任务".into())
}

#[tauri::command]
pub fn export_job_cancel(app: tauri::AppHandle, job_id: String) -> Result<ExportJobRecord, String> {
    validate_job_id(&job_id)?;
    let state = app.state::<ExportJobsState>();
    let mut jobs = state
        .jobs
        .lock()
        .map_err(|_| "导出任务状态不可用，请重新启动应用")?;
    let job = jobs
        .get_mut(&job_id)
        .ok_or_else(|| "找不到导出任务".to_string())?;
    request_cancel(job)?;
    persist_record(&app, job)?;
    Ok(job.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> ExportJobRecord {
        ExportJobRecord {
            job_id: "job-test".into(),
            project_id: "project-test".into(),
            revision: 1,
            snapshot_path: "snapshot.json".into(),
            output_path: "C:/output.mp4".into(),
            state: ExportJobState::Queued,
            created_at: timestamp(),
            updated_at: timestamp(),
            progress: 0.0,
            cancel_requested: false,
            retry_available: true,
            error: None,
        }
    }

    fn snapshot(directory: &Path) -> ExportSnapshot {
        let project = KaicutProject::new();
        ExportSnapshot {
            format_version: EXPORT_SNAPSHOT_FORMAT_VERSION,
            job_id: "job-test".into(),
            created_at: timestamp(),
            project_id: project.project_id.clone(),
            revision: 1,
            output_path: directory.join("output.mp4").to_string_lossy().into_owned(),
            duration_frames: 60,
            project,
            media: Vec::new(),
        }
    }

    #[test]
    fn running_cancel_keeps_job_active_until_worker_reaps_child() {
        let mut job = record();
        job.transition(ExportJobEvent::Start).unwrap();
        request_cancel(&mut job).unwrap();
        request_cancel(&mut job).unwrap();
        assert_eq!(job.state, ExportJobState::Running);
        assert!(job.cancel_requested);
        job.transition(ExportJobEvent::Cancel).unwrap();
        assert_eq!(job.state, ExportJobState::Cancelled);
    }

    #[test]
    fn snapshot_tampering_is_rejected_against_authoritative_memory() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot(directory.path());
        let path = write_snapshot_to_dir(directory.path(), &snapshot).unwrap();
        let mut job = record();
        job.snapshot_path = path.to_string_lossy().into_owned();
        assert!(verify_snapshot_file(&job, &snapshot).is_ok());
        let mut modified = snapshot.clone();
        modified.output_path = directory
            .path()
            .join("another.mp4")
            .to_string_lossy()
            .into_owned();
        fs::write(&path, serde_json::to_vec(&modified).unwrap()).unwrap();
        assert!(
            verify_snapshot_file(&job, &snapshot)
                .unwrap_err()
                .contains("修改")
        );
    }

    #[test]
    fn encoder_progress_drains_bounded_lines_and_ignores_invalid_or_backwards_values() {
        let progress = AtomicU64::new(0);
        let mut input = vec![b'x'; 64 * 1024];
        input.extend_from_slice(
            b"\nout_time_us=1000000\r\nout_time_us=-1\nout_time_us=not-a-time\nout_time_us=10\n",
        );
        read_progress(input.as_slice(), &progress);
        assert_eq!(progress.load(Ordering::Relaxed), 1_000_000);
        let bytes = vec![b'x'; 128 * 1024];
        assert_eq!(read_stderr(bytes.as_slice()).len(), 64 * 1024);
    }

    #[test]
    fn completed_publication_never_overwrites_a_racing_output() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("final.mp4");
        let temporary = NamedTempFile::new_in(directory.path())
            .unwrap()
            .into_temp_path();
        let original = temporary.to_path_buf();
        fs::write(&temporary, b"encoded-video").unwrap();
        fs::write(&destination, b"user-existing-file").unwrap();
        let mut job = record();
        job.transition(ExportJobEvent::Start).unwrap();
        assert!(publish_output(temporary, &destination, &mut job).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"user-existing-file");
        assert_eq!(job.state, ExportJobState::Running);
        assert!(
            !original.exists(),
            "failed publication drops only the owned temporary file"
        );
    }

    #[test]
    fn recovered_cleanup_removes_only_owned_temporary_and_preserves_final_output() {
        let directory = tempfile::tempdir().unwrap();
        let mut job = record();
        let output = directory.path().join("final.mp4");
        job.output_path = output.to_string_lossy().into_owned();
        let owned = directory.path().join(".kaicut-export-job-test-ab123.mp4");
        fs::write(&owned, b"partial").unwrap();
        fs::write(&output, b"final").unwrap();
        cleanup_recovered_temporary(&job, owned.to_str()).unwrap();
        assert!(!owned.exists());
        assert_eq!(fs::read(&output).unwrap(), b"final");
        assert!(cleanup_recovered_temporary(&job, output.to_str()).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"final");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn locked_cancelled_output_retains_tracking_and_recovers_after_release() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let mut job = record();
        let destination = directory.path().join("final.mp4");
        job.output_path = destination.to_string_lossy().into_owned();
        let temporary = tempfile::Builder::new()
            .prefix(&format!(".kaicut-export-{}-", job.job_id))
            .suffix(".mp4")
            .tempfile_in(directory.path())
            .unwrap()
            .into_temp_path();
        let path = temporary.to_path_buf();
        fs::write(&path, b"partial-encoding").unwrap();
        // A Windows reader may permit encoding writes but deny file deletion.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2) // FILE_SHARE_READ | FILE_SHARE_WRITE, no DELETE.
            .open(&path)
            .unwrap();
        let mut tracked =
            HashMap::from([(job.job_id.clone(), path.to_string_lossy().into_owned())]);
        job.transition(ExportJobEvent::Start).unwrap();
        request_cancel(&mut job).unwrap();
        drop(temporary);
        assert!(
            path.exists(),
            "the real Windows handle must block TempPath removal"
        );
        let warning = cleanup_tracked_temporary(&job, &mut tracked).unwrap_err();
        assert!(warning.contains("删除"));
        finish_unsuccessful_worker(&mut job, Err("导出已取消".into()), Some(warning)).unwrap();
        assert_eq!(job.state, ExportJobState::Cancelled);
        assert!(!job.cancel_requested);
        let disk = DiskJob {
            record: job.clone(),
            temporary_output_path: tracked.get(&job.job_id).cloned(),
        };
        let history = directory.path().join("job.json");
        fs::write(&history, serde_json::to_vec(&disk).unwrap()).unwrap();
        let restored: DiskJob = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
        assert_eq!(restored.temporary_output_path.as_deref(), path.to_str());
        assert!(
            restored
                .record
                .error
                .as_deref()
                .unwrap()
                .contains("清理失败")
        );
        assert!(!destination.exists());
        assert!(cleanup_tracked_temporary(&job, &mut tracked).is_err());
        assert_eq!(tracked.get(&job.job_id).map(String::as_str), path.to_str());

        // Restart/retry after the external reader exits can clean the recorded
        // temporary file, without touching an unrelated successful output.
        fs::write(&destination, b"successful-user-output").unwrap();
        drop(held);
        let mut tracked = HashMap::from([(
            restored.record.job_id.clone(),
            restored.temporary_output_path.unwrap(),
        )]);
        cleanup_tracked_temporary(&job, &mut tracked).unwrap();
        assert!(!path.exists());
        assert!(!tracked.contains_key(&job.job_id));
        assert_eq!(fs::read(destination).unwrap(), b"successful-user-output");
    }

    #[test]
    fn recovered_cleanup_rejects_unrelated_names_directories_and_other_parents() {
        let directory = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut job = record();
        job.output_path = directory
            .path()
            .join("final.mp4")
            .to_string_lossy()
            .into_owned();
        for path in [
            directory.path().join("user.mp4"),
            directory
                .path()
                .join(".kaicut-export-job-different-ab12.mp4"),
            other.path().join(".kaicut-export-job-test-ab12.mp4"),
        ] {
            fs::write(&path, b"preserve").unwrap();
            assert!(cleanup_recovered_temporary(&job, path.to_str()).is_err());
            assert_eq!(fs::read(path).unwrap(), b"preserve");
        }
        let nested = directory.path().join(".kaicut-export-job-test-ab123.mp4");
        fs::create_dir(&nested).unwrap();
        assert!(cleanup_recovered_temporary(&job, nested.to_str()).is_err());
        assert!(nested.is_dir());
    }

    #[test]
    fn recovered_filter_cleanup_preserves_snapshot_and_unrelated_text_files() {
        let directory = tempfile::tempdir().unwrap();
        let owned = directory.path().join("filter-job-test-abc12.txt");
        let foreign = directory.path().join("filter-job-other-abc12.txt");
        let snapshot = directory.path().join("snapshot.json");
        for path in [&owned, &foreign, &snapshot] {
            fs::write(path, b"data").unwrap();
        }
        cleanup_filter_scripts(directory.path(), "job-test");
        assert!(!owned.exists());
        assert!(foreign.exists());
        assert!(snapshot.exists());
    }

    #[test]
    fn cancellation_before_publication_preserves_absent_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("final.mp4");
        let temporary = NamedTempFile::new_in(directory.path())
            .unwrap()
            .into_temp_path();
        let temporary_path = temporary.to_path_buf();
        fs::write(&temporary, b"encoded-video").unwrap();
        let mut job = record();
        job.transition(ExportJobEvent::Start).unwrap();
        request_cancel(&mut job).unwrap();
        assert!(
            publish_output(temporary, &destination, &mut job)
                .unwrap_err()
                .contains("取消")
        );
        assert!(!destination.exists());
        assert!(!temporary_path.exists());
    }

    #[test]
    fn successful_publication_completes_job_and_prevents_late_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("final.mp4");
        let temporary = NamedTempFile::new_in(directory.path())
            .unwrap()
            .into_temp_path();
        fs::write(&temporary, b"encoded-video").unwrap();
        let mut job = record();
        job.transition(ExportJobEvent::Start).unwrap();
        publish_output(temporary, &destination, &mut job).unwrap();
        assert_eq!(job.state, ExportJobState::Completed);
        assert_eq!(job.progress, 100.0);
        assert!(request_cancel(&mut job).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"encoded-video");
    }

    #[test]
    fn output_validation_rejects_wrong_streams_dimensions_and_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot(directory.path());
        let mut value = serde_json::json!({
            "streams": [
                {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
                 "duration":"2.0", "nb_frames":"60", "r_frame_rate":"30/1", "avg_frame_rate":"30/1"},
                {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2}
            ], "format": {"duration":"2.020"}
        });
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_ok());
        value["streams"][0]["duration"] = "1.0".into();
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_err());
        value["streams"][0]["duration"] = "2.0".into();
        value["streams"][0]["nb_frames"] = "30".into();
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_err());
        value["streams"][0]["nb_frames"] = "60".into();
        value["streams"][0]["avg_frame_rate"] = "25/1".into();
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_err());
        value["streams"][0]["avg_frame_rate"] = "30/1".into();
        value["format"]["duration"] = "1.0".into();
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_err());
        value["format"]["duration"] = "2.0".into();
        value["streams"][0]["width"] = 640.into();
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_err());
        value["streams"][0]["width"] = 1920.into();
        value["streams"][1]["codec_name"] = "mp3".into();
        assert!(validate_output_metadata(&serde_json::to_vec(&value).unwrap(), &snapshot).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn supervisor_cancels_a_real_child_and_returns_without_waiting_for_its_work() {
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = Command::new(shell);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.WriteLine('out_time_us=500000'); Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::media_engine::hide_console(&mut command);
        let started = Instant::now();
        let result = supervise_encoder(
            command,
            30.0,
            || started.elapsed() > Duration::from_millis(350),
            |_| {},
        );
        assert!(result.unwrap_err().contains("取消"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires packaged LGPL sidecars and available Windows h264_mf; retains sample under target/export-validation"]
    fn real_sidecar_worker_renders_validates_publishes_and_cancels() {
        use crate::project::{MediaAsset, MediaKind, TimelineClip};
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let artifacts = manifest.join("target/export-validation").join(new_job_id());
        fs::create_dir_all(&artifacts).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("测试 图像%03d.png");
        fs::copy(manifest.join("icons/128x128.png"), &image).unwrap();
        let original_bytes = fs::read(&image).unwrap();
        let metadata = fs::metadata(&image).unwrap();
        let ffmpeg = manifest.join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe");
        let ffprobe = manifest.join("binaries/ffprobe-x86_64-pc-windows-msvc.exe");
        let mut snapshot = snapshot(directory.path());
        snapshot.output_path = artifacts
            .join("worker-sample.mp4")
            .to_string_lossy()
            .into_owned();
        snapshot.project.media_refs.push(MediaAsset {
            id: "image".into(),
            name: "测试 图像%03d.png".into(),
            kind: MediaKind::Image,
            extension: "png".into(),
            size: metadata.len(),
            last_modified: metadata
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            source_path: Some(image.to_string_lossy().into_owned()),
        });
        snapshot.project.tracks[0].clips.push(TimelineClip {
            id: "clip-1".into(),
            media_id: "image".into(),
            track_id: "video-1".into(),
            start_frame: 0,
            duration_frames: 60,
            source_in_frame: 0,
            source_duration_frames: 60,
            fade_in_frames: 0,
            fade_out_frames: 0,
        });
        snapshot.media = media_snapshot(&snapshot.project);
        let snapshot_path = write_snapshot_to_dir(directory.path(), &snapshot).unwrap();
        let mut job = record();
        job.snapshot_path = snapshot_path.to_string_lossy().into_owned();
        job.output_path = snapshot.output_path.clone();
        job.transition(ExportJobEvent::Start).unwrap();
        verify_snapshot_file(&job, &snapshot).unwrap();
        verify_sources(&snapshot).unwrap();
        let temporary = tempfile::Builder::new()
            .prefix(".kaicut-export-job-test-")
            .suffix(".mp4")
            .tempfile_in(&artifacts)
            .unwrap()
            .into_temp_path();
        let temporary_path = temporary.to_path_buf();
        let script = directory.path().join("filters.txt");
        let plan = crate::export_render::render_plan_cancellable(
            &snapshot,
            &ffprobe,
            &temporary,
            &script,
            || false,
        )
        .unwrap();
        fs::write(&script, plan.filter_graph).unwrap();
        let mut command = Command::new(&ffmpeg);
        command
            .args(plan.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::media_engine::hide_console(&mut command);
        let calls = AtomicU64::new(0);
        let (status, stderr) = supervise_encoder(
            command,
            duration_seconds(&snapshot),
            || false,
            |_| {
                calls.fetch_add(1, Ordering::Relaxed);
            },
        )
        .unwrap();
        assert!(status.success(), "{}", String::from_utf8_lossy(&stderr));
        assert!(
            calls.load(Ordering::Relaxed) > 0,
            "real worker must report progress"
        );
        verify_output(&ffprobe, &temporary, &snapshot).unwrap();
        verify_sources(&snapshot).unwrap();
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)
            .unwrap()
            .sync_all()
            .unwrap();
        publish_output(temporary, Path::new(&snapshot.output_path), &mut job).unwrap();
        assert_eq!(job.state, ExportJobState::Completed);
        assert!(!temporary_path.exists());
        assert!(Path::new(&snapshot.output_path).exists());
        let mut probe = Command::new(&ffprobe);
        probe
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(&snapshot.output_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::media_engine::hide_console(&mut probe);
        let (status, metadata, _) = crate::media_engine::collect_child_output(
            probe.spawn().unwrap(),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(status.success());
        validate_output_metadata(&metadata, &snapshot).unwrap();
        fs::write(artifacts.join("worker-sample.ffprobe.json"), metadata).unwrap();

        // Same executable, planner and supervisor, this time with enough work
        // that cancellation has to terminate a real encoder process.
        let cancelled_output = directory.path().join("cancelled.mp4");
        snapshot.output_path = cancelled_output.to_string_lossy().into_owned();
        snapshot.duration_frames = 3600;
        snapshot.project.tracks[0].clips[0].duration_frames = 3600;
        snapshot.project.tracks[0].clips[0].source_duration_frames = 3600;
        let temporary = tempfile::Builder::new()
            .prefix(".kaicut-export-job-test-")
            .suffix(".mp4")
            .tempfile_in(directory.path())
            .unwrap()
            .into_temp_path();
        let cancelled_temporary = temporary.to_path_buf();
        let plan = crate::export_render::render_plan_cancellable(
            &snapshot,
            &ffprobe,
            &temporary,
            &script,
            || false,
        )
        .unwrap();
        fs::write(&script, plan.filter_graph).unwrap();
        let mut command = Command::new(&ffmpeg);
        command
            .args(plan.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::media_engine::hide_console(&mut command);
        let start = Instant::now();
        let result = supervise_encoder(
            command,
            duration_seconds(&snapshot),
            || start.elapsed() > Duration::from_millis(350),
            |_| {},
        );
        assert!(result.unwrap_err().contains("取消"));
        assert!(start.elapsed() < Duration::from_secs(5));
        drop(temporary);
        assert!(!cancelled_temporary.exists());
        assert!(!cancelled_output.exists());
        assert_eq!(fs::read(image).unwrap(), original_bytes);
        println!("Worker validation sample: {}", artifacts.display());
    }

    #[test]
    fn state_transitions_allow_expected_lifecycle_only() {
        assert_eq!(
            transition_state(ExportJobState::Queued, ExportJobEvent::Start).unwrap(),
            ExportJobState::Running
        );
        assert_eq!(
            transition_state(ExportJobState::Queued, ExportJobEvent::Cancel).unwrap(),
            ExportJobState::Cancelled
        );
        assert_eq!(
            transition_state(ExportJobState::Running, ExportJobEvent::Complete).unwrap(),
            ExportJobState::Completed
        );
        assert!(transition_state(ExportJobState::Completed, ExportJobEvent::Cancel).is_err());
    }

    #[test]
    fn cancelled_and_failed_jobs_can_retry_but_completed_jobs_cannot() {
        for state in [ExportJobState::Cancelled, ExportJobState::Failed] {
            assert_eq!(
                transition_state(state, ExportJobEvent::Start).unwrap(),
                ExportJobState::Running
            );
        }
        assert!(transition_state(ExportJobState::Completed, ExportJobEvent::Start).is_err());
        assert!(transition_state(ExportJobState::Running, ExportJobEvent::Start).is_err());
    }

    #[test]
    fn failed_and_interrupted_records_keep_actionable_error() {
        let mut record = ExportJobRecord {
            job_id: "job-test".into(),
            project_id: "project-test".into(),
            revision: 1,
            snapshot_path: "snapshot.json".into(),
            output_path: "C:/output.mp4".into(),
            state: ExportJobState::Running,
            created_at: timestamp(),
            updated_at: timestamp(),
            progress: 0.0,
            cancel_requested: false,
            retry_available: true,
            error: None,
        };
        record.transition(ExportJobEvent::Fail).unwrap();
        assert_eq!(record.state, ExportJobState::Failed);
        assert!(record.error.is_some());
    }

    #[test]
    fn snapshot_is_written_atomically_and_excludes_runtime_fields() {
        let directory = tempfile::tempdir().unwrap();
        let project = KaicutProject::new();
        let snapshot = ExportSnapshot {
            format_version: EXPORT_SNAPSHOT_FORMAT_VERSION,
            job_id: "job-test".into(),
            created_at: timestamp(),
            project_id: project.project_id.clone(),
            revision: 3,
            output_path: directory
                .path()
                .join("output.mp4")
                .to_string_lossy()
                .into_owned(),
            duration_frames: 0,
            media: Vec::new(),
            project,
        };
        let path =
            write_snapshot_to_dir(directory.path().join("jobs").as_path(), &snapshot).unwrap();
        let parsed: ExportSnapshot = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(parsed, snapshot);
        assert!(
            serde_json::to_string(&parsed)
                .unwrap()
                .find("playhead")
                .is_none()
        );
        assert!(
            serde_json::to_string(&parsed)
                .unwrap()
                .find("selection")
                .is_none()
        );
    }

    #[test]
    fn snapshot_job_directory_is_immutable() {
        let directory = tempfile::tempdir().unwrap();
        let project = KaicutProject::new();
        let snapshot = ExportSnapshot {
            format_version: EXPORT_SNAPSHOT_FORMAT_VERSION,
            job_id: "job-test".into(),
            created_at: timestamp(),
            project_id: project.project_id.clone(),
            revision: 1,
            output_path: directory
                .path()
                .join("output.mp4")
                .to_string_lossy()
                .into_owned(),
            duration_frames: 0,
            project,
            media: Vec::new(),
        };
        write_snapshot_to_dir(directory.path().join("jobs").as_path(), &snapshot).unwrap();
        assert!(write_snapshot_to_dir(directory.path().join("jobs").as_path(), &snapshot).is_err());
    }

    #[test]
    fn job_ids_are_path_safe() {
        assert!(validate_job_id("job-123").is_ok());
        assert!(validate_job_id("../escape").is_err());
        assert!(validate_job_id("JOB-123").is_err());
    }

    #[test]
    fn media_snapshot_contains_only_referenced_assets() {
        let project = KaicutProject::new();
        assert!(media_snapshot(&project).is_empty());
    }
}
