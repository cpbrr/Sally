pub mod audio;
pub mod commands;
pub mod config;
pub mod error;
pub mod gemini;
pub mod lang;
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
                        Ok(mut cfg) => {
                            // Readout must always start off, regardless of
                            // whatever a previous session (or an older
                            // build that still persisted it) left in
                            // .env — it's a live-only toggle now, never
                            // written back by set_readout.
                            cfg.readout_enabled = false;
                            // Garbage-collect the local-diarization embedding
                            // model (feature removed in v0.9.0; Gemini cleanup
                            // handles speaker attribution now) and the
                            // speaker-change segmentation model (feature
                            // removed later — see git history).
                            let models_dir = cfg.data_dir.join("models");
                            for stale in [
                                models_dir.join("speaker_embedding_campp.onnx"),
                                models_dir.join("segmentation_pyannote3.onnx"),
                            ] {
                                if stale.exists() {
                                    let _ = std::fs::remove_file(stale);
                                }
                            }
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
            commands::request_mic_permission,
            commands::list_audio_apps,
            commands::test_connectivity,
            commands::start_meeting,
            commands::pause_meeting,
            commands::resume_meeting,
            commands::set_mic_muted,
            commands::set_readout,
            commands::set_readout_volume,
            commands::switch_mic,
            commands::switch_capture_app,
            commands::end_meeting,
            commands::get_last_meeting,
            commands::list_meetings,
            commands::open_meeting,
            commands::meeting_chunks,
            commands::apply_review,
            commands::export_without_timestamps,
            commands::clean_and_summarize,
            commands::recover_meetings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Sally")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<AppState>();
                if let Ok(mut guard) = state.session.try_lock() {
                    if let Some(mut session) = guard.take() {
                        let _ = session.control_tx.try_send(crate::session::Control::Stop);
                        if let Some(done_rx) = session.done_rx.take() {
                            let _ = done_rx.blocking_recv();
                        }
                    }
                }
            }
        });
}

