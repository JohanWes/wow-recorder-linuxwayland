use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager as _};

use crate::config::ConfigState;

const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/JohanWes/wow-recorder-linuxwayland/releases/latest";
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    latest_version: String,
    latest_release_tag: String,
    current_release_tag: String,
    release_url: String,
}

#[derive(Clone, Serialize)]
struct UpdateProgress {
    stage: String,
    message: String,
}

pub fn compare_versions(v1: &str, v2: &str) -> std::cmp::Ordering {
    let clean = |version: &str| {
        version
            .strip_prefix('v')
            .unwrap_or(version)
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let a = clean(v1);
    let b = clean(v2);
    for index in 0..a.len().max(b.len()) {
        let ordering = a
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(index).copied().unwrap_or(0));
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

pub fn extract_version_from_tag(tag: &str) -> String {
    let version = tag
        .strip_prefix("linux-")
        .or_else(|| tag.strip_prefix('v'))
        .unwrap_or(tag);
    let Some((candidate, suffix)) = version.rsplit_once('-') else {
        return version.to_owned();
    };
    if suffix.len() >= 7
        && suffix
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        candidate.to_owned()
    } else {
        version.to_owned()
    }
}

fn read_installed_release_tag() -> String {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("../share/warcraftrecorder/release-tag"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/share/warcraftrecorder/release-tag"));
    }
    for candidate in candidates {
        match fs::read_to_string(&candidate) {
            Ok(tag) => return tag.trim().to_owned(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "[UpdateService] Failed to read installed release tag from {}: {error}",
                candidate.display()
            ),
        }
    }
    String::new()
}

pub fn check(app: &AppHandle, config: &ConfigState) -> Result<Option<UpdateInfo>, String> {
    let current_version = app.package_info().version.to_string();
    let current_release_tag = read_installed_release_tag();

    if std::env::var("WR_UPDATE_DRY_RUN").as_deref() == Ok("true") {
        eprintln!("[UpdateService] Dry run mode - simulating update available");
        return Ok(Some(UpdateInfo {
            latest_version: format!("v{current_version}.999 (dry run)"),
            latest_release_tag: format!("dry-run-{current_version}.999"),
            current_version,
            current_release_tag,
            release_url: "https://github.com/JohanWes/wow-recorder-linuxwayland/releases/latest"
                .into(),
        }));
    }

    if cfg!(debug_assertions) {
        eprintln!("[UpdateService] Skipping update check in dev mode");
        return Ok(None);
    }

    eprintln!("[UpdateService] Checking for updates...");
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github.v3+json",
            "-H",
            "User-Agent: WarcraftRecorder",
            GITHUB_RELEASES_API,
        ])
        .output()
        .map_err(|error| format!("could not run curl: {error}"))?;
    if !output.status.success() {
        eprintln!(
            "[UpdateService] Failed to check for updates: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Ok(None);
    }
    let release: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("could not parse GitHub release response: {error}"))?;
    let latest_tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("GitHub release response is missing tag_name")?;
    let release_url = release
        .get("html_url")
        .and_then(Value::as_str)
        .ok_or("GitHub release response is missing html_url")?;
    let latest_version = extract_version_from_tag(latest_tag);
    let comparison = compare_versions(&latest_version, &current_version);

    eprintln!(
        "[UpdateService] Current: {} ({}), Latest: {} ({})",
        current_version,
        if current_release_tag.is_empty() {
            "unknown release tag"
        } else {
            &current_release_tag
        },
        latest_version,
        latest_tag
    );
    if comparison == std::cmp::Ordering::Less {
        eprintln!("[UpdateService] Already up to date");
        return Ok(None);
    }
    if comparison == std::cmp::Ordering::Equal && current_release_tag == latest_tag {
        eprintln!("[UpdateService] Already on latest release tag");
        return Ok(None);
    }
    let dismissed = config
        .all()?
        .get("dismissedUpdateVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if dismissed == latest_tag {
        eprintln!("[UpdateService] Release {latest_tag} was dismissed, skipping");
        return Ok(None);
    }

    eprintln!("[UpdateService] Update available: {latest_version}");
    Ok(Some(UpdateInfo {
        current_version,
        latest_version,
        latest_release_tag: latest_tag.to_owned(),
        current_release_tag,
        release_url: release_url.to_owned(),
    }))
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = app.state::<ConfigState>();
        check(&app, &config)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn emit_progress(app: &AppHandle, stage: &str, message: impl Into<String>) {
    let _ = app.emit(
        "updateProgress",
        UpdateProgress {
            stage: stage.into(),
            message: message.into(),
        },
    );
}

fn emit_line(app: &AppHandle, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if line.contains("[install] Done") {
        emit_progress(app, "done", line);
    } else if line.contains("[install] Downloading")
        && line.contains("AppImage")
        && !line.contains("sha256")
    {
        emit_progress(app, "downloading", line);
    } else if line.contains("[install] Checksum verified") {
        emit_progress(app, "verifying", line);
    } else if line.contains("[install] Installed binary") {
        emit_progress(app, "installing", line);
    }
}

fn install(app: AppHandle) -> Result<(), String> {
    let mut child = Command::new("bash")
        .args(["-c", &format!("curl -fsSL {INSTALL_SCRIPT_URL} | bash")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start install script: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("could not read installer stdout")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or("could not read installer stderr")?;
    let stderr_reader = std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stderr.read_to_string(&mut output);
        output
    });
    for line in BufReader::new(stdout).lines() {
        emit_line(&app, &line.map_err(|error| error.to_string())?);
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if status.success() {
        eprintln!("[UpdateService] Install script completed successfully");
        Ok(())
    } else {
        let message = if stderr.trim().is_empty() {
            format!("exit status {status}")
        } else {
            stderr.trim().to_owned()
        };
        emit_progress(&app, "error", &message);
        Err(message)
    }
}

static UPDATE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub async fn perform_update(app: AppHandle) -> Result<(), String> {
    if UPDATE_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return Err("an update is already in progress".into());
    }
    let result = perform_update_inner(app).await;
    UPDATE_IN_FLIGHT.store(false, Ordering::SeqCst);
    result
}

async fn perform_update_inner(app: AppHandle) -> Result<(), String> {
    if std::env::var("WR_UPDATE_INSTALL_DRY_RUN").as_deref() == Ok("true") {
        let stages = [
            (
                "downloading",
                "[install] Downloading WarcraftRecorder.AppImage...",
            ),
            ("verifying", "[install] Checksum verified (abcdef12)."),
            (
                "installing",
                "[install] Installed binary: ~/.local/bin/warcraftrecorder",
            ),
            ("done", "[install] Done. Run 'warcraftrecorder' to start."),
        ];
        for (stage, message) in stages {
            tokio::time::sleep(Duration::from_millis(800)).await;
            emit_progress(&app, stage, message);
        }
        return Ok(());
    }
    tauri::async_runtime::spawn_blocking(move || install(app))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_versions_from_release_tags() {
        assert_eq!(extract_version_from_tag("linux-7.7.1-43e3ebf"), "7.7.1");
        assert_eq!(extract_version_from_tag("v8.0.0"), "8.0.0");
        assert_eq!(extract_version_from_tag("8.0.0-beta"), "8.0.0-beta");
    }

    #[test]
    fn compares_numeric_dotted_versions() {
        assert_eq!(
            compare_versions("8.0.0", "7.7.1"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(compare_versions("v8.0", "8.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("7.7.1", "8.0.0"), std::cmp::Ordering::Less);
    }
}
