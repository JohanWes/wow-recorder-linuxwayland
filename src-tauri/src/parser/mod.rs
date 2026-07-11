//! Retail combat-log parsing.  This module deliberately has no Tauri state:
//! callers provide settings and consume [`ParserEvent`]s.
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc as tokio_mpsc;

pub mod constants {
    /// Enough retail data to classify logs. Unknown raids intentionally retain zone 0,
    /// matching the Electron fallback.
    pub fn difficulty(id: i64) -> Option<(&'static str, bool)> {
        match id {
            17 => Some(("lfr", true)),
            14 => Some(("normal", true)),
            15 => Some(("heroic", true)),
            16 => Some(("mythic", true)),
            8 => Some(("mythic", false)),
            _ => None,
        }
    }
    pub fn arena(id: i64) -> &'static str {
        match id {
            2547 => "Nagrand Arena",
            1134 => "Tiger's Peak",
            1504 => "Black Rook Hold Arena",
            _ => "Unknown Arena",
        }
    }
    pub fn battleground(id: i64) -> Option<&'static str> {
        match id {
            30 => Some("Alterac Valley"),
            2107 | 1681 => Some("Arathi Basin"),
            1105 | 2245 => Some("Deepwind Gorge"),
            566 => Some("Eye of the Storm"),
            968 => Some("Eye of the Storm"),
            628 => Some("Isle of Conquest"),
            1803 => Some("Seething Shore"),
            727 => Some("Silvershard Mines"),
            998 => Some("Temple of Kotmogu"),
            761 => Some("The Battle for Gilneas"),
            726 => Some("Twin Peaks"),
            489 | 2106 => Some("Warsong Gulch"),
            2656 => Some("Deephaul Ravine"),
            2188 => Some("Wintergrasp"),
            _ => None,
        }
    }
    pub fn dungeon(map: i64) -> Option<(&'static str, [f64; 3])> {
        match map {
            405 => Some(("Brackenhide Hollow", [1800., 1440., 1080.])),
            376 => Some(("The Necrotic Wake", [1800., 1440., 1080.])),
            _ => None,
        }
    }
    pub fn dungeon_encounter(id: i64) -> bool {
        matches!(id, 2567 | 2568 | 2569 | 2570)
    }
    pub fn raid_zone(id: i64) -> i64 {
        match id {
            9999 => 0,
            3181 | 3182 => 2769,
            _ => 0,
        }
    }
    /// Current-tier flag. Keep this small data table easy to update with a season.
    pub fn current_raid(id: i64) -> bool {
        matches!(id, 3181 | 3182)
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    original: String,
    timestamp: String,
    args: Vec<String>,
    pos: usize,
}
impl LogLine {
    pub fn new(line: impl Into<String>) -> Self {
        let original = line.into();
        let p = original.find("  ").map(|v| v + 2).unwrap_or(0);
        let timestamp = original[..p.saturating_sub(2)].to_owned();
        let mut s = Self {
            original,
            timestamp,
            args: vec![],
            pos: p,
        };
        s.parse_to(1);
        s
    }
    pub fn original(&self) -> &str {
        &self.original
    }
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }
    pub fn event_type(&mut self) -> String {
        self.arg(0).unwrap_or_default().to_owned()
    }
    pub fn arg(&mut self, n: usize) -> Option<&str> {
        self.parse_to(n + 1);
        self.args.get(n).map(String::as_str)
    }
    pub fn date(&self) -> DateTime<Local> {
        let nums: Vec<u32> = self
            .timestamp
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|x| x.parse().ok())
            .collect();
        let now = Local::now();
        let (m, d, y, h, mi, s) = if nums.len() >= 7 {
            (nums[0], nums[1], nums[2] as i32, nums[3], nums[4], nums[5])
        } else {
            (
                nums.get(0).copied().unwrap_or(now.month()),
                nums.get(1).copied().unwrap_or(now.day()),
                now.year(),
                nums.get(2).copied().unwrap_or(0),
                nums.get(3).copied().unwrap_or(0),
                nums.get(4).copied().unwrap_or(0),
            )
        };
        let nd = NaiveDate::from_ymd_opt(y, m, d)
            .unwrap_or_else(|| now.date_naive())
            .and_hms_opt(h, mi, s)
            .unwrap_or_else(|| now.naive_local());
        Local.from_local_datetime(&nd).single().unwrap_or(now)
    }
    fn parse_to(&mut self, wanted: usize) {
        let bytes = self.original.as_bytes();
        let mut value = String::new();
        let mut quoted = false;
        let mut depth = 0usize;
        while self.pos < bytes.len() && self.args.len() < wanted {
            let c = bytes[self.pos] as char;
            self.pos += 1;
            if c == '\n' || c == '\r' {
                break;
            }
            if quoted {
                if c == '"' {
                    quoted = false
                } else {
                    value.push(c)
                };
                continue;
            }
            match c {
                '"' => quoted = true,
                '[' | '(' => {
                    depth += 1;
                    value.push(c)
                }
                ']' | ')' => {
                    depth = depth.saturating_sub(1);
                    value.push(c)
                }
                ',' if depth == 0 => {
                    self.args.push(std::mem::take(&mut value));
                }
                _ => value.push(c),
            }
        }
        if self.args.len() < wanted && (!value.is_empty() || self.pos >= bytes.len()) {
            self.args.push(value);
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCombatant {
    #[serde(rename = "_GUID")]
    pub guid: String,
    #[serde(rename = "_teamID", skip_serializing_if = "Option::is_none")]
    pub team_id: Option<i64>,
    #[serde(rename = "_specID", skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<i64>,
    #[serde(rename = "_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "_realm", skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    #[serde(rename = "_region", skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Death {
    pub name: String,
    pub spec_id: i64,
    pub date: DateTime<Local>,
    pub timestamp: f64,
    pub friendly: bool,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub category: String,
    #[serde(rename = "zoneID", skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<i64>,
    #[serde(rename = "encounterID", skip_serializing_if = "Option::is_none")]
    pub encounter_id: Option<i64>,
    #[serde(rename = "difficultyID", skip_serializing_if = "Option::is_none")]
    pub difficulty_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    pub duration: f64,
    pub result: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deaths: Option<Vec<Death>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<RawCombatant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solo_shuffle_rounds_won: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solo_shuffle_rounds_played: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keystone_level: Option<i64>,
    #[serde(rename = "mapID", skip_serializing_if = "Option::is_none")]
    pub map_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affixes: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encounter_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_mmr: Option<i64>,
    pub overrun: f64,
    pub flavour: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique_hash: Option<String>,
    pub combatants: Vec<RawCombatant>,
}

#[derive(Debug, Clone)]
pub struct ParserSettings {
    pub record_raids: bool,
    pub record_dungeons: bool,
    pub record_2v2: bool,
    pub record_3v3: bool,
    pub record_skirmish: bool,
    pub record_solo_shuffle: bool,
    pub record_battlegrounds: bool,
    pub min_encounter_duration: f64,
    pub min_keystone_level: i64,
    pub min_raid_difficulty: String,
    pub raid_overrun: f64,
    pub dungeon_overrun: f64,
    pub record_current_raid_encounters_only: bool,
    pub inactivity_minutes: u64,
}
impl Default for ParserSettings {
    fn default() -> Self {
        Self {
            record_raids: true,
            record_dungeons: true,
            record_2v2: true,
            record_3v3: true,
            record_skirmish: true,
            record_solo_shuffle: true,
            record_battlegrounds: true,
            min_encounter_duration: 0.,
            min_keystone_level: 0,
            min_raid_difficulty: "lfr".into(),
            raid_overrun: 3.,
            dungeon_overrun: 0.,
            record_current_raid_encounters_only: false,
            inactivity_minutes: 10,
        }
    }
}
#[derive(Debug, Clone)]
pub enum ParserEvent {
    /// Begin recording an activity.
    ActivityStarted {
        start_date: DateTime<Local>,
        category: String,
        offset_hint: f64,
    },
    /// Stop recording and save the completed or force-ended activity.
    ActivityEnded {
        metadata: Metadata,
        activity_start: DateTime<Local>,
        activity_end: DateTime<Local>,
        overrun_seconds: f64,
        video_name: String,
    },
    /// Stop recording and discard it without producing metadata.
    ForceEnd,
}

#[derive(Debug, Clone)]
enum Kind {
    Raid {
        id: i64,
        name: String,
        difficulty: i64,
    },
    Dungeon {
        zone: i64,
        zone_name: String,
        map: i64,
        level: i64,
        affixes: Vec<i64>,
        cmd_duration: Option<f64>,
    },
    Arena {
        zone: i64,
        category: String,
    },
    Solo {
        zone: i64,
        rounds: Vec<Round>,
    },
    Bg {
        zone: i64,
    },
    Manual,
}
#[derive(Debug, Clone)]
struct Round {
    start: DateTime<Local>,
    end: Option<DateTime<Local>>,
    result: bool,
}
#[derive(Debug, Clone)]
struct Activity {
    kind: Kind,
    start: DateTime<Local>,
    end: Option<DateTime<Local>>,
    result: bool,
    overrun: f64,
    combatants: HashMap<String, RawCombatant>,
    player: Option<String>,
    deaths: Vec<Death>,
}
impl Activity {
    fn category(&self) -> &str {
        match &self.kind {
            Kind::Raid { .. } => "Raids",
            Kind::Dungeon { .. } => "Mythic+",
            Kind::Arena { category, .. } => category,
            Kind::Solo { .. } => "Solo Shuffle",
            Kind::Bg { .. } => "Battlegrounds",
            Kind::Manual => "Manual",
        }
    }
    fn player(&self) -> Option<RawCombatant> {
        self.player
            .as_ref()
            .and_then(|p| self.combatants.get(p))
            .cloned()
    }
}

pub struct Parser {
    settings: ParserSettings,
    tx: tokio_mpsc::Sender<ParserEvent>,
    activity: Option<Activity>,
    _watcher: Option<RecommendedWatcher>,
    watched: Option<mpsc::Receiver<WatchMessage>>,
}
enum WatchMessage {
    Line(String),
    Timeout,
}
impl Parser {
    pub fn new(settings: ParserSettings, tx: tokio_mpsc::Sender<ParserEvent>) -> Self {
        Self {
            settings,
            tx,
            activity: None,
            _watcher: None,
            watched: None,
        }
    }
    pub fn watch(&mut self, dir: impl AsRef<Path>) -> notify::Result<()> {
        let dir = dir.as_ref().to_path_buf();
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |r| {
            let _ = tx.send(r);
        })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
        let (line_tx, line_rx) = mpsc::channel();
        let timeout = self.settings.inactivity_minutes;
        thread::spawn(move || {
            let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
            let mut last = Instant::now();
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(e)) => {
                        for p in e.paths {
                            let name = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
                            if !name.starts_with("WoWCombatLog") || !name.ends_with(".txt") {
                                continue;
                            }
                            if matches!(e.kind, EventKind::Remove(_) | EventKind::Create(_)) {
                                offsets.remove(&p);
                            }
                            if let Ok(meta) = fs::metadata(&p) {
                                let off = offsets.get(&p).copied().unwrap_or(0);
                                if meta.len() < off {
                                    offsets.remove(&p);
                                }
                                let off = offsets.get(&p).copied().unwrap_or(0);
                                if meta.len() > off {
                                    if let Ok(b) = fs::read(&p) {
                                        offsets.insert(p.clone(), meta.len());
                                        for line in
                                            String::from_utf8_lossy(&b[off as usize..]).lines()
                                        {
                                            let _ = line_tx
                                                .send(WatchMessage::Line(line.trim().to_owned()));
                                        }
                                        last = Instant::now();
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if last.elapsed() >= Duration::from_secs(timeout * 60) {
                    let _ = line_tx.send(WatchMessage::Timeout);
                    last = Instant::now()
                }
            }
        });
        self._watcher = Some(watcher);
        self.watched = Some(line_rx);
        Ok(())
    }
    /// Drain filesystem events. Call this from the manager's periodic task.
    pub fn poll_watch(&mut self) {
        loop {
            let next = self.watched.as_ref().and_then(|r| r.try_recv().ok());
            match next {
                Some(WatchMessage::Line(line)) => self.inject_raw_line(&line),
                Some(WatchMessage::Timeout) => self.force_end(),
                None => break,
            }
        }
    }
    pub fn inject_raw_line(&mut self, line: &str) {
        self.handle(LogLine::new(line.trim()));
    }
    pub fn handle_manual_recording_toggle(&mut self) {
        if self
            .activity
            .as_ref()
            .map(|a| matches!(a.kind, Kind::Manual))
            .unwrap_or(false)
        {
            self.end(Local::now(), true)
        } else if self.activity.is_none() {
            self.start(Kind::Manual, Local::now(), 0.)
        }
    }
    pub fn drop_activity(&mut self) {
        self.activity = None
    }
    pub fn force_end(&mut self) {
        if self.activity.is_some() {
            self.end(Local::now(), false);
        }
    }
    fn start(&mut self, kind: Kind, start: DateTime<Local>, overrun: f64) {
        let cat = match &kind {
            Kind::Raid { .. } => self.settings.record_raids,
            Kind::Dungeon { .. } => self.settings.record_dungeons,
            Kind::Arena { category, .. } => match category.as_str() {
                "2v2" => self.settings.record_2v2,
                "3v3" => self.settings.record_3v3,
                "Skirmish" => self.settings.record_skirmish,
                _ => false,
            },
            Kind::Solo { .. } => self.settings.record_solo_shuffle,
            Kind::Bg { .. } => self.settings.record_battlegrounds,
            Kind::Manual => true,
        };
        if !cat {
            return;
        }
        let a = Activity {
            kind,
            start,
            end: None,
            result: false,
            overrun,
            combatants: HashMap::new(),
            player: None,
            deaths: vec![],
        };
        let _ = self.tx.try_send(ParserEvent::ActivityStarted {
            start_date: start,
            category: a.category().into(),
            offset_hint: (Local::now() - start).num_milliseconds() as f64 / 1000.,
        });
        self.activity = Some(a)
    }
    fn handle(&mut self, mut l: LogLine) {
        let ty = l.event_type();
        if matches!(self.activity.as_ref().map(|a| &a.kind), Some(Kind::Manual)) {
            return;
        }
        match ty.as_str() {
            "ENCOUNTER_START" => {
                let id = num(l.arg(1));
                if constants::dungeon_encounter(id) {
                    return;
                }
                if self.activity.is_none() {
                    let d = num(l.arg(3));
                    let cur = self.settings.record_current_raid_encounters_only;
                    let allowed = !cur || constants::current_raid(id);
                    let min = diff_rank(&self.settings.min_raid_difficulty);
                    if allowed
                        && constants::difficulty(d)
                            .map(|x| x.1 && diff_rank(x.0) >= min)
                            .unwrap_or(false)
                    {
                        self.start(
                            Kind::Raid {
                                id,
                                name: l.arg(2).unwrap_or("").to_owned(),
                                difficulty: d,
                            },
                            l.date(),
                            3.,
                        )
                    }
                }
            }
            "ENCOUNTER_END" => {
                if matches!(
                    self.activity.as_ref().map(|a| &a.kind),
                    Some(Kind::Raid { .. })
                ) {
                    let ok = num(l.arg(5)) != 0;
                    if ok {
                        if let Some(a) = self.activity.as_mut() {
                            a.overrun = self.settings.raid_overrun;
                        }
                    }
                    self.end(l.date(), ok)
                }
            }
            "CHALLENGE_MODE_START" => {
                if self.activity.is_none() {
                    let map = num(l.arg(3));
                    let lvl = num(l.arg(4));
                    if lvl >= self.settings.min_keystone_level {
                        let zone_name = l.arg(1).unwrap_or("").to_owned();
                        let aff = parse_list(l.arg(5).unwrap_or(""));
                        self.start(
                            Kind::Dungeon {
                                zone: num(l.arg(2)),
                                zone_name,
                                map,
                                level: lvl,
                                affixes: aff,
                                cmd_duration: None,
                            },
                            l.date(),
                            0.,
                        )
                    }
                }
            }
            "CHALLENGE_MODE_END" => {
                if matches!(
                    self.activity.as_ref().map(|a| &a.kind),
                    Some(Kind::Dungeon { .. })
                ) {
                    let ok = num(l.arg(2)) != 0;
                    if let Some(a) = self.activity.as_mut() {
                        if let Kind::Dungeon { cmd_duration, .. } = &mut a.kind {
                            *cmd_duration = Some(num(l.arg(4)) as f64 / 1000.);
                        }
                        if ok {
                            a.overrun = self.settings.dungeon_overrun
                        }
                    }
                    self.end(l.date(), ok)
                }
            }
            "ARENA_MATCH_START" => {
                let zone = num(l.arg(1));
                let k = l.arg(3).unwrap_or("");
                if k == "Rated Solo Shuffle" {
                    if let Some(Activity {
                        kind: Kind::Solo { rounds, .. },
                        ..
                    }) = self.activity.as_mut()
                    {
                        rounds.push(Round {
                            start: l.date(),
                            end: None,
                            result: false,
                        })
                    } else if self.activity.is_none() {
                        self.start(
                            Kind::Solo {
                                zone,
                                rounds: vec![Round {
                                    start: l.date(),
                                    end: None,
                                    result: false,
                                }],
                            },
                            l.date(),
                            3.,
                        )
                    }
                } else if self.activity.is_none() {
                    let cat = match k {
                        "2v2" => "2v2",
                        "3v3" | "5v5" => "3v3",
                        "Skirmish" => "Skirmish",
                        _ => return,
                    };
                    self.start(
                        Kind::Arena {
                            zone,
                            category: cat.into(),
                        },
                        l.date(),
                        3.,
                    )
                }
            }
            "ARENA_MATCH_END" => {
                if matches!(
                    self.activity.as_ref().map(|a| &a.kind),
                    Some(Kind::Solo { .. })
                ) {
                    self.end(l.date(), true)
                } else if matches!(
                    self.activity.as_ref().map(|a| &a.kind),
                    Some(Kind::Arena { .. })
                ) {
                    let win = num(l.arg(1));
                    let result = self
                        .activity
                        .as_ref()
                        .and_then(|a| a.player())
                        .and_then(|p| p.team_id)
                        .map(|x| x == win)
                        .unwrap_or(false);
                    self.end(l.date(), result)
                }
            }
            "ZONE_CHANGE" | "MAP_CHANGE" => {
                let z = num(l.arg(1));
                let bg = constants::battleground(z).is_some();
                if bg && self.activity.is_none() {
                    self.start(Kind::Bg { zone: z }, l.date(), 3.)
                } else if !bg
                    && matches!(
                        self.activity.as_ref().map(|a| &a.kind),
                        Some(Kind::Bg { .. }) | Some(Kind::Arena { .. })
                    )
                {
                    self.end(l.date(), false)
                }
            }
            "COMBATANT_INFO" => self.combatant(&mut l),
            "SPELL_AURA_APPLIED" => self.aura(&mut l),
            "UNIT_DIED" => self.death(&mut l),
            _ => {}
        }
    }
    fn combatant(&mut self, l: &mut LogLine) {
        if let Some(a) = self.activity.as_mut() {
            let id = l.arg(1).unwrap_or("").to_owned();
            a.combatants.entry(id.clone()).or_insert(RawCombatant {
                guid: id,
                team_id: Some(num(l.arg(2))),
                spec_id: Some(num(l.arg(25))),
                ..Default::default()
            });
        }
    }
    fn aura(&mut self, l: &mut LogLine) {
        if let Some(a) = self.activity.as_mut() {
            let flags = hex(l.arg(3));
            if flags & 0x400 != 0 {
                let id = l.arg(1).unwrap_or("").to_owned();
                let c = a.combatants.entry(id.clone()).or_insert(RawCombatant {
                    guid: id.clone(),
                    ..Default::default()
                });
                if c.name.is_none() {
                    let (n, r) = split_name(l.arg(2).unwrap_or(""));
                    c.name = Some(n);
                    c.realm = r
                }
                if flags & 1 != 0 {
                    a.player = Some(id)
                }
            }
        }
    }
    fn death(&mut self, l: &mut LogLine) {
        if let Some(a) = self.activity.as_mut() {
            let flags = hex(l.arg(7));
            if flags & 0x400 == 0 || num(l.arg(9)) != 0 {
                return;
            }
            let id = l.arg(5).unwrap_or("").to_owned();
            let name = l.arg(6).unwrap_or("").to_owned();
            let d = Death {
                name,
                spec_id: a.combatants.get(&id).and_then(|x| x.spec_id).unwrap_or(0),
                date: l.date(),
                timestamp: ((l.date() - a.start).num_milliseconds() as f64 / 1000. - 2.).max(0.),
                friendly: flags & 0x10 != 0,
            };
            if let Kind::Solo { rounds, .. } = &mut a.kind {
                if let Some(r) = rounds.last_mut() {
                    r.end = Some(d.date);
                    let mine = a
                        .player
                        .as_ref()
                        .and_then(|p| a.combatants.get(p))
                        .and_then(|c| c.team_id);
                    r.result = mine
                        .map(|t| if d.friendly { t != 0 } else { true })
                        .unwrap_or(false)
                }
            }
            a.deaths.push(d)
        }
    }
    fn end(&mut self, end: DateTime<Local>, result: bool) {
        let Some(mut a) = self.activity.take() else {
            return;
        };
        a.end = Some(end);
        a.result = result;
        let duration = (end - a.start).num_milliseconds() as f64 / 1000. + a.overrun;
        if matches!(a.kind, Kind::Raid { .. }) && duration < a.settings_min(&self.settings) {
            let _ = self.tx.try_send(ParserEvent::ForceEnd);
            return;
        }
        let metadata = metadata(&a, duration);
        let name = video_name(&a);
        let _ = self.tx.try_send(ParserEvent::ActivityEnded {
            metadata,
            activity_start: a.start,
            activity_end: end,
            overrun_seconds: a.overrun,
            video_name: name,
        });
    }
}
trait Min {
    fn settings_min(&self, s: &ParserSettings) -> f64;
}
impl Min for Activity {
    fn settings_min(&self, s: &ParserSettings) -> f64 {
        s.min_encounter_duration
    }
}
fn num(x: Option<&str>) -> i64 {
    x.unwrap_or("0").parse().unwrap_or(0)
}
fn hex(x: Option<&str>) -> i64 {
    i64::from_str_radix(x.unwrap_or("0").trim_start_matches("0x"), 16).unwrap_or(0)
}
fn parse_list(s: &str) -> Vec<i64> {
    s.trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect()
}
fn split_name(s: &str) -> (String, Option<String>) {
    let s = s.trim_matches('"');
    match s.rsplit_once('-') {
        Some((n, r)) => (n.into(), Some(r.into())),
        None => (s.into(), None),
    }
}
fn diff_rank(s: &str) -> i32 {
    match s.to_lowercase().as_str() {
        "lfr" => 0,
        "normal" => 1,
        "heroic" => 2,
        "mythic" => 3,
        _ => 0,
    }
}
fn metadata(a: &Activity, d: f64) -> Metadata {
    let (zone, enc, diff, map, aff) = match &a.kind {
        Kind::Raid { id, difficulty, .. } => (
            Some(constants::raid_zone(*id)),
            Some(*id),
            Some(*difficulty),
            None,
            None,
        ),
        Kind::Dungeon {
            zone, map, affixes, ..
        } => (Some(*zone), None, None, Some(*map), Some(affixes.clone())),
        Kind::Arena { zone, .. } | Kind::Solo { zone, .. } | Kind::Bg { zone } => {
            (Some(*zone), None, None, None, None)
        }
        Kind::Manual => (None, None, None, None, None),
    };
    let (won, played) = match &a.kind {
        Kind::Solo { rounds, .. } => (
            Some(rounds.iter().filter(|r| r.result).count()),
            Some(rounds.len()),
        ),
        _ => (None, None),
    };
    Metadata {
        category: a.category().into(),
        zone_id: zone,
        encounter_id: enc,
        difficulty_id: diff,
        difficulty: diff.and_then(|x| constants::difficulty(x).map(|x| x.0.to_owned())),
        duration: d,
        result: a.result,
        deaths: if a.deaths.is_empty() {
            None
        } else {
            Some(a.deaths.clone())
        },
        player: a.player(),
        solo_shuffle_rounds_won: won,
        solo_shuffle_rounds_played: played,
        keystone_level: match &a.kind {
            Kind::Dungeon { level, .. } => Some(*level),
            _ => None,
        },
        map_id: map,
        zone_name: match &a.kind {
            Kind::Dungeon { map, zone_name, .. } => Some(
                constants::dungeon(*map)
                    .map(|x| x.0.to_owned())
                    .unwrap_or_else(|| zone_name.clone()),
            ),
            Kind::Arena { zone, .. } | Kind::Solo { zone, .. } => {
                Some(constants::arena(*zone).to_owned())
            }
            Kind::Bg { zone } => constants::battleground(*zone).map(str::to_owned),
            _ => None,
        },
        upgrade_level: match &a.kind {
            Kind::Dungeon {
                map,
                cmd_duration: Some(actual),
                ..
            } if a.result => constants::dungeon(*map).map(|(_, timers)| {
                if *actual <= timers[2] {
                    3
                } else if *actual <= timers[1] {
                    2
                } else if *actual <= timers[0] {
                    1
                } else {
                    0
                }
            }),
            _ => None,
        },
        affixes: aff,
        encounter_name: match &a.kind {
            Kind::Raid { name, .. } => Some(name.clone()),
            _ => None,
        },
        team_mmr: None,
        overrun: a.overrun,
        flavour: "Retail".into(),
        start: Some(a.start.timestamp_millis()),
        unique_hash: Some(format!("{:x}", a.start.timestamp_millis())),
        combatants: a.combatants.values().cloned().collect(),
    }
}
fn video_name(a: &Activity) -> String {
    let p = a
        .player()
        .and_then(|x| x.name)
        .map(|x| format!("{x} - "))
        .unwrap_or_default();
    match &a.kind {
        Kind::Raid {
            name, difficulty, ..
        } => format!(
            "{p}{name} [{}] ({})",
            constants::difficulty(*difficulty)
                .map(|x| x.0)
                .unwrap_or("unknown"),
            if a.result { "Kill" } else { "Wipe" }
        ),
        Kind::Dungeon {
            map,
            zone_name,
            level,
            ..
        } => format!(
            "{p}{} +{level} ({})",
            constants::dungeon(*map).map(|x| x.0).unwrap_or(zone_name),
            if a.result { "+1" } else { "Abandoned" }
        ),
        Kind::Arena { zone, category } => format!(
            "{p}{category} {} ({})",
            constants::arena(*zone),
            if a.result { "Win" } else { "Loss" }
        ),
        Kind::Solo { zone, .. } => format!("{p}Solo Shuffle {}", constants::arena(*zone)),
        Kind::Bg { zone } => format!(
            "{p}{}",
            constants::battleground(*zone).unwrap_or("Unknown Battleground")
        ),
        Kind::Manual => "Manual".into(),
    }
}

/// Compact, timestamp-free retail fixtures for the `test_run` command. Prefix
/// each entry with `"{timestamp}  "` immediately before injection.
pub mod test_data {
    pub const RAID: &[&str] = &[
        "ENCOUNTER_START,3181,\"Alleria Windrunner\",16,0,0",
        "COMBATANT_INFO,Player-test,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,62",
        "SPELL_AURA_APPLIED,Player-test,\"Tester-Realm\",0x511",
        "ENCOUNTER_END,3181,\"Alleria Windrunner\",16,0,1",
    ];
    pub const DUNGEON: &[&str] = &[
        "CHALLENGE_MODE_START,\"Brackenhide Hollow\",2520,405,18,[10,11,124]",
        "COMBATANT_INFO,Player-test,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,62",
        "SPELL_AURA_APPLIED,Player-test,\"Tester-Realm\",0x511",
        "CHALLENGE_MODE_END,2520,1,18,120000",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replay_fixture(contents: &str) -> Vec<ParserEvent> {
        let (tx, mut rx) = tokio_mpsc::channel(16);
        let mut parser = Parser::new(ParserSettings::default(), tx);
        for line in contents.lines() {
            parser.inject_raw_line(line);
        }
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[test]
    fn nested_log_line() {
        let mut l =
            LogLine::new("7/27/2024 21:39:13.0951  COMBATANT_INFO,x,1,[(1,2),(3,\"x,y\")],z");
        assert_eq!(l.event_type(), "COMBATANT_INFO");
        assert_eq!(l.arg(3), Some("[(1,2),(3,x,y)]"));
        assert_eq!(l.date().year(), 2024)
    }
    #[test]
    fn raid_events() {
        let (tx, mut rx) = tokio_mpsc::channel(8);
        let mut p = Parser::new(ParserSettings::default(), tx);
        p.inject_raw_line("7/27/2024 21:00:00.0  ENCOUNTER_START,3181,\"Boss\",16");
        p.inject_raw_line("7/27/2024 21:00:01.0  SPELL_AURA_APPLIED,Player-x,\"Me-Realm\",0x511");
        p.inject_raw_line("7/27/2024 21:01:00.0  ENCOUNTER_END,3181,\"Boss\",16,0,1");
        let _ = rx.try_recv();
        match rx.try_recv().unwrap() {
            ParserEvent::ActivityEnded { metadata, .. } => assert_eq!(metadata.category, "Raids"),
            _ => panic!(),
        }
    }
    #[test]
    fn arena_events() {
        let (tx, mut rx) = tokio_mpsc::channel(8);
        let mut p = Parser::new(ParserSettings::default(), tx);
        p.inject_raw_line("7/27/2024 21:00:00.0  ARENA_MATCH_START,2547,0,2v2,1");
        p.inject_raw_line("7/27/2024 21:00:01.0  COMBATANT_INFO,Player-x,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,62");
        p.inject_raw_line("7/27/2024 21:00:02.0  SPELL_AURA_APPLIED,Player-x,\"Me-Realm\",0x511");
        p.inject_raw_line("7/27/2024 21:01:00.0  ARENA_MATCH_END,1");
        let _ = rx.try_recv();
        match rx.try_recv().unwrap() {
            ParserEvent::ActivityEnded { metadata, .. } => {
                assert_eq!(metadata.category, "2v2");
                assert!(metadata.result);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn unknown_dungeon_records_with_log_zone_name() {
        let (tx, mut rx) = tokio_mpsc::channel(8);
        let mut p = Parser::new(ParserSettings::default(), tx);
        p.inject_raw_line(
            "7/27/2024 21:00:00.0  CHALLENGE_MODE_START,\"Current Season Dungeon\",9999,999,12,[9,10]",
        );
        p.inject_raw_line(
            "7/27/2024 21:00:01.0  COMBATANT_INFO,Player-x,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,62",
        );
        p.inject_raw_line("7/27/2024 21:00:02.0  SPELL_AURA_APPLIED,Player-x,\"Me-Realm\",0x511");
        p.inject_raw_line("7/27/2024 21:10:00.0  CHALLENGE_MODE_END,9999,1,12,600000");

        assert!(matches!(
            rx.try_recv(),
            Ok(ParserEvent::ActivityStarted { .. })
        ));
        match rx.try_recv().unwrap() {
            ParserEvent::ActivityEnded {
                metadata,
                video_name,
                ..
            } => {
                assert_eq!(
                    metadata.zone_name.as_deref(),
                    Some("Current Season Dungeon")
                );
                assert_eq!(metadata.keystone_level, Some(12));
                assert_eq!(metadata.upgrade_level, None);
                assert!(video_name.contains("Current Season Dungeon +12"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn replays_completed_dungeon_fixture() {
        let events = replay_fixture(include_str!(
            "../../tests/fixtures/retail_dungeon_complete.log"
        ));
        assert!(matches!(events.first(), Some(ParserEvent::ActivityStarted { .. })));
        match events.last().unwrap() {
            ParserEvent::ActivityEnded { metadata, .. } => {
                assert_eq!(metadata.category, "Mythic+");
                assert_eq!(metadata.zone_name.as_deref(), Some("Current Season Dungeon"));
                assert!(metadata.result);
            }
            _ => panic!("fixture did not end its dungeon"),
        }
    }

    #[test]
    fn replays_raid_wipe_fixture() {
        let events = replay_fixture(include_str!(
            "../../tests/fixtures/retail_raid_wipe.log"
        ));
        match events.last().unwrap() {
            ParserEvent::ActivityEnded { metadata, .. } => {
                assert_eq!(metadata.category, "Raids");
                assert!(!metadata.result);
            }
            _ => panic!("fixture did not end its raid"),
        }
    }
}
