mod commands;
mod config;
mod events;
mod manager;
mod media_server;
mod parser;
mod recorder;
mod storage;
mod types;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let config = config::ConfigState::load(&app.handle()).map_err(std::io::Error::other)?;
            app.manage(config);
            let manager = manager::Manager::new(app.handle().clone());
            manager.start();
            app.manage(manager);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::config_get_all,
            commands::config_set,
            commands::config_set_values,
            commands::reconfigure_base,
            commands::select_path,
            commands::select_file,
            commands::get_videos,
            commands::get_video_url,
            commands::delete_videos,
            commands::protect_videos,
            commands::tag_videos,
            commands::open_in_explorer,
            commands::clip_video,
            commands::recorder_start,
            commands::recorder_restart,
            commands::recorder_stop,
            commands::recorder_save_replay,
            commands::get_gsr_audio_devices,
            commands::toggle_manual_recording,
            commands::force_stop_recording,
            commands::test_run,
            commands::write_clipboard,
            commands::open_url,
            commands::get_app_version
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Warcraft Recorder");
}
