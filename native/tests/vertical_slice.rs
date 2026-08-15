// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless vertical slice: config, log polling, activity detection, recorder
//! control, storage, and media jobs behind the real coordinator.
//!
//! The recorder and FFmpeg are replaced by shell fakes; every
//! other type is the production one operating on a temp directory. The
//! coordinator core is stepped with `tick()`, so no test sleeps or timing
//! assertions are needed.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use warcraft_recorder::config::{
    ActivitySettings, AuthorizedPath, CaptureSettings, Config, FlavorConfig, LayoutSettings,
    ManualSettings, StorageSettings,
};
use warcraft_recorder::coordinator::{AppSnapshot, ClipRange, Command, Coordinator, Setup, start};
use warcraft_recorder::domain::{
    Category, MeterFight, MeterMetric, Outcome, RecorderStatus, StorageLimit, TimelineKind,
};
use warcraft_recorder::media_jobs::MediaConfig;
use warcraft_recorder::meter::{MeterProjection, project_current, project_overall};
use warcraft_recorder::recorder::Timeouts;
use warcraft_recorder::storage::{RECOVERY_DIR, now_unix_ms};

const PLAYER_GUID: &str = "Player-1092-0A70E103";
const PLAYER_NAME: &str = "Testplayer-Testrealm";
/// Hostile boss the player's spells land on; its flags are not friendly.
const BOSS_GUID: &str = "Creature-0-3013-2820-74284-0000266503";
const BOSS_FLAGS: &str = "0x10a48";
const SELF_FLAGS: &str = "0x511";
/// Long enough for real process spawns and FFmpeg fakes, short enough to fail
/// fast. Nothing is asserted about how long a step actually takes.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

// --- Harness ---

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
        let mut coordinator =
            Coordinator::new(setup(&root), commands_rx, snapshot_tx, Box::new(|| {}));
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
        // Startup publishes the scanned library before it arms, so settle on
        // the post-arm snapshot rather than the first one out.
        harness.pump(|snapshot| {
            !matches!(
                snapshot.status,
                RecorderStatus::SetupRequired | RecorderStatus::WaitingForWow
            )
        });
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
        // The regular hook fires after the subsequent stop signal; this
        // fixture emits both artifacts before it drives that signal.
        writeln!(
            file,
            "{}\tregular\t{}",
            now_unix_ms() + 1_000,
            path.display()
        )
        .unwrap();
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
            // Ends are asynchronous now, so this budget spans however long a
            // test spends between the stop request and the fake hook's event,
            // including `Config::save`'s two fsyncs. Keep it far enough above
            // that work that a loaded filesystem cannot expire it: the failure
            // mode is a dropped recording and a 20 s `pump` timeout, not a
            // clear assertion. Only the missing-artifact test waits it out.
            regular_event: Duration::from_secs(2),
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

// --- Combat-log helpers ---

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
    let mut lines = vec![
        line(
            at_ms,
            "COMBAT_LOG_VERSION,22,ADVANCED_LOG_ENABLED,1,BUILD_VERSION,12.1.0,PROJECT_ID,1",
        ),
        line(at_ms, "ENCOUNTER_START,2820,\"Test Encounter\",16,20,2549"),
    ];
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

/// The player's spell on the hostile boss in the retail advanced-block shape.
/// The tiny HP values keep the destination below the boss-health floor, so the
/// existing boss-health behavior is untouched.
fn spell_damage(at_ms: i64) -> String {
    line(
        at_ms,
        &format!(
            "SPELL_DAMAGE,{PLAYER_GUID},\"{PLAYER_NAME}\",{SELF_FLAGS},0x0,{BOSS_GUID},\"Test Boss\",\
             {BOSS_FLAGS},0x0,585,\"Smite\",0x2,{BOSS_GUID},0000000000000000,105,152,0,0,189,2084,0,0,0,\
             0,0,0,0,0,0,0,0,1500,0,2,0,0,0,1,0,0,0,0.000,1,1"
        ),
    )
}

/// A self-heal in the modern suffix layout; 400 of the 1000 points overheat.
fn spell_heal(at_ms: i64) -> String {
    line(
        at_ms,
        &format!(
            "SPELL_HEAL,{PLAYER_GUID},\"{PLAYER_NAME}\",{SELF_FLAGS},0x0,{PLAYER_GUID},\"{PLAYER_NAME}\",\
             {SELF_FLAGS},0x0,2061,\"Flash Heal\",0x2,{PLAYER_GUID},0000000000000000,500,500,0,0,0,0,0,0,0,0,\
             0,0,0,0,0,0,0,1000,600,400,0,1"
        ),
    )
}

// --- Scenarios ---

#[test]
fn automatic_raid_completes_and_survives_a_restart() {
    let mut harness = Harness::new("raid");
    assert_eq!(harness.latest.status, RecorderStatus::Ready);

    let start_ms = now_unix_ms() - 2_000;
    harness.log(&raid_start(start_ms));
    harness.pump(|snapshot| snapshot.active.is_some());
    let active = harness.latest.active.clone().unwrap();
    // Detection is the encounter start itself, so the request is the extra
    // lead-in alone, clamped to the buffer.
    assert_eq!(active.requested_replay_ms, 10_000);
    assert_eq!(active.category, Category::Raids);

    harness.emit_artifacts(true);
    harness.log(&[
        spell_damage(start_ms + 250),
        spell_heal(start_ms + 300),
        spell_damage(start_ms + 1_250),
    ]);
    harness.log(&[player_death(start_ms + 1_500)]);
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
    // The version-22 advanced-layout events survived parsing into per-second
    // deltas while retaining their exact full-fight aggregates.
    let fights = &entry.meter.fights;
    assert_eq!(fights.len(), 1, "expected exactly one meter fight");
    assert_eq!(meter_total(&fights[0], MeterMetric::Damage), 3_000);
    assert_eq!(meter_total(&fights[0], MeterMetric::Healing), 600);
    let damage = fights[0].actors[0]
        .spells
        .iter()
        .find(|entry| entry.metric == MeterMetric::Damage)
        .expect("damage spell");
    assert_eq!(damage.samples.len(), 2);
    assert_eq!(
        damage
            .samples
            .iter()
            .map(|sample| sample.amount)
            .collect::<Vec<_>>(),
        vec![1_500, 1_500]
    );
    assert!(damage.samples[0].at_ms < damage.samples[1].at_ms);
    assert!(
        damage
            .samples
            .iter()
            .all(|sample| sample.at_ms <= entry.duration_ms)
    );
    let first_at = damage.samples[0].at_ms;
    let second_at = damage.samples[1].at_ms;
    assert_eq!(
        projection_total(
            &project_current(fights, first_at.saturating_sub(1)).unwrap(),
            MeterMetric::Damage,
        ),
        0
    );
    assert_eq!(
        projection_total(
            &project_current(fights, first_at).unwrap(),
            MeterMetric::Damage,
        ),
        1_500
    );
    assert_eq!(
        projection_total(&project_overall(fights, second_at), MeterMetric::Damage),
        3_000
    );

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

    // The rescanned sidecar carries the same full and per-second aggregates.
    let fights = &restarted.latest.entries[0].meter.fights;
    assert_eq!(fights, &entry.meter.fights);

    let mut restarted = restarted;
    let id = restarted.latest.entries[0].id.clone();
    restarted.send(Command::Delete { ids: vec![id] });
    restarted.pump(|snapshot| snapshot.entries.is_empty());
}

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
    let raid = harness.entries_of(&Category::Raids)[0];
    assert_eq!(raid.meter.fights.len(), 1);
    assert_eq!(
        meter_total(&raid.meter.fights[0], MeterMetric::Damage),
        7_800_000
    );
    assert_eq!(raid.meter.fights[0].actors.len(), 2);
}

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

#[test]
fn commands_are_served_while_a_capture_is_ending() {
    let mut harness = Harness::new("ending");
    harness.log(&raid_start(now_unix_ms() - 1_000));
    harness.pump(|snapshot| snapshot.active.is_some());

    // End the activity without the hook reporting any artifact yet.
    harness.log(&[raid_end(now_unix_ms(), true)]);
    harness.pump(|snapshot| snapshot.active.is_none());
    assert!(
        matches!(harness.latest.status, RecorderStatus::Finalizing { .. }),
        "status {:?}",
        harness.latest.status
    );
    assert!(harness.latest.entries.is_empty());

    harness.send(Command::SetSelectedCategory {
        category: Category::MythicPlus,
    });
    harness.pump(|snapshot| snapshot.config.interface.selected_category == Category::MythicPlus);
    assert!(
        harness.latest.entries.is_empty(),
        "the capture must still be waiting on its artifacts"
    );

    // The artifacts finally land: the recording finalizes as usual.
    harness.emit_artifacts(true);
    harness.pump(|snapshot| !snapshot.entries.is_empty());
    assert_eq!(harness.latest.entries[0].category, Category::Raids);
}

#[test]
fn the_migration_notice_survives_a_restart_and_one_dismissal_ends_it() {
    let harness = Harness::new("migration-notice");
    let config_path = harness.root.join("config.json");
    let mut config = Config::load(&config_path).expect("load the harness config");
    config.migration_notice_pending = true;
    config.save(&config_path).expect("raise the notice");
    let (root, library, capture_root, log_file) = (
        harness.root.clone(),
        harness.library.clone(),
        harness.capture_root.clone(),
        harness.log_file.clone(),
    );
    drop(harness);

    // What the first launch after a legacy import looks like to the shell.
    let mut harness = Harness::attach(root, library, capture_root, log_file);
    assert!(harness.latest.config.migration_notice_pending);

    harness.send(Command::DismissMigrationNotice);
    harness.pump(|snapshot| !snapshot.config.migration_notice_pending);
    assert!(
        !Config::load(&config_path)
            .expect("reload the saved config")
            .migration_notice_pending,
        "the dismissal must outlive the process"
    );

    // The notice offers the button that opens Settings, so the draft a save
    // carries was cloned while the notice was still pending. Applying it must
    // not bring the notice back, or it reappears on every later start.
    let mut stale = harness.latest.config.clone();
    stale.migration_notice_pending = true;
    stale.capture.fps = 30;
    harness.send(Command::SaveConfig {
        draft: Box::new(stale),
    });
    harness.pump(|snapshot| snapshot.config.capture.fps == 30);
    assert!(
        !harness.latest.config.migration_notice_pending,
        "a settings save must not resurrect the dismissed notice"
    );
    assert!(
        !Config::load(&config_path)
            .expect("reload after the settings save")
            .migration_notice_pending,
        "the resurrected notice must not reach disk either"
    );
}

#[test]
fn dismissing_the_release_notes_records_the_running_version_for_good() {
    let harness = Harness::new("release-notes");
    let config_path = harness.root.join("config.json");
    let mut config = Config::load(&config_path).expect("load the harness config");
    // What an install updated from an earlier version looks like: the field
    // is missing from its config file, so it deserializes empty.
    config.last_seen_version = String::new();
    config.save(&config_path).expect("clear the seen version");
    let (root, library, capture_root, log_file) = (
        harness.root.clone(),
        harness.library.clone(),
        harness.capture_root.clone(),
        harness.log_file.clone(),
    );
    drop(harness);

    let mut harness = Harness::attach(root, library, capture_root, log_file);
    assert!(harness.latest.config.last_seen_version.is_empty());

    harness.send(Command::DismissReleaseNotes);
    harness.pump(|snapshot| snapshot.config.last_seen_version == warcraft_recorder::VERSION);
    assert_eq!(
        Config::load(&config_path)
            .expect("reload the saved config")
            .last_seen_version,
        warcraft_recorder::VERSION,
        "the acknowledgement must outlive the process"
    );

    // Settings dialogs opened before the notes were closed carry a draft that
    // still has the old value; applying it must not replay the dialog.
    let mut stale = harness.latest.config.clone();
    stale.last_seen_version = String::new();
    stale.capture.fps = 30;
    harness.send(Command::SaveConfig {
        draft: Box::new(stale),
    });
    harness.pump(|snapshot| snapshot.config.capture.fps == 30);
    assert_eq!(
        harness.latest.config.last_seen_version,
        warcraft_recorder::VERSION,
        "a settings save must not resurrect the dismissed notes"
    );
}

#[test]
fn a_dragged_layout_outlives_the_process() {
    let mut harness = Harness::new("layout");
    assert_eq!(
        harness.latest.config.interface.layout,
        LayoutSettings::default(),
        "a clean start stores nothing, which is what lets the pane autoscale"
    );

    let layout = LayoutSettings {
        player_split: Some(612),
        column_widths: BTreeMap::from([("Dungeon".to_owned(), 240)]),
    };
    harness.send(Command::SaveLayout {
        layout: layout.clone(),
    });
    harness.pump(|snapshot| snapshot.config.interface.layout == layout);

    let (root, library, capture_root, log_file) = (
        harness.root.clone(),
        harness.library.clone(),
        harness.capture_root.clone(),
        harness.log_file.clone(),
    );
    drop(harness);

    let harness = Harness::attach(root, library, capture_root, log_file);
    assert_eq!(harness.latest.config.interface.layout, layout);
}

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

    let mut handle = start(setup(&root), Box::new(|| {}));
    let mut snapshot = handle.snapshots.recv_timeout(STEP_TIMEOUT);
    while let Ok(current) = &snapshot {
        if current.status == RecorderStatus::Ready {
            break;
        }
        snapshot = handle.snapshots.recv_timeout(STEP_TIMEOUT);
    }
    assert_eq!(
        snapshot.expect("armed snapshot").status,
        RecorderStatus::Ready
    );
    assert!(handle.send(Command::Disarm));
    handle.shutdown();
}

/// Actor totals derive structurally from the spell entries.
fn meter_total(fight: &MeterFight, metric: MeterMetric) -> u64 {
    fight
        .actors
        .iter()
        .flat_map(|actor| &actor.spells)
        .filter(|entry| entry.metric == metric)
        .map(|entry| entry.amount)
        .sum()
}

fn projection_total(projection: &MeterProjection, metric: MeterMetric) -> u64 {
    projection
        .actors
        .iter()
        .flat_map(|actor| &actor.spells)
        .filter(|entry| entry.metric == metric)
        .map(|entry| entry.amount)
        .sum()
}

fn read_dir_count(path: &Path) -> usize {
    fs::read_dir(path)
        .map(|entries| entries.count())
        .unwrap_or(0)
}
