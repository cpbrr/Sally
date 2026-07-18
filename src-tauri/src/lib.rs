pub mod audio;
pub mod commands;
pub mod config;
pub mod diarization;
pub mod error;
pub mod gemini;
pub mod lang;
pub mod models;
pub mod readout;
pub mod session;
pub mod store;
pub mod timeline;

use commands::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            // Load config through the data-dir pointer, if setup already ran.
            let state = app.state::<AppState>();
            if let Ok(dir) = app.path().app_config_dir() {
                if let Some(data_dir) = config::read_data_dir_pointer(&dir) {
                    match config::AppConfig::load(data_dir) {
                        Ok(cfg) => {
                            // No contention at setup time; try_lock is safe on
                            // any thread (blocking_lock panics inside tokio).
                            if let Ok(mut guard) = state.config.try_lock() {
                                *guard = Some(cfg);
                            }
                        }
                        Err(e) => log::error!("failed to load config: {e}"),
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_boot_info,
            commands::get_api_key,
            commands::save_settings,
            commands::list_audio_devices,
            commands::list_audio_apps,
            commands::test_connectivity,
            commands::start_meeting,
            commands::pause_meeting,
            commands::resume_meeting,
            commands::set_readout,
            commands::end_meeting,
            commands::get_last_meeting,
            commands::apply_review,
            commands::export_without_timestamps,
            commands::clean_and_summarize,
            commands::recover_meetings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Sally");
}
