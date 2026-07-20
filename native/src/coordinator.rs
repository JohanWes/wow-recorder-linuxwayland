// SPDX-License-Identifier: GPL-3.0-or-later

//! Coordinator-owned application state and commands.
//!
//! One thread owns `Config`, the log tailers, the activity engine, the
//! `Recorder`, the library index, and the in-flight recording draft. The GTK
//! thread holds a `CoordinatorHandle`: one bounded command sender, one
//! capacity-one snapshot receiver, and one capacity-one stopped receiver. A
//! second thread runs the serial `MediaWorker`; the coordinator owns its join
//! handle and dispatches at most one job at a time so automatic finalization
//! always precedes queued user transcodes.
//!
//! Behavior notes recorded against WR-000 and the legacy TypeScript:
//! - Advanced-combat-logging status is read from `<log dir>/../WTF/Config.wtf`
//!   when the tailers are (re)opened. The legacy per-file watcher is not
//!   rebuilt; the status refreshes on arm/save.
//! - Test recordings synthesize the minimum parsed events for the chosen
//!   category rather than replaying the legacy `testButtonData` log dumps,
//!   which existed only to populate the legacy UI.
//! - The legacy Ctrl+Alt "test without an end line" variant is not rebuilt;
//!   `ForceEnd` already covers stopping a running test.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::activity::{ActivityAction, ActivityEngine, RecordingDraft};
use crate::config::{Config, ConfigError, LoadedConfig, ValidationProblem, load_or_import};
use crate::domain::{
    ActivityDetails, Category, CorrelatedActivity, DeathMarkerVisibility, GameFlavor, LibraryEntry,
    MarkerVisibility, MediaFacts, Outcome, Problem, RecorderStatus, RecordingId, RecoveryAction,
    StorageLimit, WorkKind, WorkProgress,
};
use crate::logwatch::LogTailer;
use crate::media_jobs::{
    KillAudio, KillSegment, MediaConfig, MediaControl, MediaEvent, MediaJob, MediaWorker,
};
use crate::parser::{CombatEvent, ParseTimeContext, ParsedEvent, PlayerObservationKind};
use crate::recorder::{
    CaptureConfig, Recorder, RecorderError, RecorderEvent, RecordingMode, StartRequest, Timeouts,
};
use crate::storage::{EntryUpdate, LibraryIndex, Storage, now_unix_ms};

/// Force-end an automatic recording after this long without new log data.
const RETAIL_DATA_TIMEOUT_MS: i64 = 10_000;
const CLASSIC_DATA_TIMEOUT_MS: i64 = 2_000;
/// Commands handled per tick before the loop returns to polling.
const COMMAND_BATCH: usize = 16;
/// Bounded problem list surfaced in the snapshot.
const MAX_PROBLEMS: usize = 8;

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

/// Ranges are milliseconds into the source recording's media.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipRange {
    pub source: RecordingId,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Arm,
    Disarm,
    ForceEnd,
    StartManual,
    StopManual,
    RunTest {
        category: Category,
    },
    ReselectCaptureTarget,
    SaveConfig {
        draft: Box<Config>,
    },
    SetProtected {
        ids: Vec<RecordingId>,
        value: bool,
    },
    SetTag {
        id: RecordingId,
        tag: String,
    },
    Delete {
        ids: Vec<RecordingId>,
    },
    CreateClip(ClipRange),
    CreateKillVideo {
        segments: Vec<ClipRange>,
        width: u32,
        height: u32,
        fps: u32,
        audio: KillAudio,
    },
    SetSelectedCategory {
        category: Category,
    },
    SetMarkerVisibility {
        deaths: DeathMarkerVisibility,
        encounters: MarkerVisibility,
        rounds: MarkerVisibility,
    },
    Shutdown,
}

/// The in-flight recording, as the UI needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveRecordingView {
    pub id: RecordingId,
    pub category: Category,
    pub title: String,
    pub mode: RecordingMode,
    /// Wall-clock anchor for the elapsed-time display.
    pub started_unix_ms: i64,
    pub requested_replay_ms: u64,
    /// Set once the activity ended and only the overrun is left to record.
    pub overrun_until_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppSnapshot {
    pub entries: Arc<[LibraryEntry]>,
    pub correlations: Arc<[CorrelatedActivity]>,
    pub category_counts: Vec<(Category, usize)>,
    pub status: RecorderStatus,
    pub active: Option<ActiveRecordingView>,
    pub config: Config,
    pub setup_problems: Vec<ValidationProblem>,
    /// One entry per enabled flavour: its config field name and whether
    /// advanced combat logging is on.
    pub advanced_logging: Vec<(&'static str, bool)>,
    pub problems: Vec<Problem>,
    pub work: Option<WorkProgress>,
    pub queued_jobs: usize,
    pub storage_used_bytes: u64,
    pub storage_limit: StorageLimit,
    pub protected_over_limit: bool,
}

/// GTK-side handle. Dropping it closes the command channel, which stops the
/// coordinator on its next tick.
pub struct CoordinatorHandle {
    commands: SyncSender<Command>,
    pub snapshots: Receiver<Arc<AppSnapshot>>,
    pub stopped: Receiver<()>,
    join: Option<JoinHandle<()>>,
}

impl CoordinatorHandle {
    /// Returns `false` when the queue is full or the coordinator is gone; the
    /// caller shows one Busy problem rather than blocking the GTK thread.
    pub fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }

    /// Request shutdown and join the coordinator thread.
    pub fn shutdown(mut self) {
        let _ = self.commands.try_send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Everything the coordinator needs that is not user configuration.
#[derive(Clone, Debug)]
pub struct Setup {
    pub config_path: PathBuf,
    pub legacy_config_path: PathBuf,
    /// App-private directory for the recorder's token/hook/events files.
    pub data_dir: PathBuf,
    pub gsr_binary: PathBuf,
    pub media: MediaConfig,
    /// Year used to expand the combat log's month/day timestamps.
    pub year: i32,
    pub recorder_timeouts: Timeouts,
    /// Idle pacing for one coordinator tick.
    pub poll_interval: Duration,
    /// Test-recording length; raids run four times as long, exactly like the
    /// legacy 5 s / 20 s pair.
    pub test_duration: Duration,
}

impl Setup {
    pub fn from_environment() -> Result<Self, ConfigError> {
        Ok(Self {
            config_path: crate::config::config_path_from_environment()?,
            legacy_config_path: crate::config::legacy_config_path_from_environment()?,
            data_dir: crate::config::config_path_from_environment()?
                .parent()
                .unwrap_or(Path::new("."))
                .join("recorder"),
            gsr_binary: PathBuf::from("gpu-screen-recorder"),
            media: MediaConfig::default(),
            year: local_year(),
            recorder_timeouts: Timeouts::default(),
            poll_interval: Duration::from_millis(50),
            test_duration: Duration::from_secs(5),
        })
    }
}

/// Start the coordinator and its media worker.
pub fn start(setup: Setup) -> CoordinatorHandle {
    let (commands_tx, commands_rx) = mpsc::sync_channel(64);
    let (snapshot_tx, snapshots) = mpsc::sync_channel(1);
    let (stopped_tx, stopped) = mpsc::sync_channel(1);
    let join = std::thread::Builder::new()
        .name("coordinator".to_owned())
        .spawn(move || {
            let mut coordinator = Coordinator::new(setup, commands_rx, snapshot_tx);
            coordinator.startup();
            while coordinator.tick() {}
            let _ = stopped_tx.try_send(());
        })
        .expect("spawn coordinator thread");
    CoordinatorHandle {
        commands: commands_tx,
        snapshots,
        stopped,
        join: Some(join),
    }
}

// ---------------------------------------------------------------------------
// Coordinator state
// ---------------------------------------------------------------------------

/// The recording the coordinator is currently driving.
struct ActiveRecording {
    draft: RecordingDraft,
    mode: RecordingMode,
    started_unix_ms: i64,
    requested_replay_ms: u64,
    /// Wall-clock time the capture stops; set when the activity ended and the
    /// configured overrun is running out.
    stop_at_ms: Option<i64>,
}

pub struct Coordinator {
    setup: Setup,
    config: Config,
    engine: ActivityEngine,
    recorder: Recorder,
    armed: bool,
    storage: Storage,
    tailers: Vec<LogTailer>,
    /// Per flavour: wall-clock and log time of the newest observed event.
    last_event: HashMap<GameFlavor, (i64, i64)>,
    index: LibraryIndex,
    active: Option<ActiveRecording>,
    /// Injected test end event, released once its wall-clock deadline passes.
    pending_test_end: Option<(i64, ParsedEvent)>,

    media_jobs: Sender<MediaJob>,
    media_control: SyncSender<MediaControl>,
    media_events: Receiver<MediaEvent>,
    media_join: Option<JoinHandle<()>>,
    finalize_queue: VecDeque<MediaJob>,
    user_queue: VecDeque<MediaJob>,
    media_busy: Option<WorkKind>,
    work: Option<WorkProgress>,

    problems: Vec<Problem>,
    setup_problems: Vec<ValidationProblem>,
    advanced_logging: Vec<(&'static str, bool)>,
    storage_used_bytes: u64,
    protected_over_limit: bool,

    commands: Receiver<Command>,
    snapshot_tx: SyncSender<Arc<AppSnapshot>>,
    pending_snapshot: Option<Arc<AppSnapshot>>,
    dirty: bool,
    stopping: bool,
}

impl Coordinator {
    pub fn new(
        setup: Setup,
        commands: Receiver<Command>,
        snapshot_tx: SyncSender<Arc<AppSnapshot>>,
    ) -> Self {
        let loaded = load_or_import(&setup.config_path, &setup.legacy_config_path);
        let (config, mut problems) = match loaded {
            Ok(LoadedConfig {
                config,
                import_warnings,
                ..
            }) => (
                config,
                import_warnings
                    .into_iter()
                    .map(|warning| {
                        make_problem(
                            "Some legacy settings could not be imported.",
                            Some(format!("{}: {}", warning.key, warning.message)),
                            Some(RecoveryAction::OpenSettings),
                        )
                    })
                    .collect(),
            ),
            Err(error) => (
                Config::default(),
                vec![make_problem(
                    "Settings could not be loaded; defaults are in use.",
                    Some(error.to_string()),
                    Some(RecoveryAction::OpenSettings),
                )],
            ),
        };
        problems.truncate(MAX_PROBLEMS);

        let storage = build_storage(&config);
        let (media_jobs, jobs_rx) = mpsc::channel();
        let (media_control, control_rx) = mpsc::sync_channel(1);
        let (events_tx, media_events) = mpsc::channel();
        let worker = MediaWorker::new(
            setup.media.clone(),
            build_storage(&config),
            jobs_rx,
            control_rx,
            events_tx,
        );
        let media_join = std::thread::Builder::new()
            .name("media".to_owned())
            .spawn(move || worker.run())
            .expect("spawn media worker thread");

        Self {
            setup,
            config,
            engine: ActivityEngine::new(),
            recorder: Recorder::new(),
            armed: false,
            storage,
            tailers: Vec::new(),
            last_event: HashMap::new(),
            index: LibraryIndex::default(),
            active: None,
            pending_test_end: None,
            media_jobs,
            media_control,
            media_events,
            media_join: Some(media_join),
            finalize_queue: VecDeque::new(),
            user_queue: VecDeque::new(),
            media_busy: None,
            work: None,
            problems,
            setup_problems: Vec::new(),
            advanced_logging: Vec::new(),
            storage_used_bytes: 0,
            protected_over_limit: false,
            commands,
            snapshot_tx,
            pending_snapshot: None,
            dirty: true,
            stopping: false,
        }
    }

    /// Sweep interrupted artifacts, scan the library, validate, and arm.
    pub fn startup(&mut self) {
        self.recorder = Recorder::with_timeouts(self.setup.recorder_timeouts);
        let _ = self.storage.prepare();
        let report = self.storage.sweep_orphans();
        if !report.failures.is_empty() {
            self.push_problem(
                "Some interrupted recordings could not be moved to Recovery.",
                Some(report.failures.join("; ")),
                Some(RecoveryAction::OpenLogs),
            );
        }
        self.rescan();
        self.setup_problems = self.config.validate();
        self.open_tailers();
        if self.setup_problems.is_empty() {
            self.arm();
        }
        self.enforce_limit();
        self.dirty = true;
    }

    /// One iteration of the coordinator loop. Returns `false` once stopped.
    pub fn tick(&mut self) -> bool {
        if self.drain_commands() {
            self.shutdown();
            return false;
        }
        self.poll_recorder();
        self.poll_logs();
        self.poll_media();
        self.check_deadlines();
        self.dispatch_media();
        self.publish();
        true
    }

    /// Blocking-with-timeout command drain; the timeout is the idle pacing.
    /// Returns `true` when shutdown was requested.
    fn drain_commands(&mut self) -> bool {
        let first = match self.commands.recv_timeout(self.setup.poll_interval) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            // The GTK side is gone: stop exactly like an explicit Shutdown.
            Err(RecvTimeoutError::Disconnected) => return true,
        };
        let mut batch: Vec<Command> = first.into_iter().collect();
        while batch.len() < COMMAND_BATCH {
            match self.commands.try_recv() {
                Ok(command) => batch.push(command),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return true,
            }
        }
        for command in batch {
            if command == Command::Shutdown {
                return true;
            }
            self.handle_command(command);
        }
        false
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    fn handle_command(&mut self, command: Command) {
        self.dirty = true;
        match command {
            Command::Arm => {
                self.setup_problems = self.config.validate();
                if self.setup_problems.is_empty() {
                    self.arm();
                }
            }
            Command::Disarm => self.disarm(),
            Command::ForceEnd => self.force_end(),
            Command::StartManual => self.start_manual(),
            Command::StopManual => self.stop_manual(),
            Command::RunTest { category } => self.run_test(&category),
            Command::ReselectCaptureTarget => self.reselect_target(),
            Command::SaveConfig { draft } => self.save_config(*draft),
            Command::SetProtected { ids, value } => {
                self.update_entries(&ids, &EntryUpdate::Protected(value));
            }
            Command::SetTag { id, tag } => {
                self.update_entries(std::slice::from_ref(&id), &EntryUpdate::Tag(tag));
            }
            Command::Delete { ids } => self.delete_entries(&ids),
            Command::CreateClip(range) => self.queue_clip(&range),
            Command::CreateKillVideo {
                segments,
                width,
                height,
                fps,
                audio,
            } => self.queue_kill_video(&segments, width, height, fps, audio),
            Command::SetSelectedCategory { category } => {
                let mut draft = self.config.clone();
                draft.interface.selected_category = category;
                self.patch_config(draft);
            }
            Command::SetMarkerVisibility {
                deaths,
                encounters,
                rounds,
            } => {
                let mut draft = self.config.clone();
                draft.interface.death_markers = deaths;
                draft.interface.encounter_markers = encounters;
                draft.interface.round_markers = rounds;
                self.patch_config(draft);
            }
            Command::Shutdown => {}
        }
    }

    // -----------------------------------------------------------------------
    // Recorder lifecycle
    // -----------------------------------------------------------------------

    fn capture_config(&self) -> CaptureConfig {
        CaptureConfig {
            gsr_binary: self.setup.gsr_binary.clone(),
            data_dir: self.setup.data_dir.clone(),
            capture_root: capture_root(&self.config),
            settings: self.config.capture.clone(),
        }
    }

    fn arm(&mut self) {
        let config = self.capture_config();
        match self.recorder.arm(&config) {
            Ok(()) => {
                self.armed = true;
            }
            Err(error) => {
                self.armed = false;
                self.push_recorder_problem(&error);
            }
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
        if let Err(error) = self.recorder.shutdown() {
            self.push_recorder_problem(&error);
        }
    }

    fn reselect_target(&mut self) {
        let config = self.capture_config();
        match self.recorder.reselect_target(&config) {
            Ok(selection) => {
                self.armed = true;
                if let Some(token) = selection.token {
                    self.store_token(token);
                }
            }
            Err(error) => {
                self.armed = false;
                self.push_recorder_problem(&error);
            }
        }
    }

    fn store_token(&mut self, token: String) {
        if self.config.capture.capture_target_token.as_deref() == Some(token.as_str()) {
            return;
        }
        self.config.capture.capture_target_token = Some(token);
        if let Err(error) = self.config.save(&self.setup.config_path) {
            self.push_problem(
                "The capture target could not be saved.",
                Some(error.to_string()),
                Some(RecoveryAction::ReselectCaptureTarget),
            );
        }
    }

    fn poll_recorder(&mut self) {
        for event in self.recorder.poll(now_unix_ms()) {
            match event {
                RecorderEvent::TargetTokenAvailable(token) => self.store_token(token),
                RecorderEvent::RestartFailed { message } => self.push_problem(
                    "Screen capture could not be restarted.",
                    Some(message),
                    Some(RecoveryAction::ReselectCaptureTarget),
                ),
                RecorderEvent::ChildExited { code } => {
                    tracing::warn!(?code, "capture child exited");
                }
                RecorderEvent::Restarted => {
                    tracing::info!("capture restarted");
                    self.dirty = true;
                }
                RecorderEvent::RestartScheduled { attempt, .. } => {
                    tracing::info!(attempt, "capture restart scheduled");
                }
                RecorderEvent::Diagnostic(message) => tracing::debug!(%message, "recorder"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Log polling and the activity engine
    // -----------------------------------------------------------------------

    fn open_tailers(&mut self) {
        self.tailers.clear();
        self.advanced_logging.clear();
        self.last_event.clear();
        let context = ParseTimeContext::new(self.setup.year, self.setup.media.utc_offset_minutes);
        for (field, flavor, source) in enabled_log_sources(&self.config) {
            self.advanced_logging
                .push((field, advanced_logging_enabled(&source)));
            match LogTailer::open(source.clone(), flavor, context) {
                Ok(tailer) => self.tailers.push(tailer),
                Err(error) => self.push_problem(
                    format!("The {field} log folder could not be watched."),
                    Some(error.to_string()),
                    Some(RecoveryAction::OpenSettings),
                ),
            }
        }
    }

    fn poll_logs(&mut self) {
        let mut events = Vec::new();
        for tailer in &mut self.tailers {
            match tailer.poll() {
                Ok(polled) => events.extend(polled),
                Err(error) => tracing::warn!(%error, "log poll failed"),
            }
            for diagnostic in tailer.take_diagnostics() {
                tracing::debug!(?diagnostic, "log diagnostic");
            }
        }
        for event in events {
            self.feed(event);
        }
    }

    /// The single entry point for parsed events, shared by live logs and test
    /// recordings.
    fn feed(&mut self, event: ParsedEvent) {
        self.last_event
            .insert(event.flavor.clone(), (now_unix_ms(), event.occurred_at_ms));
        let actions = self.engine.handle(event, &self.config.activities);
        for action in actions {
            self.apply(action);
        }
    }

    fn apply(&mut self, action: ActivityAction) {
        self.dirty = true;
        match action {
            ActivityAction::Begin {
                draft,
                detected_at_ms,
            } => self.begin(*draft, detected_at_ms),
            ActivityAction::Update { id, item } => {
                if let Some(active) = self.active.as_mut()
                    && active.draft.id == id
                {
                    active.draft.timeline.push(item);
                }
            }
            ActivityAction::Complete { id, .. } | ActivityAction::Abandon { id, .. } => {
                let Some(draft) = self.engine.take_finished(&id) else {
                    return;
                };
                let Some(active) = self.active.as_mut() else {
                    return;
                };
                if active.draft.id != id {
                    return;
                }
                let overrun_ms = draft.overrun_ms as i64;
                active.draft = draft;
                active.stop_at_ms = Some(now_unix_ms() + overrun_ms);
            }
            ActivityAction::Discard { id, reason } => {
                let _ = self.engine.take_finished(&id);
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.draft.id == id)
                {
                    tracing::info!(?reason, "discarding recording");
                    self.active = None;
                    self.cancel_capture(&id);
                }
            }
        }
    }

    fn begin(&mut self, draft: RecordingDraft, detected_at_ms: i64) {
        if self.active.is_some() {
            self.drop_activity(&draft.flavor);
            return;
        }
        let capacity_ms = u64::from(self.config.capture.replay_buffer_seconds) * 1_000;
        let lead_in_ms = i64::from(self.config.capture.extra_lead_in_seconds) * 1_000;
        let requested_replay_ms =
            (detected_at_ms - draft.started_at_ms + lead_in_ms).clamp(0, capacity_ms as i64) as u64;
        self.start_capture(draft, requested_replay_ms, RecordingMode::Automatic);
    }

    /// Start the capture for a draft, dropping the activity when the recorder
    /// refuses. There is no on-disk pending state to clean up.
    fn start_capture(
        &mut self,
        draft: RecordingDraft,
        requested_replay_ms: u64,
        mode: RecordingMode,
    ) {
        let request = StartRequest {
            id: draft.id.clone(),
            requested_replay_ms,
            mode: mode.clone(),
        };
        match self.recorder.begin(request) {
            Ok(started) => {
                self.active = Some(ActiveRecording {
                    draft,
                    mode,
                    started_unix_ms: started.regular_started_at_ms,
                    requested_replay_ms,
                    stop_at_ms: None,
                });
            }
            Err(error) => {
                self.push_recorder_problem(&error);
                self.drop_activity(&draft.flavor);
            }
        }
    }

    /// Clear the engine's activity for a flavour without recording anything.
    fn drop_activity(&mut self, flavor: &GameFlavor) {
        for action in self.engine.force_end(flavor.clone(), now_unix_ms()) {
            if let ActivityAction::Complete { id, .. }
            | ActivityAction::Abandon { id, .. }
            | ActivityAction::Discard { id, .. } = action
            {
                let _ = self.engine.take_finished(&id);
            }
        }
    }

    fn force_end(&mut self) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        match active.mode {
            RecordingMode::Manual => {
                self.stop_manual();
                return;
            }
            RecordingMode::Automatic | RecordingMode::Test(_) => {}
        }
        let flavor = active.draft.flavor.clone();
        let occurred_at_ms = self
            .last_event
            .get(&flavor)
            .map_or_else(now_unix_ms, |(_, log_ms)| *log_ms);
        self.pending_test_end = None;
        for action in self.engine.force_end(flavor, occurred_at_ms) {
            self.apply(action);
        }
    }

    /// Retail 10 s, classic/era 2 s without new log data ends at last-data time.
    fn check_data_timeout(&mut self, now_ms: i64) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        if active.mode != RecordingMode::Automatic || active.stop_at_ms.is_some() {
            return;
        }
        let flavor = active.draft.flavor.clone();
        let limit = if flavor == GameFlavor::Retail {
            RETAIL_DATA_TIMEOUT_MS
        } else {
            CLASSIC_DATA_TIMEOUT_MS
        };
        let Some((seen_wall_ms, seen_log_ms)) = self.last_event.get(&flavor).copied() else {
            return;
        };
        if now_ms - seen_wall_ms < limit {
            return;
        }
        for action in self.engine.force_end(flavor, seen_log_ms) {
            self.apply(action);
        }
    }

    // -----------------------------------------------------------------------
    // Manual and test recordings
    // -----------------------------------------------------------------------

    fn start_manual(&mut self) {
        if self.active.is_some() || !self.armed {
            self.push_problem(
                "A manual recording could not be started.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        let started_at_ms = now_unix_ms();
        let draft = RecordingDraft {
            id: RecordingId::new(),
            category: Category::Manual,
            flavor: GameFlavor::Retail,
            started_at_ms,
            overrun_ms: 0,
            details: ActivityDetails::Manual,
            player: None,
            combatants: Vec::new(),
            timeline: Vec::new(),
            outcome: None,
            ended_at_ms: None,
            duration_ms: None,
            title: Some("Manual recording".to_owned()),
            activity_hash: None,
        };
        self.start_capture(draft, 0, RecordingMode::Manual);
    }

    fn stop_manual(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.mode != RecordingMode::Manual {
            return;
        }
        let ended_at_ms = now_unix_ms();
        active.draft.outcome = Some(Outcome::Unknown);
        active.draft.ended_at_ms = Some(ended_at_ms);
        active.draft.duration_ms = Some((ended_at_ms - active.draft.started_at_ms).max(0) as u64);
        active.stop_at_ms = Some(ended_at_ms);
    }

    /// Inject the minimum events for the chosen category, then release the end
    /// event once the test duration has elapsed.
    fn run_test(&mut self, category: &Category) {
        if self.active.is_some() || self.pending_test_end.is_some() || !self.armed {
            self.push_problem(
                "A test recording could not be started.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        let duration = if *category == Category::Raids {
            self.setup.test_duration * 4
        } else {
            self.setup.test_duration
        };
        let start_ms = now_unix_ms();
        let end_ms = start_ms + duration.as_millis() as i64;
        let Some((start_events, end_event)) = test_events(category, start_ms, end_ms) else {
            self.push_problem(
                "That category has no test recording.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        };
        for event in start_events {
            self.feed(event);
        }
        if self.active.is_some() {
            self.pending_test_end = Some((end_ms, end_event));
        }
    }

    // -----------------------------------------------------------------------
    // Ending a capture
    // -----------------------------------------------------------------------

    fn check_deadlines(&mut self) {
        let now_ms = now_unix_ms();
        if let Some((due_ms, _)) = &self.pending_test_end
            && *due_ms <= now_ms
        {
            let (_, event) = self.pending_test_end.take().expect("checked above");
            self.feed(event);
        }
        self.check_data_timeout(now_ms);
        if self
            .active
            .as_ref()
            .and_then(|active| active.stop_at_ms)
            .is_some_and(|stop_at_ms| stop_at_ms <= now_ms)
        {
            self.end_capture();
        }
    }

    fn end_capture(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        self.dirty = true;
        match self.recorder.end(&active.draft.id) {
            Ok(artifacts) => {
                self.finalize_queue.push_back(MediaJob::FinalizeRecording {
                    draft: Box::new(active.draft),
                    artifacts,
                    facts: self.media_facts(),
                });
            }
            Err(error) => {
                self.push_recorder_problem(&error);
                let report = self.storage.sweep_orphans();
                tracing::info!(quarantined = report.quarantined.len(), "capture end failed");
            }
        }
    }

    /// Stop capture without producing an entry and quarantine what GSR wrote.
    fn cancel_capture(&mut self, id: &RecordingId) {
        match self.recorder.cancel(id) {
            Ok(_) => {
                let report = self.storage.sweep_orphans();
                if !report.failures.is_empty() {
                    tracing::warn!(failures = ?report.failures, "discard sweep failed");
                }
            }
            Err(error) => self.push_recorder_problem(&error),
        }
    }

    fn media_facts(&self) -> MediaFacts {
        MediaFacts {
            fps: Some(self.config.capture.fps),
            width: None,
            height: None,
            codec: Some(self.config.capture.codec),
        }
    }

    // -----------------------------------------------------------------------
    // Media jobs
    // -----------------------------------------------------------------------

    /// Finalization is always chosen before queued user work, and only one job
    /// is in flight at a time.
    fn dispatch_media(&mut self) {
        if self.media_busy.is_some() {
            return;
        }
        let Some(job) = self
            .finalize_queue
            .pop_front()
            .or_else(|| self.user_queue.pop_front())
        else {
            return;
        };
        self.media_busy = Some(job.kind());
        self.dirty = true;
        if self.media_jobs.send(job).is_err() {
            self.media_busy = None;
            self.push_problem(
                "The media worker is unavailable.",
                None,
                Some(RecoveryAction::Quit),
            );
        }
    }

    fn poll_media(&mut self) {
        while let Ok(event) = self.media_events.try_recv() {
            self.dirty = true;
            match event {
                MediaEvent::Progress(progress) => self.work = Some(progress),
                MediaEvent::Completed { entry, .. } => {
                    self.media_busy = None;
                    self.work = None;
                    self.insert_entry(*entry);
                    self.enforce_limit();
                }
                MediaEvent::Failed { kind, message } => {
                    self.media_busy = None;
                    self.work = None;
                    self.push_problem(
                        match kind {
                            WorkKind::Finalize => "A recording could not be saved.",
                            WorkKind::Clip => "The clip could not be created.",
                            WorkKind::KillVideo => "The kill video could not be rendered.",
                        },
                        Some(message),
                        Some(RecoveryAction::OpenLogs),
                    );
                }
                MediaEvent::Cancelled { .. } => {
                    self.media_busy = None;
                    self.work = None;
                }
            }
        }
    }

    fn queue_clip(&mut self, range: &ClipRange) {
        let Some(source) = self.entry(&range.source).cloned() else {
            return;
        };
        self.user_queue.push_back(MediaJob::CreateClip {
            source: Box::new(source),
            start_ms: range.start_ms,
            end_ms: range.end_ms,
        });
    }

    fn queue_kill_video(
        &mut self,
        ranges: &[ClipRange],
        width: u32,
        height: u32,
        fps: u32,
        audio: KillAudio,
    ) {
        let mut segments = Vec::with_capacity(ranges.len());
        for range in ranges {
            let Some(source) = self.entry(&range.source).cloned() else {
                self.push_problem(
                    "A kill-video source is no longer in the library.",
                    None,
                    Some(RecoveryAction::Retry),
                );
                return;
            };
            segments.push(KillSegment {
                source,
                start_ms: range.start_ms,
                end_ms: range.end_ms,
            });
        }
        self.user_queue.push_back(MediaJob::CreateKillVideo {
            segments,
            width,
            height,
            fps,
            audio,
        });
    }

    // -----------------------------------------------------------------------
    // Library mutations
    // -----------------------------------------------------------------------

    fn entry(&self, id: &RecordingId) -> Option<&LibraryEntry> {
        self.index.entries.iter().find(|entry| &entry.id == id)
    }

    fn insert_entry(&mut self, entry: LibraryEntry) {
        self.index
            .entries
            .retain(|existing| existing.id != entry.id);
        let position = self
            .index
            .entries
            .partition_point(|existing| existing.start_unix_ms > entry.start_unix_ms);
        self.index.entries.insert(position, entry);
        self.recount();
    }

    fn update_entries(&mut self, ids: &[RecordingId], change: &EntryUpdate) {
        for id in ids {
            let Some(entry) = self.entry(id).cloned() else {
                continue;
            };
            match self.storage.update(&entry, change) {
                Ok(updated) => {
                    if let Some(slot) = self.index.entries.iter_mut().find(|slot| &slot.id == id) {
                        *slot = updated;
                    }
                }
                Err(error) => self.push_problem(
                    format!("\"{}\" could not be updated.", entry.title),
                    Some(error.to_string()),
                    Some(RecoveryAction::Retry),
                ),
            }
        }
        self.enforce_limit();
    }

    fn delete_entries(&mut self, ids: &[RecordingId]) {
        let entries: Vec<LibraryEntry> = ids
            .iter()
            .filter_map(|id| self.entry(id).cloned())
            .collect();
        let result = self.storage.delete(&entries);
        self.index
            .entries
            .retain(|entry| !result.deleted.contains(&entry.id));
        for (id, error) in &result.failures {
            let title = self
                .entry(id)
                .map_or_else(|| id.to_string(), |entry| entry.title.clone());
            self.push_problem(
                format!("\"{title}\" could not be deleted."),
                Some(error.clone()),
                Some(RecoveryAction::Retry),
            );
        }
        self.recount();
    }

    fn rescan(&mut self) {
        self.index = self.storage.scan();
        self.recount();
    }

    fn recount(&mut self) {
        self.storage_used_bytes = self
            .index
            .entries
            .iter()
            .map(|entry| std::fs::metadata(&entry.media_path).map_or(0, |metadata| metadata.len()))
            .fold(0, u64::saturating_add);
        self.dirty = true;
    }

    fn enforce_limit(&mut self) {
        let StorageLimit::Gib(_) = self.config.storage.limit else {
            self.protected_over_limit = false;
            return;
        };
        let result = self
            .storage
            .enforce_limit(self.config.storage.limit, &self.index.entries);
        self.protected_over_limit = result.protected_over_limit;
        if !result.evicted.is_empty() {
            self.index
                .entries
                .retain(|entry| !result.evicted.contains(&entry.id));
        }
        self.recount();
    }

    // -----------------------------------------------------------------------
    // Configuration
    // -----------------------------------------------------------------------

    /// UI-only fields: save atomically, reconfigure nothing.
    fn patch_config(&mut self, draft: Config) {
        if let Err(error) = draft.save(&self.setup.config_path) {
            self.push_problem(
                "Settings could not be saved.",
                Some(error.to_string()),
                Some(RecoveryAction::Retry),
            );
            return;
        }
        self.config = draft;
    }

    fn save_config(&mut self, draft: Config) {
        if self.active.is_some() || self.media_busy == Some(WorkKind::Finalize) {
            self.push_problem(
                "Settings cannot change while a recording is being captured or saved.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        let problems = draft.validate();
        if !problems.is_empty() {
            self.setup_problems = problems;
            self.push_problem(
                "Those settings are not usable yet.",
                None,
                Some(RecoveryAction::OpenSettings),
            );
            return;
        }

        let logs_changed = draft.flavors != self.config.flavors
            || draft.validate_log_paths != self.config.validate_log_paths;
        let storage_changed = draft.storage.recording_dir != self.config.storage.recording_dir
            || draft.storage.separate_buffer_dir != self.config.storage.separate_buffer_dir
            || draft.storage.buffer_dir != self.config.storage.buffer_dir;
        let capture_changed = draft.capture != self.config.capture;
        let limit_changed = draft.storage.limit != self.config.storage.limit;

        if let Err(error) = draft.save(&self.setup.config_path) {
            self.push_problem(
                "Settings could not be saved.",
                Some(error.to_string()),
                Some(RecoveryAction::Retry),
            );
            return;
        }
        self.config = draft;
        self.setup_problems = Vec::new();

        if logs_changed {
            self.open_tailers();
        }
        if storage_changed {
            self.storage = build_storage(&self.config);
            let _ = self.storage.prepare();
            self.rescan();
        }
        if capture_changed || storage_changed {
            self.arm();
        }
        if limit_changed || storage_changed {
            self.enforce_limit();
        }
        if !self.armed {
            // The saved config stands; only the runtime is down.
            self.push_problem(
                "Screen capture did not restart with the new settings.",
                None,
                Some(RecoveryAction::Retry),
            );
        }
    }

    // -----------------------------------------------------------------------
    // Snapshot and problems
    // -----------------------------------------------------------------------

    fn push_problem(
        &mut self,
        summary: impl Into<String>,
        safe_detail: Option<String>,
        recovery_action: Option<RecoveryAction>,
    ) {
        let problem = make_problem(summary, safe_detail, recovery_action);
        tracing::warn!(summary = %problem.summary, detail = ?problem.safe_detail, "problem");
        self.problems.push(problem);
        if self.problems.len() > MAX_PROBLEMS {
            self.problems.remove(0);
        }
        self.dirty = true;
    }

    fn push_recorder_problem(&mut self, error: &RecorderError) {
        let (summary, detail, action) = match error {
            RecorderError::SelectionDenied { log_tail } => (
                "Screen capture was not allowed.",
                Some(log_tail.clone()),
                RecoveryAction::ReselectCaptureTarget,
            ),
            RecorderError::SpawnFailed { message, log_tail } => (
                "Screen capture could not start.",
                Some(format!("{message}\n{log_tail}")),
                RecoveryAction::OpenLogs,
            ),
            RecorderError::MissingRegularArtifact => (
                "The recording produced no video file.",
                None,
                RecoveryAction::OpenLogs,
            ),
            RecorderError::InvalidSettings(message) => (
                "The capture settings are not usable.",
                Some(message.clone()),
                RecoveryAction::OpenSettings,
            ),
            RecorderError::Busy | RecorderError::NotArmed | RecorderError::WrongId => (
                "The recorder was not ready for that.",
                Some(format!("{error:?}")),
                RecoveryAction::Retry,
            ),
            RecorderError::Io(error) => (
                "Screen capture failed.",
                Some(error.to_string()),
                RecoveryAction::OpenLogs,
            ),
        };
        self.push_problem(summary, detail, Some(action));
    }

    fn status(&self) -> RecorderStatus {
        if !self.setup_problems.is_empty() {
            return RecorderStatus::SetupRequired;
        }
        if let Some(active) = &self.active {
            let title = active
                .draft
                .title
                .clone()
                .unwrap_or_else(|| format!("{:?}", active.draft.category));
            return match active.stop_at_ms {
                Some(_) => RecorderStatus::Overrunning {
                    title,
                    started_unix_ms: active.started_unix_ms,
                },
                None => RecorderStatus::Recording {
                    category: active.draft.category.clone(),
                    title,
                    started_unix_ms: active.started_unix_ms,
                    manual: active.mode == RecordingMode::Manual,
                    test: matches!(active.mode, RecordingMode::Test(_)),
                },
            };
        }
        if self.media_busy == Some(WorkKind::Finalize) {
            return RecorderStatus::Finalizing {
                title: "Saving recording".to_owned(),
            };
        }
        if !self.armed {
            return RecorderStatus::WaitingForWow;
        }
        RecorderStatus::Ready
    }

    fn build_snapshot(&self) -> Arc<AppSnapshot> {
        let active = self.active.as_ref().map(|active| ActiveRecordingView {
            id: active.draft.id.clone(),
            category: active.draft.category.clone(),
            title: active
                .draft
                .title
                .clone()
                .unwrap_or_else(|| format!("{:?}", active.draft.category)),
            mode: active.mode.clone(),
            started_unix_ms: active.started_unix_ms,
            requested_replay_ms: active.requested_replay_ms,
            overrun_until_ms: active.stop_at_ms,
        });
        Arc::new(AppSnapshot {
            entries: Arc::from(self.index.entries.clone()),
            correlations: Arc::from(self.index.correlations.clone()),
            category_counts: category_counts(&self.index.entries),
            status: self.status(),
            active,
            config: self.config.clone(),
            setup_problems: self.setup_problems.clone(),
            advanced_logging: self.advanced_logging.clone(),
            problems: self.problems.clone(),
            work: self.work.clone(),
            queued_jobs: self.finalize_queue.len() + self.user_queue.len(),
            storage_used_bytes: self.storage_used_bytes,
            storage_limit: self.config.storage.limit,
            protected_over_limit: self.protected_over_limit,
        })
    }

    /// Publish the newest state, keeping at most one unsent snapshot locally.
    fn publish(&mut self) {
        if !self.dirty && self.pending_snapshot.is_none() {
            return;
        }
        let snapshot = if self.dirty {
            self.dirty = false;
            self.pending_snapshot = None;
            self.build_snapshot()
        } else {
            self.pending_snapshot.take().expect("checked above")
        };
        if let Err(TrySendError::Full(unsent)) = self.snapshot_tx.try_send(snapshot) {
            self.pending_snapshot = Some(unsent);
        }
    }

    // -----------------------------------------------------------------------
    // Shutdown
    // -----------------------------------------------------------------------

    fn shutdown(&mut self) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        // Resolve the active capture the same way a force end would.
        if self.active.is_some() {
            self.force_end();
            if let Some(active) = self.active.as_mut() {
                active.stop_at_ms = Some(now_unix_ms());
            }
            self.check_deadlines();
        }
        if let Err(error) = self.recorder.shutdown() {
            tracing::warn!(?error, "recorder shutdown failed");
        }
        self.armed = false;
        // Finalization gets the media worker's grace period; user jobs cancel.
        self.user_queue.clear();
        self.dispatch_media();
        let _ = self.media_control.try_send(MediaControl::Shutdown);
        if let Some(join) = self.media_join.take() {
            let _ = join.join();
        }
        self.poll_media();
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn make_problem(
    summary: impl Into<String>,
    safe_detail: Option<String>,
    recovery_action: Option<RecoveryAction>,
) -> Problem {
    Problem {
        summary: summary.into(),
        safe_detail,
        occurred_unix_ms: now_unix_ms(),
        recovery_action,
    }
}

/// GSR's `replay`/`regular`/`staging` directories live under the replay-buffer
/// directory when one is configured, and under the library root otherwise.
fn capture_root(config: &Config) -> PathBuf {
    if config.storage.separate_buffer_dir {
        config.storage.buffer_dir.path.clone()
    } else {
        config.storage.recording_dir.path.clone()
    }
}

fn build_storage(config: &Config) -> Storage {
    Storage::new(
        config.storage.recording_dir.path.clone(),
        capture_root(config),
    )
}

fn enabled_log_sources(config: &Config) -> Vec<(&'static str, GameFlavor, PathBuf)> {
    [
        ("retail", GameFlavor::Retail, &config.flavors.retail),
        ("retail_ptr", GameFlavor::Retail, &config.flavors.retail_ptr),
        ("classic", GameFlavor::Classic, &config.flavors.classic),
        (
            "classic_ptr",
            GameFlavor::Classic,
            &config.flavors.classic_ptr,
        ),
        ("era", GameFlavor::Era, &config.flavors.era),
    ]
    .into_iter()
    .filter(|(_, _, flavor)| flavor.enabled && !flavor.log_dir.path.as_os_str().is_empty())
    .map(|(field, game, flavor)| (field, game, flavor.log_dir.path.clone()))
    .collect()
}

/// Legacy `checkAdvancedCombatLogging`: the `Config.wtf` next to the Logs
/// folder must contain `SET advancedCombatLogging "1"`.
fn advanced_logging_enabled(log_dir: &Path) -> bool {
    let Some(parent) = log_dir.parent() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(parent.join("WTF").join("Config.wtf")) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("SET advancedCombatLogging") && line.rsplit(' ').next() == Some("\"1\"")
    })
}

fn category_counts(entries: &[LibraryEntry]) -> Vec<(Category, usize)> {
    let mut counts: Vec<(Category, usize)> = Vec::new();
    for entry in entries {
        match counts
            .iter_mut()
            .find(|(category, _)| category == &entry.category)
        {
            Some((_, count)) => *count += 1,
            None => counts.push((entry.category.clone(), 1)),
        }
    }
    counts
}

fn local_year() -> i32 {
    // Days since the epoch to a civil year, without pulling in a date crate.
    let days = now_unix_ms().div_euclid(86_400_000);
    let mut year = 1970;
    let mut remaining = days;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let length = if leap { 366 } else { 365 };
        if remaining < length {
            return year;
        }
        remaining -= length;
        year += 1;
    }
}

/// The minimum parsed events that drive one category through the activity
/// engine, used by test recordings.
fn test_events(
    category: &Category,
    start_ms: i64,
    end_ms: i64,
) -> Option<(Vec<ParsedEvent>, ParsedEvent)> {
    const GUID: &str = "Player-1092-0A70E103";
    const NAME: &str = "Testplayer-Testrealm";
    // Affiliation mine, friendly, player-controlled, player type.
    const SELF_FLAGS: u64 = 0x511;

    let retail = |event: CombatEvent, at_ms: i64| ParsedEvent {
        flavor: GameFlavor::Retail,
        occurred_at_ms: at_ms,
        event,
    };
    let arena = |zone_id: u32, match_type: &str| CombatEvent::ArenaStarted {
        zone_id,
        match_type: match_type.to_owned(),
    };

    let (start, end) = match category {
        Category::TwoVTwo => (
            arena(2547, "2v2"),
            CombatEvent::ArenaEnded {
                winning_team_id: 0,
                team_0_mmr: 1673,
                team_1_mmr: 1668,
            },
        ),
        Category::ThreeVThree => (
            arena(980, "3v3"),
            CombatEvent::ArenaEnded {
                winning_team_id: 0,
                team_0_mmr: 1673,
                team_1_mmr: 1668,
            },
        ),
        Category::SoloShuffle => (
            arena(2547, "Rated Solo Shuffle"),
            CombatEvent::ArenaEnded {
                winning_team_id: 0,
                team_0_mmr: 1673,
                team_1_mmr: 1668,
            },
        ),
        Category::Raids => (
            CombatEvent::EncounterStarted {
                encounter_id: 2820,
                name: "Test Encounter".to_owned(),
                difficulty_id: 16,
                group_size: 20,
                instance_id: 2549,
            },
            CombatEvent::EncounterEnded {
                encounter_id: 2820,
                name: "Test Encounter".to_owned(),
                difficulty_id: 16,
                group_size: 20,
                success: true,
            },
        ),
        Category::Battlegrounds => (
            CombatEvent::ZoneChanged {
                zone_id: 30,
                name: "Alterac Valley".to_owned(),
                instance_id: 30,
            },
            CombatEvent::ZoneChanged {
                zone_id: 0,
                name: String::new(),
                instance_id: 0,
            },
        ),
        Category::MythicPlus => (
            CombatEvent::ChallengeStarted {
                name: "Test Dungeon".to_owned(),
                zone_id: 2286,
                map_id: 377,
                level: 10,
                affixes: vec![9, 6, 3],
            },
            CombatEvent::ChallengeEnded {
                zone_id: 2286,
                success: true,
                duration_ms: (end_ms - start_ms).max(0) as u64,
            },
        ),
        _ => return None,
    };

    let start_events = vec![
        retail(start, start_ms),
        retail(
            CombatEvent::Combatant {
                guid: GUID.to_owned(),
                team_id: Some(0),
                spec_id: Some(577),
            },
            start_ms,
        ),
        retail(
            CombatEvent::PlayerObserved {
                kind: PlayerObservationKind::AuraApplied,
                guid: GUID.to_owned(),
                name: NAME.to_owned(),
                flags: SELF_FLAGS,
                target_guid: GUID.to_owned(),
                target_name: NAME.to_owned(),
                target_flags: SELF_FLAGS,
                spell_name: "Test Recording".to_owned(),
            },
            start_ms,
        ),
    ];
    Some((start_events, retail(end, end_ms)))
}
