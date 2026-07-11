use std::process::Command;

use serde_json::{Map, Value};
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::{DialogExt, FilePath};
use tauri_plugin_opener::OpenerExt;

use crate::{
    config::ConfigState,
    types::{Metadata, RendererVideo},
};

fn stub(command: &str) {
    eprintln!("{command} is not implemented yet");
}
fn file_path(path: Option<FilePath>) -> String {
    match path {
        Some(FilePath::Path(path)) => path.to_string_lossy().into_owned(),
        Some(FilePath::Url(url)) => url.to_string(),
        None => String::new(),
    }
}

#[tauri::command]
pub fn config_get_all(config: State<'_, ConfigState>) -> Result<Value, String> {
    config.all()
}
#[tauri::command]
pub fn config_set(config: State<'_, ConfigState>, key: String, value: Value) -> Result<(), String> {
    config.set(key, value)
}
#[tauri::command]
pub fn config_set_values(
    config: State<'_, ConfigState>,
    values: Map<String, Value>,
) -> Result<(), String> {
    config.set_values(values)
}
#[tauri::command]
pub fn reconfigure_base() -> Result<(), String> {
    stub("reconfigure_base");
    Ok(())
}

#[tauri::command]
pub fn select_path(app: AppHandle) -> Result<String, String> {
    Ok(file_path(app.dialog().file().blocking_pick_folder()))
}
#[tauri::command]
pub fn select_file(app: AppHandle) -> Result<String, String> {
    Ok(file_path(app.dialog().file().blocking_pick_file()))
}

#[tauri::command]
pub fn get_videos() -> Result<Vec<RendererVideo>, String> {
    stub("get_videos");
    Ok(vec![])
}
#[tauri::command]
pub fn delete_videos(video_paths: Vec<String>) -> Result<(), String> {
    let _ = video_paths;
    stub("delete_videos");
    Ok(())
}
#[tauri::command]
pub fn protect_videos(video_paths: Vec<String>, protect: bool) -> Result<(), String> {
    let _ = (video_paths, protect);
    stub("protect_videos");
    Ok(())
}
#[tauri::command]
pub fn tag_videos(video_paths: Vec<String>, tag: String) -> Result<(), String> {
    let _ = (video_paths, tag);
    stub("tag_videos");
    Ok(())
}
#[tauri::command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open path: {error}"))
}
#[tauri::command]
pub fn clip_video(
    source: String,
    offset: f64,
    duration: f64,
    metadata: Metadata,
) -> Result<(), String> {
    let _ = (source, offset, duration, metadata);
    stub("clip_video");
    Ok(())
}

#[tauri::command]
pub fn recorder_start() -> Result<(), String> {
    stub("recorder_start");
    Ok(())
}
#[tauri::command]
pub fn recorder_restart() -> Result<(), String> {
    stub("recorder_restart");
    Ok(())
}
#[tauri::command]
pub fn recorder_stop() -> Result<(), String> {
    stub("recorder_stop");
    Ok(())
}
#[tauri::command]
pub fn recorder_save_replay() -> Result<(), String> {
    stub("recorder_save_replay");
    Ok(())
}
#[tauri::command]
pub fn get_gsr_audio_devices() -> Result<Value, String> {
    stub("get_gsr_audio_devices");
    Ok(serde_json::json!({ "inputs": [], "outputs": [] }))
}
#[tauri::command]
pub fn toggle_manual_recording() -> Result<(), String> {
    stub("toggle_manual_recording");
    Ok(())
}
#[tauri::command]
pub fn force_stop_recording() -> Result<(), String> {
    stub("force_stop_recording");
    Ok(())
}
#[tauri::command]
pub fn test_run(category: String, end_test: bool) -> Result<(), String> {
    let _ = (category, end_test);
    stub("test_run");
    Ok(())
}

#[tauri::command]
pub fn write_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard()
        .write_text(text)
        .map_err(|error| error.to_string())
}
#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|error| error.to_string())
}
#[tauri::command]
pub fn get_app_version(app: AppHandle) -> Result<String, String> {
    Ok(app.package_info().version.to_string())
}
