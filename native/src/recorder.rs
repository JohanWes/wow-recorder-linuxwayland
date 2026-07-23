// SPDX-License-Identifier: GPL-3.0-or-later

//! gpu-screen-recorder lifecycle adapter.
//!
//! One long-lived GSR replay-buffer child, at most one active recording, and
//! the WR-000 signal/hook protocol: SIGUSR1 saves the replay pre-roll,
//! SIGRTMIN toggles the regular recording, and a generated `-sc` hook script
//! appends `epoch_ms<TAB>kind<TAB>path` records that `poll`/`end` correlate
//! against the configured replay/regular directories.
//!
//! Behavior notes recorded against the legacy TypeScript and WR-000/WR-002:
//! - The hook receives `$1 = saved artifact path, $2 = event kind` (the real
//!   GSR `-sc` argv; WR-006's illustrative snippet had them swapped).
//! - Restart delays are 2, 4, 8, 16, then capped 30 seconds indefinitely.
//!   Per WR-000, a successful automatic respawn does not reset the attempt
//!   counter; only a deliberate `arm` does.
//! - Crash recovery of interrupted recordings is not Recorder's job; the only
//!   persistent state is the truncate-on-arm events file.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::config::CaptureSettings;
use crate::domain::{Category, Codec, RecordingId, ReplayStorage};
use crate::process;

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
    /// Detection delay plus lead-in, already clamped by WR-008.
    pub requested_replay_ms: u64,
    pub mode: RecordingMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStarted {
    pub id: RecordingId,
    pub requested_replay_ms: u64,
    pub regular_started_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureArtifacts {
    /// GSR-saved replay pre-roll; missing becomes WR-007's regular-only
    /// fallback.
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
        at_ms: i64,
    },
    Restarted,
    RestartFailed {
        message: String,
    },
    /// The portal wrote (or replaced) the reusable capture-target token.
    TargetTokenAvailable(String),
    Diagnostic(String),
}

/// Bounded waits from the proven baseline. Tests shrink them; production uses
/// the defaults.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// Post-spawn stability check before arm is considered successful.
    pub arm_stability: Duration,
    /// Wait for the hook's replay event after SIGUSR1.
    pub replay_event: Duration,
    /// Wait for the hook's regular event after the stop SIGRTMIN.
    pub regular_event: Duration,
    /// Wait for the old child to exit during reselection/shutdown before
    /// escalating.
    pub exit_grace: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            arm_stability: Duration::from_millis(500),
            replay_event: Duration::from_secs(20),
            regular_event: Duration::from_secs(30),
            exit_grace: Duration::from_secs(2),
        }
    }
}

const GSR_EXIT_SELECTION_DENIED: i32 = 60;
const LOG_TAIL_BYTES: u64 = 8 * 1024;
const MAX_RESTART_DELAY_SECONDS: u64 = 30;

struct GsrEvent {
    kind: String,
    path: PathBuf,
}

struct ActiveCapture {
    id: RecordingId,
    requested_replay_ms: u64,
    regular_started_at_ms: i64,
    replay_deadline: Instant,
}

pub struct Recorder {
    config: Option<CaptureConfig>,
    child: Option<Child>,
    desired_running: bool,
    restart_attempts: u32,
    restart_at_ms: Option<i64>,
    events_offset: u64,
    pending: Vec<GsrEvent>,
    active: Option<ActiveCapture>,
    last_token: Option<String>,
    ignored_events: u32,
    timeouts: Timeouts,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

fn now_wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

impl Recorder {
    pub fn new() -> Self {
        Self::with_timeouts(Timeouts::default())
    }

    pub fn with_timeouts(timeouts: Timeouts) -> Self {
        Self {
            config: None,
            child: None,
            desired_running: false,
            restart_attempts: 0,
            restart_at_ms: None,
            events_offset: 0,
            pending: Vec::new(),
            active: None,
            last_token: None,
            ignored_events: 0,
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
        self.events_offset = 0;
        self.pending.clear();
        self.ignored_events = 0;

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
            let log_tail = read_log_tail(&Self::log_path(config));
            if status.code() == Some(GSR_EXIT_SELECTION_DENIED) {
                return Err(RecorderError::SelectionDenied { log_tail });
            }
            return Err(RecorderError::SpawnFailed {
                message: format!("gpu-screen-recorder exited immediately: {status}"),
                log_tail,
            });
        }
        self.child = Some(child);
        Ok(())
    }

    /// Register the replay wait, save the pre-roll (SIGUSR1), and start the
    /// regular recording (SIGRTMIN). Never waits for media.
    pub fn begin(&mut self, request: StartRequest) -> Result<CaptureStarted, RecorderError> {
        if self.active.is_some() {
            return Err(RecorderError::Busy);
        }
        let child = self.live_child()?;
        let started = CaptureStarted {
            id: request.id.clone(),
            requested_replay_ms: request.requested_replay_ms,
            regular_started_at_ms: now_wall_ms(),
        };
        process::send_signal(child, libc::SIGUSR1)?;
        process::send_signal(child, process::sigrtmin())?;
        self.active = Some(ActiveCapture {
            id: request.id,
            requested_replay_ms: request.requested_replay_ms,
            regular_started_at_ms: started.regular_started_at_ms,
            replay_deadline: Instant::now() + self.timeouts.replay_event,
        });
        Ok(started)
    }

    /// Stop the regular recording and return the GSR-named artifacts. Missing
    /// replay is tolerated; missing regular is an error.
    pub fn end(&mut self, id: &RecordingId) -> Result<CaptureArtifacts, RecorderError> {
        match &self.active {
            None => return Err(RecorderError::NotArmed),
            Some(active) if &active.id != id => return Err(RecorderError::WrongId),
            Some(_) => {}
        }
        let child = self.live_child()?;
        process::send_signal(child, process::sigrtmin())?;
        let config = self.config.clone().expect("armed with config");
        let active = self.active.take().expect("checked above");

        let regular = self.wait_for_event(
            &config,
            "regular",
            &Self::regular_dir(&config),
            Instant::now() + self.timeouts.regular_event,
        );
        let replay = self.wait_for_event(
            &config,
            "replay",
            &Self::replay_dir(&config),
            active.replay_deadline,
        );
        // Any event still pending after the session was noise (wrong kind,
        // duplicate, or outside the managed directories).
        self.ignored_events += self.pending.len() as u32;
        self.pending.clear();
        let Some(regular) = regular else {
            return Err(RecorderError::MissingRegularArtifact);
        };
        Ok(CaptureArtifacts {
            replay,
            regular,
            requested_replay_ms: active.requested_replay_ms,
            regular_started_at_ms: active.regular_started_at_ms,
            regular_stopped_at_ms: now_wall_ms(),
        })
    }

    /// Stop the active session and hand its artifacts to the coordinator for
    /// cleanup. Recorder never unlinks them itself.
    pub fn cancel(&mut self, id: &RecordingId) -> Result<CaptureArtifacts, RecorderError> {
        self.end(id)
    }

    /// WR-002's token contract: stop the child, invalidate the token only
    /// after it exited, and re-arm to trigger portal selection. A denied
    /// selection restores the previous usable token.
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
                    events.push(RecorderEvent::ChildExited {
                        code: status.code(),
                    });
                    if self.desired_running {
                        self.schedule_restart(now_ms, &mut events);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    events.push(RecorderEvent::Diagnostic(format!(
                        "child status check failed: {error}"
                    )));
                }
            }
        } else if self.desired_running
            && self
                .restart_at_ms
                .is_some_and(|deadline| now_ms >= deadline)
        {
            self.restart_at_ms = None;
            let config = self.config.clone().expect("desired_running implies config");
            match self.spawn_child(&config) {
                // Per WR-000 the attempt counter survives an automatic
                // respawn; only a deliberate arm resets it.
                Ok(()) => events.push(RecorderEvent::Restarted),
                Err(error) => {
                    events.push(RecorderEvent::RestartFailed {
                        message: format!("{error:?}"),
                    });
                    self.schedule_restart(now_ms, &mut events);
                }
            }
        }

        if let Some(config) = &self.config {
            let token = read_token(&Self::token_path(config));
            if let Some(token) = token
                && self.last_token.as_deref() != Some(token.as_str())
            {
                self.last_token = Some(token.clone());
                events.push(RecorderEvent::TargetTokenAvailable(token));
            }
        }
        if self.ignored_events > 0 {
            events.push(RecorderEvent::Diagnostic(format!(
                "ignored {} unexpected GSR hook event(s)",
                self.ignored_events
            )));
            self.ignored_events = 0;
        }
        events
    }

    fn schedule_restart(&mut self, now_ms: i64, events: &mut Vec<RecorderEvent>) {
        self.restart_attempts += 1;
        let delay_seconds = MAX_RESTART_DELAY_SECONDS.min(1u64 << self.restart_attempts.min(63));
        let at_ms = now_ms + (delay_seconds * 1_000) as i64;
        self.restart_at_ms = Some(at_ms);
        events.push(RecorderEvent::RestartScheduled {
            attempt: self.restart_attempts,
            at_ms,
        });
    }

    /// Stop the replay child with the SIGINT-then-kill escalation and leave no
    /// child or scheduled restart behind.
    pub fn shutdown(&mut self) -> Result<(), RecorderError> {
        self.desired_running = false;
        self.restart_at_ms = None;
        self.active = None;
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
            if timestamp.parse::<i64>().is_err()
                || path.is_empty()
                || !matches!(kind, "regular" | "replay" | "screenshot")
            {
                self.ignored_events += 1;
                continue;
            }
            self.pending.push(GsrEvent {
                kind: kind.to_string(),
                path: PathBuf::from(path),
            });
        }
        Ok(())
    }

    fn wait_for_event(
        &mut self,
        config: &CaptureConfig,
        kind: &str,
        directory: &Path,
        deadline: Instant,
    ) -> Option<PathBuf> {
        let canonical_dir = directory.canonicalize().ok()?;
        loop {
            let _ = self.read_new_events(config);
            let matched = self.pending.iter().position(|event| {
                event.kind == kind
                    && event
                        .path
                        .parent()
                        .and_then(|parent| parent.canonicalize().ok())
                        .is_some_and(|parent| parent == canonical_dir)
            });
            if let Some(index) = matched {
                return Some(self.pending.remove(index).path);
            }
            // Anything unconsumed of this kind was outside the expected
            // directory: reject it with one bounded diagnostic.
            let before = self.pending.len();
            self.pending.retain(|event| event.kind != kind);
            self.ignored_events += (before - self.pending.len()) as u32;
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Exact WR-000 argv order. Paths and devices stay single `OsString`
/// arguments; no shell is involved.
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

fn read_log_tail(path: &Path) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return String::new();
    };
    let start = size.saturating_sub(LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut tail = Vec::new();
    let _ = file.read_to_end(&mut tail);
    String::from_utf8_lossy(&tail).into_owned()
}

fn text_tail(text: &str) -> String {
    let start = text
        .len()
        .saturating_sub(usize::try_from(LOG_TAIL_BYTES).unwrap_or(usize::MAX));
    String::from_utf8_lossy(&text.as_bytes()[start..]).into_owned()
}

/// Legacy `parseGsrAudioDevices`: sectioned `--list-audio-devices` output,
/// `default_output`/`default_input`/`device:<nonspace>` values, de-duplicated,
/// with defaults always present.
fn parse_audio_devices(text: &str) -> AudioDevices {
    #[derive(PartialEq)]
    enum Section {
        Outputs,
        Inputs,
        Unknown,
    }
    let mut section = Section::Unknown;
    let mut outputs = Vec::new();
    let mut inputs = Vec::new();
    let mut all = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let lower = line.to_ascii_lowercase();
        if lower.contains("device") && lower.ends_with(':') {
            if lower.contains("output") {
                section = Section::Outputs;
                continue;
            }
            if lower.contains("input") {
                section = Section::Inputs;
                continue;
            }
        }
        let normalized = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line);
        let (value, detail) = match normalized.split_once('|') {
            Some((value, detail)) => (value.trim(), detail.trim()),
            None => match normalized.split_once(char::is_whitespace) {
                Some((value, detail)) => (value, detail.trim()),
                None => (normalized, ""),
            },
        };
        let recognized = value == "default_output"
            || value == "default_input"
            || (value.starts_with("device:") && value.len() > "device:".len());
        if !recognized {
            continue;
        }
        let label = if detail.is_empty() {
            value.to_string()
        } else {
            format!("{value} — {detail}")
        };
        let device = AudioDevice {
            id: value.to_string(),
            label,
        };
        match section {
            Section::Outputs => outputs.push(device),
            Section::Inputs => inputs.push(device),
            Section::Unknown => all.push(device),
        }
    }

    fn unique(devices: Vec<AudioDevice>, exclude: &str) -> Vec<AudioDevice> {
        let mut seen = std::collections::HashSet::new();
        devices
            .into_iter()
            .filter(|device| device.id != exclude && seen.insert(device.id.clone()))
            .collect()
    }

    let mut final_outputs = unique(
        if outputs.is_empty() {
            all.clone()
        } else {
            outputs
        },
        "default_input",
    );
    let mut final_inputs = unique(
        if inputs.is_empty() { all } else { inputs },
        "default_output",
    );
    if !final_outputs
        .iter()
        .any(|device| device.id == "default_output")
    {
        final_outputs.insert(
            0,
            AudioDevice {
                id: "default_output".to_string(),
                label: "default_output — Default output device".to_string(),
            },
        );
    }
    if !final_inputs
        .iter()
        .any(|device| device.id == "default_input")
    {
        final_inputs.insert(
            0,
            AudioDevice {
                id: "default_input".to_string(),
                label: "default_input — Default input device".to_string(),
            },
        );
    }
    AudioDevices {
        outputs: final_outputs,
        inputs: final_inputs,
    }
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
        }
    }

    fn test_config(name: &str) -> CaptureConfig {
        let root =
            std::env::temp_dir().join(format!("wr-recorder-{name}-{}", uuid::Uuid::new_v4()));
        CaptureConfig {
            gsr_binary: fake_gsr(),
            data_dir: root.join("data dir with späce"),
            capture_root: root.join("capture"),
            settings: CaptureSettings::default(),
        }
    }

    fn append_event(config: &CaptureConfig, kind: &str, path: &Path) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(Recorder::events_path(config))
            .unwrap();
        writeln!(file, "{}\t{}\t{}", now_wall_ms(), kind, path.display()).unwrap();
    }

    fn touch(path: &Path) {
        fs::write(path, b"media").unwrap();
    }

    #[test]
    fn argv_matches_baseline_and_preserves_awkward_paths() {
        let mut config = test_config("argv");
        config.settings.audio_output = "device:out put".to_string();
        config.settings.audio_input = Some("device:mic".to_string());
        let args = build_gsr_args(&config);
        let expected: Vec<OsString> = vec![
            "-w".into(),
            "portal".into(),
            "-restore-portal-session".into(),
            "yes".into(),
            "-portal-session-token-filepath".into(),
            config.data_dir.join("gsr-portal.token").into_os_string(),
            "-r".into(),
            "180".into(),
            "-replay-storage".into(),
            "ram".into(),
            "-restart-replay-on-save".into(),
            "no".into(),
            "-c".into(),
            "mkv".into(),
            "-f".into(),
            "60".into(),
            "-bm".into(),
            "cbr".into(),
            "-q".into(),
            "20000".into(),
            "-k".into(),
            "h264".into(),
            "-ac".into(),
            "aac".into(),
            "-cursor".into(),
            "no".into(),
            "-o".into(),
            config.capture_root.join("replay").into_os_string(),
            "-ro".into(),
            config.capture_root.join("regular").into_os_string(),
            "-sc".into(),
            config.data_dir.join("gsr-hook.sh").into_os_string(),
            "-v".into(),
            "no".into(),
            "-a".into(),
            "device:out put|device:mic".into(),
        ];
        assert_eq!(args, expected);

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
        let started = recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 12_000,
                mode: RecordingMode::Automatic,
            })
            .unwrap();
        assert_eq!(started.requested_replay_ms, 12_000);
        // A second begin cannot disturb the active session.
        assert!(matches!(
            recorder.begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
                mode: RecordingMode::Manual,
            }),
            Err(RecorderError::Busy)
        ));
        // Ending the wrong ID is rejected and the session stays live.
        assert!(matches!(
            recorder.end(&RecordingId::new()),
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
        append_event(&config, "regular", &regular);

        let artifacts = recorder.end(&id).unwrap();
        assert_eq!(artifacts.replay.as_deref(), Some(replay.as_path()));
        assert_eq!(artifacts.regular, regular);
        assert!(artifacts.regular_stopped_at_ms >= artifacts.regular_started_at_ms);
        // The ignored events surface as one bounded diagnostic.
        let events = recorder.poll(now_wall_ms());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RecorderEvent::Diagnostic(_))),
            "expected diagnostic, got {events:?}"
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
                mode: RecordingMode::Automatic,
            })
            .unwrap();
        let regular = Recorder::regular_dir(&config).join("Video_1.mkv");
        touch(&regular);
        append_event(&config, "regular", &regular);
        let artifacts = recorder.end(&id).unwrap();
        assert_eq!(artifacts.replay, None);
        assert_eq!(artifacts.regular, regular);

        // Second recording produces no regular event at all.
        let id = RecordingId::new();
        recorder
            .begin(StartRequest {
                id: id.clone(),
                requested_replay_ms: 0,
                mode: RecordingMode::Test(Category::Raids),
            })
            .unwrap();
        assert!(matches!(
            recorder.end(&id),
            Err(RecorderError::MissingRegularArtifact)
        ));
        // The failed session was cleared; a new begin works.
        recorder
            .begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
                mode: RecordingMode::Manual,
            })
            .unwrap();
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
            let scheduled = events.iter().find_map(|event| match event {
                RecorderEvent::RestartScheduled { at_ms, .. } => Some(at_ms - now_ms),
                _ => None,
            });
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, RecorderEvent::ChildExited { .. }))
            );
            let delay = scheduled.expect("restart scheduled");
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
        assert!(events.contains(&RecorderEvent::RestartScheduled {
            attempt: 7,
            at_ms: now_ms + 30_000,
        }));
        let events = recorder.poll(now_ms + 30_000);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RecorderEvent::RestartFailed { .. }))
        );
        let retry_at = events.iter().find_map(|event| match event {
            RecorderEvent::RestartScheduled { at_ms, .. } => Some(*at_ms),
            _ => None,
        });
        assert_eq!(retry_at, Some(now_ms + 60_000));
        fs::remove_file(config.data_dir.join("fake-exit")).unwrap();
        assert!(
            recorder
                .poll(retry_at.unwrap())
                .contains(&RecorderEvent::Restarted)
        );

        // A deliberate arm resets the attempt counter.
        recorder.arm(&config).unwrap();
        process::send_signal(recorder.child.as_ref().unwrap(), libc::SIGKILL).unwrap();
        recorder.child.as_mut().unwrap().wait().unwrap();
        let events = recorder.poll(now_ms);
        assert!(events.contains(&RecorderEvent::RestartScheduled {
            attempt: 1,
            at_ms: now_ms + 2_000,
        }));

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
        recorder.poll(now_wall_ms());

        let selection = recorder.reselect_target(&config).unwrap();
        // The old token was deleted; GSR has not written a new one yet.
        assert_eq!(selection.token, None);
        assert!(!token_path.exists());
        // The portal writes the new token later; poll reports it.
        fs::write(&token_path, "new-token").unwrap();
        let events = recorder.poll(now_wall_ms());
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
    fn audio_discovery_parses_sections_and_inserts_defaults() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let config = test_config("audio");
        recorder.arm(&config).unwrap();
        let devices = recorder.audio_devices().unwrap();
        assert_eq!(
            devices
                .outputs
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            vec!["default_output", "device:alsa_output.pci.analog-stereo"]
        );
        assert_eq!(
            devices
                .inputs
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            vec!["default_input", "device:alsa_input.usb-mic"]
        );
        recorder.shutdown().unwrap();

        // Unsectioned output falls back to the shared list, de-duplicates,
        // and always includes both defaults.
        let parsed =
            parse_audio_devices("device:x Some Device\ndevice:x Some Device\ngarbage line\n");
        assert_eq!(
            parsed
                .outputs
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            vec!["default_output", "device:x"]
        );
        assert_eq!(parsed.inputs.len(), 2);
        assert_eq!(parsed.outputs[1].label, "device:x — Some Device");

        let parsed = parse_audio_devices(
            "Output devices:\ndefault_output|Default output\ndevice:alsa_output.test|Built-in output\nInput devices:\ndefault_input|Default input\ndevice:alsa_input.test|USB microphone\n",
        );
        assert_eq!(
            parsed
                .outputs
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            vec!["default_output", "device:alsa_output.test"]
        );
        assert_eq!(
            parsed.outputs[1].label,
            "device:alsa_output.test — Built-in output"
        );
        assert_eq!(
            parsed
                .inputs
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            vec!["default_input", "device:alsa_input.test"]
        );
        assert_eq!(
            parsed.inputs[1].label,
            "device:alsa_input.test — USB microphone"
        );
    }

    #[test]
    fn begin_requires_a_live_armed_child() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        assert!(matches!(
            recorder.begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
                mode: RecordingMode::Automatic,
            }),
            Err(RecorderError::NotArmed)
        ));
        assert!(matches!(
            recorder.end(&RecordingId::new()),
            Err(RecorderError::NotArmed)
        ));
    }

    #[test]
    fn invalid_audio_setting_is_rejected_before_spawn() {
        let mut recorder = Recorder::with_timeouts(test_timeouts());
        let mut config = test_config("invalid");
        config.settings.audio_output = "a|b".to_string();
        assert!(matches!(
            recorder.arm(&config),
            Err(RecorderError::InvalidSettings(_))
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
        recorder
            .begin(StartRequest {
                id: RecordingId::new(),
                requested_replay_ms: 0,
                mode: RecordingMode::Test(Category::Raids),
            })
            .unwrap();
        recorder.shutdown().unwrap();
    }
}
