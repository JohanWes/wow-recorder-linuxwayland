use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::types::{ActivityStatus, DiskStatus, MicStatus, RecStatus, RendererVideo, SoundAlerts};

pub fn emit(app: &AppHandle, event: &str, payload: impl Serialize + Clone) {
    if let Err(error) = app.emit(event, payload) {
        eprintln!("failed to emit {event}: {error}");
    }
}

pub fn update_rec_status(app: &AppHandle, status: RecStatus, msg: Option<String>) {
    emit(
        app,
        "updateRecStatus",
        serde_json::json!({ "status": status, "msg": msg }),
    );
}
pub fn update_activity_status(app: &AppHandle, status: Option<ActivityStatus>) {
    emit(app, "updateActivityStatus", status);
}
pub fn set_disk_videos(app: &AppHandle, videos: Vec<RendererVideo>) {
    emit(app, "setDiskVideos", videos);
}
pub fn update_disk_status(app: &AppHandle, status: DiskStatus) {
    emit(app, "updateDiskStatus", status);
}
pub fn update_mic_status(app: &AppHandle, status: MicStatus) {
    emit(app, "updateMicStatus", status);
}
pub fn play_audio(app: &AppHandle, alert: SoundAlerts) {
    emit(app, "playAudio", alert);
}
pub fn pause_player(app: &AppHandle) {
    emit(app, "pausePlayer", ());
}
pub fn update_advanced_logging_status(app: &AppHandle, retail: bool) {
    emit(
        app,
        "updateAdvancedLoggingStatus",
        serde_json::json!({ "retail": retail, "classic": false, "era": false, "retailPtr": false, "classicPtr": false }),
    );
}
pub fn update_error_report(app: &AppHandle, date: String, reason: String) {
    emit(
        app,
        "updateErrorReport",
        serde_json::json!({ "date": date, "reason": reason }),
    );
}
