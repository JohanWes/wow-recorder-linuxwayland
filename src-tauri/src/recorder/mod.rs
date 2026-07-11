//! Linux gpu-screen-recorder backend.
//!
//! `RecorderParams` is deliberately independent of Tauri.  The manager builds it
//! from persisted configuration and owns the policy for when this recorder runs.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::watch;

const MANAGED_MESSAGE: &str = "This folder is managed by Warcraft Recorder, files in it may be automatically created, modified or deleted.";
static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// Capture configuration supplied by the manager; no Tauri state is read here.
#[derive(Debug, Clone)]
pub struct RecorderParams {
    pub obs_path: PathBuf,
    pub data_dir: PathBuf,
    pub fps: u32,
    pub capture_cursor: bool,
    pub buffer_seconds: u32,
    pub codec: String,
    pub bitrate_kbps: u32,
    pub replay_storage: String,
    pub lead_in_seconds: f64,
    /// Explicit output device. `None` uses the old `linuxGsrAudio` setting.
    pub audio_output: Option<String>,
    pub audio_input: Option<String>,
    pub legacy_audio: Option<String>,
}

impl RecorderParams {
    fn replay_dir(&self) -> PathBuf {
        self.obs_path.join("replay")
    }
    fn regular_dir(&self) -> PathBuf {
        self.obs_path.join("regular")
    }
    fn staging_dir(&self) -> PathBuf {
        self.obs_path.join("staging")
    }
    fn token_file(&self) -> PathBuf {
        self.data_dir.join("gsr-portal.token")
    }
    fn events_file(&self) -> PathBuf {
        self.data_dir.join("gsr-events.tsv")
    }
    fn hook_file(&self) -> PathBuf {
        self.data_dir.join("gsr-hook.sh")
    }
}

/// Public capture state, consumed by the recording manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecorderState {
    #[default]
    None,
    Recording,
}

/// A file-save notification emitted by the GSR hook script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GsrEvent {
    pub ts: i64,
    pub kind: GsrEventKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GsrEventKind {
    Regular,
    Replay,
    Screenshot,
}

impl GsrEventKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "regular" => Some(Self::Regular),
            "replay" => Some(Self::Replay),
            "screenshot" => Some(Self::Screenshot),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Replay => "replay",
            Self::Screenshot => "screenshot",
        }
    }
}

#[derive(Default)]
struct EventLogData {
    offset: u64,
    pending: Vec<GsrEvent>,
}

/// Incremental tail reader for the tab-separated event log produced by GSR's hook.
#[derive(Clone)]
pub struct GsrEventLog {
    path: PathBuf,
    data: Arc<Mutex<EventLogData>>,
}

impl GsrEventLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            data: Arc::default(),
        }
    }

    pub fn start(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        File::options()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io_error)?;
        self.read_new();
        Ok(())
    }

    /// Wait for and consume the first event of `kind` at or after `after_ms`.
    pub fn wait_for(
        &self,
        kind: GsrEventKind,
        after_ms: i64,
        timeout_ms: u64,
    ) -> Result<GsrEvent, String> {
        let deadline = now_ms().saturating_add(timeout_ms as i64);
        while now_ms() < deadline {
            self.read_new();
            if let Ok(mut data) = self.data.lock() {
                if let Some(index) = data
                    .pending
                    .iter()
                    .position(|event| event.kind == kind && event.ts >= after_ms)
                {
                    return Ok(data.pending.remove(index));
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "[LinuxRecorder] Timed out waiting for GSR {} event",
            kind.as_str()
        ))
    }

    fn read_new(&self) {
        let Ok(mut data) = self.data.lock() else {
            return;
        };
        let Ok(metadata) = fs::metadata(&self.path) else {
            return;
        };
        // A replacement/truncation of the file starts a new stream.
        if metadata.len() < data.offset {
            data.offset = 0;
        }
        if metadata.len() <= data.offset {
            return;
        }
        let Ok(mut file) = File::open(&self.path) else {
            return;
        };
        if file.seek(SeekFrom::Start(data.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::with_capacity((metadata.len() - data.offset) as usize);
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        data.offset = metadata.len();
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some(event) = parse_event_line(line) {
                data.pending.push(event);
            }
        }
    }
}

fn parse_event_line(line: &str) -> Option<GsrEvent> {
    let mut pieces = line.split('\t');
    let ts = pieces.next()?.parse().ok()?;
    let kind = GsrEventKind::parse(pieces.next()?)?;
    let path = pieces.collect::<Vec<_>>().join("\t");
    (!path.is_empty()).then_some(GsrEvent {
        ts,
        kind,
        path: PathBuf::from(path),
    })
}

struct Inner {
    params: Mutex<Option<RecorderParams>>,
    child: Mutex<Option<Child>>,
    event_log: Mutex<Option<GsrEventLog>>,
    desired_running: AtomicBool,
    restarting: AtomicBool,
    restart_attempts: AtomicU64,
    state: Mutex<RecorderState>,
    state_tx: watch::Sender<RecorderState>,
    pending_replay: Mutex<Option<thread::JoinHandle<Option<GsrEvent>>>>,
    pending_replay_offset: Mutex<f64>,
    activity_active: AtomicBool,
    protected: Mutex<HashSet<PathBuf>>,
    last_file: Mutex<Option<PathBuf>>,
}

/// Manages GSR's replay buffer and the regular-recording lifecycle.
#[derive(Clone)]
pub struct Recorder {
    inner: Arc<Inner>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub fn new() -> Self {
        let (state_tx, _) = watch::channel(RecorderState::None);
        Self {
            inner: Arc::new(Inner {
                params: Mutex::new(None),
                child: Mutex::new(None),
                event_log: Mutex::new(None),
                desired_running: AtomicBool::new(false),
                restarting: AtomicBool::new(false),
                restart_attempts: AtomicU64::new(0),
                state: Mutex::new(RecorderState::None),
                state_tx,
                pending_replay: Mutex::new(None),
                pending_replay_offset: Mutex::new(0.0),
                activity_active: AtomicBool::new(false),
                protected: Mutex::new(HashSet::new()),
                last_file: Mutex::new(None),
            }),
        }
    }

    /// Subscribe to capture-state changes.
    pub fn subscribe(&self) -> watch::Receiver<RecorderState> {
        self.inner.state_tx.subscribe()
    }
    pub fn state(&self) -> RecorderState {
        self.inner
            .state
            .lock()
            .map(|s| *s)
            .unwrap_or(RecorderState::None)
    }

    /// Create the managed GSR directory layout and retain configuration.
    pub async fn configure_base(&self, params: RecorderParams) -> Result<(), String> {
        for path in [
            &params.obs_path,
            &params.replay_dir(),
            &params.regular_dir(),
            &params.staging_dir(),
        ] {
            fs::create_dir_all(path).map_err(io_error)?;
        }
        let managed = params.obs_path.join("managed.txt");
        if !managed.exists() {
            fs::write(managed, MANAGED_MESSAGE).map_err(io_error)?;
        }
        *self.inner.params.lock().map_err(lock_error)? = Some(params);
        Ok(())
    }

    /// Start the replay buffer. Calling it while running is harmless.
    pub async fn start_buffer(&self) -> Result<(), String> {
        if self.state() == RecorderState::Recording {
            self.inner.desired_running.store(true, Ordering::SeqCst);
            return Ok(());
        }
        self.inner.desired_running.store(true, Ordering::SeqCst);
        self.inner.restart_attempts.store(0, Ordering::SeqCst);
        self.ensure_gsr_available()?;
        self.spawn_gsr_replay()?;
        self.set_state(RecorderState::Recording);
        Ok(())
    }

    /// Save replay pre-roll and begin a regular recording.
    pub async fn start_recording(&self, offset: f64) -> Result<(), String> {
        let pid = self.pid()?;
        let now = now_ms();
        self.inner.activity_active.store(true, Ordering::SeqCst);
        *self
            .inner
            .pending_replay_offset
            .lock()
            .map_err(lock_error)? = offset;
        let event_log = self.inner.event_log.lock().map_err(lock_error)?.clone();
        let inner = Arc::clone(&self.inner);
        let pending = event_log.map(|log| {
            thread::spawn(move || {
                log.wait_for(GsrEventKind::Replay, now, 20_000)
                    .ok()
                    .map(|event| {
                        if let Ok(mut files) = inner.protected.lock() {
                            files.insert(event.path.clone());
                        }
                        event
                    })
            })
        });
        *self.inner.pending_replay.lock().map_err(lock_error)? = pending;
        if let Err(error) =
            send_signal(pid, libc::SIGUSR1).and_then(|_| send_signal(pid, libc::SIGRTMIN()))
        {
            self.inner.activity_active.store(false, Ordering::SeqCst);
            return Err(error);
        }
        Ok(())
    }

    /// Stop regular capture, combine replay and regular files, and set `last_file`.
    pub async fn stop(&self) -> Result<(), String> {
        let pid = self.pid()?;
        let now = now_ms();
        let event_log = self.inner.event_log.lock().map_err(lock_error)?.clone();
        let regular_wait = event_log.map(|log| {
            thread::spawn(move || log.wait_for(GsrEventKind::Regular, now, 30_000).ok())
        });
        send_signal(pid, libc::SIGRTMIN())?;
        let regular = regular_wait.and_then(|handle| handle.join().ok().flatten());
        let replay = self
            .inner
            .pending_replay
            .lock()
            .map_err(lock_error)?
            .take()
            .and_then(|handle| handle.join().ok().flatten());
        *self
            .inner
            .pending_replay_offset
            .lock()
            .map_err(lock_error)? = 0.0;
        let Some(regular) = regular else {
            self.inner.activity_active.store(false, Ordering::SeqCst);
            return Err("[LinuxRecorder] No regular recording produced".into());
        };
        if let Ok(mut files) = self.inner.protected.lock() {
            files.insert(regular.path.clone());
        }
        let result = self.build_combined_activity_file(
            replay.as_ref().map(|event| event.path.as_path()),
            &regular.path,
        );
        if let Ok(path) = &result {
            if let Ok(mut last) = self.inner.last_file.lock() {
                *last = Some(path.clone());
            }
            let _ = fs::remove_file(&regular.path);
            if let Some(replay) = &replay {
                let _ = fs::remove_file(&replay.path);
            }
        }
        self.inner.activity_active.store(false, Ordering::SeqCst);
        if let Ok(mut files) = self.inner.protected.lock() {
            files.remove(&regular.path);
            if let Some(replay) = &replay {
                files.remove(&replay.path);
            }
        }
        result.map(|_| ())
    }

    /// Save the current replay buffer and return its output path.
    pub async fn save_replay_now(&self) -> Result<PathBuf, String> {
        let pid = self.pid()?;
        let now = now_ms();
        let log = self
            .inner
            .event_log
            .lock()
            .map_err(lock_error)?
            .clone()
            .ok_or("[LinuxRecorder] Event log not started")?;
        // Begin tailing before signalling GSR: a very fast hook must not race us.
        let waiter = thread::spawn(move || log.wait_for(GsrEventKind::Replay, now, 20_000));
        send_signal(pid, libc::SIGUSR1)?;
        waiter
            .join()
            .ok()
            .and_then(Result::ok)
            .map(|event| event.path)
            .ok_or("[LinuxRecorder] No replay file produced".into())
    }

    /// Stop capture and cancel automatic restart.
    pub fn shutdown(&self) {
        self.inner.desired_running.store(false, Ordering::SeqCst);
        let _ = self.reap_shutdown_child();
        if let Ok(mut log) = self.inner.event_log.lock() {
            *log = None;
        }
        self.inner.activity_active.store(false, Ordering::SeqCst);
        if let Ok(mut files) = self.inner.protected.lock() {
            files.clear();
        }
        self.set_state(RecorderState::None);
    }

    /// Restart capture, clearing the stored portal selection when requested.
    pub async fn restart_capture(&self, force_portal: bool) -> Result<(), String> {
        self.inner.desired_running.store(false, Ordering::SeqCst);
        let reaper = self.reap_shutdown_child();
        if let Ok(mut log) = self.inner.event_log.lock() {
            *log = None;
        }
        self.inner.activity_active.store(false, Ordering::SeqCst);
        if let Ok(mut files) = self.inner.protected.lock() {
            files.clear();
        }
        self.set_state(RecorderState::None);
        if let Some(reaper) = reaper {
            let _ = reaper.recv_timeout(Duration::from_secs(2));
        }
        if force_portal {
            let params = self.params()?;
            match fs::remove_file(params.token_file()) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(io_error(e)),
            }
        }
        self.start_buffer().await
    }

    /// Delete generated video files, preserving in-flight GSR files during activity recording.
    pub async fn cleanup(&self, obs_path: impl AsRef<Path>) -> Result<(), String> {
        let root = obs_path.as_ref();
        self.clean_dir(root)?;
        if !self.inner.activity_active.load(Ordering::SeqCst) {
            self.clean_dir(&root.join("replay"))?;
            self.clean_dir(&root.join("regular"))?;
        }
        self.clean_dir(&root.join("staging"))
    }

    /// Retrieve the last combined activity path once.
    pub fn get_and_clear_last_file(&self) -> Option<PathBuf> {
        self.inner.last_file.lock().ok()?.take()
    }

    fn params(&self) -> Result<RecorderParams, String> {
        self.inner
            .params
            .lock()
            .map_err(lock_error)?
            .clone()
            .ok_or("[LinuxRecorder] Base config not set".into())
    }
    fn pid(&self) -> Result<i32, String> {
        self.inner
            .child
            .lock()
            .map_err(lock_error)?
            .as_ref()
            .map(|child| child.id() as i32)
            .ok_or("[LinuxRecorder] Capture not started. Start Capture first.".into())
    }
    fn set_state(&self, state: RecorderState) {
        if let Ok(mut current) = self.inner.state.lock() {
            *current = state;
        }
        let _ = self.inner.state_tx.send(state);
    }

    fn ensure_gsr_available(&self) -> Result<(), String> {
        let status = Command::new("gpu-screen-recorder")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| {
                format!("[LinuxRecorder] gpu-screen-recorder not available in PATH: {e}")
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "[LinuxRecorder] gpu-screen-recorder not available in PATH: {status}"
            ))
        }
    }

    fn spawn_gsr_replay(&self) -> Result<(), String> {
        let params = self.params()?;
        write_hook_script(&params.hook_file(), &params.events_file())?;
        let log = GsrEventLog::new(params.events_file());
        log.start()?;
        let mut args = vec![
            "-w".into(),
            "portal".into(),
            "-restore-portal-session".into(),
            "yes".into(),
            "-portal-session-token-filepath".into(),
            params.token_file().display().to_string(),
            "-r".into(),
            params.buffer_seconds.to_string(),
            "-replay-storage".into(),
            params.replay_storage.clone(),
            "-restart-replay-on-save".into(),
            "no".into(),
            "-c".into(),
            "mkv".into(),
            "-f".into(),
            params.fps.to_string(),
            "-bm".into(),
            "cbr".into(),
            "-q".into(),
            params.bitrate_kbps.to_string(),
            "-k".into(),
            params.codec.clone(),
            "-ac".into(),
            "aac".into(),
            "-cursor".into(),
            if params.capture_cursor { "yes" } else { "no" }.into(),
            "-o".into(),
            params.replay_dir().display().to_string(),
            "-ro".into(),
            params.regular_dir().display().to_string(),
            "-sc".into(),
            params.hook_file().display().to_string(),
            "-v".into(),
            "no".into(),
        ];
        let output = params.audio_output.unwrap_or_else(|| {
            params
                .legacy_audio
                .unwrap_or_else(|| "default_output".into())
        });
        let mut audio = vec![output];
        if let Some(input) = params.audio_input.filter(|value| !value.is_empty()) {
            audio.push(input);
        }
        audio.retain(|value| !value.is_empty());
        audio.sort();
        audio.dedup();
        if !audio.is_empty() {
            args.push("-a".into());
            args.push(audio.join("|"));
        }
        let mut child = Command::new("gpu-screen-recorder")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(io_error)?;
        if let Some(stdout) = child.stdout.take() {
            forward_gsr_output(stdout, "stdout");
        }
        if let Some(stderr) = child.stderr.take() {
            forward_gsr_output(stderr, "stderr");
        }
        thread::sleep(Duration::from_millis(500));
        if let Some(status) = child.try_wait().map_err(io_error)? {
            return Err(format!(
                "[LinuxRecorder] gsr exited immediately with {status}"
            ));
        }
        let pid = child.id();
        *self.inner.child.lock().map_err(lock_error)? = Some(child);
        *self.inner.event_log.lock().map_err(lock_error)? = Some(log);
        self.monitor_child(pid);
        Ok(())
    }

    fn monitor_child(&self, pid: u32) {
        let this = self.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(200));
            let exited = {
                let Ok(mut guard) = this.inner.child.lock() else {
                    return;
                };
                match guard.as_mut() {
                    Some(child) if child.id() == pid => child.try_wait().ok().flatten().is_some(),
                    _ => return,
                }
            };
            if exited {
                if let Ok(mut child) = this.inner.child.lock() {
                    *child = None;
                }
                this.set_state(RecorderState::None);
                if this.inner.desired_running.load(Ordering::SeqCst) {
                    this.schedule_restart();
                }
                return;
            }
        });
    }
    fn schedule_restart(&self) {
        if self.inner.restarting.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        thread::spawn(move || {
            let attempt = this.inner.restart_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let delay = (1000_u64.saturating_mul(2_u64.pow(attempt.min(4) as u32))).min(30_000);
            thread::sleep(Duration::from_millis(delay));
            this.inner.restarting.store(false, Ordering::SeqCst);
            if this.inner.desired_running.load(Ordering::SeqCst)
                && this.state() != RecorderState::Recording
            {
                if this
                    .ensure_gsr_available()
                    .and_then(|_| this.spawn_gsr_replay())
                    .is_ok()
                {
                    this.set_state(RecorderState::Recording);
                } else if this.inner.desired_running.load(Ordering::SeqCst) {
                    this.schedule_restart();
                }
            }
        });
    }

    /// Remove the child from shared state, signal it, and reap it off-thread.
    fn reap_shutdown_child(&self) -> Option<mpsc::Receiver<()>> {
        let child = self.inner.child.lock().ok()?.take()?;
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let pid = child.id() as i32;
            let _ = send_signal(pid, libc::SIGINT);
            let mut child = child;
            let _ = child.wait();
            let _ = done_tx.send(());
        });
        Some(done_rx)
    }

    fn clean_dir(&self, dir: &Path) -> Result<(), String> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_error(e)),
        };
        let protected = self.inner.protected.lock().map_err(lock_error)?;
        for entry in entries.flatten() {
            let path = entry.path();
            let video = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("mp4") || x.eq_ignore_ascii_case("mkv"));
            if video && !protected.contains(&path) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }

    fn build_combined_activity_file(
        &self,
        replay: Option<&Path>,
        regular: &Path,
    ) -> Result<PathBuf, String> {
        let params = self.params()?;
        let combined = params.obs_path.join(format!(
            "activity-{}-{:x}.mkv",
            now_ms(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        let fallback = || {
            fs::copy(regular, &combined)
                .map(|_| combined.clone())
                .map_err(io_error)
        };
        let Some(replay) = replay.filter(|file| file.exists()) else {
            return fallback();
        };
        let offset = *self
            .inner
            .pending_replay_offset
            .lock()
            .map_err(lock_error)?;
        let trimmed = params.staging_dir().join(format!(
            "replay-trim-{}-{:x}.mkv",
            now_ms(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        let list = params.staging_dir().join(format!(
            "concat-{}-{:x}.txt",
            now_ms(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        let wanted = (offset.max(0.0) + params.lead_in_seconds)
            .round()
            .max(1.0)
            .to_string();
        let attempt = (|| -> Result<(), String> {
            command_status(
                Command::new("ffmpeg")
                    .arg("-sseof")
                    .arg(format!("-{wanted}"))
                    .arg("-i")
                    .arg(replay)
                    .args([
                        "-c:v",
                        "copy",
                        "-c:a",
                        "copy",
                        "-avoid_negative_ts",
                        "make_zero",
                    ])
                    .arg(&trimmed),
            )?;
            let mut file = File::create(&list).map_err(io_error)?;
            writeln!(
                file,
                "file '{}'\nfile '{}'",
                concat_escape(&trimmed),
                concat_escape(regular)
            )
            .map_err(io_error)?;
            command_status(
                Command::new("ffmpeg")
                    .args(["-f", "concat", "-safe", "0", "-i"])
                    .arg(&list)
                    .args([
                        "-c:v",
                        "copy",
                        "-c:a",
                        "copy",
                        "-avoid_negative_ts",
                        "make_zero",
                    ])
                    .arg(&combined),
            )
        })();
        let _ = fs::remove_file(&list);
        let _ = fs::remove_file(&trimmed);
        if attempt.is_ok() {
            Ok(combined)
        } else {
            fallback()
        }
    }
}

fn write_hook_script(hook: &Path, events: &Path) -> Result<(), String> {
    if let Some(parent) = hook.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let script = format!("#!/usr/bin/env bash\nset -euo pipefail\nfilepath=\"$1\"\nkind=\"$2\"\nts=$(date +%s%3N)\nprintf '%s\\t%s\\t%s\\n' \"$ts\" \"$kind\" \"$filepath\" >> \"{}\"\n", events.display());
    fs::write(hook, script).map_err(io_error)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).map_err(io_error)
}
fn send_signal(pid: i32, signal: i32) -> Result<(), String> {
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "[LinuxRecorder] failed to send signal {signal}: {}",
            std::io::Error::last_os_error()
        ))
    }
}
fn command_status(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(io_error)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("[LinuxRecorder] ffmpeg exited with {status}"))
    }
}

fn forward_gsr_output<R: Read + Send + 'static>(reader: R, stream: &'static str) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().flatten() {
            eprintln!("[LinuxRecorder] gsr {stream}: {line}");
        }
    });
}

fn concat_escape(path: &Path) -> String {
    path.display().to_string().replace('\'', "'\\''")
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn io_error(error: std::io::Error) -> String {
    error.to_string()
}
fn lock_error<T>(_: std::sync::PoisonError<T>) -> String {
    "[LinuxRecorder] internal lock poisoned".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_tsv_event_lines_including_tabs_in_paths() {
        let event = parse_event_line("123\treplay\t/tmp/a\tb.mkv").unwrap();
        assert_eq!(event.ts, 123);
        assert_eq!(event.kind, GsrEventKind::Replay);
        assert_eq!(event.path, PathBuf::from("/tmp/a\tb.mkv"));
        assert!(parse_event_line("bad\treplay\t/tmp/a.mkv").is_none());
    }

    #[test]
    fn concat_escape_uses_single_backslash_for_apostrophes() {
        let escaped = concat_escape(Path::new("/tmp/it's.mkv"));
        assert_eq!(escaped, "/tmp/it'\\''s.mkv");
        assert!(!escaped.contains("\\\\"));
    }
}
