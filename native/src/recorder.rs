// SPDX-License-Identifier: GPL-3.0-or-later

//! gpu-screen-recorder lifecycle adapter.
//!
//! One long-lived GSR replay-buffer child, at most one active recording, and
//! the signal/hook protocol: a save signal sized to the pre-roll (see
//! `ReplaySave`) saves the replay, SIGRTMIN toggles the regular recording, and
//! a generated `-sc` hook script appends
//! `epoch_ms<TAB>kind<TAB>path` records that `poll`/`end` correlate against the
//! configured replay/regular directories.
//!
//! - The hook receives `$1 = saved artifact path, $2 = event kind`.
//! - Restart delays are 2, 4, 8, 16, then capped 30 seconds indefinitely. The
//!   attempt counter resets on a deliberate `arm`, or when the child that
//!   exited had stayed up for `STABLE_CHILD`: a crash loop keeps backing off,
//!   an occasional exit during a long session starts over at 2 seconds.
//! - Crash recovery of interrupted recordings is not Recorder's job; the only
//!   persistent state is the truncate-on-arm events file.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::CaptureSettings;
use crate::domain::{Category, Codec, RecordingId, ReplayStorage};
use crate::process;
use crate::storage::now_unix_ms;

/// Everything Recorder needs to arm a capture session, assembled by the
/// coordinator from validated configuration.
#[derive(Clone, Debug)]
pub struct CaptureConfig {
    /// GSR executable; the production value is `gpu-screen-recorder` on PATH.
    pub gsr_binary: PathBuf,
    /// App-private directory for the portal token, hook script, events file,
    /// and recorder log.
    pub data_dir: PathBuf,
    /// Capture root containing the `replay`, `regular`, and `staging`
    /// subdirectories.
    pub capture_root: PathBuf,
    pub settings: CaptureSettings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordingMode {
    Automatic,
    Manual,
    Test(Category),
}

#[derive(Clone, Debug)]
pub struct StartRequest {
    pub id: RecordingId,
    /// Detection delay plus lead-in, already clamped by the coordinator.
    pub requested_replay_ms: u64,
}

/// The smallest GSR replay save covering a pre-roll. GSR adds its keyframe
/// interval to a partial save, so each one holds at least its nominal length
/// and the finalize trim stays exact. Saving the whole buffer only to keep a
/// few seconds would write hundreds of megabytes per capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaySave {
    None,
    Last10s,
    Last30s,
    Last60s,
    Full,
}

impl ReplaySave {
    pub fn for_pre_roll(requested_replay_ms: u64) -> Self {
        match requested_replay_ms {
            0 => Self::None,
            1..=10_000 => Self::Last10s,
            10_001..=30_000 => Self::Last30s,
            30_001..=60_000 => Self::Last60s,
            _ => Self::Full,
        }
    }

    fn signal(self) -> Option<i32> {
        match self {
            Self::None => None,
            Self::Last10s => Some(process::sigrtmin() + 1),
            Self::Last30s => Some(process::sigrtmin() + 2),
            Self::Last60s => Some(process::sigrtmin() + 3),
            Self::Full => Some(libc::SIGUSR1),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureArtifacts {
    /// GSR-saved replay pre-roll; missing falls back to regular-only.
    pub replay: Option<PathBuf>,
    pub regular: PathBuf,
    pub requested_replay_ms: u64,
    pub regular_started_at_ms: i64,
    pub regular_stopped_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTargetSelection {
    /// Reusable portal token when GSR has already written it; otherwise `poll`
    /// reports it later as `TargetTokenAvailable`.
    pub token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AudioDevices {
    pub outputs: Vec<AudioDevice>,
    pub inputs: Vec<AudioDevice>,
}

#[derive(Debug)]
pub enum RecorderError {
    /// A recording is already active.
    Busy,
    /// No live armed GSR child.
    NotArmed,
    /// The supplied recording ID is not the active one.
    WrongId,
    /// GSR produced no regular recording within the bounded wait.
    MissingRegularArtifact,
    InvalidSettings(String),
    /// Portal selection was denied/cancelled (GSR exit code 60).
    SelectionDenied {
        log_tail: String,
    },
    SpawnFailed {
        message: String,
        log_tail: String,
    },
    Io(io::Error),
}

impl From<io::Error> for RecorderError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecorderEvent {
    ChildExited {
        code: Option<i32>,
    },
    RestartScheduled {
        attempt: u32,
    },
    Restarted,
    /// A requested end resolved: `None` means the bounded wait produced no
    /// regular recording, which the coordinator reports and sweeps.
    CaptureEnded {
        artifacts: Option<CaptureArtifacts>,
    },
    RestartFailed {
        message: String,
    },
    /// The portal wrote (or replaced) the reusable capture-target token.
    TargetTokenAvailable(String),
}

/// Bounded waits. Tests shrink them; production uses the defaults.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// Post-spawn stability check before arm is considered successful.
    pub arm_stability: Duration,
    /// Wait for the hook's replay event after the save signal.
    pub replay_event: Duration,
    /// Wait for the hook's regular event after the stop SIGRTMIN.
    pub regular_event: Duration,
    /// Wait for the old child to exit during reselection/shutdown before
    /// escalating.
    pub exit_grace: Duration,
    /// Minimum spacing between the SIGRTMIN toggles GSR reads once per
    /// capture-loop iteration.
    pub toggle_gap: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            arm_stability: Duration::from_millis(500),
            replay_event: Duration::from_secs(20),
            regular_event: Duration::from_secs(30),
            exit_grace: Duration::from_secs(2),
            toggle_gap: Duration::from_millis(250),
        }
    }
}

const GSR_EXIT_SELECTION_DENIED: i32 = 60;
const MAX_RESTART_DELAY_SECONDS: u64 = 30;
/// A child that ran this long before exiting was not crash-looping.
const STABLE_CHILD: Duration = Duration::from_secs(60);
/// The portal token only changes after a target selection, so `poll` need
/// not reread it every coordinator tick.
const TOKEN_CHECK_INTERVAL: Duration = Duration::from_secs(2);

struct GsrEvent {
    timestamp_ms: i64,
    kind: String,
    path: PathBuf,
}

struct ActiveCapture {
    id: RecordingId,
    requested_replay_ms: u64,
    regular_started_at_ms: i64,
    /// `None` when no replay save was requested: the end resolves on the
    /// regular event alone.
    replay_deadline: Option<Instant>,
}

/// A requested end whose hook events have not all arrived yet. `poll` resolves
/// it so the coordinator thread never sleeps through GSR's flush and mux.
struct PendingEnd {
    config: CaptureConfig,
    active: ActiveCapture,
    regular_deadline: Instant,
    regular_stopped_at_ms: i64,
    regular: Option<PathBuf>,
    sigint_sent: bool,
}

pub struct Recorder {
    config: Option<CaptureConfig>,
    child: Option<Child>,
    spawned_at: Option<Instant>,
    desired_running: bool,
    restart_attempts: u32,
    restart_at_ms: Option<i64>,
    events_offset: u64,
    pending: Vec<GsrEvent>,
    active: Option<ActiveCapture>,
    ending: Option<PendingEnd>,
    last_token: Option<String>,
    token_checked_at: Option<Instant>,
    last_toggle_at: Option<Instant>,
    timeouts: Timeouts,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self::with_timeouts(Timeouts::default())
    }

    pub fn with_timeouts(timeouts: Timeouts) -> Self {
        Self {
            config: None,
            child: None,
            spawned_at: None,
            desired_running: false,
            restart_attempts: 0,
            restart_at_ms: None,
            events_offset: 0,
            pending: Vec::new(),
            active: None,
            ending: None,
            last_token: None,
            token_checked_at: None,
            last_toggle_at: None,
            timeouts,
        }
    }

    fn token_path(config: &CaptureConfig) -> PathBuf {
        config.data_dir.join("gsr-portal.token")
    }

    fn hook_path(config: &CaptureConfig) -> PathBuf {
        config.data_dir.join("gsr-hook.sh")
    }

    fn events_path(config: &CaptureConfig) -> PathBuf {
        config.data_dir.join("gsr-events.tsv")
    }

    fn log_path(config: &CaptureConfig) -> PathBuf {
        config.data_dir.join("gsr.log")
    }

    fn replay_dir(config: &CaptureConfig) -> PathBuf {
        config.capture_root.join("replay")
    }

    fn regular_dir(config: &CaptureConfig) -> PathBuf {
        config.capture_root.join("regular")
    }

    /// Validate GSR, prepare directories/hook/events/token, spawn the replay
    /// buffer, and confirm it stays alive. A deliberate arm resets the restart
    /// attempt counter.
    pub fn arm(&mut self, config: &CaptureConfig) -> Result<(), RecorderError> {
        if config.settings.audio_output.contains('|')
            || config
                .settings
                .audio_input
                .as_deref()
                .is_some_and(|input| input.contains('|'))
        {
            return Err(RecorderError::InvalidSettings(
                "audio device IDs must not contain '|'".to_string(),
            ));
        }
        // Check the replacement binary before touching a live capture. The
        // remaining setup errors occur only after the deliberate replacement
        // begins; invalid settings and an unavailable binary do not disarm.
        let version = Command::new(&config.gsr_binary)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match version {
            Ok(status) if status.success() => {}
            Ok(status) => {
                return Err(RecorderError::SpawnFailed {
                    message: format!("gpu-screen-recorder --version failed: {status}"),
                    log_tail: String::new(),
                });
            }
            Err(error) => {
                return Err(RecorderError::SpawnFailed {
                    message: format!("gpu-screen-recorder not available: {error}"),
                    log_tail: String::new(),
                });
            }
        }
        self.desired_running = false;
        self.restart_at_ms = None;
        if let Some(mut child) = self.child.take() {
            process::terminate(&mut child, self.timeouts.exit_grace)?;
        }
        self.active = None;

        fs::create_dir_all(&config.data_dir)?;
        for dir in ["replay", "regular", "staging"] {
            fs::create_dir_all(config.capture_root.join(dir))?;
        }
        let managed = config.capture_root.join("managed.txt");
        if !managed.exists() {
            fs::write(
                &managed,
                "This folder is managed by Warcraft Recorder, files in it may be automatically created, modified or deleted.",
            )?;
        }

        let events_path = Self::events_path(config);
        write_hook_script(&Self::hook_path(config), &events_path)?;
        fs::write(&events_path, b"")?;
        fs::write(Self::log_path(config), b"")?;
        self.events_offset = 0;
        self.pending.clear();

        let token_path = Self::token_path(config);
        if let Some(token) = &config.settings.capture_target_token
            && !token.is_empty()
            && !token_path.exists()
        {
            fs::write(&token_path, token)?;
        }

        self.spawn_child(config)?;
        self.config = Some(config.clone());
        self.desired_running = true;
        self.restart_attempts = 0;
        self.restart_at_ms = None;
        self.last_token = read_token(&token_path);
        self.token_checked_at = None;
        Ok(())
    }

    fn spawn_child(&mut self, config: &CaptureConfig) -> Result<(), RecorderError> {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::log_path(config))?;
        let mut command = Command::new(&config.gsr_binary);
        command
            // Stable Flatpak constrains the GTK process allocator arenas for
            // its RSS gate; the recorder must retain its own defaults.
            .env_remove("MALLOC_ARENA_MAX")
            .args(build_gsr_args(config))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let mut child = command
            .spawn()
            .map_err(|error| RecorderError::SpawnFailed {
                message: format!("failed to spawn gpu-screen-recorder: {error}"),
                log_tail: String::new(),
            })?;

        std::thread::sleep(self.timeouts.arm_stability);
        if let Some(status) = child.try_wait()? {
            let log_tail = process::read_log_tail(&Self::log_path(config));
            if status.code() == Some(GSR_EXIT_SELECTION_DENIED) {
                return Err(RecorderError::SelectionDenied { log_tail });
            }
            return Err(RecorderError::SpawnFailed {
                message: format!("gpu-screen-recorder exited immediately: {status}"),
                log_tail,
            });
        }
        self.child = Some(child);
        self.spawned_at = Some(Instant::now());
        Ok(())
    }

    /// Register the replay wait, save the pre-roll (`ReplaySave`, none for a
    /// zero pre-roll), and start the regular recording (SIGRTMIN). Never waits
    /// for media. Returns the regular recording's wall-clock start.
    pub fn begin(&mut self, request: StartRequest) -> Result<i64, RecorderError> {
        // A pending end still owns the hook events GSR has yet to write, and
        // `resolve_end` clears whatever is left over. Starting here would let
        // it swallow the new capture's replay event, costing it the entire
        // pre-roll. The coordinator defers instead, and this keeps that
        // invariant local to the recorder.
        if self.active.is_some() || self.ending.is_some() {
            return Err(RecorderError::Busy);
        }
        let child = self.live_child()?;
        let regular_started_at_ms = now_unix_ms();
        let save = ReplaySave::for_pre_roll(request.requested_replay_ms).signal();
        if let Some(signal) = save {
            process::send_signal(child, signal)?;
        }
        process::send_signal(child, process::sigrtmin())?;
        self.last_toggle_at = Some(Instant::now());
        self.active = Some(ActiveCapture {
            id: request.id,
            requested_replay_ms: request.requested_replay_ms,
            regular_started_at_ms,
            replay_deadline: save.map(|_| Instant::now() + self.timeouts.replay_event),
        });
        Ok(regular_started_at_ms)
    }

    /// Stop the regular recording and resolve its artifacts through `poll`.
    /// GSR needs however long the encoder flush and mux take, and the
    /// coordinator owns every piece of UI state while it waits, so the wait
    /// must not happen here. Missing replay stays tolerated; a missing regular
    /// recording arrives as `CaptureEnded { artifacts: None }`.
    ///
    /// Discarding a capture uses the same request: the coordinator sweeps the
    /// artifacts instead of finalizing them. Recorder never unlinks them.
    pub fn request_end(&mut self, id: &RecordingId) -> Result<(), RecorderError> {
        if self.ending.is_some() {
            return Err(RecorderError::Busy);
        }
        match &self.active {
            None => return Err(RecorderError::NotArmed),
            Some(active) if &active.id != id => return Err(RecorderError::WrongId),
            Some(_) => {}
        }
        // GSR reads the toggle as a flag, once per capture-loop iteration, so
        // two inside one iteration collapse into one and invert it for good. A
        // discard ends a capture from the batch that began it, so only that
        // case ever waits here.
        if let Some(sent_at) = self.last_toggle_at {
            let remaining = self.timeouts.toggle_gap.saturating_sub(sent_at.elapsed());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
        }
        let child = self.live_child()?;
        // The stop timestamp is sampled before the signal: the hook event
        // that arrives later is admitted against this bound, never a clock
        // read taken while the end resolves.
        let regular_stopped_at_ms = now_unix_ms();
        process::send_signal(child, process::sigrtmin())?;
        self.last_toggle_at = Some(Instant::now());
        let config = self.config.clone().expect("armed with config");
        let active = self.active.take().expect("checked above");
        self.ending = Some(PendingEnd {
            config,
            active,
            regular_deadline: Instant::now() + self.timeouts.regular_event,
            regular_stopped_at_ms,
            regular: None,
            sigint_sent: false,
        });
        Ok(())
    }

    /// Whether a replay-buffer child is currently available.
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// True between `request_end` and its `CaptureEnded`.
    pub fn is_ending(&self) -> bool {
        self.ending.is_some()
    }

    /// Shutdown only: drive the requested end to its conclusion so the
    /// finalization is queued before GSR is killed.
    ///
    /// `deadline` keeps quitting responsive. The full 30 s regular wait is
    /// right for a running app but not for a window the user just closed: a
    /// quit that appears hung invites a force-kill, which orphans GSR and
    /// leaves it capturing the screen forever. On expiry the end resolves with
    /// whatever arrived, exactly as a timed-out poll would.
    pub fn finish_end_blocking(&mut self, deadline: Instant) -> Vec<RecorderEvent> {
        let mut events = Vec::new();
        while self.ending.is_some() {
            self.poll_pending_end(&mut events);
            if self.ending.is_none() {
                break;
            }
            if Instant::now() >= deadline {
                self.resolve_end(None, &mut events);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        events
    }

    /// One nonblocking step of a requested end. The regular recording is
    /// awaited first against its own deadline, then the replay pre-roll
    /// against the absolute deadline taken at `begin`, if one was saved.
    fn poll_pending_end(&mut self, events: &mut Vec<RecorderEvent>) {
        let Some(config) = self.ending.as_ref().map(|ending| ending.config.clone()) else {
            return;
        };
        let _ = self.read_new_events(&config);
        let now = Instant::now();

        if self
            .ending
            .as_ref()
            .is_some_and(|ending| ending.regular.is_none())
        {
            let regular_stopped_at_ms = self
                .ending
                .as_ref()
                .map(|ending| ending.regular_stopped_at_ms)
                .expect("checked above");
            let regular = self.take_event(
                "regular",
                &Self::regular_dir(&config),
                regular_stopped_at_ms,
            );
            let ending = self.ending.as_mut().expect("checked above");
            ending.regular = regular;
            if ending.regular.is_none() {
                if now < ending.regular_deadline {
                    return;
                }
                // The toggle inverted, and it cannot be read back: replace the
                // child. Its exit resolves this end, and poll reports that
                // first so the coordinator disarms before a deferred capture
                // can start on a child that is going away.
                if !ending.sigint_sent {
                    ending.sigint_sent = true;
                    ending.regular_deadline = now + self.timeouts.exit_grace;
                    if let Some(child) = self.child.as_ref() {
                        let _ = process::send_signal(child, libc::SIGINT);
                    }
                    return;
                }
                // The child ignored SIGINT. Consume any valid replay event so
                // it is not counted as noise; the coordinator sweeps the file.
                let replay_bound = ending.active.regular_started_at_ms;
                let _ = self.take_event("replay", &Self::replay_dir(&config), replay_bound);
                self.resolve_end(None, events);
                return;
            }
        }

        let active = &self
            .ending
            .as_ref()
            .expect("regular resolution keeps the pending end")
            .active;
        // Without a save, any replay event is noise for `resolve_end`.
        let Some(replay_deadline) = active.replay_deadline else {
            self.resolve_end(None, events);
            return;
        };
        let replay_bound = active.regular_started_at_ms;
        let replay = self.take_event("replay", &Self::replay_dir(&config), replay_bound);
        if replay.is_none() && now < replay_deadline {
            return;
        }
        self.resolve_end(replay, events);
    }

    fn resolve_end(&mut self, replay: Option<PathBuf>, events: &mut Vec<RecorderEvent>) {
        let Some(ending) = self.ending.take() else {
            return;
        };
        // Any event still pending after the session was noise (wrong kind,
        // stale, duplicate, or outside the managed directories).
        if !self.pending.is_empty() {
            tracing::debug!(
                count = self.pending.len(),
                "ignored unexpected GSR hook events"
            );
            self.pending.clear();
        }
        let artifacts = ending.regular.map(|regular| CaptureArtifacts {
            replay,
            regular,
            requested_replay_ms: ending.active.requested_replay_ms,
            regular_started_at_ms: ending.active.regular_started_at_ms,
            regular_stopped_at_ms: ending.regular_stopped_at_ms,
        });
        events.push(RecorderEvent::CaptureEnded { artifacts });
    }

    /// Token contract: stop the child, invalidate the token only after it
    /// exited, and re-arm to trigger portal selection. A denied selection
    /// restores the previous usable token.
    pub fn reselect_target(
        &mut self,
        config: &CaptureConfig,
    ) -> Result<CaptureTargetSelection, RecorderError> {
        let token_path = Self::token_path(config);
        if let Some(mut child) = self.child.take() {
            process::terminate(&mut child, self.timeouts.exit_grace)?;
        }
        let previous = read_token(&token_path);
        match fs::remove_file(&token_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Do not let arm restore the token we just invalidated. The portal
        // must select a new target; once it writes the replacement token,
        // poll reports it to the coordinator.
        let mut reselect_config = config.clone();
        reselect_config.settings.capture_target_token = None;
        match self.arm(&reselect_config) {
            Ok(()) => {
                let token = read_token(&token_path);
                self.last_token = token.clone();
                Ok(CaptureTargetSelection { token })
            }
            Err(error) => {
                // Cancellation preserves the prior usable target.
                if let Some(previous) = previous {
                    let _ = fs::write(&token_path, &previous);
                    let _ = self.arm(config);
                }
                Err(error)
            }
        }
    }

    /// `gpu-screen-recorder --list-audio-devices` with the recorded 2 s
    /// timeout; defaults are always present.
    pub fn audio_devices(&mut self) -> Result<AudioDevices, RecorderError> {
        let binary = self
            .config
            .as_ref()
            .map(|config| config.gsr_binary.clone())
            .unwrap_or_else(|| PathBuf::from("gpu-screen-recorder"));
        let mut child = Command::new(binary)
            .env_remove("MALLOC_ARENA_MAX")
            .arg("--list-audio-devices")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| RecorderError::SpawnFailed {
                message: format!("audio discovery failed: {error}"),
                log_tail: String::new(),
            })?;
        let timed_out = !process::wait_with_timeout(&mut child, Duration::from_secs(2))?;
        let status = if timed_out {
            // The process may have exited between the timeout check and the
            // escalation; avoid reporting a spurious kill failure in that
            // race and still collect its final status.
            match child.try_wait()? {
                Some(status) => status,
                None => {
                    child.kill()?;
                    child.wait()?
                }
            }
        } else {
            child
                .try_wait()?
                .ok_or_else(|| io::Error::other("audio discovery exited without a status"))?
        };
        let mut text = String::new();
        if let Some(stdout) = child.stdout.as_mut() {
            stdout.read_to_string(&mut text)?;
        }
        text.push('\n');
        if let Some(stderr) = child.stderr.as_mut() {
            stderr.read_to_string(&mut text)?;
        }
        if timed_out {
            return Err(RecorderError::SpawnFailed {
                message: "audio discovery timed out after 2 seconds".to_string(),
                log_tail: text_tail(&text),
            });
        }
        if !status.success() {
            return Err(RecorderError::SpawnFailed {
                message: format!("audio discovery exited unsuccessfully: {status}"),
                log_tail: text_tail(&text),
            });
        }
        Ok(parse_audio_devices(&text))
    }

    /// Advance restarts and surface child/token changes. The coordinator
    /// calls this on its loop; Recorder keeps no timer thread.
    pub fn poll(&mut self, now_ms: i64) -> Vec<RecorderEvent> {
        let mut events = Vec::new();
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    self.active = None;
                    // A capture waiting to be written will never be: nothing
                    // is left to write it. Expire both waits so
                    // `poll_pending_end` resolves in this same batch, instead
                    // of holding the coordinator's ending state, and the
                    // recovery actions it blocks, against a dead child. The
                    // replay deadline matters too: a child that wrote the
                    // regular event and then died would otherwise keep the end
                    // open for the rest of its pre-roll wait.
                    if let Some(ending) = self.ending.as_mut() {
                        let now = Instant::now();
                        ending.regular_deadline = now;
                        ending.active.replay_deadline = ending.active.replay_deadline.map(|_| now);
                        ending.sigint_sent = true;
                    }
                    events.push(RecorderEvent::ChildExited {
                        code: status.code(),
                    });
                    if self
                        .spawned_at
                        .is_some_and(|spawned_at| spawned_at.elapsed() >= STABLE_CHILD)
                    {
                        self.restart_attempts = 0;
                    }
                    if self.desired_running {
                        self.schedule_restart(now_ms, &mut events);
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "GSR child status check failed"),
            }
        } else if self.desired_running
            && self
                .restart_at_ms
                .is_some_and(|deadline| now_ms >= deadline)
        {
            self.restart_at_ms = None;
            let config = self.config.clone().expect("desired_running implies config");
            match self.spawn_child(&config) {
                // The attempt counter survives the respawn itself; it resets
                // once this child proves stable (see the exit branch above).
                Ok(()) => events.push(RecorderEvent::Restarted),
                Err(error) => {
                    events.push(RecorderEvent::RestartFailed {
                        message: format!("{error:?}"),
                    });
                    self.schedule_restart(now_ms, &mut events);
                }
            }
        }

        if let Some(config) = &self.config
            && self
                .token_checked_at
                .is_none_or(|checked_at| checked_at.elapsed() >= TOKEN_CHECK_INTERVAL)
        {
            self.token_checked_at = Some(Instant::now());
            if let Some(token) = read_token(&Self::token_path(config))
                && self.last_token.as_deref() != Some(token.as_str())
            {
                self.last_token = Some(token.clone());
                events.push(RecorderEvent::TargetTokenAvailable(token));
            }
        }
        self.poll_pending_end(&mut events);
        events
    }

    fn schedule_restart(&mut self, now_ms: i64, events: &mut Vec<RecorderEvent>) {
        self.restart_attempts += 1;
        let delay_seconds = MAX_RESTART_DELAY_SECONDS.min(1u64 << self.restart_attempts.min(63));
        self.restart_at_ms = Some(now_ms + (delay_seconds * 1_000) as i64);
        events.push(RecorderEvent::RestartScheduled {
            attempt: self.restart_attempts,
        });
    }

    /// Stop the replay child with the SIGINT-then-kill escalation and leave no
    /// child or scheduled restart behind.
    pub fn shutdown(&mut self) -> Result<(), RecorderError> {
        self.desired_running = false;
        self.restart_at_ms = None;
        self.active = None;
        self.ending = None;
        if let Some(mut child) = self.child.take() {
            process::terminate(&mut child, self.timeouts.exit_grace)?;
        }
        Ok(())
    }

    fn live_child(&mut self) -> Result<&Child, RecorderError> {
        let alive = match self.child.as_mut() {
            Some(child) => child.try_wait()?.is_none(),
            None => false,
        };
        if alive {
            Ok(self.child.as_ref().expect("alive"))
        } else {
            Err(RecorderError::NotArmed)
        }
    }

    fn read_new_events(&mut self, config: &CaptureConfig) -> io::Result<()> {
        let path = Self::events_path(config);
        let Ok(mut file) = fs::File::open(&path) else {
            return Ok(());
        };
        let size = file.metadata()?.len();
        if size <= self.events_offset {
            return Ok(());
        }
        file.seek(SeekFrom::Start(self.events_offset))?;
        let mut buffer = String::new();
        file.read_to_string(&mut buffer)?;
        // Only consume complete lines; a partially written record stays for
        // the next read.
        let complete = match buffer.rfind('\n') {
            Some(last_newline) => &buffer[..=last_newline],
            None => return Ok(()),
        };
        self.events_offset += complete.len() as u64;
        for line in complete.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.splitn(3, '\t');
            let timestamp = fields.next().unwrap_or("");
            let kind = fields.next().unwrap_or("");
            let path = fields.next().unwrap_or("");
            let Ok(timestamp_ms) = timestamp.parse::<i64>() else {
                tracing::debug!("ignored GSR hook event with a malformed timestamp");
                continue;
            };
            if path.is_empty() || !matches!(kind, "regular" | "replay") {
                tracing::debug!(kind, "ignored unexpected GSR hook event");
                continue;
            }
            self.pending.push(GsrEvent {
                timestamp_ms,
                kind: kind.to_string(),
                path: PathBuf::from(path),
            });
        }
        Ok(())
    }

    /// Consume the already-read hook event of `kind` written into `directory`
    /// with a timestamp at or after `lower_bound_ms`. Candidates are scanned
    /// in arrival order, skipping stale or wrong-directory events of the same
    /// kind; on a miss nothing is removed or counted here, and `resolve_end`
    /// remains the sole cleanup/count point for the pending end.
    fn take_event(&mut self, kind: &str, directory: &Path, lower_bound_ms: i64) -> Option<PathBuf> {
        let canonical_dir = directory.canonicalize().ok()?;
        let matched = self.pending.iter().position(|event| {
            event.kind == kind
                && event.timestamp_ms >= lower_bound_ms
                && event
                    .path
                    .parent()
                    .and_then(|parent| parent.canonicalize().ok())
                    .is_some_and(|parent| parent == canonical_dir)
        });
        if let Some(index) = matched {
            return Some(self.pending.remove(index).path);
        }
        None
    }
}

/// Paths and devices stay single `OsString` arguments; no shell is involved.
fn build_gsr_args(config: &CaptureConfig) -> Vec<OsString> {
    let settings = &config.settings;
    let codec = match settings.codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    };
    let storage = match settings.replay_storage {
        ReplayStorage::Ram => "ram",
        ReplayStorage::Disk => "disk",
    };
    let mut args: Vec<OsString> = vec![
        "-w".into(),
        "portal".into(),
        "-restore-portal-session".into(),
        "yes".into(),
        "-portal-session-token-filepath".into(),
        Recorder::token_path(config).into_os_string(),
        "-r".into(),
        settings.replay_buffer_seconds.to_string().into(),
        "-replay-storage".into(),
        storage.into(),
        "-restart-replay-on-save".into(),
        "no".into(),
        "-c".into(),
        "mkv".into(),
        "-f".into(),
        settings.fps.to_string().into(),
        "-bm".into(),
        "cbr".into(),
        "-q".into(),
        settings.bitrate_kbps.to_string().into(),
        "-k".into(),
        codec.into(),
        "-ac".into(),
        "aac".into(),
        "-cursor".into(),
        if settings.capture_cursor { "yes" } else { "no" }.into(),
        "-o".into(),
        Recorder::replay_dir(config).into_os_string(),
        "-ro".into(),
        Recorder::regular_dir(config).into_os_string(),
        "-sc".into(),
        Recorder::hook_path(config).into_os_string(),
        "-v".into(),
        "no".into(),
    ];
    let mut audio: Vec<&str> = Vec::new();
    if !settings.audio_output.is_empty() {
        audio.push(settings.audio_output.as_str());
    }
    if let Some(input) = settings.audio_input.as_deref()
        && !input.is_empty()
        && !audio.contains(&input)
    {
        audio.push(input);
    }
    if !audio.is_empty() {
        args.push("-a".into());
        args.push(audio.join("|").into());
    }
    args
}

/// GSR invokes the hook as `<script> <saved path> <kind>`. The events-file
/// path is embedded literally; hook output is never executed.
fn write_hook_script(hook_path: &Path, events_path: &Path) -> io::Result<()> {
    let script = format!(
        "#!/bin/sh\n# generated by Warcraft Recorder; $1 = saved artifact path, $2 = event kind\nprintf '%s\\t%s\\t%s\\n' \"$(date +%s%3N)\" \"$2\" \"$1\" >> \"{}\"\n",
        events_path.display()
    );
    fs::write(hook_path, script)?;
    fs::set_permissions(hook_path, fs::Permissions::from_mode(0o755))
}

fn read_token(path: &Path) -> Option<String> {
    let token = fs::read_to_string(path).ok()?;
    let token = token.trim().to_string();
    if token.is_empty() { None } else { Some(token) }
}

fn text_tail(text: &str) -> String {
    let start = text
        .len()
        .saturating_sub(usize::try_from(process::LOG_TAIL_BYTES).unwrap_or(usize::MAX));
    String::from_utf8_lossy(&text.as_bytes()[start..]).into_owned()
}

/// `--list-audio-devices` prints one `name|description` line per source. The
/// defaults always come first; every other source is passed to `-a` as
/// `device:<name>` and sorted by its PulseAudio/PipeWire name: `*.monitor`
/// sources are outputs, everything else (virtual mics included) is an input.
fn parse_audio_devices(text: &str) -> AudioDevices {
    let device = |id: &str, detail: &str| AudioDevice {
        id: id.to_string(),
        label: if detail.is_empty() { id } else { detail }.to_string(),
    };
    let mut devices = AudioDevices {
        outputs: vec![device("default_output", "Default output device")],
        inputs: vec![device("default_input", "Default input device")],
    };
    for line in text.lines() {
        let Some((name, detail)) = line.split_once('|') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.starts_with("default_") {
            continue;
        }
        let list = if name.ends_with(".monitor") {
            &mut devices.outputs
        } else {
            &mut devices.inputs
        };
        let id = format!("device:{name}");
        if !list.iter().any(|existing| existing.id == id) {
            list.push(device(&id, detail.trim()));
        }
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_gsr() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/native/bin/fake-gsr.sh")
    }

    fn test_timeouts() -> Timeouts {
        Timeouts {
            arm_stability: Duration::from_millis(150),
            replay_event: Duration::from_millis(300),
            regular_event: Duration::from_millis(300),
            exit_grace: Duration::from_millis(500),
            toggle_gap: Duration::from_millis(20),
        }
    }

    fn test_config(name: &str) -> CaptureConfig {
        let root = crate::storage::test_root(&format!("recorder-{name}"));
        CaptureConfig {
            gsr_binary: fake_gsr(),
            data_dir: root.join("data dir with späce"),
            capture_root: root.join("capture"),
            settings: CaptureSettings::default(),
        }
    }

    fn append_event_at(config: &CaptureConfig, timestamp_ms: i64, kind: &str, path: &Path) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(Recorder::events_path(config))
            .unwrap();
        writeln!(file, "{timestamp_ms}\t{kind}\t{}", path.display()).unwrap();
    }

    /// Append an event stamped with the current wall clock.
    fn append_event(config: &CaptureConfig, kind: &str, path: &Path) {
        append_event_at(config, now_unix_ms(), kind, path);
    }

    fn touch(path: &Path) {
        fs::write(path, b"media").unwrap();
    }

    /// Signals the fake GSR has handled, once it has handled the start toggle.
    /// A save signal is sent before the toggle, so it shows up by then too.
    fn received_signals(config: &CaptureConfig) -> Vec<String> {
        let path = config.data_dir.join("fake-signals");
        let toggle = process::sigrtmin().to_string();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let signals: Vec<String> = fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            if signals.contains(&toggle) || Instant::now() >= deadline {
                return signals;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Drive a requested end to its bounded conclusion, the way shutdown does,
    /// and return whatever artifacts it produced.
    fn end_artifacts(recorder: &mut Recorder) -> Option<CaptureArtifacts> {
        recorder
            .finish_end_blocking(Instant::now() + Duration::from_secs(5))
            .into_iter()
            .find_map(|event| match event {
                RecorderEvent::CaptureEnded { artifacts } => Some(artifacts),
                _ => None,
            })
            .expect("a requested end always resolves")
    }

    #[test]
    fn argv_preserves_awkward_paths_and_joins_audio_sources() {
        let mut config = test_config("argv");
        config.settings.audio_output = "device:out put".to_string();
        config.settings.audio_input = Some("device:mic".to_string());
        let args = build_gsr_args(&config);
        // The data dir contains a space and a non-ASCII byte; it stays one arg.
        assert!(args.contains(&config.data_dir.join("gsr-hook.sh").into_os_string()));
        assert_eq!(
            args[args.len() - 2..],
            ["-a".into(), "device:out put|device:mic".into()] as [OsString; 2]
        );

        // Duplicate input collapses; empty audio drops -a entirely.
        config.settings.audio_input = Some("device:out put".to_string());
        let args = build_gsr_args(&config);
        assert_eq!(args.last().unwrap(), &OsString::from("device:out put"));
        config.settings.audio_output = String::new();
        config.settings.audio_input = None;
        let args = build_gsr_args(&config);
        assert!(!args.contains(&OsString::from("-a")));
    }

    #[test]
    fn lifecycle_returns_replay_and_regular_artifacts() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let config = test_config("lifecycle");
        recorder.arm(&config).unwrap();
        // Arm truncated the events file and spawned a live child.
        assert_eq!(fs::read(Recorder::events_path(&config)).unwrap(), b"");

        let id = RecordingId::new();
        recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 12_000,
            })
            .unwrap();
        // A second begin cannot disturb the active session.
        assert!(matches!(
            recorder.begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
            }),
            Err(RecorderError::Busy)
        ));
        // Ending the wrong ID is rejected and the session stays live.
        assert!(matches!(
            recorder.request_end(&RecordingId::new()),
            Err(RecorderError::WrongId)
        ));

        let replay = Recorder::replay_dir(&config).join("Replay_1.mkv");
        let regular = Recorder::regular_dir(&config).join("Video_1.mkv");
        touch(&replay);
        touch(&regular);
        // Noise: wrong kind, outside directory, and duplicates are ignored.
        append_event(&config, "screenshot", &replay);
        append_event(&config, "replay", &config.capture_root.join("Replay_x.mkv"));
        append_event(&config, "replay", &replay);
        append_event(&config, "replay", &replay);

        recorder.request_end(&id).unwrap();
        // The regular post-save event arrives after the stop signal; its
        // timestamp must clear the stop bound.
        append_event(&config, "regular", &regular);
        assert!(recorder.is_ending());
        let artifacts = end_artifacts(&mut recorder);
        let artifacts = artifacts.expect("regular artifact");
        assert_eq!(artifacts.replay.as_deref(), Some(replay.as_path()));
        assert_eq!(artifacts.regular, regular);
        assert_eq!(artifacts.requested_replay_ms, 12_000);
        assert!(artifacts.regular_stopped_at_ms >= artifacts.regular_started_at_ms);
        // A 12 s pre-roll asked GSR for its 30 s save, not the whole buffer.
        let signals = received_signals(&config);
        assert!(signals.contains(&(process::sigrtmin() + 2).to_string()));
        assert!(!signals.contains(&"USR1".to_string()));
        recorder.shutdown().unwrap();
    }

    #[test]
    fn replay_save_is_the_smallest_covering_the_pre_roll() {
        for (requested_replay_ms, save) in [
            (0, ReplaySave::None),
            (1, ReplaySave::Last10s),
            (10_000, ReplaySave::Last10s),
            (10_001, ReplaySave::Last30s),
            (30_000, ReplaySave::Last30s),
            (60_000, ReplaySave::Last60s),
            (60_001, ReplaySave::Full),
        ] {
            assert_eq!(
                ReplaySave::for_pre_roll(requested_replay_ms),
                save,
                "{requested_replay_ms}"
            );
        }
    }

    #[test]
    fn zero_pre_roll_saves_nothing_and_ends_on_the_regular_event() {
        let mut recorder = Recorder::with_timeouts(Timeouts {
            replay_event: Duration::from_secs(60),
            ..test_timeouts()
        });
        let config = test_config("no-replay");
        recorder.arm(&config).unwrap();

        let id = RecordingId::new();
        recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 0,
            })
            .unwrap();
        // A replay event that belongs to nothing must not attach.
        let replay = Recorder::replay_dir(&config).join("Replay_1.mkv");
        let regular = Recorder::regular_dir(&config).join("Video_1.mkv");
        touch(&replay);
        touch(&regular);
        append_event(&config, "replay", &replay);
        recorder.request_end(&id).unwrap();
        append_event(&config, "regular", &regular);

        // One poll resolves it: nothing waits out the replay deadline.
        let artifacts = recorder
            .poll(now_unix_ms())
            .into_iter()
            .find_map(|event| match event {
                RecorderEvent::CaptureEnded { artifacts } => artifacts,
                _ => None,
            })
            .expect("ended on the regular event");
        assert_eq!(artifacts.replay, None);
        assert_eq!(artifacts.regular, regular);
        // Only the start/stop toggles reached GSR.
        let toggle = process::sigrtmin().to_string();
        let signals = received_signals(&config);
        assert!(signals.contains(&toggle));
        assert!(
            signals.iter().all(|signal| *signal == toggle),
            "{signals:?}"
        );
        recorder.shutdown().unwrap();
    }

    #[test]
    fn missing_replay_is_tolerated_and_missing_regular_is_an_error() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let config = test_config("missing");
        recorder.arm(&config).unwrap();

        let id = RecordingId::new();
        recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 5_000,
            })
            .unwrap();
        let regular = Recorder::regular_dir(&config).join("Video_1.mkv");
        touch(&regular);
        recorder.request_end(&id).unwrap();
        // The regular post-save event arrives after the stop signal; its
        // timestamp must clear the stop bound.
        append_event(&config, "regular", &regular);
        let artifacts = end_artifacts(&mut recorder).expect("regular artifact");
        assert_eq!(artifacts.replay, None);
        assert_eq!(artifacts.regular, regular);

        // Second recording produces no regular event at all.
        let id = RecordingId::new();
        recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 0,
            })
            .unwrap();
        recorder.request_end(&id).unwrap();
        assert!(
            end_artifacts(&mut recorder).is_none(),
            "a session with no regular event resolves to no artifacts"
        );
        // The toggle desynced, so the child is replaced, not reused.
        assert!(matches!(
            recorder.begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
            }),
            Err(RecorderError::NotArmed)
        ));
        recorder.shutdown().unwrap();
    }

    #[test]
    fn stale_same_directory_regular_does_not_win_and_current_artifacts_do() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let config = test_config("stale");
        recorder.arm(&config).unwrap();

        let id = RecordingId::new();
        let started_at_ms = recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 12_000,
            })
            .unwrap();
        let stale_regular = Recorder::regular_dir(&config).join("Video_stale.mkv");
        let current_regular = Recorder::regular_dir(&config).join("Video_current.mkv");
        let replay = Recorder::replay_dir(&config).join("Replay_1.mkv");
        touch(&stale_regular);
        touch(&current_regular);
        touch(&replay);
        // A stale regular event from a previous session shares the canonical
        // regular directory, so only its timestamp distinguishes it.
        append_event_at(&config, 0, "regular", &stale_regular);
        // The pre-roll event arrives exactly when the recording began.
        append_event_at(&config, started_at_ms, "replay", &replay);

        recorder.request_end(&id).unwrap();
        let stopped_at_ms = recorder.ending.as_ref().unwrap().regular_stopped_at_ms;
        // The current regular event arrives exactly at the stop bound; the
        // inclusive lower bound must admit it.
        append_event_at(&config, stopped_at_ms, "regular", &current_regular);

        let artifacts = end_artifacts(&mut recorder).expect("current regular artifact");
        assert_eq!(artifacts.regular, current_regular);
        assert_eq!(artifacts.replay.as_deref(), Some(replay.as_path()));
        assert_eq!(artifacts.regular_stopped_at_ms, stopped_at_ms);
        recorder.shutdown().unwrap();
    }

    #[test]
    fn restart_schedule_caps_resets_and_stops() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let config = test_config("restart");
        recorder.arm(&config).unwrap();

        let mut now_ms = 1_000_000i64;
        let mut delays = Vec::new();
        for _ in 0..6 {
            // Kill the child and observe the scheduled delay.
            process::send_signal(recorder.child.as_ref().unwrap(), libc::SIGKILL).unwrap();
            recorder.child.as_mut().unwrap().wait().unwrap();
            let events = recorder.poll(now_ms);
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, RecorderEvent::ChildExited { .. }))
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, RecorderEvent::RestartScheduled { .. }))
            );
            let delay = recorder.restart_at_ms.expect("restart scheduled") - now_ms;
            delays.push(delay);
            // Nothing happens before the deadline.
            assert!(recorder.poll(now_ms + delay - 1).is_empty());
            now_ms += delay;
            let events = recorder.poll(now_ms);
            assert!(
                events.contains(&RecorderEvent::Restarted),
                "expected restart, got {events:?}"
            );
        }
        assert_eq!(delays, vec![2_000, 4_000, 8_000, 16_000, 30_000, 30_000]);

        // A failed automatic respawn keeps retrying with the capped delay;
        // removing the failure marker allows the next scheduled attempt to
        // recover without resetting the attempt counter.
        fs::write(config.data_dir.join("fake-exit"), "1").unwrap();
        process::send_signal(recorder.child.as_ref().unwrap(), libc::SIGKILL).unwrap();
        recorder.child.as_mut().unwrap().wait().unwrap();
        let events = recorder.poll(now_ms);
        assert!(events.contains(&RecorderEvent::RestartScheduled { attempt: 7 }));
        assert_eq!(recorder.restart_at_ms, Some(now_ms + 30_000));
        let events = recorder.poll(now_ms + 30_000);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RecorderEvent::RestartFailed { .. }))
        );
        assert!(events.contains(&RecorderEvent::RestartScheduled { attempt: 8 }));
        let retry_at = recorder.restart_at_ms;
        assert_eq!(retry_at, Some(now_ms + 60_000));
        fs::remove_file(config.data_dir.join("fake-exit")).unwrap();
        assert!(
            recorder
                .poll(retry_at.unwrap())
                .contains(&RecorderEvent::Restarted)
        );

        // A child that stayed up long enough starts the backoff over.
        let now_ms = retry_at.unwrap();
        recorder.spawned_at = Instant::now().checked_sub(STABLE_CHILD);
        process::send_signal(recorder.child.as_ref().unwrap(), libc::SIGKILL).unwrap();
        recorder.child.as_mut().unwrap().wait().unwrap();
        let events = recorder.poll(now_ms);
        assert!(events.contains(&RecorderEvent::RestartScheduled { attempt: 1 }));
        assert_eq!(recorder.restart_at_ms, Some(now_ms + 2_000));
        assert!(
            recorder
                .poll(now_ms + 2_000)
                .contains(&RecorderEvent::Restarted)
        );

        // A deliberate arm resets the attempt counter.
        recorder.arm(&config).unwrap();
        process::send_signal(recorder.child.as_ref().unwrap(), libc::SIGKILL).unwrap();
        recorder.child.as_mut().unwrap().wait().unwrap();
        let events = recorder.poll(now_ms);
        assert!(events.contains(&RecorderEvent::RestartScheduled { attempt: 1 }));
        assert_eq!(recorder.restart_at_ms, Some(now_ms + 2_000));

        // Shutdown cancels the pending restart and leaves no child.
        recorder.shutdown().unwrap();
        assert!(recorder.poll(now_ms + 60_000).is_empty());
        assert!(recorder.child.is_none());
    }

    #[test]
    fn reselection_rotates_the_token_and_denial_restores_it() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let mut config = test_config("reselect");
        config.settings.capture_target_token = Some("configured-old-token".to_string());
        recorder.arm(&config).unwrap();
        let token_path = Recorder::token_path(&config);
        fs::write(&token_path, "old-token").unwrap();
        // Make poll adopt the current token first.
        recorder.poll(now_unix_ms());

        let selection = recorder.reselect_target(&config).unwrap();
        assert!(recorder.is_running());
        // The old token was deleted; GSR has not written a new one yet.
        assert_eq!(selection.token, None);
        assert!(!token_path.exists());
        // The portal writes the new token later; poll reports it.
        fs::write(&token_path, "new-token").unwrap();
        let events = recorder.poll(now_unix_ms());
        assert!(events.contains(&RecorderEvent::TargetTokenAvailable(
            "new-token".to_string()
        )));

        // A denied reselection restores the previous usable token.
        fs::write(config.data_dir.join("fake-exit"), "60").unwrap();
        let denied = recorder.reselect_target(&config);
        assert!(matches!(denied, Err(RecorderError::SelectionDenied { .. })));
        assert_eq!(fs::read_to_string(&token_path).unwrap(), "new-token");
        fs::remove_file(config.data_dir.join("fake-exit")).unwrap();
        recorder.shutdown().unwrap();
    }

    #[test]
    fn audio_discovery_maps_gsr_sources_to_device_ids() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        recorder.arm(&test_config("audio")).unwrap();
        let devices = recorder.audio_devices().unwrap();
        let ids = |list: &[AudioDevice]| list.iter().map(|d| d.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(&devices.outputs),
            [
                "default_output",
                "device:alsa_output.pci.analog-stereo.monitor"
            ]
        );
        assert_eq!(
            ids(&devices.inputs),
            ["default_input", "device:alsa_input.usb-mic"]
        );
        assert_eq!(devices.inputs[1].label, "Fake USB Microphone");
        assert_eq!(devices.outputs[0].label, "Default output device");
        recorder.shutdown().unwrap();

        // Duplicates collapse, lines without `|` are ignored, an empty
        // description falls back to the id, and a virtual mic is an input.
        let parsed = parse_audio_devices(
            "bluez_output.x.monitor|Headset\nbluez_output.x.monitor|Headset\nnoise\nbluez_output.y.monitor|\neasyeffects_source|Easy Effects Source\n",
        );
        assert_eq!(
            ids(&parsed.outputs),
            [
                "default_output",
                "device:bluez_output.x.monitor",
                "device:bluez_output.y.monitor"
            ]
        );
        assert_eq!(parsed.outputs[2].label, "device:bluez_output.y.monitor");
        assert_eq!(
            ids(&parsed.inputs),
            ["default_input", "device:easyeffects_source"]
        );
    }

    #[test]
    fn begin_requires_a_live_armed_child() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        assert!(matches!(
            recorder.begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
            }),
            Err(RecorderError::NotArmed)
        ));
        assert!(matches!(
            recorder.request_end(&RecordingId::new()),
            Err(RecorderError::NotArmed)
        ));
    }

    #[test]
    fn invalid_arm_does_not_disarm_existing_capture() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let config = test_config("invalid-live");
        recorder.arm(&config).unwrap();

        let mut invalid = config.clone();
        invalid.settings.audio_output = "a|b".to_string();
        assert!(matches!(
            recorder.arm(&invalid),
            Err(RecorderError::InvalidSettings(_))
        ));
        assert!(recorder.is_running());
        recorder
            .begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
            })
            .unwrap();
        recorder.shutdown().unwrap();
    }
}
