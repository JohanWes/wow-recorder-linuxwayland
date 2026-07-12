//! Disk-backed video storage and the serial post-recording video queue.
//!
//! This module deliberately has no Tauri state dependency.  The manager owns
//! configuration and starts [`VideoProcessQueue::run`]; its completion callback
//! is the appropriate place to refresh `setDiskVideos` and `updateDiskStatus`.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, UNIX_EPOCH},
};

use tokio::sync::mpsc;

use crate::types::{DiskStatus, Metadata, RendererVideo, VideoCategory};

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Json(serde_json::Error),
    Ffmpeg(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "storage I/O error: {error}"),
            Self::Json(error) => write!(f, "metadata JSON error: {error}"),
            Self::Ffmpeg(error) => write!(f, "ffmpeg cut failed: {error}"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub type StorageResult<T> = Result<T, StorageError>;

/// A pending stream-copy operation.  The JSON field names intentionally match
/// the Electron `VideoQueueItem` shape.
#[derive(Debug, Clone)]
pub struct VideoQueueItem {
    pub name: String,
    pub source: PathBuf,
    pub suffix: String,
    pub offset: f64,
    pub duration: f64,
    pub clip: bool,
    pub metadata: Metadata,
}

#[derive(Debug, Clone)]
pub struct QueueCompletion {
    pub output_path: Option<PathBuf>,
    pub disk_status: DiskStatus,
    pub error: Option<String>,
}

type CompletionCallback = dyn Fn(QueueCompletion) + Send + Sync + 'static;

/// Enqueue handle for the serial video processing worker.
#[derive(Clone)]
pub struct VideoProcessQueue {
    sender: mpsc::UnboundedSender<VideoQueueItem>,
}

/// The receiving half is owned exclusively by the worker so dropping every
/// queue handle closes the channel and lets the worker exit.
pub struct VideoProcessWorker {
    receiver: mpsc::UnboundedReceiver<VideoQueueItem>,
    storage_path: PathBuf,
    max_storage_gb: u64,
    on_complete: Arc<CompletionCallback>,
}

impl VideoProcessQueue {
    pub fn new(
        storage_path: impl Into<PathBuf>,
        max_storage_gb: u64,
        on_complete: impl Fn(QueueCompletion) + Send + Sync + 'static,
    ) -> (Self, VideoProcessWorker) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let worker = VideoProcessWorker {
            receiver,
            storage_path: storage_path.into(),
            max_storage_gb,
            on_complete: Arc::new(on_complete),
        };
        (Self { sender }, worker)
    }

    pub fn enqueue(&self, item: VideoQueueItem) -> Result<(), VideoQueueItem> {
        self.sender.send(item).map_err(|error| error.0)
    }
}

impl VideoProcessWorker {
    /// Process messages one at a time until every queue handle has been dropped.
    pub async fn run(mut self) {
        while let Some(item) = self.receiver.recv().await {
            let completion = match process_video_item(&self.storage_path, item) {
                Ok(output_path) => {
                    let status = DiskSizeMonitor::new(&self.storage_path, self.max_storage_gb)
                        .run()
                        .unwrap_or_else(|error| {
                            eprintln!("disk size monitor failed: {error}");
                            DiskSizeMonitor::new(&self.storage_path, self.max_storage_gb)
                                .status()
                                .unwrap_or(DiskStatus {
                                    usage: 0.0,
                                    limit: 0.0,
                                })
                        });
                    QueueCompletion {
                        output_path: Some(output_path),
                        disk_status: status,
                        error: None,
                    }
                }
                Err(error) => QueueCompletion {
                    output_path: None,
                    disk_status: DiskSizeMonitor::new(&self.storage_path, self.max_storage_gb)
                        .status()
                        .unwrap_or(DiskStatus {
                            usage: 0.0,
                            limit: self.max_storage_gb as f64 * GIB as f64,
                        }),
                    error: Some(error.to_string()),
                },
            };
            (self.on_complete)(completion);
        }
    }
}

/// TS-compatible sanitisation used for generated video filenames.
pub fn sanitize_output_name(filename: &str) -> String {
    let mut output = String::with_capacity(filename.len());
    let mut last_space = false;
    for character in filename.chars() {
        let character = if matches!(character, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*') {
            ' '
        } else {
            character
        };
        if character == ' ' {
            if !last_space {
                output.push(character);
            }
            last_space = true;
        } else {
            output.push(character);
            last_space = false;
        }
    }
    output
}

pub fn output_video_path(storage_path: &Path, item: &VideoQueueItem) -> PathBuf {
    let mut name = item.name.clone();
    if !item.suffix.is_empty() {
        name.push_str(" - ");
        name.push_str(&item.suffix);
    }
    storage_path.join(format!("{}.mp4", sanitize_output_name(&name)))
}

/// Invoke ffmpeg as the Electron implementation does.  Kept separate so unit
/// tests can cover queue bookkeeping without requiring ffmpeg to be installed.
pub fn run_ffmpeg_cut(
    source: &Path,
    output: &Path,
    offset: f64,
    duration: Option<f64>,
    clip: bool,
) -> StorageResult<()> {
    let start = offset.max(0.0).to_string();
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-ss", &start, "-i"]).arg(source);
    if let Some(duration) = duration {
        command.args(["-t", &duration.to_string()]);
    }
    // Stream copy snaps to the previous keyframe; only mid-stream clip cuts
    // need the frame-accurate re-encode (both streams, or copied audio still
    // leads by the keyframe backoff). Offset-zero cuts are already aligned.
    if clip && offset > 0.0 {
        command.args([
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-c:a", "aac", "-b:a", "192k",
        ]);
    } else {
        command.args(["-c:v", "copy", "-c:a", "copy"]);
    }
    let result = command
        .args(["-avoid_negative_ts", "make_zero", "-movflags", "+faststart"])
        .arg(output)
        .status()
        .map_err(StorageError::Io)?;
    if result.success() {
        Ok(())
    } else {
        Err(StorageError::Ffmpeg(format!("exit status {result}")))
    }
}

/// Cut a video, persist its sidecar, then remove non-clip temporary sources.
pub fn process_video_item(storage_path: &Path, mut item: VideoQueueItem) -> StorageResult<PathBuf> {
    fs::create_dir_all(storage_path)?;
    let output = output_video_path(storage_path, &item);
    let duration = item.clip.then_some(item.duration);
    run_ffmpeg_cut(&item.source, &output, item.offset, duration, item.clip)?;
    if item.clip {
        // Manager normally supplies this exact shape.  Keeping it here as
        // well makes a direct queue caller unable to create an unclassified
        // clip, while preserving an already supplied parent category.
        let source_category = item.metadata.category;
        item.metadata.parent_category.get_or_insert(source_category);
        item.metadata.category = VideoCategory::Clips;
        item.metadata.protected = Some(true);
    }
    write_metadata_file(&output, &item.metadata)?;
    if !item.clip && fs::remove_file(&item.source).is_err() {
        // The old implementation retries once because encoders can retain a
        // handle briefly after ffmpeg exits.
        thread::sleep(Duration::from_secs(2));
        let _ = fs::remove_file(&item.source);
    }
    Ok(output)
}

pub fn metadata_path(video_path: &Path) -> PathBuf {
    video_path.with_extension("json")
}

pub fn thumbnail_path(video_path: &Path) -> PathBuf {
    video_path.with_extension("png")
}

pub fn get_metadata_for_video(video_path: &Path) -> StorageResult<Metadata> {
    Ok(serde_json::from_slice(&fs::read(metadata_path(
        video_path,
    ))?)?)
}

pub fn write_metadata_file(video_path: &Path, metadata: &Metadata) -> StorageResult<()> {
    fs::write(
        metadata_path(video_path),
        serde_json::to_vec_pretty(metadata)?,
    )?;
    Ok(())
}

fn mtime_millis(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn sorted_video_paths(
    storage_path: &Path,
    newest_first: bool,
) -> StorageResult<Vec<(PathBuf, fs::Metadata)>> {
    let mut videos = Vec::new();
    for entry in fs::read_dir(storage_path)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("mp4") {
            videos.push((path, entry.metadata()?));
        }
    }
    videos.sort_by_key(|(_, metadata)| mtime_millis(metadata));
    if newest_first {
        videos.reverse();
    }
    Ok(videos)
}

fn metadata_delete(metadata: &Metadata) -> bool {
    metadata
        .extra
        .get("delete")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

/// List valid videos newest first.  Sidecars flagged `delete: true` are hidden
/// immediately and removed after the caller has had a chance to refresh UI.
pub fn list_videos(storage_path: &Path) -> StorageResult<Vec<RendererVideo>> {
    let mut result = Vec::new();
    for (path, file_metadata) in sorted_video_paths(storage_path, true)? {
        let metadata = match get_metadata_for_video(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                eprintln!("failed to load metadata for {}: {error}", path.display());
                continue;
            }
        };
        if metadata_delete(&metadata) {
            delayed_delete_video(path);
            continue;
        }
        let video_name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        result.push(RendererVideo {
            is_protected: metadata.protected.unwrap_or(false),
            metadata,
            video_name: video_name.clone(),
            mtime: mtime_millis(&file_metadata),
            video_source: path.to_string_lossy().into_owned(),
            media_url: None,
            cloud: false,
            multi_pov: Vec::new(),
            unique_id: format!("{video_name}-disk"),
        });
    }
    Ok(result)
}

/// Remove a video, metadata and optional thumbnail.  A failed removal marks
/// its metadata for a later deletion attempt, matching Electron's resilience.
pub fn delete_video(video_path: &Path) -> StorageResult<()> {
    // Leave the sidecar in place until the MP4 and optional thumbnail have
    // gone; otherwise a failed delete could not be persisted as `delete:true`.
    if let Err(error) = remove_if_present(video_path) {
        let _ = mark_video_for_delete(video_path);
        return Err(StorageError::Io(error));
    }
    if let Err(error) = remove_if_present(&thumbnail_path(video_path)) {
        let _ = mark_video_for_delete(video_path);
        return Err(StorageError::Io(error));
    }
    if let Err(error) = remove_if_present(&metadata_path(video_path)) {
        let _ = mark_video_for_delete(video_path);
        return Err(StorageError::Io(error));
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn delete_videos(video_paths: &[PathBuf]) -> StorageResult<()> {
    let mut first_error = None;
    for path in video_paths {
        if let Err(error) = delete_video(path) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub fn mark_video_for_delete(video_path: &Path) -> StorageResult<()> {
    let mut metadata = get_metadata_for_video(video_path)?;
    metadata
        .extra
        .insert("delete".into(), serde_json::Value::Bool(true));
    write_metadata_file(video_path, &metadata)
}

pub fn protect_videos(video_paths: &[PathBuf], protect: bool) -> StorageResult<()> {
    rewrite_metadata(video_paths, |metadata| metadata.protected = Some(protect))
}

pub fn tag_videos(video_paths: &[PathBuf], tag: &str) -> StorageResult<()> {
    let tag = (!tag.trim().is_empty()).then(|| tag.to_owned());
    rewrite_metadata(video_paths, |metadata| metadata.tag = tag.clone())
}

fn rewrite_metadata(video_paths: &[PathBuf], mutate: impl Fn(&mut Metadata)) -> StorageResult<()> {
    for path in video_paths {
        let mut metadata = get_metadata_for_video(path)?;
        mutate(&mut metadata);
        write_metadata_file(path, &metadata)?;
    }
    Ok(())
}

pub fn delayed_delete_video(video_path: PathBuf) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(2));
        let _ = delete_video(&video_path);
    });
}

/// Calculates video-only usage and applies the configured cap.  Metadata and
/// thumbnails are intentionally excluded, as in the Electron implementation.
pub struct DiskSizeMonitor {
    storage_path: PathBuf,
    max_storage_gb: u64,
}

impl DiskSizeMonitor {
    pub fn new(storage_path: impl Into<PathBuf>, max_storage_gb: u64) -> Self {
        Self {
            storage_path: storage_path.into(),
            max_storage_gb,
        }
    }

    pub fn usage(&self) -> StorageResult<u64> {
        Ok(sorted_video_paths(&self.storage_path, true)?
            .into_iter()
            .map(|(_, metadata)| metadata.len())
            .sum())
    }

    pub fn status(&self) -> StorageResult<DiskStatus> {
        Ok(DiskStatus {
            usage: self.usage()? as f64,
            limit: self.max_storage_gb as f64 * GIB as f64,
        })
    }

    /// Prune oldest unprotected videos once the cap has been exceeded.  It
    /// leaves usage just below the cap (95%), preserving TS behaviour.
    pub fn run(&self) -> StorageResult<DiskStatus> {
        if self.max_storage_gb == 0 {
            return self.status();
        }
        self.prune_to_limit_bytes(self.max_storage_gb.saturating_mul(GIB))?;
        self.status()
    }

    pub fn prune_to_limit_bytes(&self, limit_bytes: u64) -> StorageResult<Vec<PathBuf>> {
        let usage = self.usage()?;
        if usage <= limit_bytes {
            return Ok(Vec::new());
        }
        let target = limit_bytes.saturating_mul(95) / 100;
        let mut remaining = usage;
        let mut removed = Vec::new();
        for (path, file_metadata) in sorted_video_paths(&self.storage_path, false)? {
            if remaining <= target {
                break;
            }
            match get_metadata_for_video(&path) {
                Ok(metadata) if metadata.protected.unwrap_or(false) => continue,
                Ok(_) => {}
                Err(_) => {
                    // A video with a broken sidecar cannot safely appear in the
                    // UI and follows the old monitor's cleanup path.
                }
            }
            let size = file_metadata.len();
            delete_video(&path)?;
            remaining = remaining.saturating_sub(size);
            removed.push(path);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wcr-storage-test-{}-{nonce}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn metadata() -> Metadata {
        serde_json::from_value(serde_json::json!({
            "category": "Manual", "duration": 3.5, "result": true, "flavour": "Retail",
            "tag": "before", "combatants": []
        }))
        .unwrap()
    }

    #[test]
    fn output_name_sanitisation_matches_typescript() {
        assert_eq!(sanitize_output_name("A<>:/|?*  B"), "A B");
        assert_eq!(
            sanitize_output_name(" already   spaced "),
            " already spaced "
        );
    }

    #[test]
    fn metadata_sidecar_round_trip() {
        let dir = temp_dir();
        let video = dir.join("recording.mp4");
        fs::write(&video, []).unwrap();
        let metadata = metadata();
        write_metadata_file(&video, &metadata).unwrap();
        let loaded = get_metadata_for_video(&video).unwrap();
        assert_eq!(loaded.duration, 3.5);
        assert_eq!(loaded.tag.as_deref(), Some("before"));
        assert_eq!(loaded.extra.get("combatants"), Some(&serde_json::json!([])));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn monitor_prunes_oldest_unprotected_first() {
        let dir = temp_dir();
        let oldest = dir.join("oldest.mp4");
        let newest = dir.join("newest.mp4");
        fs::write(&oldest, vec![0; 10]).unwrap();
        write_metadata_file(&oldest, &metadata()).unwrap();
        thread::sleep(Duration::from_millis(20));
        fs::write(&newest, vec![0; 10]).unwrap();
        write_metadata_file(&newest, &metadata()).unwrap();

        let removed = DiskSizeMonitor::new(&dir, 1)
            .prune_to_limit_bytes(15)
            .unwrap();
        assert_eq!(removed, vec![oldest.clone()]);
        assert!(!oldest.exists());
        assert!(newest.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
