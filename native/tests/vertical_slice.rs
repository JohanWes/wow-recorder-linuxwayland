// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless vertical slice: config, log polling, activity detection, recorder
//! control, storage, and media jobs behind the real coordinator.
//!
//! The recorder and FFmpeg are replaced by the WR-006/WR-007 fakes; every
//! other type is the production one operating on a temp directory. The
//! coordinator core is stepped with `tick()`, so no test sleeps or timing
//! assertions are needed.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use warcraft_recorder::config::{
    ActivitySettings, AuthorizedPath, CaptureSettings, Config, FlavorConfig, ManualSettings,
    StorageSettings,
};
use warcraft_recorder::coordinator::{AppSnapshot, ClipRange, Command, Coordinator, Setup, start};
use warcraft_recorder::domain::{Category, Outcome, RecorderStatus, StorageLimit, TimelineKind};
use warcraft_recorder::media_jobs::MediaConfig;
use warcraft_recorder::recorder::Timeouts;
use warcraft_recorder::storage::{RECOVERY_DIR, now_unix_ms};

const PLAYER_GUID: &str = "Player-1092-0A70E103";
const PLAYER_NAME: &str = "Testplayer-Testrealm";
const SELF_FLAGS: &str = "0x511";
/// Long enough for real process spawns and FFmpeg fakes, short enough to fail
/// fast. Nothing is asserted about how long a step actually takes.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    root: PathBuf,
    library: PathBuf,
    capture_root: PathBuf,
    log_file: PathBuf,
    coordinator: Coordinator,
    commands: SyncSender<Command>,
    snapshots: Receiver<Arc<AppSnapshot>>,
    latest: Arc<AppSnapshot>,
    replay_index: u32,
}

impl Harness {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("wr-slice-{name}-{}", uuid::Uuid::new_v4()));
        let library = root.join("recordings with space");
        let capture_root = root.join("buffer");
        let log_dir = root.join("wow/_retail_/Logs");
        for directory in [&library, &capture_root, &log_dir] {
            fs::create_dir_all(directory).unwrap();
        }
        let log_file = log_dir.join("WoWCombatLog.txt");
        fs::write(&log_file, b"").unwrap();
        write_config(&root, &library, &capture_root, &log_dir);
        Self::attach(root, library, capture_root, log_file)
    }

    /// Build a coordinator over an existing tree, as a restart would.
    fn attach(root: PathBuf, library: PathBuf, capture_root: PathBuf, log_file: PathBuf) -> Self {
        let (commands, commands_rx) = mpsc::sync_channel(64);
        let (snapshot_tx, snapshots) = mpsc::sync_channel(1);
        let mut coordinator = Coordinator::new(setup(&root), commands_rx, snapshot_tx);
        coordinator.startup();
        let mut harness = Self {
            root,
            library,
            capture_root,
            log_file,
            coordinator,
            commands,
            snapshots,
            latest: Arc::new(empty_snapshot()),
            replay_index: 0,
        };
        harness.pump(|snapshot| !matches!(snapshot.status, RecorderStatus::SetupRequired));
        harness
    }

    /// Step the coordinator until the condition holds. Panics on timeout so a
    /// broken flow fails loudly instead of asserting on a stale snapshot.
    fn pump(&mut self, mut done: impl FnMut(&AppSnapshot) -> bool) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            self.coordinator.tick();
            while let Ok(snapshot) = self.snapshots.try_recv() {
                self.latest = snapshot;
            }
            if done(&self.latest) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out; status {:?}, problems {:?}",
                self.latest.status,
                self.latest.problems
            );
        }
    }

    fn send(&self, command: Command) {
        assert!(
            self.commands.try_send(command).is_ok(),
            "command queue full"
        );
    }

    fn log(&self, lines: &[String]) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&self.log_file)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    /// Stand in for gpu-screen-recorder saving its two artifacts and calling
    /// the `-sc` hook.
    fn emit_artifacts(&mut self, replay: bool) {
        self.replay_index += 1;
        let index = self.replay_index;
        let events = self.root.join("recorder/gsr-events.tsv");
        let mut file = fs::OpenOptions::new().append(true).open(&events).unwrap();
        if replay {
            let path = self.capture_root.join(format!("replay/Replay_{index}.mkv"));
            fs::write(&path, b"replay media").unwrap();
            writeln!(file, "{}\treplay\t{}", now_unix_ms(), path.display()).unwrap();
        }
        let path = self.capture_root.join(format!("regular/Video_{index}.mkv"));
        fs::write(&path, b"regular media").unwrap();
        writeln!(file, "{}\tregular\t{}", now_unix_ms(), path.display()).unwrap();
    }

    fn entries_of(&self, category: &Category) -> Vec<&warcraft_recorder::domain::LibraryEntry> {
        self.latest
            .entries
            .iter()
            .filter(|entry| &entry.category == category)
            .collect()
    }
}

fn setup(root: &Path) -> Setup {
    Setup {
        config_path: root.join("config.json"),
        legacy_config_path: root.join("missing-legacy.json"),
        data_dir: root.join("recorder"),
        gsr_binary: fixture_bin("fake-gsr.sh"),
        media: MediaConfig {
            ffmpeg: fixture_bin("fake-ffmpeg.sh"),
            utc_offset_minutes: 0,
            poll_interval: Duration::from_millis(10),
            finalize_grace: Duration::from_secs(5),
            sigint_grace: Duration::from_millis(300),
        },
        year: current_year(),
        recorder_timeouts: Timeouts {
            arm_stability: Duration::from_millis(150),
            replay_event: Duration::from_millis(400),
            regular_event: Duration::from_millis(400),
            exit_grace: Duration::from_millis(500),
        },
        poll_interval: Duration::from_millis(5),
        test_duration: Duration::from_millis(200),
    }
}

fn fixture_bin(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/native/bin")
        .join(name)
}

fn write_config(root: &Path, library: &Path, capture_root: &Path, log_dir: &Path) {
    let config = Config {
        flavors: warcraft_recorder::config::FlavorSettings {
            retail: FlavorConfig {
                enabled: true,
                log_dir: AuthorizedPath::authorized(log_dir),
            },
            ..Default::default()
        },
        activities: ActivitySettings {
            min_raid_duration_seconds: 0,
            raid_overrun_seconds: 0,
            dungeon_overrun_seconds: 0,
            ..Default::default()
        },
        storage: StorageSettings {
            recording_dir: AuthorizedPath::authorized(library),
            separate_buffer_dir: true,
            buffer_dir: AuthorizedPath::authorized(capture_root),
            limit: StorageLimit::Unlimited,
        },
        manual: ManualSettings {
            enabled: true,
            ..Default::default()
        },
        capture: CaptureSettings {
            extra_lead_in_seconds: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    config.save(&root.join("config.json")).unwrap();
}

fn empty_snapshot() -> AppSnapshot {
    AppSnapshot {
        entries: Arc::new(Vec::new()),
        correlations: Arc::new(Vec::new()),
        category_counts: Vec::new(),
        status: RecorderStatus::SetupRequired,
        active: None,
        config: Config::default(),
        setup_problems: Vec::new(),
        advanced_logging: Vec::new(),
        problems: Vec::new(),
        work: None,
        queued_jobs: 0,
        storage_used_bytes: 0,
        storage_limit: StorageLimit::Unlimited,
        protected_over_limit: false,
    }
}

// ---------------------------------------------------------------------------
// Combat-log helpers
// ---------------------------------------------------------------------------

/// `M/D HH:MM:SS.mmm` in UTC, matching the harness's zero UTC offset.
fn stamp(unix_ms: i64) -> String {
    let days = unix_ms.div_euclid(86_400_000);
    let ms_of_day = unix_ms.rem_euclid(86_400_000);
    let (_, month, day) = civil_from_days(days);
    format!(
        "{}/{} {:02}:{:02}:{:02}.{:03}",
        month,
        day,
        ms_of_day / 3_600_000,
        (ms_of_day / 60_000) % 60,
        (ms_of_day / 1_000) % 60,
        ms_of_day % 1_000
    )
}

fn current_year() -> i32 {
    civil_from_days(now_unix_ms().div_euclid(86_400_000)).0
}

/// Howard Hinnant's `civil_from_days`, the inverse of the parser's conversion.
fn civil_from_days(days: i64) -> (i32, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    ((year + i64::from(month <= 2)) as i32, month, day)
}

fn line(at_ms: i64, payload: &str) -> String {
    format!("{}  {payload}", stamp(at_ms))
}

/// The combatant plus self-cast pair every activity needs before its metadata
/// is complete.
fn player_lines(at_ms: i64) -> Vec<String> {
    vec![
        line(
            at_ms,
            &format!(
                "COMBATANT_INFO,{PLAYER_GUID},0,1,1,1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1,577"
            ),
        ),
        line(
            at_ms,
            &format!(
                "SPELL_AURA_APPLIED,{PLAYER_GUID},\"{PLAYER_NAME}\",{SELF_FLAGS},0x0,\
                 {PLAYER_GUID},\"{PLAYER_NAME}\",{SELF_FLAGS},0x0,1,\"Test Aura\",0x1,BUFF"
            ),
        ),
    ]
}

fn raid_start(at_ms: i64) -> Vec<String> {
    let mut lines = vec![line(
        at_ms,
        "ENCOUNTER_START,2820,\"Test Encounter\",16,20,2549",
    )];
    lines.extend(player_lines(at_ms));
    lines
}

fn raid_end(at_ms: i64, success: bool) -> String {
    line(
        at_ms,
        &format!(
            "ENCOUNTER_END,2820,\"Test Encounter\",16,20,{}",
            u8::from(success)
        ),
    )
}

fn player_death(at_ms: i64) -> String {
    line(
        at_ms,
        &format!(
            "UNIT_DIED,0000000000000000,nil,0x80000000,0x80000000,\
             {PLAYER_GUID},\"{PLAYER_NAME}\",{SELF_FLAGS},0x0"
        ),
    )
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Automatic PvE: replay lead-in, two-artifact capture, post-trim markers,
/// tag/protect/bulk delete, and a restart that sweeps and rescans.
#[test]
fn automatic_raid_completes_and_survives_a_restart() {
    let mut harness = Harness::new("raid");
    assert_eq!(harness.latest.status, RecorderStatus::Ready);

    let start_ms = now_unix_ms() - 1_000;
    harness.log(&raid_start(start_ms));
    harness.pump(|snapshot| snapshot.active.is_some());
    let active = harness.latest.active.clone().unwrap();
    // Detection is the encounter start itself, so the request is the extra
    // lead-in alone, clamped to the buffer.
    assert_eq!(active.requested_replay_ms, 10_000);
    assert_eq!(active.category, Category::Raids);

    harness.emit_artifacts(true);
    harness.log(&[player_death(start_ms + 500)]);
    harness.log(&[raid_end(now_unix_ms(), true)]);
    harness.pump(|snapshot| !snapshot.entries.is_empty());

    let entry = harness.latest.entries[0].clone();
    assert_eq!(entry.category, Category::Raids);
    assert_eq!(entry.outcome, Outcome::Win);
    assert!(entry.media_path.exists(), "media was not written");
    assert!(entry.sidecar_path.exists(), "sidecar was not written");
    // The replay is in front of the media, so the death sits later than its
    // 500 ms activity offset and still inside the media.
    let death = entry
        .timeline
        .iter()
        .find(|item| item.kind() == &TimelineKind::Death)
        .expect("death marker");
    assert!(
        death.start_ms() > 500 && death.start_ms() <= entry.duration_ms,
        "death at {} ms, duration {} ms",
        death.start_ms(),
        entry.duration_ms
    );
    // The intermediates were consumed by finalization.
    assert!(read_dir_count(&harness.capture_root.join("replay")) == 0);
    assert!(read_dir_count(&harness.capture_root.join("regular")) == 0);

    // Tag and protect go through the real sidecar.
    harness.send(Command::SetTag {
        id: entry.id.clone(),
        tag: "  keeper  ".to_owned(),
    });
    harness.send(Command::SetProtected {
        ids: vec![entry.id.clone()],
        value: true,
    });
    harness.pump(|snapshot| {
        snapshot
            .entries
            .first()
            .is_some_and(|entry| entry.protected && entry.tag.as_deref() == Some("  keeper  "))
    });

    // Restart: a stray artifact is quarantined and the entry is rescanned.
    let stray = harness.capture_root.join("regular/Video_stray.mkv");
    fs::write(&stray, b"interrupted").unwrap();
    let (root, library, capture_root, log_file) = (
        harness.root.clone(),
        harness.library.clone(),
        harness.capture_root.clone(),
        harness.log_file.clone(),
    );
    drop(harness);
    let restarted = Harness::attach(root, library.clone(), capture_root, log_file);
    assert_eq!(restarted.latest.entries.len(), 1);
    assert!(restarted.latest.entries[0].protected);
    assert!(!stray.exists(), "stray artifact was not swept");
    assert!(
        read_dir_count(&library.join(RECOVERY_DIR)) > 0,
        "nothing was quarantined"
    );

    let mut restarted = restarted;
    let id = restarted.latest.entries[0].id.clone();
    restarted.send(Command::Delete { ids: vec![id] });
    restarted.pump(|snapshot| snapshot.entries.is_empty());
}

/// PvP: a solo shuffle force-ended mid-activity is abandoned with zero overrun
/// and still produces a library entry with its round marker.
#[test]
fn force_ended_solo_shuffle_is_abandoned_and_saved() {
    let mut harness = Harness::new("shuffle");
    let start_ms = now_unix_ms() - 1_000;
    let mut lines = vec![line(
        start_ms,
        "ARENA_MATCH_START,2547,33,Rated Solo Shuffle,1",
    )];
    lines.extend(player_lines(start_ms));
    harness.log(&lines);
    harness.pump(|snapshot| snapshot.active.is_some());
    assert_eq!(
        harness.latest.active.as_ref().unwrap().category,
        Category::SoloShuffle
    );

    harness.emit_artifacts(true);
    harness.send(Command::ForceEnd);
    harness.pump(|snapshot| !snapshot.entries.is_empty());

    let entry = &harness.latest.entries[0];
    assert_eq!(entry.category, Category::SoloShuffle);
    assert_eq!(entry.outcome, Outcome::Loss);
    assert!(
        entry
            .timeline
            .iter()
            .any(|item| item.kind() == &TimelineKind::Round),
        "expected the unended round marker"
    );
}

/// Manual start/stop and a test recording reuse the same recorder, finalize,
/// and storage path.
#[test]
fn manual_and_test_recordings_reuse_the_capture_pipeline() {
    let mut harness = Harness::new("manual");

    harness.send(Command::StartManual);
    harness.pump(|snapshot| snapshot.active.is_some());
    assert_eq!(
        harness.latest.active.as_ref().unwrap().category,
        Category::Manual
    );
    harness.emit_artifacts(true);
    harness.send(Command::StopManual);
    harness.pump(|snapshot| !snapshot.entries.is_empty());
    let manual = harness.latest.entries[0].clone();
    assert_eq!(manual.category, Category::Manual);
    assert_eq!(manual.title, "Manual recording");

    // The test recording injects its own start and end.
    harness.send(Command::RunTest {
        category: Category::Raids,
    });
    harness.pump(|snapshot| snapshot.active.is_some());
    harness.emit_artifacts(true);
    harness.pump(|snapshot| snapshot.entries.len() == 2);
    assert_eq!(harness.entries_of(&Category::Raids).len(), 1);
}

/// Automatic finalization is dispatched before user transcodes that were
/// queued in the same tick, and both run on the one serial worker.
#[test]
fn finalization_precedes_queued_user_jobs() {
    let mut harness = Harness::new("queue");

    // One finished recording to clip against.
    let start_ms = now_unix_ms() - 1_000;
    harness.log(&raid_start(start_ms));
    harness.pump(|snapshot| snapshot.active.is_some());
    harness.emit_artifacts(true);
    harness.log(&[raid_end(now_unix_ms(), true)]);
    harness.pump(|snapshot| !snapshot.entries.is_empty());
    let source = harness.latest.entries[0].clone();

    // A second recording, then complete it and queue a clip before the tick's
    // single dispatch: both jobs are queued before dispatch chooses a job.
    harness.log(&raid_start(now_unix_ms() - 1_000));
    harness.pump(|snapshot| snapshot.active.is_some());
    harness.emit_artifacts(true);
    harness.log(&[raid_end(now_unix_ms(), true)]);
    harness.send(Command::CreateClip(ClipRange {
        source: source.id.clone(),
        start_ms: 0,
        end_ms: source.duration_ms.min(2_000),
    }));

    let mut order: Vec<Category> = Vec::new();
    harness.pump(|snapshot| {
        for entry in snapshot.entries.iter() {
            if !order.contains(&entry.category) && entry.id != source.id {
                order.push(entry.category.clone());
            }
        }
        order.len() == 2
    });
    assert_eq!(order, vec![Category::Raids, Category::Clip]);
}

/// Without a replay artifact the recording still saves, and markers that fall
/// before the media start are clipped away.
#[test]
fn missing_replay_falls_back_to_the_regular_recording() {
    let mut harness = Harness::new("regular-only");
    let start_ms = now_unix_ms() - 1_000;
    harness.log(&raid_start(start_ms));
    harness.pump(|snapshot| snapshot.active.is_some());
    harness.emit_artifacts(false);
    harness.log(&[player_death(start_ms + 500)]);
    harness.log(&[raid_end(now_unix_ms(), true)]);
    harness.pump(|snapshot| !snapshot.entries.is_empty());

    let entry = &harness.latest.entries[0];
    assert_eq!(entry.category, Category::Raids);
    assert!(entry.media_path.exists());
    assert!(
        !entry
            .timeline
            .iter()
            .any(|item| item.kind() == &TimelineKind::Death),
        "a death before the media start should be clipped: {:?}",
        entry.timeline
    );
}

/// A missing regular artifact is an actionable problem, not a silently lost
/// library entry.
#[test]
fn missing_regular_artifact_reports_a_problem() {
    let mut harness = Harness::new("failure");
    let start_ms = now_unix_ms() - 1_000;
    harness.log(&raid_start(start_ms));
    harness.pump(|snapshot| snapshot.active.is_some());
    // GSR never reports either artifact.
    harness.log(&[raid_end(now_unix_ms(), true)]);
    harness.pump(|snapshot| !snapshot.problems.is_empty());
    assert!(harness.latest.entries.is_empty());
    assert!(
        harness
            .latest
            .problems
            .iter()
            .any(|problem| problem.summary.contains("no video file")),
        "problems: {:?}",
        harness.latest.problems
    );
}

/// The production wiring starts, publishes, and shuts down without leaking a
/// thread or requiring GTK.
#[test]
fn production_handle_starts_and_shuts_down() {
    let root = std::env::temp_dir().join(format!("wr-slice-handle-{}", uuid::Uuid::new_v4()));
    let library = root.join("recordings with space");
    let capture_root = root.join("buffer");
    let log_dir = root.join("wow/_retail_/Logs");
    for directory in [&library, &capture_root, &log_dir] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(log_dir.join("WoWCombatLog.txt"), b"").unwrap();
    write_config(&root, &library, &capture_root, &log_dir);

    let mut handle = start(setup(&root));
    let snapshot = handle
        .snapshots
        .recv_timeout(STEP_TIMEOUT)
        .expect("first snapshot");
    assert_eq!(snapshot.status, RecorderStatus::Ready);
    assert!(handle.send(Command::Disarm));
    handle.shutdown();
}

fn read_dir_count(path: &Path) -> usize {
    fs::read_dir(path)
        .map(|entries| entries.count())
        .unwrap_or(0)
}
