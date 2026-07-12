use std::{collections::HashSet, path::PathBuf, process::Command, sync::Arc};

use serde_json::{Map, Value};
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::{DialogExt, FilePath};
use tauri_plugin_opener::OpenerExt;

use crate::{
    config::ConfigState,
    manager::{self, Manager},
    parser,
    storage::{self, VideoQueueItem},
    types::{Metadata, RendererVideo},
};
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
pub async fn reconfigure_base(manager: State<'_, Arc<Manager>>) -> Result<(), String> {
    manager.reconfigure().await
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
pub async fn get_videos(manager: State<'_, Arc<Manager>>) -> Result<Vec<RendererVideo>, String> {
    manager.videos().await
}
#[tauri::command]
pub async fn get_rec_status(manager: State<'_, Arc<Manager>>) -> Result<Value, String> {
    let (status, msg) = manager.rec_status().await;
    Ok(serde_json::json!({ "status": status, "msg": msg }))
}
#[tauri::command]
pub async fn get_video_url(
    manager: State<'_, Arc<Manager>>,
    path: String,
) -> Result<String, String> {
    manager.video_url(path).await
}
#[tauri::command]
pub async fn delete_videos(
    manager: State<'_, Arc<Manager>>,
    video_paths: Vec<String>,
) -> Result<(), String> {
    let video_paths = validated_paths(&manager, video_paths).await?;
    storage::delete_videos(&video_paths).map_err(|e| e.to_string())?;
    manager.refresh_disk().await;
    Ok(())
}
#[tauri::command]
pub async fn protect_videos(
    manager: State<'_, Arc<Manager>>,
    video_paths: Vec<String>,
    protect: bool,
) -> Result<(), String> {
    let video_paths = validated_paths(&manager, video_paths).await?;
    storage::protect_videos(&video_paths, protect).map_err(|e| e.to_string())?;
    manager.refresh_disk().await;
    Ok(())
}
#[tauri::command]
pub async fn tag_videos(
    manager: State<'_, Arc<Manager>>,
    video_paths: Vec<String>,
    tag: String,
) -> Result<(), String> {
    let video_paths = validated_paths(&manager, video_paths).await?;
    storage::tag_videos(&video_paths, &tag).map_err(|e| e.to_string())?;
    manager.refresh_disk().await;
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
pub async fn clip_video(
    manager: State<'_, Arc<Manager>>,
    source: String,
    offset: f64,
    duration: f64,
    metadata: Metadata,
) -> Result<(), String> {
    let source = manager.validate_video_path(source).await?;
    manager
        .enqueue(VideoQueueItem {
            name: manager::source_name(&source),
            source,
            suffix: format!("Clip {}", chrono::Local::now().format("%Y-%m-%d %H-%M-%S")),
            offset,
            duration,
            clip: true,
            metadata: manager::clip_metadata(metadata),
        })
        .await
}

#[tauri::command]
pub async fn recorder_start(manager: State<'_, Arc<Manager>>) -> Result<(), String> {
    let result = manager.recorder().start_buffer().await;
    manager.refresh_status().await;
    result
}
#[tauri::command]
pub async fn recorder_restart(manager: State<'_, Arc<Manager>>) -> Result<(), String> {
    let result = manager.recorder().restart_capture(true).await;
    manager.refresh_status().await;
    result
}
#[tauri::command]
pub async fn recorder_stop(manager: State<'_, Arc<Manager>>) -> Result<(), String> {
    manager.recorder().shutdown();
    manager.refresh_status().await;
    Ok(())
}
#[tauri::command]
pub async fn recorder_save_replay(
    manager: State<'_, Arc<Manager>>,
    config: State<'_, ConfigState>,
) -> Result<(), String> {
    let source = manager.recorder().save_replay_now().await?;
    let duration = config
        .all()?
        .get("linuxGsrBufferSeconds")
        .and_then(Value::as_f64)
        .unwrap_or(180.0);
    manager
        .enqueue(VideoQueueItem {
            name: manager::source_name(&source),
            source,
            suffix: format!(
                "Replay {}",
                chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
            ),
            offset: 0.0,
            duration,
            clip: true,
            metadata: manager::replay_metadata(duration),
        })
        .await
}
#[tauri::command]
pub fn get_gsr_audio_devices() -> Result<Value, String> {
    let output = Command::new("gpu-screen-recorder")
        .arg("--list-audio-devices")
        .output()
        .map(|o| {
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default();
    Ok(parse_audio_devices(&output))
}
#[tauri::command]
pub async fn toggle_manual_recording(
    manager: State<'_, Arc<Manager>>,
    config: State<'_, ConfigState>,
) -> Result<(), String> {
    if config.all()?.get("manualRecord").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    manager
        .with_parser(|p| p.handle_manual_recording_toggle())
        .await
}
#[tauri::command]
pub async fn force_stop_recording(manager: State<'_, Arc<Manager>>) -> Result<(), String> {
    manager.force_stop().await
}
#[tauri::command]
pub async fn test_run(
    manager: State<'_, Arc<Manager>>,
    category: String,
    end_test: bool,
) -> Result<(), String> {
    let lines = if category == "Raids" {
        parser::test_data::RAID
    } else {
        parser::test_data::DUNGEON
    };
    let take = if end_test {
        lines.len()
    } else {
        lines.len().saturating_sub(1)
    };
    manager
        .with_parser(|p| {
            for line in &lines[..take] {
                // chrono has no %.4f specifier; formatting with one makes
                // Display return an error and format! panic.
                let timestamp = chrono::Local::now().format("%m/%d/%Y %H:%M:%S%.3f");
                p.inject_raw_line(&format!("{timestamp}  {line}"));
            }
        })
        .await
}

async fn validated_paths(manager: &Manager, values: Vec<String>) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        result.push(manager.validate_video_path(value).await?);
    }
    Ok(result)
}

fn parse_audio_devices(text: &str) -> Value {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut all = Vec::new();
    let mut section = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if line.ends_with(':') && lower.contains("output") && lower.contains("device") {
            section = 1;
            continue;
        }
        if line.ends_with(':') && lower.contains("input") && lower.contains("device") {
            section = 2;
            continue;
        }
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line);
        let mut parts = line.splitn(2, char::is_whitespace);
        let value = parts.next().unwrap_or("");
        if value != "default_output" && value != "default_input" && !value.starts_with("device:") {
            continue;
        }
        let detail = parts.next().unwrap_or("").trim();
        let label = if detail.is_empty() {
            value.into()
        } else {
            format!("{value} — {detail}")
        };
        let device = serde_json::json!({"value": value, "label": label});
        match section {
            1 => outputs.push(device),
            2 => inputs.push(device),
            _ => all.push(device),
        }
    }
    if outputs.is_empty() {
        outputs = all.clone();
    }
    if inputs.is_empty() {
        inputs = all;
    }
    dedup_filter(&mut outputs, "default_input");
    dedup_filter(&mut inputs, "default_output");
    if !outputs.iter().any(|d| d["value"] == "default_output") {
        outputs.insert(0, serde_json::json!({"value":"default_output","label":"default_output — Default output device"}));
    }
    if !inputs.iter().any(|d| d["value"] == "default_input") {
        inputs.insert(0, serde_json::json!({"value":"default_input","label":"default_input — Default input device"}));
    }
    serde_json::json!({"inputs":inputs,"outputs":outputs})
}
fn dedup_filter(values: &mut Vec<Value>, excluded: &str) {
    let mut seen = HashSet::new();
    values.retain(|v| {
        let key = v["value"].as_str().unwrap_or("").to_owned();
        key != excluded && seen.insert(key)
    });
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
