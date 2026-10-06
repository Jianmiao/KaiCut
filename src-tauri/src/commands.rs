use crate::project::{
    MEDIA_EXTENSIONS, ProjectSession, import_media, read_project, save_project, timestamp,
    validate_text,
};
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

async fn with_session<F>(app: AppHandle, operation: F) -> Result<ProjectSession, String>
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
        *session = ProjectSession::default();
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
        *session = ProjectSession {
            status: format!("已打开 {}", project.name),
            project,
            path: Some(path_string),
            dirty: false,
        };
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn project_save(app: AppHandle) -> Result<ProjectSession, String> {
    with_session(app, |app, session| {
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

pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    let tauri::WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let state = window.state::<ProjectState>();
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
            if !session.dirty || confirm_discard(app, &session.project.name) {
                if let Err(error) = window.destroy() {
                    log::error!("Unable to close the project window: {error}");
                }
            }
        }
        state.close_prompt.store(false, Ordering::Release);
    });
}
