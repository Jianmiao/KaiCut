mod appearance;
mod commands;
mod media_engine;
mod project;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(commands::ProjectState::default())
        .manage(appearance::AppearanceState::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::project_get,
            commands::project_new,
            commands::project_open,
            commands::project_save,
            commands::project_rename,
            commands::project_import,
            media_engine::media_engine_status,
            appearance::apply_window_appearance,
        ])
        .on_window_event(commands::handle_window_event)
        .setup(|app| {
            appearance::initialize(app.handle())?;
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("KaiCut Tauri application failed to start");
}
