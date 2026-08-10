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
//! Notes:
//! - Advanced-combat-logging status is read from `<log dir>/../WTF/Config.wtf`
//!   when the tailers are (re)opened; it refreshes on arm/save.
//! - Test recordings synthesize the minimum parsed events for the chosen
//!   category. `ForceEnd` stops a running test.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::activity::{ActivityAction, ActivityEngine, RecordingDraft};
use crate::config::{
    Config, ConfigError, LayoutSettings, LoadedConfig, ValidationProblem, load_or_import,
};
use crate::domain::{
    ActivityDetails, Category, CorrelatedActivity, DeathMarkerVisibility, GameFlavor, LibraryEntry,
    MarkerVisibility, MediaFacts, Outcome, Problem, RecorderStatus, RecordingId, RecoveryAction,
    StorageLimit, WorkKind, WorkProgress,
};
use crate::logwatch::LogTailer;
use crate::media_jobs::{MediaConfig, MediaControl, MediaEvent, MediaJob, MediaWorker};
use crate::parser::{CombatEvent, ParseTimeContext, ParsedEvent, PlayerObservationKind};
use crate::recorder::{
    CaptureArtifacts, CaptureConfig, Recorder, RecorderError, RecorderEvent, RecordingMode,
    StartRequest, Timeouts,
};
use crate::storage::{EntryUpdate, LibraryIndex, Storage, now_unix_ms};

/// Force-end an automatic recording after this long without new log data: a
/// crash/alt-F4 safety net only. WoW flushes the combat log in bursts, so the
/// window must sit well above that cadence or a live activity gets force-ended
/// inside a flush gap and discarded before its player is identified.
const RETAIL_DATA_TIMEOUT_MS: i64 = 10 * 60_000;
const CLASSIC_DATA_TIMEOUT_MS: i64 = 2 * 60_000;
/// Commands handled per tick before the loop returns to polling.
const COMMAND_BATCH: usize = 16;
/// Bounded problem list surfaced in the snapshot.
const MAX_PROBLEMS: usize = 8;
const CAPTURE_STOPPED_PROBLEM: &str = "Screen capture stopped unexpectedly.";
const CAPTURE_RESTART_FAILED_PROBLEM: &str = "Screen capture could not be restarted.";
/// Keep queued transcodes finite while preserving the one-worker design.
const MAX_MEDIA_QUEUE: usize = 16;

/// How long quitting waits for gpu-screen-recorder to finish writing the
/// capture it was asked to stop. Long enough for a normal flush, short enough
/// that closing the window never looks hung.
const QUIT_END_GRACE: Duration = Duration::from_secs(5);

// --- Public interface ---

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
    SetSelectedCategory {
        category: Category,
    },
    SetMarkerVisibility {
        deaths: DeathMarkerVisibility,
        encounters: MarkerVisibility,
        rounds: MarkerVisibility,
    },
    /// Debounced UI geometry write from the shell: divider and column widths.
    SaveLayout {
        layout: LayoutSettings,
    },
    /// The user acknowledged the post-migration notice; never show it again.
    DismissMigrationNotice,
    /// The user closed the "What's new" dialog for the running version.
    DismissReleaseNotes,
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
    pub entries: Arc<Vec<LibraryEntry>>,
    pub correlations: Arc<Vec<CorrelatedActivity>>,
    pub category_counts: Vec<(Category, usize)>,
    pub status: RecorderStatus,
    pub active: Option<ActiveRecordingView>,
    pub config: Config,
    pub setup_problems: Vec<ValidationProblem>,
    /// One entry per enabled flavour: its config field name and whether
    /// advanced combat logging is on. `None` when `Config.wtf` could not be
    /// read, which is the normal case in the sandbox: the portal exports the
    /// chosen Logs folder alone, never its `WTF` sibling.
    pub advanced_logging: Vec<(&'static str, Option<bool>)>,
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
    commands: Option<SyncSender<Command>>,
    pub snapshots: Receiver<Arc<AppSnapshot>>,
    pub stopped: Receiver<()>,
    join: Option<JoinHandle<()>>,
}

impl CoordinatorHandle {
    /// Returns `false` when the queue is full or the coordinator is gone; the
    /// caller shows one Busy problem rather than blocking the GTK thread.
    pub fn send(&self, command: Command) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|commands| commands.try_send(command).is_ok())
    }

    /// Request shutdown and join the coordinator thread. Takes `&mut self` so
    /// `main` can drive it through the shared handle after the GTK loop exits;
    /// `join.take()` keeps the `Drop` guard from joining twice.
    pub fn shutdown(&mut self) {
        self.request_shutdown();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    fn request_shutdown(&mut self) {
        if let Some(commands) = self.commands.take() {
            // Dropping the sender after the best-effort command makes a full
            // queue safe too: the receiver observes disconnection after it
            // drains the already queued commands and exits.
            let _ = commands.try_send(Command::Shutdown);
        }
    }
}

impl Drop for CoordinatorHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.request_shutdown();
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
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
    /// Test-recording length; raids run four times as long.
    pub test_duration: Duration,
}

impl Setup {
    pub fn from_environment() -> Result<Self, ConfigError> {
        let (year, utc_offset_minutes) = local_clock();
        Ok(Self {
            config_path: crate::config::config_path_from_environment()?,
            legacy_config_path: crate::config::legacy_config_path_from_environment()?,
            data_dir: crate::config::config_path_from_environment()?
                .parent()
                .unwrap_or(Path::new("."))
                .join("recorder"),
            gsr_binary: PathBuf::from("gpu-screen-recorder"),
            media: MediaConfig {
                utc_offset_minutes,
                ..MediaConfig::default()
            },
            year,
            recorder_timeouts: Timeouts::default(),
            poll_interval: Duration::from_millis(50),
            test_duration: Duration::from_secs(5),
        })
    }
}

/// Start the coordinator and its media worker. `wake` runs whenever a new
/// snapshot or the stopped signal has been queued, so the shell can react
/// immediately instead of waiting out its next poll. It is called from the
/// coordinator thread and must be cheap and nonblocking.
pub fn start(setup: Setup, wake: Box<dyn Fn() + Send>) -> CoordinatorHandle {
    let (commands_tx, commands_rx) = mpsc::sync_channel(64);
    let (snapshot_tx, snapshots) = mpsc::sync_channel(1);
    let (stopped_tx, stopped) = mpsc::sync_channel(1);
    let join = std::thread::Builder::new()
        .name("coordinator".to_owned())
        .spawn(move || {
            let mut coordinator = Coordinator::new(setup, commands_rx, snapshot_tx, wake);
            coordinator.startup();
            while coordinator.tick() {}
            let _ = stopped_tx.try_send(());
            coordinator.wake();
        })
        .expect("spawn coordinator thread");
    CoordinatorHandle {
        commands: Some(commands_tx),
        snapshots,
        stopped,
        join: Some(join),
    }
}

// --- Coordinator state ---

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

/// A capture whose stop was requested and whose artifacts have not arrived.
/// `Finalize` carries the draft the media worker needs; `Discard` only waits
/// so the artifacts can be swept instead of kept.
enum EndingCapture {
    Finalize(Box<RecordingDraft>),
    Discard,
}

/// An activity held back because GSR was still writing the previous capture.
/// `finished` is set when the activity also ended inside that window: the
/// capture still has to start, because the replay buffer holds it, and then
/// stop again after whatever overrun is left.
struct DeferredBegin {
    draft: Box<RecordingDraft>,
    finished: bool,
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
    /// Set between the stop request and its `CaptureEnded`.
    ending: Option<EndingCapture>,
    /// An activity that began while the previous capture was still flushing.
    deferred_begin: Option<DeferredBegin>,
    /// Injected test end event, released once its wall-clock deadline passes.
    pending_test_end: Option<(i64, ParsedEvent)>,

    media_jobs: SyncSender<MediaJob>,
    media_events_tx: Sender<MediaEvent>,
    media_control: SyncSender<MediaControl>,
    media_events: Receiver<MediaEvent>,
    media_join: Option<JoinHandle<()>>,
    maintenance_busy: bool,
    finalize_queue: VecDeque<MediaJob>,
    user_queue: VecDeque<MediaJob>,
    media_busy: Option<WorkKind>,
    work: Option<WorkProgress>,

    problems: Vec<Problem>,
    setup_problems: Vec<ValidationProblem>,
    advanced_logging: Vec<(&'static str, Option<bool>)>,
    storage_used_bytes: u64,
    protected_over_limit: bool,

    commands: Receiver<Command>,
    snapshot_tx: SyncSender<Arc<AppSnapshot>>,
    /// Nudges the shell's main loop after a snapshot is queued.
    wake: Box<dyn Fn() + Send>,
    pending_snapshot: Option<Arc<AppSnapshot>>,
    dirty: bool,
    stopping: bool,
}

impl Coordinator {
    pub fn new(
        setup: Setup,
        commands: Receiver<Command>,
        snapshot_tx: SyncSender<Arc<AppSnapshot>>,
        wake: Box<dyn Fn() + Send>,
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
        let (events_tx, media_events) = mpsc::channel();
        let (media_jobs, media_control, media_join) = spawn_media_worker(
            setup.media.clone(),
            build_storage(&config),
            events_tx.clone(),
        )
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
            ending: None,
            deferred_begin: None,
            pending_test_end: None,
            media_jobs,
            media_events_tx: events_tx,
            media_control,
            media_events,
            media_join: Some(media_join),
            maintenance_busy: false,
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
            wake,
            pending_snapshot: None,
            dirty: true,
            stopping: false,
        }
    }

    /// Sweep interrupted artifacts, scan the library, validate, and arm.
    pub fn startup(&mut self) {
        self.recorder = Recorder::with_timeouts(self.setup.recorder_timeouts);
        self.setup_problems = self.config.validate();
        if !self.setup_problems.is_empty() {
            self.dirty = true;
            return;
        }
        if let Err(error) = self.storage.prepare() {
            self.push_problem(
                "The recording directory could not be prepared.",
                Some(error.to_string()),
                Some(RecoveryAction::OpenSettings),
            );
            self.dirty = true;
            return;
        }
        let report = self.storage.sweep_orphans();
        if !report.failures.is_empty() {
            self.push_problem(
                "Some interrupted recordings could not be moved to Recovery.",
                Some(report.failures.join("; ")),
                Some(RecoveryAction::OpenLogs),
            );
        }
        self.rescan();
        self.enforce_limit();
        self.dirty = true;
        // Show the library before arming: spawning gpu-screen-recorder waits
        // out a stability check, and there is no reason for the window to sit
        // empty through it.
        self.publish();

        let tailers_ready = self.open_tailers();
        if self.setup_problems.is_empty() && tailers_ready {
            self.arm();
        }
        self.dirty = true;
        // Make the armed recorder available before the optional one-time
        // historical-log pass reads large source files.
        self.publish();

        let retail_logs: Vec<PathBuf> = enabled_log_sources(&self.config)
            .into_iter()
            .filter(|(field, flavor, _)| *field == "retail" && *flavor == GameFlavor::Retail)
            .map(|(_, _, source)| source)
            .collect();
        let context = ParseTimeContext::new(self.setup.year, self.setup.media.utc_offset_minutes);
        if !retail_logs.is_empty() {
            match self.media_jobs.try_send(MediaJob::EnrichLegacyBloodlust {
                retail_log_dirs: retail_logs,
                context,
            }) {
                Ok(()) => self.maintenance_busy = true,
                Err(TrySendError::Full(_)) => {
                    tracing::warn!("media worker was busy before legacy timeline enrichment")
                }
                Err(TrySendError::Disconnected(_)) => {
                    tracing::warn!("media worker unavailable for legacy timeline enrichment")
                }
            }
        }
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

    // --- Commands ---

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
            Command::SaveLayout { layout } => {
                if self.config.interface.layout != layout {
                    let mut draft = self.config.clone();
                    draft.interface.layout = layout;
                    self.patch_config(draft);
                }
            }
            Command::DismissMigrationNotice => {
                if self.config.migration_notice_pending {
                    let mut draft = self.config.clone();
                    draft.migration_notice_pending = false;
                    self.patch_config(draft);
                }
            }
            Command::DismissReleaseNotes => {
                if self.config.last_seen_version != crate::VERSION {
                    let mut draft = self.config.clone();
                    draft.last_seen_version = crate::VERSION.to_owned();
                    self.patch_config(draft);
                }
            }
            Command::Shutdown => {}
        }
    }

    // --- Recorder lifecycle ---

    fn capture_config(&self) -> CaptureConfig {
        CaptureConfig {
            gsr_binary: self.setup.gsr_binary.clone(),
            data_dir: self.setup.data_dir.clone(),
            capture_root: capture_root(&self.config),
            settings: self.config.capture.clone(),
        }
    }

    /// True from the moment a capture starts until its artifacts are in hand.
    /// Anything that stops, replaces, or reconfigures gpu-screen-recorder has
    /// to wait: the child is still writing the previous recording.
    fn capture_in_flight(&self) -> bool {
        self.active.is_some() || self.ending.is_some()
    }

    fn arm(&mut self) {
        if self.capture_in_flight() {
            self.push_problem(
                "Screen capture cannot be rearmed while a recording is being captured or saved.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        let config = self.capture_config();
        match self.recorder.arm(&config) {
            Ok(()) => {
                self.armed = true;
                self.clear_recovered_capture_problems();
            }
            Err(error) => {
                self.armed = false;
                self.push_recorder_problem(&error);
            }
        }
    }

    fn disarm(&mut self) {
        if self.capture_in_flight() {
            self.push_problem(
                "Screen capture cannot be disarmed while a recording is being captured or saved.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        self.armed = false;
        if let Err(error) = self.recorder.shutdown() {
            self.push_recorder_problem(&error);
        }
    }

    fn reselect_target(&mut self) {
        if self.capture_in_flight() {
            self.push_problem(
                "The capture target cannot change while a recording is being captured or saved.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        let config = self.capture_config();
        let was_armed = self.armed;
        match self.recorder.reselect_target(&config) {
            Ok(selection) => {
                self.armed = true;
                self.clear_recovered_capture_problems();
                if let Some(token) = selection.token {
                    self.store_token(token);
                }
            }
            Err(error) => {
                self.armed = was_armed;
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
                RecorderEvent::CaptureEnded { artifacts } => self.capture_ended(artifacts),
                RecorderEvent::RestartFailed { message } => {
                    self.armed = false;
                    self.push_problem(
                        CAPTURE_RESTART_FAILED_PROBLEM,
                        Some(message),
                        Some(RecoveryAction::ReselectCaptureTarget),
                    );
                }
                RecorderEvent::ChildExited { code } => {
                    self.armed = false;
                    self.push_problem(
                        CAPTURE_STOPPED_PROBLEM,
                        Some(format!("gpu-screen-recorder exited with code {code:?}")),
                        Some(RecoveryAction::ReselectCaptureTarget),
                    );
                    if let Some(active) = self.active.take() {
                        self.pending_test_end = None;
                        self.drop_activity(&active.draft.flavor);
                        let report = self.storage.sweep_orphans();
                        if !report.failures.is_empty() {
                            tracing::warn!(failures = ?report.failures, "capture failure sweep failed");
                        }
                    }
                }
                RecorderEvent::Restarted => {
                    self.armed = true;
                    self.clear_recovered_capture_problems();
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

    // --- Log polling and the activity engine ---

    fn open_tailers(&mut self) -> bool {
        self.tailers.clear();
        self.advanced_logging.clear();
        self.last_event.clear();
        let context = ParseTimeContext::new(self.setup.year, self.setup.media.utc_offset_minutes);
        let mut all_opened = true;
        for (field, flavor, source) in enabled_log_sources(&self.config) {
            self.advanced_logging
                .push((field, advanced_logging_enabled(&source)));
            match LogTailer::open(source.clone(), flavor, context) {
                Ok(tailer) => self.tailers.push(tailer),
                Err(error) => {
                    all_opened = false;
                    self.push_problem(
                        format!("The {field} log folder could not be watched."),
                        Some(error.to_string()),
                        Some(RecoveryAction::OpenSettings),
                    );
                }
            }
        }
        all_opened
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
                // The activity both began and ended while the previous capture
                // was flushing. The replay buffer still holds it, so keep the
                // authoritative finished draft and let `capture_ended` start
                // and immediately stop its capture.
                if let Some(deferred) = self.deferred_begin.as_mut()
                    && deferred.draft.id == id
                {
                    *deferred.draft = draft;
                    deferred.finished = true;
                    return;
                }
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
                    .deferred_begin
                    .as_ref()
                    .is_some_and(|deferred| deferred.draft.id == id)
                {
                    // Never captured, nothing written: just forget it.
                    self.deferred_begin = None;
                    tracing::info!(?reason, "discarding deferred recording");
                    return;
                }
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
        // GSR is still writing the previous capture. Hold the draft instead of
        // dropping the activity; the replay buffer keeps filling, so the
        // lead-in is recomputed from the real start when the capture begins.
        if self.ending.is_some() {
            if self.deferred_begin.is_some() {
                self.drop_activity(&draft.flavor);
                return;
            }
            self.deferred_begin = Some(DeferredBegin {
                draft: Box::new(draft),
                finished: false,
            });
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
        let occurred_at_ms = now_unix_ms();
        self.pending_test_end = None;
        for action in self.engine.force_end(flavor, occurred_at_ms) {
            self.apply(action);
        }
    }

    /// Retail 10 min, classic/era 2 min without new log data ends at last-data time.
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
        let wall_gap_ms = now_ms - seen_wall_ms;
        if wall_gap_ms < limit {
            return;
        }
        tracing::warn!(
            wall_gap_ms,
            seen_log_ms,
            "data timeout: force-ending automatic recording (log idle on disk)"
        );
        for action in self.engine.force_end(flavor, seen_log_ms) {
            self.apply(action);
        }
    }

    // --- Manual and test recordings ---

    fn start_manual(&mut self) {
        if self.capture_in_flight() || !self.armed || !self.config.manual.enabled {
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
        if self.capture_in_flight() || self.pending_test_end.is_some() || !self.armed {
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

    // --- Ending a capture ---

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

    /// Ask the recorder to stop and remember what to do with the artifacts.
    /// `CaptureEnded` finishes the job; the coordinator keeps serving commands
    /// and snapshots while GSR flushes.
    fn end_capture(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        self.dirty = true;
        match self.recorder.request_end(&active.draft.id) {
            Ok(()) => self.ending = Some(EndingCapture::Finalize(Box::new(active.draft))),
            Err(error) => {
                self.push_recorder_problem(&error);
                let report = self.storage.sweep_orphans();
                tracing::info!(quarantined = report.quarantined.len(), "capture end failed");
            }
        }
    }

    /// Stop capture without producing an entry and quarantine what GSR wrote.
    fn cancel_capture(&mut self, id: &RecordingId) {
        self.dirty = true;
        match self.recorder.request_end(id) {
            Ok(()) => self.ending = Some(EndingCapture::Discard),
            Err(error) => self.push_recorder_problem(&error),
        }
    }

    /// The recorder resolved a requested end. Missing artifacts mean GSR never
    /// wrote the regular recording within its bounded wait.
    fn capture_ended(&mut self, artifacts: Option<CaptureArtifacts>) {
        self.dirty = true;
        match (self.ending.take(), artifacts) {
            (Some(EndingCapture::Finalize(draft)), Some(artifacts)) => {
                self.queue_finalization(draft, artifacts);
            }
            (Some(EndingCapture::Finalize(_)), None) => {
                self.push_recorder_problem(&RecorderError::MissingRegularArtifact);
                let report = self.storage.sweep_orphans();
                tracing::info!(quarantined = report.quarantined.len(), "capture end failed");
            }
            (Some(EndingCapture::Discard), _) => {
                let report = self.storage.sweep_orphans();
                if !report.failures.is_empty() {
                    tracing::warn!(failures = ?report.failures, "discard sweep failed");
                }
            }
            (None, _) => {}
        }
        // Quitting drains this same path and the recorder is killed right
        // after, so a capture started here would be one that nothing ever ends
        // or saves; hold the draft rather than losing it to a shutdown that may
        // still be cancelled by a later tick.
        if self.stopping {
            return;
        }
        let Some(deferred) = self.deferred_begin.take() else {
            return;
        };
        // A disarmed recorder has no child left to start the capture (that is
        // how a mid-flush GSR crash arrives here) and `start_capture` would
        // stack a second, baffling problem on top of the crash the user was
        // already told about. Drop the activity instead: nothing was written.
        if !self.armed {
            tracing::info!("dropping deferred recording: capture is no longer armed");
            return;
        }
        let overrun_ms = deferred.draft.overrun_ms as i64;
        let ended_at_ms = deferred.draft.ended_at_ms;
        // The activity started while the previous capture was flushing; treat
        // now as the detection time so the requested pre-roll still reaches
        // back to the real activity start.
        self.begin(*deferred.draft, now_unix_ms());
        // It already ended too. The capture had to start anyway so the replay
        // buffer is written; stop it on the overrun the live path would have
        // used, anchored to when the activity actually ended. A deadline
        // already in the past simply stops on the next tick.
        if deferred.finished
            && let Some(active) = self.active.as_mut()
        {
            let anchor_ms = ended_at_ms.unwrap_or_else(now_unix_ms);
            active.stop_at_ms = Some(anchor_ms + overrun_ms);
        }
    }

    fn queue_finalization(&mut self, draft: Box<RecordingDraft>, artifacts: CaptureArtifacts) {
        if self.finalize_queue.len() >= MAX_MEDIA_QUEUE {
            let report = self.storage.sweep_orphans();
            if !report.failures.is_empty() {
                tracing::warn!(failures = ?report.failures, "finalization queue overflow sweep failed");
            }
            self.push_problem(
                "The recording could not be queued for saving.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        self.finalize_queue.push_back(MediaJob::FinalizeRecording {
            draft,
            artifacts,
            facts: self.media_facts(),
        });
        if self.maintenance_busy {
            let _ = self.media_control.try_send(MediaControl::CancelMaintenance);
        }
    }

    fn media_facts(&self) -> MediaFacts {
        MediaFacts {
            fps: Some(self.config.capture.fps),
            width: None,
            height: None,
            codec: Some(self.config.capture.codec),
            has_content: true,
        }
    }

    // --- Media jobs ---

    /// Finalization is always chosen before queued user work, and only one job
    /// is in flight at a time. A capture that has been asked to stop counts as
    /// queued finalization: its artifacts are moments away, and letting a clip
    /// jump the queue in that window would reorder the library.
    fn dispatch_media(&mut self) {
        if self.media_busy.is_some() || self.maintenance_busy {
            return;
        }
        let finalization_pending = matches!(self.ending, Some(EndingCapture::Finalize(_)));
        let Some(job) = self.finalize_queue.pop_front().or_else(|| {
            if finalization_pending {
                None
            } else {
                self.user_queue.pop_front()
            }
        }) else {
            return;
        };
        let kind = job.kind();
        match self.media_jobs.try_send(job) {
            Ok(()) => {
                self.media_busy = Some(kind);
                self.dirty = true;
            }
            Err(TrySendError::Full(job)) => {
                if kind == WorkKind::Finalize {
                    self.finalize_queue.push_front(job);
                } else {
                    self.user_queue.push_front(job);
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                self.push_problem(
                    "The media worker is unavailable.",
                    None,
                    Some(RecoveryAction::Quit),
                );
            }
        }
    }

    fn poll_media(&mut self) {
        while let Ok(event) = self.media_events.try_recv() {
            self.dirty = true;
            match event {
                MediaEvent::TimelineEnriched { enriched, failures } => {
                    self.maintenance_busy = false;
                    if enriched != 0 {
                        tracing::info!(sidecars = enriched, "enriched legacy Bloodlust timelines");
                        self.rescan();
                    }
                    for failure in failures {
                        tracing::warn!(%failure, "legacy Bloodlust enrichment failed");
                    }
                }
                MediaEvent::Progress(progress) => self.work = Some(progress),
                MediaEvent::Completed { .. } => {
                    self.media_busy = None;
                    self.work = None;
                    self.rescan();
                    self.enforce_limit();
                }
                MediaEvent::Failed { kind, message } => {
                    self.media_busy = None;
                    self.work = None;
                    self.push_problem(
                        match kind {
                            WorkKind::Finalize => "A recording could not be saved.",
                            WorkKind::Clip => "The clip could not be created.",
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
            self.push_problem(
                "The clip source is no longer in the library.",
                Some(range.source.to_string()),
                Some(RecoveryAction::Retry),
            );
            return;
        };
        if self.user_queue.len() >= MAX_MEDIA_QUEUE {
            self.push_problem(
                "The media work queue is full.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        self.user_queue.push_back(MediaJob::CreateClip {
            source: Box::new(source),
            start_ms: range.start_ms,
            end_ms: range.end_ms,
        });
        if self.maintenance_busy {
            let _ = self.media_control.try_send(MediaControl::CancelMaintenance);
        }
    }

    // --- Library mutations ---

    fn entry(&self, id: &RecordingId) -> Option<&LibraryEntry> {
        self.index.entries.iter().find(|entry| &entry.id == id)
    }

    fn update_entries(&mut self, ids: &[RecordingId], change: &EntryUpdate) {
        for id in ids {
            let Some(entry) = self.entry(id).cloned() else {
                self.push_problem(
                    "A selected recording is no longer in the library.",
                    Some(id.to_string()),
                    Some(RecoveryAction::Retry),
                );
                continue;
            };
            match self.storage.update(&entry, change) {
                Ok(updated) => {
                    if let Some(slot) = Arc::make_mut(&mut self.index.entries)
                        .iter_mut()
                        .find(|slot| &slot.id == id)
                    {
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
        // Correlation keys on activity hash, start and category; a tag or
        // protect flag cannot move an entry between groups, so there is
        // nothing to recompute.
        self.enforce_limit();
    }

    fn delete_entries(&mut self, ids: &[RecordingId]) {
        let mut entries = Vec::new();
        for id in ids {
            match self.entry(id).cloned() {
                Some(entry) => entries.push(entry),
                None => self.push_problem(
                    "A selected recording is no longer in the library.",
                    Some(id.to_string()),
                    Some(RecoveryAction::Retry),
                ),
            }
        }
        let result = self.storage.delete(&entries);
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
        // Files went away: reconcile entries, correlations and usage with the
        // directory. Rebuilding the groups by hand loses any activity whose
        // primary was deleted while one of its viewpoints survived a failure.
        self.rescan();
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
            .map(|entry| {
                let media =
                    std::fs::metadata(&entry.media_path).map_or(0, |metadata| metadata.len());
                let sidecar =
                    std::fs::metadata(&entry.sidecar_path).map_or(0, |metadata| metadata.len());
                media.saturating_add(sidecar)
            })
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
        if result.evicted.is_empty() && !result.partially_deleted {
            self.recount();
        } else {
            self.rescan();
        }
    }

    // --- Configuration ---

    fn restart_media_worker(&mut self) -> Result<(), String> {
        let _ = self.media_control.send(MediaControl::Shutdown {
            pending_finalizations: Vec::new(),
        });
        if let Some(join) = self.media_join.take() {
            join.join()
                .map_err(|_| "the previous media worker panicked".to_owned())?;
        }
        let (media_jobs, media_control, media_join) = spawn_media_worker(
            self.setup.media.clone(),
            build_storage(&self.config),
            self.media_events_tx.clone(),
        )?;
        self.media_jobs = media_jobs;
        self.media_control = media_control;
        self.media_join = Some(media_join);
        Ok(())
    }

    /// UI-only fields: adopt, show, then save atomically; reconfigure nothing.
    ///
    /// Persisting first would put two `fsync`s in front of every category
    /// click. Nothing rereads the file at runtime, so the snapshot can go out
    /// first; a failed write rolls the preference back.
    fn patch_config(&mut self, draft: Config) {
        let previous = std::mem::replace(&mut self.config, draft);
        self.dirty = true;
        self.publish();
        if let Err(error) = self.config.save(&self.setup.config_path) {
            // The rename may already have made the new file visible; only the
            // directory sync failed. Rolling back then would leave the next
            // launch loading a value this one just told the user it dropped.
            if !error.is_committed() {
                self.config = previous;
            }
            self.push_problem(
                "Settings could not be saved.",
                Some(error.to_string()),
                Some(RecoveryAction::Retry),
            );
        }
    }

    fn save_config(&mut self, mut draft: Config) {
        if self.capture_in_flight()
            || self.media_busy.is_some()
            || !self.finalize_queue.is_empty()
            || !self.user_queue.is_empty()
        {
            self.push_problem(
                "Settings cannot change while a recording is being captured or saved.",
                None,
                Some(RecoveryAction::Retry),
            );
            return;
        }
        // Settings never owns the dismissal flags. Its draft was cloned from
        // a snapshot taken while a notice was still up -- the migration notice
        // itself offers the button that opens Settings -- so honouring the
        // draft here resurrects a notice the user already dismissed, on every
        // save.
        draft.migration_notice_pending = self.config.migration_notice_pending;
        draft.last_seen_version = self.config.last_seen_version.clone();
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

        let mut runtime_ready = true;
        if logs_changed && !self.open_tailers() {
            runtime_ready = false;
        }
        if storage_changed {
            self.storage = build_storage(&self.config);
            if let Err(error) = self.storage.prepare() {
                runtime_ready = false;
                self.push_problem(
                    "The new recording directory could not be prepared.",
                    Some(error.to_string()),
                    Some(RecoveryAction::OpenSettings),
                );
            }
            self.rescan();
            if let Err(error) = self.restart_media_worker() {
                runtime_ready = false;
                self.push_problem(
                    "The media worker could not switch to the new storage directory.",
                    Some(error),
                    Some(RecoveryAction::Retry),
                );
            }
        }
        if runtime_ready && (capture_changed || storage_changed) {
            self.arm();
        }
        if !runtime_ready {
            self.disarm();
        } else if limit_changed || storage_changed {
            self.enforce_limit();
        }
        if (capture_changed || storage_changed) && !self.armed {
            // The saved config stands; only the runtime is down.
            self.push_problem(
                "Screen capture did not restart with the new settings.",
                None,
                Some(RecoveryAction::Retry),
            );
        }
    }

    // --- Snapshot and problems ---

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

    fn clear_recovered_capture_problems(&mut self) {
        if clear_recovered_capture_problems(&mut self.problems) {
            self.dirty = true;
        }
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
        // A stop was requested and GSR is still writing: the recording is not
        // over from the user's side, so do not fall back to Ready. A discarded
        // capture is not being saved, so say what is actually happening.
        if matches!(self.ending, Some(EndingCapture::Discard)) {
            return RecorderStatus::Finalizing {
                title: "Discarding recording".to_owned(),
            };
        }
        if self.ending.is_some() || self.media_busy == Some(WorkKind::Finalize) {
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
            entries: Arc::clone(&self.index.entries),
            correlations: Arc::clone(&self.index.correlations),
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
        match self.snapshot_tx.try_send(snapshot) {
            Ok(()) => self.wake(),
            Err(TrySendError::Full(unsent)) => self.pending_snapshot = Some(unsent),
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Nudge the shell's main loop. Cheap and nonblocking by contract.
    pub fn wake(&self) {
        (self.wake)();
    }

    // --- Shutdown ---

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
        // Quitting is the one place that must wait: `recorder.shutdown` kills
        // GSR, so the requested end has to produce its artifacts first or the
        // recording is lost. Same bounded deadlines the poll path uses.
        if self.recorder.is_ending() {
            for event in self
                .recorder
                .finish_end_blocking(Instant::now() + QUIT_END_GRACE)
            {
                if let RecorderEvent::CaptureEnded { artifacts } = event {
                    self.capture_ended(artifacts);
                }
            }
        }
        if let Err(error) = self.recorder.shutdown() {
            tracing::warn!(?error, "recorder shutdown failed");
        }
        self.armed = false;
        // Finalization gets the media worker's grace period; user jobs cancel.
        self.user_queue.clear();
        // Transfer every not-yet-submitted finalization in the shutdown
        // message. This cannot race with the capacity-one job channel when
        // enrichment or another media job has not been received yet.
        let pending_finalizations = self.finalize_queue.drain(..).collect();
        let _ = self.media_control.send(MediaControl::Shutdown {
            pending_finalizations,
        });
        if let Some(join) = self.media_join.take() {
            let _ = join.join();
        }
        let report = self.storage.sweep_orphans();
        if !report.failures.is_empty() {
            tracing::warn!(failures = ?report.failures, "shutdown sweep failed");
        }
        self.finalize_queue.clear();
        self.poll_media();
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// --- Free helpers ---

fn clear_recovered_capture_problems(problems: &mut Vec<Problem>) -> bool {
    let previous_len = problems.len();
    problems.retain(|problem| {
        !matches!(
            problem.summary.as_str(),
            CAPTURE_STOPPED_PROBLEM | CAPTURE_RESTART_FAILED_PROBLEM
        )
    });
    problems.len() != previous_len
}

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

type MediaWorkerHandles = (
    SyncSender<MediaJob>,
    SyncSender<MediaControl>,
    JoinHandle<()>,
);

fn spawn_media_worker(
    config: MediaConfig,
    storage: Storage,
    events: Sender<MediaEvent>,
) -> Result<MediaWorkerHandles, String> {
    let (jobs, jobs_rx) = mpsc::sync_channel(1);
    let (control, control_rx) = mpsc::sync_channel(1);
    let worker = MediaWorker::new(config, storage, jobs_rx, control_rx, events);
    let join = std::thread::Builder::new()
        .name("media".to_owned())
        .spawn(move || worker.run())
        .map_err(|error| format!("spawn media worker: {error}"))?;
    Ok((jobs, control, join))
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

/// Whether the `Config.wtf` beside the Logs folder carries
/// `SET advancedCombatLogging "1"`. `None` means the file could not be read
/// rather than that the setting is off: the folder portal exports the chosen
/// Logs directory on its own, so the `WTF` sibling is outside the sandbox and
/// a failed read says nothing about the game's configuration.
fn advanced_logging_enabled(log_dir: &Path) -> Option<bool> {
    let parent = log_dir.parent()?;
    let text = std::fs::read_to_string(parent.join("WTF").join("Config.wtf")).ok()?;
    Some(text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("SET advancedCombatLogging") && line.rsplit(' ').next() == Some("\"1\"")
    }))
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

fn local_clock() -> (i32, i32) {
    let seconds = now_unix_ms().div_euclid(1_000) as libc::time_t;
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    // `localtime_r` follows the system's configured zone, including DST.
    if unsafe { libc::localtime_r(&seconds, &mut local) }.is_null() {
        return (local_year(), 0);
    }
    (local.tm_year + 1900, utc_offset_minutes(local.tm_gmtoff))
}

fn utc_offset_minutes(seconds: libc::c_long) -> i32 {
    (seconds / 60) as i32
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
                spell_id: 0,
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

#[cfg(test)]
mod tests {
    use super::{
        CAPTURE_RESTART_FAILED_PROBLEM, CAPTURE_STOPPED_PROBLEM, Problem, RecoveryAction,
        clear_recovered_capture_problems, utc_offset_minutes,
    };

    #[test]
    fn timezone_offset_converts_seconds_to_minutes() {
        assert_eq!(utc_offset_minutes(7_200), 120);
        assert_eq!(utc_offset_minutes(-12_600), -210);
    }

    #[test]
    fn recovered_capture_problems_are_removed_without_touching_other_problems() {
        let preserved = Problem {
            summary: "The capture target could not be saved.".to_owned(),
            safe_detail: Some("disk full".to_owned()),
            occurred_unix_ms: 42,
            recovery_action: Some(RecoveryAction::ReselectCaptureTarget),
        };
        let mut problems = vec![
            Problem {
                summary: CAPTURE_STOPPED_PROBLEM.to_owned(),
                safe_detail: None,
                occurred_unix_ms: 1,
                recovery_action: Some(RecoveryAction::ReselectCaptureTarget),
            },
            Problem {
                summary: CAPTURE_RESTART_FAILED_PROBLEM.to_owned(),
                safe_detail: None,
                occurred_unix_ms: 2,
                recovery_action: Some(RecoveryAction::ReselectCaptureTarget),
            },
            Problem {
                summary: CAPTURE_RESTART_FAILED_PROBLEM.to_owned(),
                safe_detail: None,
                occurred_unix_ms: 3,
                recovery_action: Some(RecoveryAction::ReselectCaptureTarget),
            },
            preserved.clone(),
        ];

        assert!(clear_recovered_capture_problems(&mut problems));
        assert_eq!(problems, vec![preserved]);
        assert!(!clear_recovered_capture_problems(&mut problems));
    }
}
