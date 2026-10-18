mod appearance;
mod commands;
mod export_job;
mod export_render;
mod mcp_bridge;
mod mcp_project;
mod media_engine;
mod media_thumbnail;
mod media_waveform;
mod native_preview_host;
#[cfg(feature = "native-preview-queue")]
pub mod preview_queue;
#[cfg(feature = "native-preview-surface")]
pub mod preview_surface;
mod project;
mod recent;
mod recovery;
mod timeline;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    if std::env::args_os().any(|argument| argument == "--mcp") {
        if std::env::args_os().any(|argument| argument == "--mcp-hidden") {
            for window in &mut context.config_mut().app.windows {
                window.visible = false;
                window.focus = false;
            }
        }
        // Explicit debug-only isolated profile for real protocol tests. It
        // never reads or overwrites the user's recovery/project metadata.
        #[cfg(debug_assertions)]
        if std::env::args_os().any(|argument| argument == "--mcp-validation") {
            context.config_mut().identifier =
                format!("com.kaicut.mcp-validation-{}", std::process::id());
        }
    }
    tauri::Builder::default()
        .manage(commands::ProjectState::default())
        .manage(export_job::ExportJobsState::default())
        .manage(appearance::AppearanceState::default())
        .manage(mcp_bridge::BridgeState::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::project_get,
            commands::project_new,
            commands::project_open,
            commands::project_save,
            commands::project_recent_list,
            commands::project_open_recent,
            commands::project_rename,
            commands::project_import,
            commands::media_check_links,
            commands::export_preflight,
            commands::export_choose_output,
            commands::export_snapshot_create,
            export_job::export_job_status,
            export_job::export_job_cancel,
            export_job::export_job_start,
            export_job::export_job_list,
            commands::media_relink,
            timeline::timeline_edit,
            timeline::timeline_undo,
            timeline::timeline_redo,
            timeline::timeline_audio_gain,
            recovery::project_recovery_checkpoint,
            recovery::project_recovery_status,
            recovery::project_recovery_restore,
            recovery::project_recovery_discard,
            media_engine::media_engine_status,
            media_engine::media_probe,
            media_thumbnail::media_thumbnail,
            media_waveform::media_waveform,
            appearance::apply_window_appearance,
            mcp_bridge::mcp_ui_ack,
            native_preview_host::native_preview_host_status,
            native_preview_host::native_preview_host_layout,
            native_preview_host::native_preview_host_release,
            native_preview_host::native_preview_validation_capture,
            native_preview_host::native_source_capability,
            native_preview_host::native_source_open,
            native_preview_host::native_source_cancel_open,
            native_preview_host::native_source_control,
            native_preview_host::native_source_status,
        ])
        .on_window_event(|window, event| {
            native_preview_host::handle_window_event(window, event);
            commands::handle_window_event(window, event);
        })
        .setup(|app| {
            if let Err(error) = export_job::initialize(app.handle()) {
                log::warn!("Unable to load export history: {error}");
            }
            appearance::initialize(app.handle())?;
            mcp_bridge::initialize(app.handle())?;
            native_preview_host::initialize(app.handle())?;
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .build(context)
        .expect("KaiCut Tauri application failed to start")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                if native_preview_host::source_exit_pending(app) {
                    api.prevent_exit();
                }
            }
            if matches!(event, tauri::RunEvent::Exit) {
                native_preview_host::shutdown();
                mcp_bridge::shutdown(app);
            }
        });
}
