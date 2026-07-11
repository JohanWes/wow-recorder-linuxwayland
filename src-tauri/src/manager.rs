use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{Local, SecondsFormat};
use serde_json::{Map, Value};
use tauri::{AppHandle, Manager as _};
use tokio::sync::{mpsc, Mutex};

use crate::{
    config::ConfigState,
    events,
    media_server::MediaServer,
    parser::{self, Parser, ParserEvent, ParserSettings},
    recorder::{Recorder, RecorderParams, RecorderState},
    storage::{self, DiskSizeMonitor, VideoProcessQueue, VideoQueueItem},
    types::{ActivityStatus, Flavour, Metadata, RecStatus, VideoCategory},
};

struct Runtime {
    parser: Option<Parser>,
    events: mpsc::Receiver<ParserEvent>,
    queue: Option<VideoProcessQueue>,
    storage_path: PathBuf,
    max_storage_gb: u64,
    config_valid: bool,
    config_message: String,
    reconfiguring: bool,
    activity_active: bool,
}

pub struct Manager {
    app: AppHandle,
    recorder: Recorder,
    runtime: Mutex<Runtime>,
    reconfigure_lock: Mutex<()>,
    media_server: MediaServer,
}

impl Manager {
    pub fn new(app: AppHandle) -> Arc<Self> {
        let (_tx, rx) = mpsc::channel(32);
        let media_server = MediaServer::start().expect("could not start local media server");
        Arc::new(Self {
            app,
            recorder: Recorder::new(),
            runtime: Mutex::new(Runtime {
                parser: None,
                events: rx,
                queue: None,
                storage_path: PathBuf::new(),
                max_storage_gb: 0,
                config_valid: false,
                config_message: String::new(),
                reconfiguring: false,
                activity_active: false,
            }),
            reconfigure_lock: Mutex::new(()),
            media_server,
        })
    }

    pub fn start(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            if let Err(error) = manager.reconfigure().await {
                eprintln!("initial configuration failed: {error}");
            }
        });
        let manager = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                timer.tick().await;
                manager.poll().await;
            }
        });
    }

    pub async fn reconfigure(&self) -> Result<(), String> {
        let _guard = self.reconfigure_lock.lock().await;
        let old_storage = {
            let mut runtime = self.runtime.lock().await;
            runtime.reconfiguring = true;
            runtime.activity_active = false;
            runtime.parser = None;
            let (_tx, rx) = mpsc::channel(1);
            runtime.events = rx;
            runtime.queue = None;
            std::mem::take(&mut runtime.storage_path)
        };
        self.refresh_status().await;
        self.recorder.shutdown();

        let result = self.configure_from_current().await;
        let new_storage = self.runtime.lock().await.storage_path.clone();
        if !old_storage.as_os_str().is_empty() && old_storage != new_storage {
            self.media_server.clear();
        }
        {
            let mut runtime = self.runtime.lock().await;
            runtime.reconfiguring = false;
            match &result {
                Ok(()) => {
                    runtime.config_valid = true;
                    runtime.config_message.clear();
                }
                Err(error) => {
                    runtime.config_valid = false;
                    runtime.config_message = error.clone();
                }
            }
        }
        self.refresh_status().await;
        self.refresh_disk().await;
        result
    }

    async fn configure_from_current(&self) -> Result<(), String> {
        let value = self.app.state::<ConfigState>().all()?;
        let config = value.as_object().ok_or("config is not an object")?;
        let storage_path = required_dir(config, "storagePath")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let record_retail = boolean(config, "recordRetail", false);
        let retail_log_path = if record_retail {
            Some(required_dir(config, "retailLogPath")?)
        } else {
            None
        };
        let obs_path = if boolean(config, "separateBufferPath", false) {
            required_dir(config, "bufferStoragePath")?
        } else {
            storage_path.join(".temp")
        };
        if obs_path == storage_path {
            return Err("storagePath and buffer path must differ".into());
        }
        let data_dir = self.app.path().app_data_dir().map_err(|e| e.to_string())?;
        let params = RecorderParams {
            obs_path,
            data_dir,
            fps: unsigned(config, "obsFPS", 60) as u32,
            capture_cursor: boolean(config, "captureCursor", false),
            buffer_seconds: unsigned(config, "linuxGsrBufferSeconds", 180) as u32,
            codec: string(config, "linuxGsrCodec", "h264"),
            bitrate_kbps: unsigned(config, "linuxGsrBitrateKbps", 20_000) as u32,
            replay_storage: string(config, "linuxGsrReplayStorage", "ram"),
            lead_in_seconds: number(config, "linuxGsrLeadInSeconds", 0.0),
            audio_output: config
                .get("linuxGsrAudioOutput")
                .and_then(Value::as_str)
                .map(str::to_owned),
            audio_input: config
                .get("linuxGsrAudioInput")
                .and_then(Value::as_str)
                .map(str::to_owned),
            legacy_audio: config
                .get("linuxGsrAudio")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        self.recorder.configure_base(params).await?;

        let (tx, rx) = mpsc::channel(32);
        let mut parser = Parser::new(parser_settings(config), tx);
        if let Some(log_path) = retail_log_path {
            parser
                .watch(log_path)
                .map_err(|e| format!("could not watch retail log path: {e}"))?;
        }

        let max_storage_gb = unsigned(config, "maxStorage", 50);
        let app = self.app.clone();
        let completion_storage = storage_path.clone();
        let (queue, worker) =
            VideoProcessQueue::new(&storage_path, max_storage_gb, move |completion| {
                if let Some(error) = completion.error {
                    events::update_error_report(&app, Local::now().to_rfc3339(), error);
                }
                events::update_disk_status(&app, completion.disk_status);
                match storage::list_videos(&completion_storage) {
                    Ok(videos) => events::set_disk_videos(&app, videos),
                    Err(error) => eprintln!("failed to refresh videos: {error}"),
                }
            });
        tauri::async_runtime::spawn_blocking(move || tauri::async_runtime::block_on(worker.run()));
        {
            let mut runtime = self.runtime.lock().await;
            runtime.parser = Some(parser);
            runtime.events = rx;
            runtime.queue = Some(queue);
            runtime.storage_path = storage_path;
            runtime.max_storage_gb = max_storage_gb;
        }
        if record_retail {
            self.recorder.start_buffer().await?;
        }
        Ok(())
    }

    async fn poll(&self) {
        let mut pending = Vec::new();
        {
            let mut runtime = self.runtime.lock().await;
            if let Some(parser) = runtime.parser.as_mut() {
                parser.poll_watch();
            }
            while let Ok(event) = runtime.events.try_recv() {
                pending.push(event);
            }
        }
        for event in pending {
            if let Err(error) = self.handle_event(event).await {
                self.report_error(error);
            }
        }
    }

    async fn handle_event(&self, event: ParserEvent) -> Result<(), String> {
        match event {
            ParserEvent::ActivityStarted {
                start_date,
                category,
                offset_hint,
            } => {
                self.recorder.start_recording(offset_hint).await?;
                self.runtime.lock().await.activity_active = true;
                events::update_activity_status(
                    &self.app,
                    Some(ActivityStatus {
                        category: parse_category(&category)?,
                        start: start_date.timestamp_millis(),
                    }),
                );
                self.refresh_status().await;
            }
            ParserEvent::ActivityEnded {
                metadata,
                activity_start,
                activity_end,
                overrun_seconds,
                video_name,
            } => {
                self.recorder.stop().await?;
                let source = self
                    .recorder
                    .get_and_clear_last_file()
                    .ok_or("recorder did not produce a combined file")?;
                let metadata = convert_metadata(metadata)?;
                self.enqueue(VideoQueueItem {
                    name: video_name,
                    source,
                    suffix: String::new(),
                    offset: 0.0,
                    duration: (activity_end - activity_start).num_milliseconds() as f64 / 1000.0
                        + overrun_seconds,
                    clip: false,
                    metadata,
                })
                .await?;
                self.runtime.lock().await.activity_active = false;
                events::update_activity_status(&self.app, None);
                self.refresh_status().await;
            }
            ParserEvent::ForceEnd => {
                self.recorder.stop().await?;
                if let Some(path) = self.recorder.get_and_clear_last_file() {
                    let _ = std::fs::remove_file(path);
                }
                self.runtime.lock().await.activity_active = false;
                events::update_activity_status(&self.app, None);
                self.refresh_status().await;
            }
        }
        Ok(())
    }

    pub async fn enqueue(&self, item: VideoQueueItem) -> Result<(), String> {
        self.runtime
            .lock()
            .await
            .queue
            .as_ref()
            .ok_or("video queue is not configured")?
            .enqueue(item)
            .map_err(|_| "video queue is closed".to_owned())
    }
    pub async fn videos(&self) -> Result<Vec<crate::types::RendererVideo>, String> {
        let path = self.runtime.lock().await.storage_path.clone();
        storage::list_videos(&path).map_err(|e| e.to_string())
    }
    pub async fn refresh_disk(&self) {
        let runtime = self.runtime.lock().await;
        if runtime.storage_path.as_os_str().is_empty() {
            return;
        }
        if let Ok(videos) = storage::list_videos(&runtime.storage_path) {
            events::set_disk_videos(&self.app, videos);
        }
        if let Ok(status) =
            DiskSizeMonitor::new(&runtime.storage_path, runtime.max_storage_gb).status()
        {
            events::update_disk_status(&self.app, status);
        }
    }
    pub fn recorder(&self) -> Recorder {
        self.recorder.clone()
    }
    pub async fn with_parser(&self, f: impl FnOnce(&mut Parser)) -> Result<(), String> {
        let mut runtime = self.runtime.lock().await;
        let parser = runtime.parser.as_mut().ok_or("parser is not configured")?;
        f(parser);
        Ok(())
    }
    pub async fn force_stop(&self) -> Result<(), String> {
        {
            let mut runtime = self.runtime.lock().await;
            let Some(parser) = runtime.parser.as_mut() else {
                return Err("parser is not configured".into());
            };
            parser.drop_activity();
            if !runtime.activity_active {
                return Ok(());
            }
            runtime.activity_active = false;
        }
        self.recorder.stop().await?;
        if let Some(path) = self.recorder.get_and_clear_last_file() {
            let _ = std::fs::remove_file(path);
        }
        self.refresh_status().await;
        events::update_activity_status(&self.app, None);
        Ok(())
    }

    pub async fn validate_video_path(&self, path: String) -> Result<PathBuf, String> {
        let root = self.runtime.lock().await.storage_path.clone();
        let path = PathBuf::from(path)
            .canonicalize()
            .map_err(|e| format!("invalid video path: {e}"))?;
        if path.extension().and_then(|v| v.to_str()) != Some("mp4")
            || path.parent() != Some(root.as_path())
        {
            return Err("video path is outside the configured storagePath".into());
        }
        Ok(path)
    }
    pub async fn video_url(&self, path: String) -> Result<String, String> {
        let path = self.validate_video_path(path).await?;
        self.media_server
            .register(path)
            .map_err(|error| format!("could not register video for playback: {error}"))
    }
    pub async fn refresh_status(&self) {
        let runtime = self.runtime.lock().await;
        let (status, msg) = if runtime.reconfiguring {
            (RecStatus::Reconfiguring, None)
        } else if !runtime.config_valid {
            (
                RecStatus::InvalidConfig,
                Some(runtime.config_message.clone()),
            )
        } else if runtime.activity_active {
            (RecStatus::Recording, None)
        } else if self.recorder.state() == RecorderState::Recording {
            (RecStatus::ReadyToRecord, None)
        } else {
            (RecStatus::WaitingForWoW, None)
        };
        events::update_rec_status(&self.app, status, msg);
    }
    pub fn report_error(&self, error: String) {
        events::update_error_report(
            &self.app,
            Local::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            error,
        );
    }
}

fn required_dir(config: &Map<String, Value>, key: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(string(config, key, ""));
    if path.as_os_str().is_empty() || !path.is_dir() {
        Err(format!("{key} is not a valid directory"))
    } else {
        Ok(path)
    }
}
fn string(c: &Map<String, Value>, k: &str, d: &str) -> String {
    c.get(k).and_then(Value::as_str).unwrap_or(d).to_owned()
}
fn boolean(c: &Map<String, Value>, k: &str, d: bool) -> bool {
    c.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn unsigned(c: &Map<String, Value>, k: &str, d: u64) -> u64 {
    c.get(k).and_then(Value::as_u64).unwrap_or(d)
}
fn number(c: &Map<String, Value>, k: &str, d: f64) -> f64 {
    c.get(k).and_then(Value::as_f64).unwrap_or(d)
}
fn parser_settings(c: &Map<String, Value>) -> ParserSettings {
    ParserSettings {
        record_raids: boolean(c, "recordRaids", true),
        record_dungeons: boolean(c, "recordDungeons", true),
        record_2v2: boolean(c, "recordTwoVTwo", true),
        record_3v3: boolean(c, "recordThreeVThree", true),
        record_skirmish: boolean(c, "recordSkirmish", true),
        record_solo_shuffle: boolean(c, "recordSoloShuffle", true),
        record_battlegrounds: boolean(c, "recordBattlegrounds", true),
        min_encounter_duration: number(c, "minEncounterDuration", 15.0),
        min_keystone_level: c
            .get("minKeystoneLevel")
            .and_then(Value::as_i64)
            .unwrap_or(2),
        min_raid_difficulty: string(c, "minRaidDifficulty", "LFR").to_lowercase(),
        raid_overrun: number(c, "raidOverrun", 15.0),
        dungeon_overrun: number(c, "dungeonOverrun", 5.0),
        record_current_raid_encounters_only: boolean(c, "recordCurrentRaidEncountersOnly", false),
        inactivity_minutes: 10,
    }
}
fn convert_metadata(value: parser::Metadata) -> Result<Metadata, String> {
    serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn parse_category(value: &str) -> Result<VideoCategory, String> {
    serde_json::from_value(Value::String(value.to_owned())).map_err(|e| e.to_string())
}

pub fn clip_metadata(mut metadata: Metadata) -> Metadata {
    let old = metadata.category;
    metadata.parent_category.get_or_insert(old);
    metadata.category = VideoCategory::Clips;
    metadata.protected = Some(true);
    metadata.clipped_at = Some(Local::now().timestamp_millis());
    metadata
}
pub fn replay_metadata(duration: f64) -> Metadata {
    Metadata {
        category: VideoCategory::Clips,
        parent_category: Some(VideoCategory::Manual),
        duration,
        start: None,
        clipped_at: Some(Local::now().timestamp_millis()),
        result: true,
        flavour: Flavour::Retail,
        zone_id: None,
        encounter_id: None,
        map_id: None,
        zone_name: None,
        encounter_name: None,
        difficulty: None,
        protected: Some(true),
        tag: None,
        unique_hash: None,
        extra: Map::new(),
    }
}
pub fn source_name(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
