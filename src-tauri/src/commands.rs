use crate::{
    export_job::{
        self, EXPORT_SNAPSHOT_FORMAT_VERSION, ExportJobRecord, ExportJobState, ExportSnapshot,
    },
    project::{
        MEDIA_EXTENSIONS, ProjectSession, import_media, media_kind, read_project, save_project,
        timestamp, validate_text,
    },
    recent, recovery,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[derive(Default)]
pub struct ProjectState {
    pub session: Mutex<ProjectSession>,
    close_prompt: AtomicBool,
}

pub(crate) async fn with_session<F>(app: AppHandle, operation: F) -> Result<ProjectSession, String>
where
    F: FnOnce(&AppHandle, &mut ProjectSession) -> Result<(), String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ProjectState>();
        let mut session = state
            .session
            .lock()
            .map_err(|_| "项目状态不可用，请重新启动应用")?;
        let before = session.clone();
        if let Err(error) = operation(&app, &mut session) {
            *session = before;
            return Err(error);
        }
        if let Err(error) = session.project.validate() {
            *session = before;
            return Err(error);
        }
        if session.revision == before.revision
            && (session.project != before.project
                || session.path != before.path
                || session.dirty != before.dirty)
        {
            session.revision += 1;
        }
        if session.revision > crate::project::MAX_SAFE_INTEGER {
            *session = before;
            return Err("项目修订号超过支持范围，请重新启动应用".into());
        }
        session.sync_history_flags();
        Ok(session.clone())
    })
    .await
    .map_err(|error| format!("项目操作未能完成：{error}"))?
}

fn confirm_discard(app: &AppHandle, project_name: &str) -> bool {
    let mut dialog = app
        .dialog()
        .message(format!(
            "“{project_name}”有未保存的更改。继续将丢弃这些更改。"
        ))
        .title("KaiCut · 未保存的更改")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "丢弃更改".into(),
            "取消".into(),
        ));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.blocking_show()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaLinkStatus {
    pub id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreflightRequest {
    pub project_id: String,
    pub revision: u64,
    pub output_path: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreflightCheck {
    pub id: String,
    pub label: String,
    pub state: ExportPreflightState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExportPreflightState {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportEncoderSummary {
    pub available: bool,
    pub name: String,
    pub h264_mf: bool,
    pub aac: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportAudioSummary {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreflight {
    pub ready: bool,
    pub project_id: String,
    pub revision: u64,
    pub output_path: String,
    pub duration_frames: u64,
    pub snapshot: crate::project::KaicutProject,
    pub encoder: ExportEncoderSummary,
    pub audio: ExportAudioSummary,
    pub checks: Vec<ExportPreflightCheck>,
}

fn inspect_media_link(media: &crate::project::MediaAsset) -> MediaLinkStatus {
    let Some(path) = &media.source_path else {
        return MediaLinkStatus {
            id: media.id.clone(),
            state: "missing".into(),
            reason: Some("素材没有可用路径".into()),
        };
    };
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return MediaLinkStatus {
                id: media.id.clone(),
                state: "missing".into(),
                reason: Some("素材路径不是文件".into()),
            };
        }
        Err(_) => {
            return MediaLinkStatus {
                id: media.id.clone(),
                state: "missing".into(),
                reason: Some("找不到原始素材".into()),
            };
        }
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64);
    if metadata.len() != media.size || modified != Some(media.last_modified) {
        return MediaLinkStatus {
            id: media.id.clone(),
            state: "changed".into(),
            reason: Some("文件已更改，建议重新导入或重新链接".into()),
        };
    }
    MediaLinkStatus {
        id: media.id.clone(),
        state: "online".into(),
        reason: None,
    }
}

fn export_check(
    id: impl Into<String>,
    label: impl Into<String>,
    state: ExportPreflightState,
    detail: impl Into<String>,
) -> ExportPreflightCheck {
    ExportPreflightCheck {
        id: id.into(),
        label: label.into(),
        state,
        detail: Some(detail.into()),
    }
}

fn timeline_duration_frames(project: &crate::project::KaicutProject) -> u64 {
    project
        .tracks
        .iter()
        .flat_map(|track| {
            track
                .clips
                .iter()
                .filter_map(|clip| clip.start_frame.checked_add(clip.duration_frames))
        })
        .max()
        .unwrap_or(0)
}

fn validate_export_output(
    project: &crate::project::KaicutProject,
    output_path: &str,
    _overwrite: bool,
) -> ExportPreflightCheck {
    if output_path.trim().is_empty()
        || output_path.chars().count() > 32_768
        || output_path.chars().any(char::is_control)
    {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出路径不能为空、不能包含控制字符且长度不能超过 32768 个字符",
        );
    }
    let output = std::path::Path::new(output_path);
    if !output.is_absolute() {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出路径必须是绝对路径",
        );
    }
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("mp4"))
        != Some(true)
    {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "首版导出预设要求使用 .mp4 输出文件",
        );
    }
    let Some(parent) = output.parent() else {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "无法确定输出目录",
        );
    };
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if file_name.is_empty()
        || file_name.ends_with([' ', '.'])
        || file_name
            .chars()
            .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出文件名含有 Windows 不支持的字符",
        );
    }
    let stem = file_name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出文件名是 Windows 保留设备名，请更名",
        );
    }
    let Ok(metadata) = std::fs::metadata(parent) else {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出目录不存在或无法读取",
        );
    };
    if !metadata.is_dir() {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出路径的父级必须是目录",
        );
    }
    // On Windows a directory's readonly bit is not its ACL/write permission.
    if !cfg!(windows) && metadata.permissions().readonly() {
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出目录是只读目录",
        );
    }
    if output.symlink_metadata().is_ok() {
        if !output.is_file() {
            return export_check(
                "output",
                "输出路径",
                ExportPreflightState::Fail,
                "输出路径已被目录或链接占用，请选择新文件名",
            );
        }
        if let Ok(output_canonical) = output.canonicalize() {
            for media in &project.media_refs {
                if let Some(source_path) = &media.source_path {
                    if std::path::Path::new(source_path)
                        .canonicalize()
                        .ok()
                        .as_ref()
                        == Some(&output_canonical)
                    {
                        return export_check(
                            "output",
                            "输出路径",
                            ExportPreflightState::Fail,
                            format!("输出文件不能覆盖源素材：{}", media.name),
                        );
                    }
                }
            }
        }
        return export_check(
            "output",
            "输出路径",
            ExportPreflightState::Fail,
            "输出文件已存在，请选择新文件名；当前导出不会覆盖文件",
        );
    }
    export_check(
        "output",
        "输出路径",
        ExportPreflightState::Pass,
        "输出目录存在，路径格式为 MP4；开始导出时确认实际写入权限",
    )
}

fn build_export_preflight(
    app: &AppHandle,
    session: ProjectSession,
    request: ExportPreflightRequest,
) -> Result<ExportPreflight, String> {
    if session.project.project_id != request.project_id {
        return Err("导出预检的项目已切换，请刷新后重试".into());
    }
    if session.revision != request.revision {
        return Err("导出预检的项目已更新，请刷新后重试".into());
    }

    let duration_frames = timeline_duration_frames(&session.project);
    let clip_count: usize = session
        .project
        .tracks
        .iter()
        .map(|track| track.clips.len())
        .sum();
    let timeline_check = if clip_count > 64 {
        export_check(
            "timeline",
            "时间线",
            ExportPreflightState::Fail,
            "当前导出支持最多 64 个片段，请拆分为较短项目",
        )
    } else if duration_frames == 0 {
        export_check(
            "timeline",
            "时间线",
            ExportPreflightState::Fail,
            "时间线为空，至少需要一个片段才能导出",
        )
    } else {
        export_check(
            "timeline",
            "时间线",
            ExportPreflightState::Pass,
            format!("时间线时长 {} 帧", duration_frames),
        )
    };

    let used_media: HashSet<&str> = session
        .project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(|clip| clip.media_id.as_str()))
        .collect();
    let mut missing_used = Vec::new();
    let mut changed_used = Vec::new();
    let mut unavailable_unused = 0usize;
    for media in &session.project.media_refs {
        let status = inspect_media_link(media);
        if used_media.contains(media.id.as_str()) {
            match status.state.as_str() {
                "online" => {}
                "changed" => changed_used.push(media.name.clone()),
                _ => missing_used.push(media.name.clone()),
            }
        } else if status.state != "online" {
            unavailable_unused += 1;
        }
    }
    let media_check = if !missing_used.is_empty() || !changed_used.is_empty() {
        let mut details = Vec::new();
        if !missing_used.is_empty() {
            details.push(format!("找不到：{}", missing_used.join("、")));
        }
        if !changed_used.is_empty() {
            details.push(format!("已更改：{}", changed_used.join("、")));
        }
        export_check(
            "media",
            "素材链接",
            ExportPreflightState::Fail,
            details.join("；"),
        )
    } else if unavailable_unused > 0 {
        export_check(
            "media",
            "素材链接",
            ExportPreflightState::Warn,
            format!("已使用素材均在线；另有 {unavailable_unused} 个未使用素材不可用"),
        )
    } else {
        export_check(
            "media",
            "素材链接",
            ExportPreflightState::Pass,
            format!("已检查 {} 个项目素材引用", used_media.len()),
        )
    };

    let (h264_mf, aac) = crate::media_engine::packaged_binary(app, "ffmpeg")
        .map(|path| {
            let capabilities = crate::media_engine::probe_encoders(&path);
            (capabilities.h264_mf, capabilities.aac)
        })
        .unwrap_or((false, false));
    let encoder_available = h264_mf && aac;
    let encoder = ExportEncoderSummary {
        available: encoder_available,
        name: "h264_mf + aac".into(),
        h264_mf,
        aac,
        detail: if encoder_available {
            "打包 FFmpeg 编译列表包含 h264_mf 和 AAC；尚未执行设备级编码自检".into()
        } else {
            "打包 FFmpeg 缺少 h264_mf 或 AAC，当前默认导出预设不可用".into()
        },
    };
    let ffmpeg_check = export_check(
        "ffmpeg",
        "编码器能力",
        if encoder_available {
            ExportPreflightState::Warn
        } else {
            ExportPreflightState::Fail
        },
        encoder.detail.clone(),
    );

    let audio_clip_count = session
        .project
        .tracks
        .iter()
        .filter(|track| matches!(track.kind, crate::project::TrackKind::Audio))
        .map(|track| track.clips.len())
        .sum::<usize>();
    let audio = ExportAudioSummary {
        sample_rate: 48_000,
        channels: 2,
    };
    let audio_check = export_check(
        "audio",
        "音频输出",
        if audio_clip_count == 0 {
            ExportPreflightState::Warn
        } else {
            ExportPreflightState::Pass
        },
        if audio_clip_count == 0 {
            "AAC · 48 kHz 双声道；视频原声按可见片段导出，无声区间补静音".into()
        } else {
            format!(
                "{} 个音频片段按轨道音量、静音和淡入淡出混音，并包含可见视频原声；AAC · 48 kHz 双声道",
                audio_clip_count
            )
        },
    );
    let output_check =
        validate_export_output(&session.project, &request.output_path, request.overwrite);
    let checks = vec![
        timeline_check,
        media_check,
        ffmpeg_check,
        audio_check,
        output_check,
    ];
    let ready = checks
        .iter()
        .all(|check| !matches!(check.state, ExportPreflightState::Fail));
    Ok(ExportPreflight {
        ready,
        project_id: session.project.project_id.clone(),
        revision: session.revision,
        output_path: request.output_path,
        duration_frames,
        snapshot: session.project,
        encoder,
        audio,
        checks,
    })
}

pub(crate) fn allow_project_media_assets(app: &AppHandle, project: &crate::project::KaicutProject) {
    for media in &project.media_refs {
        let Some(path) = media.source_path.as_deref() else {
            continue;
        };
        if std::path::Path::new(path).is_file() {
            if let Err(error) = app.asset_protocol_scope().allow_file(path) {
                log::warn!("无法加入媒体播放资源范围：{error}");
            }
        }
    }
}

#[tauri::command]
pub async fn project_get(app: AppHandle) -> Result<ProjectSession, String> {
    with_session(app, |_, _| Ok(())).await
}

#[tauri::command]
pub async fn project_new(app: AppHandle) -> Result<ProjectSession, String> {
    with_session(app, |app, session| {
        if session.dirty && !confirm_discard(app, &session.project.name) {
            session.status = "已取消新建，未保存的更改已保留".into();
            return Ok(());
        }
        session.replace_project(crate::project::KaicutProject::new(), None, false);
        session.status = "已新建项目".into();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn project_open(app: AppHandle) -> Result<ProjectSession, String> {
    with_session(app, |app, session| {
        if session.dirty && !confirm_discard(app, &session.project.name) {
            session.status = "已取消打开，未保存的更改已保留".into();
            return Ok(());
        }
        let mut dialog = app
            .dialog()
            .file()
            .set_title("打开 KaiCut 项目")
            .add_filter("KaiCut 项目", &["kaicut"]);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        let Some(selected) = dialog.blocking_pick_file() else {
            session.status = "已取消打开项目".into();
            return Ok(());
        };
        let path = selected
            .into_path()
            .map_err(|error| format!("无效的项目路径：{error}"))?;
        let project = read_project(&path)?;
        let path_string = path
            .to_str()
            .ok_or("项目路径必须是有效 Unicode")?
            .to_string();
        session.status = format!("已打开 {}", project.name);
        session.replace_project(project, Some(path_string), false);
        allow_project_media_assets(app, &session.project);
        if let Err(error) = recent::record(app, &path, &session.project) {
            log::warn!("无法更新最近项目：{error}");
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn project_save(app: AppHandle) -> Result<ProjectSession, String> {
    let clear_app = app.clone();
    let result = with_session(app, |app, session| {
        let path = if let Some(path) = &session.path {
            std::path::PathBuf::from(path)
        } else {
            let suggested_name: String = session
                .project
                .name
                .chars()
                .map(|character| {
                    if matches!(character, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*')
                        || character as u32 == 92
                    {
                        '_'
                    } else {
                        character
                    }
                })
                .collect();
            let mut dialog = app
                .dialog()
                .file()
                .set_title("保存 KaiCut 项目")
                .set_file_name(format!("{suggested_name}.kaicut"))
                .add_filter("KaiCut 项目", &["kaicut"]);
            if let Some(window) = app.get_webview_window("main") {
                dialog = dialog.set_parent(&window);
            }
            let Some(selected) = dialog.blocking_save_file() else {
                session.status = "已取消保存，未保存的更改已保留".into();
                return Ok(());
            };
            let mut selected = selected
                .into_path()
                .map_err(|error| format!("无效的保存路径：{error}"))?;
            if selected.extension().is_none() {
                selected.set_extension("kaicut");
            }
            selected
        };
        let path_string = path
            .to_str()
            .ok_or("项目路径必须是有效 Unicode")?
            .to_string();
        let mut snapshot = session.project.clone();
        snapshot.updated_at = timestamp();
        save_project(&path, &snapshot)?;
        session.project = snapshot;
        session.path = Some(path_string);
        session.dirty = false;
        session.status = "项目已保存".into();
        Ok(())
    })
    .await?;
    if !result.dirty {
        if let Some(path) = &result.path {
            if let Err(error) =
                recent::record(&clear_app, std::path::Path::new(path), &result.project)
            {
                log::warn!("无法更新最近项目：{error}");
            }
        }
        recovery::clear_for_project(&clear_app, &result.project.project_id);
    }
    Ok(result)
}

#[tauri::command]
pub async fn project_recent_list(app: AppHandle) -> Result<Vec<recent::RecentProject>, String> {
    recent::list(&app)
}

#[tauri::command]
pub async fn project_open_recent(app: AppHandle, path: String) -> Result<ProjectSession, String> {
    with_session(app, move |app, session| {
        if session.dirty && !confirm_discard(app, &session.project.name) {
            session.status = "已取消打开，未保存的更改已保留".into();
            return Ok(());
        }
        let path = std::path::PathBuf::from(path);
        let project = read_project(&path)?;
        let path_string = path
            .to_str()
            .ok_or("项目路径必须是有效 Unicode")?
            .to_string();
        session.status = format!("已打开 {}", project.name);
        session.replace_project(project, Some(path_string), false);
        if let Err(error) = recent::record(app, &path, &session.project) {
            log::warn!("无法更新最近项目：{error}");
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn project_rename(app: AppHandle, name: String) -> Result<ProjectSession, String> {
    with_session(app, move |_, session| {
        let name = name.trim();
        validate_text(name, "项目名称", 120)?;
        if session.project.name != name {
            session.project.name = name.into();
            session.project.updated_at = timestamp();
            session.dirty = true;
            session.status = "项目名称已更新，尚未保存".into();
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn project_import(app: AppHandle) -> Result<ProjectSession, String> {
    with_session(app, |app, session| {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("导入媒体")
            .add_filter("视频、音频与图片", MEDIA_EXTENSIONS);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        let Some(selected) = dialog.blocking_pick_files() else {
            session.status = "已取消导入媒体".into();
            return Ok(());
        };
        let paths = selected
            .into_iter()
            .map(|path| {
                path.into_path()
                    .map_err(|error| format!("无效的素材路径：{error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (added, skipped) = import_media(&mut session.project, paths)?;
        allow_project_media_assets(app, &session.project);
        if added > 0 {
            session.dirty = true;
        }
        session.status = if skipped > 0 {
            format!("已导入 {added} 项素材，跳过 {skipped} 项重复或不支持的文件")
        } else {
            format!("已导入 {added} 项素材")
        };
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn media_check_links(app: AppHandle) -> Result<Vec<MediaLinkStatus>, String> {
    let session = with_session(app, |_, _| Ok(())).await?;
    Ok(session
        .project
        .media_refs
        .iter()
        .map(inspect_media_link)
        .collect())
}

/// Validate a deterministic export request and return an immutable project
/// snapshot. Only capability probing runs here; encoding is a separate action.
/// The worker consumes the committed snapshot independently of later edits.
#[tauri::command]
pub async fn export_preflight(
    app: AppHandle,
    project_id: String,
    revision: u64,
    output_path: String,
    overwrite: Option<bool>,
) -> Result<ExportPreflight, String> {
    let session = with_session(app.clone(), |_, _| Ok(())).await?;
    let request = ExportPreflightRequest {
        project_id,
        revision,
        output_path,
        overwrite: overwrite.unwrap_or(false),
    };
    tauri::async_runtime::spawn_blocking(move || build_export_preflight(&app, session, request))
        .await
        .map_err(|error| format!("导出预检未能完成：{error}"))?
}

#[tauri::command]
pub async fn export_choose_output(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("选择导出位置")
            .set_file_name("开剪导出.mp4")
            .add_filter("MP4 视频", &["mp4"]);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        let Some(selected) = dialog.blocking_save_file() else {
            return Ok(None);
        };
        let mut path = selected
            .into_path()
            .map_err(|error| format!("无法读取输出路径：{error}"))?;
        if path.extension().is_none() {
            path.set_extension("mp4");
        }
        Ok(Some(
            path.to_str()
                .ok_or("输出路径必须是有效 Unicode")?
                .to_owned(),
        ))
    })
    .await
    .map_err(|error| format!("选择导出位置失败：{error}"))?
}

/// Create the immutable input for a future export worker. The submitted
/// preflight is compared with a fresh Rust-side preflight for the exact
/// project revision, so a stale or client-forged result cannot create a job.
/// This command deliberately does not start FFmpeg or touch the output path.
#[tauri::command]
pub async fn export_snapshot_create(
    app: AppHandle,
    project_id: String,
    revision: u64,
    output_path: String,
    overwrite: Option<bool>,
    preflight: ExportPreflight,
) -> Result<ExportJobRecord, String> {
    let session = with_session(app.clone(), |_, _| Ok(())).await?;
    let request = ExportPreflightRequest {
        project_id,
        revision,
        output_path,
        overwrite: overwrite.unwrap_or(false),
    };
    let current = {
        let app_for_preflight = app.clone();
        let request_for_preflight = request.clone();
        tauri::async_runtime::spawn_blocking(move || {
            build_export_preflight(&app_for_preflight, session, request_for_preflight)
        })
        .await
        .map_err(|error| format!("导出快照预检未能完成：{error}"))??
    };
    if !current.ready {
        let detail = current
            .checks
            .iter()
            .filter(|check| matches!(check.state, ExportPreflightState::Fail))
            .filter_map(|check| check.detail.as_deref())
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("导出预检未通过：{detail}"));
    }
    if preflight != current {
        return Err("导出预检结果已过期，请重新执行预检".into());
    }

    let job_id = export_job::new_job_id();
    let snapshot = ExportSnapshot {
        format_version: EXPORT_SNAPSHOT_FORMAT_VERSION,
        job_id: job_id.clone(),
        created_at: export_job::timestamp(),
        project_id: current.project_id.clone(),
        revision: current.revision,
        output_path: current.output_path.clone(),
        duration_frames: current.duration_frames,
        media: export_job::media_snapshot(&current.snapshot),
        project: current.snapshot.clone(),
    };
    if snapshot.media.iter().any(|media| media.state != "online") {
        return Err("导出快照中的素材链接已变化，请重新链接后再试".into());
    }
    let jobs_dir = export_job::export_jobs_dir(&app)?;
    let write_snapshot = snapshot.clone();
    let snapshot_path = tauri::async_runtime::spawn_blocking(move || {
        export_job::write_snapshot_to_dir(&jobs_dir, &write_snapshot)
    })
    .await
    .map_err(|error| format!("导出快照未能完成：{error}"))??;
    let now = export_job::timestamp();
    let record = ExportJobRecord {
        job_id: job_id.clone(),
        project_id: current.project_id,
        revision: current.revision,
        snapshot_path: snapshot_path
            .to_str()
            .ok_or("导出快照路径必须是有效 Unicode")?
            .to_string(),
        output_path: current.output_path,
        state: ExportJobState::Queued,
        progress: 0.0,
        cancel_requested: false,
        retry_available: true,
        created_at: now.clone(),
        updated_at: now,
        error: None,
    };
    export_job::register_job(&app, record, snapshot)
}

#[tauri::command]
pub async fn media_relink(app: AppHandle, media_id: String) -> Result<ProjectSession, String> {
    with_session(app, move |app, session| {
        let Some(existing) = session
            .project
            .media_refs
            .iter()
            .find(|media| media.id == media_id)
            .cloned()
        else {
            return Err("找不到要重新链接的素材".into());
        };
        let mut dialog = app
            .dialog()
            .file()
            .set_title("重新链接素材")
            .add_filter("匹配的媒体格式", &[existing.extension.as_str()]);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        let Some(selected) = dialog.blocking_pick_file() else {
            session.status = "已取消重新链接".into();
            return Ok(());
        };
        let path = selected
            .into_path()
            .map_err(|error| format!("无效的素材路径：{error}"))?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(kind) = media_kind(&extension) else {
            return Err("所选文件格式不受支持".into());
        };
        if kind != existing.kind || extension != existing.extension {
            return Err("所选文件与原素材的类型或扩展名不匹配".into());
        }
        let source = path
            .canonicalize()
            .map_err(|error| format!("无法访问素材：{error}"))?;
        let metadata = source
            .metadata()
            .map_err(|error| format!("无法读取素材属性：{error}"))?;
        let modified = metadata
            .modified()
            .map_err(|error| format!("无法读取素材修改时间：{error}"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "素材修改时间早于 1970 年，当前不支持".to_string())?
            .as_millis();
        let last_modified = u64::try_from(modified).map_err(|_| "素材修改时间超出支持范围")?;
        let source_path = source
            .to_str()
            .ok_or("素材路径必须是有效 Unicode")?
            .to_string();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("素材文件名必须是有效 Unicode")?
            .to_string();
        let asset = session
            .project
            .media_refs
            .iter_mut()
            .find(|media| media.id == media_id)
            .ok_or("找不到要重新链接的素材")?;
        asset.name = name;
        asset.extension = extension;
        asset.size = metadata.len();
        asset.last_modified = last_modified;
        asset.source_path = Some(source_path);
        let updated_asset = asset.clone();
        let used = session.timeline_references_media(&media_id);
        if used {
            let ffprobe = crate::media_engine::packaged_binary(app, "ffprobe")
                .ok_or("缺少 ffprobe，不能验证已使用素材的新时长")?;
            let frames =
                crate::timeline::source_frames(&ffprobe, &updated_asset, &session.project)?;
            let frames = if updated_asset.kind == crate::project::MediaKind::Image {
                session.project.max_timeline_frames()
            } else {
                frames
            };
            session.reconcile_relinked_media(&media_id, frames)?;
        }
        allow_project_media_assets(app, &session.project);
        session.project.updated_at = timestamp();
        session.dirty = true;
        session.status = if used {
            "素材已重新链接，尚未保存；旧时间线历史已清理"
        } else {
            "素材已重新链接，尚未保存"
        }
        .into();
        Ok(())
    })
    .await
}

pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    let tauri::WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let state = window.state::<ProjectState>();
    if export_job::has_running_jobs(window.app_handle()) {
        api.prevent_close();
        if state.close_prompt.swap(true, Ordering::AcqRel) {
            return;
        }
        let window = window.clone();
        tauri::async_runtime::spawn_blocking(move || {
            window
                .app_handle()
                .dialog()
                .message("正在导出。请等待完成，或在导出窗口取消任务后再关闭开剪。")
                .title("KaiCut · 正在导出")
                .blocking_show();
            window
                .state::<ProjectState>()
                .close_prompt
                .store(false, Ordering::Release);
        });
        return;
    }
    match state.session.try_lock() {
        Ok(session) if !session.dirty => return,
        Ok(_) => {}
        Err(_) => {
            api.prevent_close();
            return;
        }
    }
    api.prevent_close();
    if state.close_prompt.swap(true, Ordering::AcqRel) {
        return;
    }
    let window = window.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let app = window.app_handle();
        let state = app.state::<ProjectState>();
        if let Ok(session) = state.session.lock() {
            if (!session.dirty || confirm_discard(app, &session.project.name))
                && !export_job::has_running_jobs(app)
            {
                if let Err(error) = window.destroy() {
                    log::error!("Unable to close the project window: {error}");
                }
            }
        }
        state.close_prompt.store(false, Ordering::Release);
    });
}

#[cfg(test)]
mod tests {
    use super::{ExportPreflightState, inspect_media_link, validate_export_output};
    use crate::project::{KaicutProject, MediaAsset, MediaKind};
    use serde_json::json;
    use std::fs;

    #[test]
    fn media_link_status_detects_online_changed_and_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.mp4");
        fs::write(&path, b"one").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let modified = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let asset = MediaAsset {
            id: "media-1".into(),
            name: "clip.mp4".into(),
            kind: MediaKind::Video,
            extension: "mp4".into(),
            size: metadata.len(),
            last_modified: modified,
            source_path: Some(path.to_string_lossy().into_owned()),
        };
        assert_eq!(inspect_media_link(&asset).state, "online");
        fs::write(&path, b"changed-size").unwrap();
        assert_eq!(inspect_media_link(&asset).state, "changed");
        fs::remove_file(path).unwrap();
        assert_eq!(inspect_media_link(&asset).state, "missing");
    }

    #[test]
    fn export_output_validation_requires_absolute_mp4_and_existing_parent() {
        let project = KaicutProject::new();
        assert!(matches!(
            validate_export_output(&project, "output.mp4", false).state,
            ExportPreflightState::Fail
        ));
        assert!(matches!(
            validate_export_output(&project, "C:\\missing\\output.mov", false).state,
            ExportPreflightState::Fail
        ));
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("render.mp4");
        assert!(matches!(
            validate_export_output(&project, output.to_str().unwrap(), false).state,
            ExportPreflightState::Pass
        ));
    }

    #[test]
    fn export_output_validation_never_overwrites_existing_files() {
        let project = KaicutProject::new();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("render.mp4");
        fs::write(&output, b"existing").unwrap();
        assert!(matches!(
            validate_export_output(&project, output.to_str().unwrap(), false).state,
            ExportPreflightState::Fail
        ));
        assert!(matches!(
            validate_export_output(&project, output.to_str().unwrap(), true).state,
            ExportPreflightState::Fail
        ));
    }

    #[test]
    fn export_output_rejects_directories_reserved_names_and_source_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let mut project = KaicutProject::new();
        let folder = directory.path().join("folder.mp4");
        fs::create_dir(&folder).unwrap();
        for name in ["folder.mp4", "AUX.mp4", "clip:stream.mp4", "bad?.mp4"] {
            assert!(matches!(
                validate_export_output(
                    &project,
                    directory.path().join(name).to_str().unwrap(),
                    true
                )
                .state,
                ExportPreflightState::Fail
            ));
        }
        let source = directory.path().join("source.mp4");
        fs::write(&source, b"source").unwrap();
        project.media_refs.push(MediaAsset {
            id: "media".into(),
            name: "source.mp4".into(),
            kind: MediaKind::Video,
            extension: "mp4".into(),
            size: 6,
            last_modified: 0,
            source_path: Some(source.to_string_lossy().into_owned()),
        });
        let check = validate_export_output(&project, source.to_str().unwrap(), true);
        assert!(matches!(check.state, ExportPreflightState::Fail));
        assert!(check.detail.unwrap().contains("源素材"));
        assert_eq!(fs::read(source).unwrap(), b"source");
    }

    #[test]
    fn export_preflight_check_uses_stable_camel_case_schema() {
        let check = super::ExportPreflightCheck {
            id: "ffmpeg".into(),
            label: "编码器能力".into(),
            state: ExportPreflightState::Pass,
            detail: Some("ok".into()),
        };
        assert_eq!(
            serde_json::to_value(check).unwrap(),
            json!({"id":"ffmpeg","label":"编码器能力","state":"pass","detail":"ok"})
        );
    }
}
