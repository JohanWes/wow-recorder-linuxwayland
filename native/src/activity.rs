// SPDX-License-Identifier: GPL-3.0-or-later

//! Activity state transitions and recording actions.
//!
//! Translates timestamped parsed events into automatic-recording actions and
//! recording metadata/timeline, using deterministic GTK-free state. One active
//! automatic activity is retained per flavour (`Retail`, `Classic`, `Era`);
//! PTR log sources share their base flavour's state.
//!
//! - Era log sources apply the Era rules and write `Classic` as the metadata
//!   flavour. `GameFlavor::Unknown` events are ignored.
//! - Events that would corrupt state (e.g. `CHALLENGE_MODE_*` arriving over a
//!   different in-flight category) are ignored.
//! - On a failed `Begin`, the coordinator clears the active activity with
//!   `force_end` and drops the emitted `Abandon` action.
//! - The coordinator drives data-timeout force ends: retail 10 min, classic/era
//!   2 min without new log data, ending at last-data time.
//! - `force_end` reuses the most recent config seen by `handle`, so the raid
//!   minimum-duration discard still applies to force-ended raids.

use std::collections::HashMap;

use crate::config::ActivitySettings;
use crate::domain::{
    ActivityDetails, BLOODLUST_DURATION_MS, Category, CombatantSummary, GameFlavor, MeterData,
    Outcome, PlayerSummary, RaidDifficulty, RecordingId, RoundSummary, TimelineItem, TimelineKind,
};
use crate::meter::MeterAccumulator;
use crate::parser::{CombatEvent, ParsedEvent, PlayerObservationKind, is_bloodlust_spell};

const RAID_DEFAULT_OVERRUN_MS: u64 = 3_000;
const PVP_DEFAULT_OVERRUN_MS: u64 = 3_000;
const MIN_RETAIL_BOSS_HP: u64 = 100_000_000;
const CHALLENGERS_PERIL_AFFIX: u32 = 152;
const CHALLENGERS_PERIL_ADJUST_MS: i64 = 90_000;
const MIN_FINAL_SEGMENT_MS: i64 = 10_000;
const DEATH_MARKER_BACK_OFFSET_MS: i64 = 2;
const BELOREN_ENCOUNTER_ID: u32 = 3182;
const ALLERIA_ENCOUNTER_ID: u32 = 3181;
const BELOREN_UNIT_NAME: &str = "Belo'ren";
const ALLERIA_UNIT_NAME: &str = "Alleria Windrunner";
const BELOREN_PHASE_SPELL: &str = "Rebirth";
const EMPTY_GUID: &str = "0000000000000000";
pub(crate) const AFFILIATION_MINE: u64 = 0x1;
pub(crate) const REACTION_FRIENDLY: u64 = 0x10;
pub(crate) const CONTROL_PLAYER: u64 = 0x100;
const TYPE_PLAYER: u64 = 0x400;

fn is_unit_player(flags: u64) -> bool {
    flags & CONTROL_PLAYER != 0 && flags & TYPE_PLAYER != 0
}

pub(crate) fn is_unit_friendly(flags: u64) -> bool {
    flags & REACTION_FRIENDLY != 0
}

fn is_unit_self(flags: u64) -> bool {
    is_unit_friendly(flags) && flags & AFFILIATION_MINE != 0
}

/// Player-controlled and friendly: players and their pets/guardians alike.
pub(crate) fn is_player_controlled_friendly(flags: u64) -> bool {
    flags & CONTROL_PLAYER != 0 && is_unit_friendly(flags)
}

/// Name, realm, region from a `Name-Realm(-x-Region)` string.
fn ambiguate(name_realm: &str) -> (String, Option<String>, Option<String>) {
    let parts: Vec<&str> = name_realm.split('-').collect();
    let name = parts.first().unwrap_or(&"").to_string();
    let realm = parts.get(1).map(|value| (*value).to_string());
    let region = parts.get(3).map(|value| (*value).to_string());
    (name, realm, region)
}

pub(crate) fn relative_ms(started_at_ms: i64, at_ms: i64) -> u64 {
    (at_ms - started_at_ms).max(0) as u64
}
/// One logical recording in flight, emitted by `Begin` and completed by
/// `take_finished` after a `Complete`/`Abandon`/`Discard` action. End-time
/// fields are `None` until the activity finishes.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingDraft {
    pub id: RecordingId,
    pub category: Category,
    pub flavor: GameFlavor,
    /// Occurrence start of the activity (combat-log event time). Distinct from
    /// the later detection time carried by `Begin`.
    pub started_at_ms: i64,
    pub overrun_ms: u64,
    pub details: ActivityDetails,
    pub player: Option<PlayerSummary>,
    pub combatants: Vec<CombatantSummary>,
    pub timeline: Vec<TimelineItem>,
    pub outcome: Option<Outcome>,
    pub ended_at_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    pub title: Option<String>,
    pub activity_hash: Option<String>,
    pub meter: MeterData,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ActivityAction {
    Begin {
        draft: Box<RecordingDraft>,
        detected_at_ms: i64,
    },
    Update {
        id: RecordingId,
        item: TimelineItem,
    },
    Complete {
        id: RecordingId,
        outcome: Outcome,
        ended_at_ms: i64,
    },
    Abandon {
        id: RecordingId,
        ended_at_ms: i64,
        reason: AbandonReason,
    },
    Discard {
        id: RecordingId,
        reason: DiscardReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbandonReason {
    /// User force stop or coordinator data timeout: zero overrun, loss-style
    /// outcome, ended at the supplied time.
    ForceEnd,
    /// Another activity event superseded the in-flight one (arena start during
    /// an activity, raid encounter during Mythic+, battleground zone-in during
    /// an activity). Same recorded metadata shape as a force end.
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    /// Raid duration (including overrun) below the configured minimum.
    BelowMinDuration,
    /// The recording player could not be identified or has no combatant/name,
    /// so the legacy app would fail to build metadata and drop the video.
    IncompleteMetadata,
}

/// Deterministic activity engine. No filesystem, process, GTK, sleeps, global
/// singletons, or wall-clock reads: all times come from events or arguments.
#[derive(Default)]
pub struct ActivityEngine {
    retail: FlavorState,
    classic: FlavorState,
    era: FlavorState,
    finished: Vec<RecordingDraft>,
    config: Option<ActivitySettings>,
}

impl ActivityEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle(&mut self, event: ParsedEvent, config: &ActivitySettings) -> Vec<ActivityAction> {
        self.config = Some(config.clone());
        let ParsedEvent {
            flavor,
            occurred_at_ms,
            event,
        } = event;
        let mut actions = Vec::new();
        match flavor {
            GameFlavor::Retail => handle_event(
                &mut self.retail,
                Rules::Retail,
                &event,
                occurred_at_ms,
                config,
                &mut self.finished,
                &mut actions,
            ),
            GameFlavor::Classic => handle_event(
                &mut self.classic,
                Rules::Classic,
                &event,
                occurred_at_ms,
                config,
                &mut self.finished,
                &mut actions,
            ),
            GameFlavor::Era => handle_event(
                &mut self.era,
                Rules::Era,
                &event,
                occurred_at_ms,
                config,
                &mut self.finished,
                &mut actions,
            ),
            GameFlavor::Unknown(_) => {}
        }
        actions
    }

    /// Force-end the flavour's active automatic activity. Returns no action
    /// when that flavour has none, so it can never end the wrong flavour.
    pub fn force_end(&mut self, flavor: GameFlavor, occurred_at_ms: i64) -> Vec<ActivityAction> {
        let state = match flavor {
            GameFlavor::Retail => &mut self.retail,
            GameFlavor::Classic => &mut self.classic,
            GameFlavor::Era => &mut self.era,
            GameFlavor::Unknown(_) => return Vec::new(),
        };
        let Some(active) = state.active.take() else {
            return Vec::new();
        };
        let config = self.config.clone().unwrap_or_default();
        let mut actions = Vec::new();
        finish(
            active,
            occurred_at_ms,
            EndKind::Abandon(AbandonReason::ForceEnd),
            &config,
            &mut self.finished,
            &mut actions,
        );
        actions
    }

    /// Take the finished draft for a `Complete`/`Abandon`/`Discard` action.
    pub fn take_finished(&mut self, id: &RecordingId) -> Option<RecordingDraft> {
        let position = self.finished.iter().position(|draft| &draft.id == id)?;
        Some(self.finished.remove(position))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rules {
    Retail,
    Classic,
    Era,
}

#[derive(Default)]
struct FlavorState {
    active: Option<ActiveActivity>,
}

struct ActiveActivity {
    id: RecordingId,
    category: Category,
    flavor: GameFlavor,
    started_at_ms: i64,
    overrun_ms: u64,
    combatants: Combatants,
    player_guid: Option<String>,
    timeline: Vec<TimelineItem>,
    meter: MeterAccumulator,
    kind: ActiveKind,
}

enum ActiveKind {
    Raid(RaidState),
    Challenge(ChallengeState),
    Arena(ArenaState),
    Battleground { zone_id: u32 },
    SoloShuffle(ShuffleState),
}

struct RaidState {
    encounter_id: u32,
    encounter_name: String,
    difficulty_id: u32,
    current_hp: u64,
    max_hp: u64,
    boss_unit_name: &'static str,
    boss_unit_active: bool,
}

struct ChallengeState {
    zone_id: u32,
    map_id: u32,
    level: u32,
    affixes: Vec<u32>,
    cm_duration_ms: Option<u64>,
    segments: Vec<CmSegment>,
}

struct CmSegment {
    kind: TimelineKind,
    start_ms: i64,
    end_ms: Option<i64>,
    label: Option<String>,
    result: Option<bool>,
}

struct ArenaState {
    zone_id: u32,
}

struct ShuffleState {
    zone_id: u32,
    rounds: Vec<ShuffleRound>,
}

struct ShuffleRound {
    start_ms: i64,
    end_ms: Option<i64>,
    result: bool,
    combatants: Combatants,
    player_guid: Option<String>,
    has_death: bool,
    item_emitted: bool,
}

impl ShuffleRound {
    fn new(start_ms: i64) -> Self {
        Self {
            start_ms,
            end_ms: None,
            result: false,
            combatants: Combatants::default(),
            player_guid: None,
            has_death: false,
            item_emitted: false,
        }
    }
}

/// Insertion-ordered combatant map matching JS `Map` semantics.
#[derive(Default)]
struct Combatants {
    entries: Vec<CombatantState>,
    index: HashMap<String, usize>,
}

impl Combatants {
    fn get(&self, guid: &str) -> Option<&CombatantState> {
        self.index
            .get(guid)
            .map(|position| &self.entries[*position])
    }

    fn contains(&self, guid: &str) -> bool {
        self.index.contains_key(guid)
    }

    /// Insert or replace, keeping the original position on replacement.
    /// Returns true when the GUID is new.
    fn upsert(&mut self, combatant: CombatantState) -> bool {
        if let Some(position) = self.index.get(&combatant.guid) {
            self.entries[*position] = combatant;
            return false;
        }
        self.index
            .insert(combatant.guid.clone(), self.entries.len());
        self.entries.push(combatant);
        true
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn iter(&self) -> impl Iterator<Item = &CombatantState> {
        self.entries.iter()
    }
}

#[derive(Clone, Default)]
struct CombatantState {
    guid: String,
    team_id: Option<u8>,
    spec_id: Option<u16>,
    name: Option<String>,
    realm: Option<String>,
    region: Option<String>,
}

impl CombatantState {
    /// A GUID is not required: the map key guarantees one.
    fn is_fully_defined(&self) -> bool {
        self.team_id.is_some()
            && self.name.is_some()
            && self.realm.is_some()
            && self.spec_id.is_some()
    }
}

#[derive(Clone, Copy)]
enum EndKind {
    Complete(Outcome),
    Abandon(AbandonReason),
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    state: &mut FlavorState,
    rules: Rules,
    event: &CombatEvent,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    match event {
        CombatEvent::ZoneChanged { zone_id, .. } => {
            handle_zone_change(state, rules, *zone_id, at_ms, config, finished, actions);
        }
        CombatEvent::EncounterStarted {
            encounter_id,
            name,
            difficulty_id,
            ..
        } => handle_encounter_start(
            state,
            rules,
            *encounter_id,
            name,
            *difficulty_id,
            at_ms,
            config,
            finished,
            actions,
        ),
        CombatEvent::EncounterEnded {
            difficulty_id,
            success,
            ..
        } => handle_encounter_end(
            state,
            rules,
            *difficulty_id,
            *success,
            at_ms,
            config,
            finished,
            actions,
        ),
        CombatEvent::ChallengeStarted {
            zone_id,
            map_id,
            level,
            affixes,
            ..
        } => handle_challenge_start(
            state, rules, *zone_id, *map_id, *level, affixes, at_ms, config, actions,
        ),
        CombatEvent::ChallengeEnded {
            success,
            duration_ms,
            ..
        } => handle_challenge_end(
            state,
            rules,
            *success,
            *duration_ms,
            at_ms,
            config,
            finished,
            actions,
        ),
        CombatEvent::ArenaStarted {
            zone_id,
            match_type,
        } => handle_arena_start(
            state, rules, *zone_id, match_type, at_ms, config, finished, actions,
        ),
        CombatEvent::ArenaEnded {
            winning_team_id, ..
        } => handle_arena_end(
            state,
            rules,
            *winning_team_id,
            at_ms,
            config,
            finished,
            actions,
        ),
        CombatEvent::Combatant {
            guid,
            team_id,
            spec_id,
        } => handle_combatant_info(state, rules, guid, *team_id, *spec_id),
        CombatEvent::PlayerObserved {
            kind,
            spell_id,
            guid,
            name,
            flags,
            target_guid,
            target_name,
            target_flags,
            spell_name,
            owner_guid,
        } => handle_player_observed(
            state,
            rules,
            *kind,
            *spell_id,
            guid,
            name,
            *flags,
            target_guid,
            target_name,
            *target_flags,
            spell_name,
            owner_guid.as_deref(),
            at_ms,
            actions,
        ),
        CombatEvent::UnitDied {
            guid,
            name,
            flags,
            unconscious,
        } => handle_unit_died(
            state,
            rules,
            guid,
            name,
            *flags,
            *unconscious,
            at_ms,
            config,
            finished,
            actions,
        ),
        CombatEvent::Damage {
            source_guid,
            source_name,
            source_flags,
            source_owner_guid,
            dest_name,
            dest_flags,
            dest_raid_marker,
            spell_name,
            amount,
            dest_current_hp,
            dest_max_hp,
        } => {
            // Boss HP keeps flowing from the same event: destination HP is
            // only set when the advanced block identified the destination.
            if let (Some(current), Some(maximum)) = (dest_current_hp, dest_max_hp) {
                handle_boss_health(state, rules, dest_name, *current, *maximum);
            }
            if let Some(active) = state.active.as_mut() {
                if let Some(owner) = source_owner_guid {
                    active.meter.record_owner(source_guid, owner, None);
                }
                active.meter.damage(
                    source_guid,
                    source_name,
                    *source_flags,
                    dest_name,
                    *dest_flags,
                    *dest_raid_marker,
                    spell_name,
                    *amount,
                    at_ms,
                );
            }
        }
        CombatEvent::Heal {
            source_guid,
            source_name,
            source_flags,
            dest_name,
            dest_flags: _,
            dest_raid_marker,
            spell_name,
            amount,
            overheal,
        } => {
            if let Some(active) = state.active.as_mut() {
                active.meter.heal(
                    source_guid,
                    source_name,
                    *source_flags,
                    dest_name,
                    *dest_raid_marker,
                    spell_name,
                    *amount,
                    *overheal,
                    at_ms,
                );
            }
        }
        CombatEvent::Interrupt {
            source_guid,
            source_name,
            source_flags,
            dest_name,
            dest_flags: _,
            dest_raid_marker,
            spell_name,
        } => {
            if let Some(active) = state.active.as_mut() {
                active.meter.interrupt(
                    source_guid,
                    source_name,
                    *source_flags,
                    dest_name,
                    *dest_raid_marker,
                    spell_name,
                    at_ms,
                );
            }
        }
        CombatEvent::Dispel {
            source_guid,
            source_name,
            source_flags,
            dest_name,
            dest_flags: _,
            dest_raid_marker,
            spell_name,
        } => {
            if let Some(active) = state.active.as_mut() {
                active.meter.dispel(
                    source_guid,
                    source_name,
                    *source_flags,
                    dest_name,
                    *dest_raid_marker,
                    spell_name,
                    at_ms,
                );
            }
        }
        CombatEvent::Summon {
            source_guid,
            source_name,
            pet_guid,
        } => {
            if let Some(active) = state.active.as_mut() {
                active
                    .meter
                    .record_owner(pet_guid, source_guid, Some(source_name));
            }
        }
        CombatEvent::BossCast {
            source_name,
            spell_name,
            ..
        } => handle_boss_cast(state, rules, source_name, spell_name),
    }
}

// --- Shared activity helpers ---

fn allow_record(config: &ActivitySettings, category: &Category) -> bool {
    match category {
        Category::TwoVTwo => config.record_two_v_two,
        Category::ThreeVThree => config.record_three_v_three,
        Category::FiveVFive => config.record_five_v_five,
        Category::Skirmish => config.record_skirmish,
        Category::SoloShuffle => config.record_solo_shuffle,
        Category::MythicPlus => config.record_dungeons,
        Category::Raids => config.record_raids,
        Category::Battlegrounds => config.record_battlegrounds,
        _ => false,
    }
}

fn begin(
    state: &mut FlavorState,
    active: ActiveActivity,
    detected_at_ms: i64,
    config: &ActivitySettings,
    actions: &mut Vec<ActivityAction>,
) {
    if !allow_record(config, &active.category) {
        return;
    }
    let draft = Box::new(draft_for(&active));
    state.active = Some(active);
    actions.push(ActivityAction::Begin {
        draft,
        detected_at_ms,
    });
}

fn draft_for(active: &ActiveActivity) -> RecordingDraft {
    RecordingDraft {
        id: active.id.clone(),
        category: active.category.clone(),
        flavor: active.flavor.clone(),
        started_at_ms: active.started_at_ms,
        overrun_ms: active.overrun_ms,
        details: initial_details(active),
        player: None,
        combatants: Vec::new(),
        timeline: Vec::new(),
        outcome: None,
        ended_at_ms: None,
        duration_ms: None,
        title: None,
        activity_hash: None,
        meter: MeterData::default(),
    }
}

fn initial_details(active: &ActiveActivity) -> ActivityDetails {
    match &active.kind {
        ActiveKind::Raid(raid) => ActivityDetails::Raid {
            zone_id: Some(raid_zone_id(raid.encounter_id)),
            zone_name: Some(raid_lookup(raid.encounter_id).short_name.to_string()),
            encounter_id: Some(raid.encounter_id),
            encounter_name: Some(raid.encounter_name.clone()),
            difficulty_id: Some(raid.difficulty_id),
            difficulty: difficulty_info(raid.difficulty_id).map(|info| info.short.to_string()),
            pull: None,
            boss_percent: None,
        },
        ActiveKind::Challenge(challenge) => ActivityDetails::Dungeon {
            zone_id: Some(challenge.zone_id),
            dungeon_name: Some(dungeon_name(
                &active.flavor,
                challenge.zone_id,
                challenge.map_id,
            )),
            map_id: Some(challenge.map_id),
            keystone_level: Some(challenge.level),
            affixes: challenge.affixes.clone(),
            upgrade_level: None,
        },
        ActiveKind::Arena(arena) => ActivityDetails::ArenaOrBattleground {
            map_id: Some(arena.zone_id),
            map_name: arena_zone_name(&active.flavor, arena.zone_id),
            team_mmr: None,
        },
        ActiveKind::Battleground { zone_id } => ActivityDetails::ArenaOrBattleground {
            map_id: Some(*zone_id),
            map_name: Some(battleground_name(*zone_id).to_string()),
            team_mmr: None,
        },
        ActiveKind::SoloShuffle(shuffle) => ActivityDetails::SoloRounds {
            map_id: Some(shuffle.zone_id),
            map_name: arena_zone_name(&active.flavor, shuffle.zone_id),
            rounds_won: None,
            rounds_played: None,
            rounds: Vec::new(),
        },
    }
}

fn active_zone_id(active: &ActiveActivity) -> Option<u32> {
    match &active.kind {
        ActiveKind::Arena(arena) => Some(arena.zone_id),
        ActiveKind::Battleground { zone_id } => Some(*zone_id),
        ActiveKind::SoloShuffle(shuffle) => Some(shuffle.zone_id),
        _ => None,
    }
}

fn is_arena_category(category: &Category) -> bool {
    matches!(
        category,
        Category::TwoVTwo
            | Category::ThreeVThree
            | Category::FiveVFive
            | Category::Skirmish
            | Category::SoloShuffle
    )
}

/// Player-flag filtering, player-GUID assignment, create-or-update with
/// name/realm/region fill-in. Returns the combatant's position when recorded.
fn process_combatant(
    combatants: &mut Combatants,
    player_guid: &mut Option<String>,
    guid: &str,
    name_realm: &str,
    flags: u64,
    allow_new: bool,
) -> Option<usize> {
    if guid == EMPTY_GUID || !is_unit_player(flags) {
        return None;
    }
    if player_guid.is_none() && is_unit_self(flags) {
        *player_guid = Some(guid.to_string());
    }
    let existing = combatants.index.get(guid).copied();
    let position = existing.or(allow_new.then_some(combatants.entries.len()))?;
    if combatants
        .get(guid)
        .is_some_and(CombatantState::is_fully_defined)
    {
        return Some(position);
    }
    let (name, realm, region) = ambiguate(name_realm);
    let mut combatant = combatants
        .get(guid)
        .cloned()
        .unwrap_or_else(|| CombatantState {
            guid: guid.to_string(),
            ..CombatantState::default()
        });
    combatant.name = Some(name);
    combatant.realm = realm;
    combatant.region = region;
    combatants.upsert(combatant);
    Some(position)
}

/// Classic arenas derive the category from the combatant count on every add.
fn update_arena_category(active: &mut ActiveActivity) {
    if !matches!(active.kind, ActiveKind::Arena(_)) || active.flavor != GameFlavor::Classic {
        return;
    }
    let size = active.combatants.len();
    active.category = if size < 5 {
        Category::TwoVTwo
    } else if size < 7 {
        Category::ThreeVThree
    } else {
        Category::FiveVFive
    };
}

fn push_timeline(
    active: &mut ActiveActivity,
    item: TimelineItem,
    actions: &mut Vec<ActivityAction>,
) {
    actions.push(ActivityAction::Update {
        id: active.id.clone(),
        item: item.clone(),
    });
    active.timeline.push(item);
}

/// The combatant map written by combatant events: the current round for solo
/// shuffle, the activity map otherwise.
fn combatant_target(active: &mut ActiveActivity) -> &mut Combatants {
    if let ActiveKind::SoloShuffle(shuffle) = &mut active.kind
        && let Some(round) = shuffle.rounds.last_mut()
    {
        return &mut round.combatants;
    }
    &mut active.combatants
}

fn set_player_guid(active: &mut ActiveActivity, guid: Option<String>) {
    if let ActiveKind::SoloShuffle(shuffle) = &mut active.kind
        && let Some(round) = shuffle.rounds.last_mut()
    {
        round.player_guid = guid;
        return;
    }
    active.player_guid = guid;
}

fn current_player_guid(active: &ActiveActivity) -> Option<&String> {
    if let ActiveKind::SoloShuffle(shuffle) = &active.kind
        && let Some(round) = shuffle.rounds.last()
    {
        return round.player_guid.as_ref();
    }
    active.player_guid.as_ref()
}

// --- Encounter handling (raids and Mythic+ boss segments) ---

#[allow(clippy::too_many_arguments)]
fn handle_encounter_start(
    state: &mut FlavorState,
    rules: Rules,
    encounter_id: u32,
    name: &str,
    difficulty_id: u32,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    if rules == Rules::Retail {
        let known_dungeon = dungeon_encounter_name(encounter_id).is_some();
        if state.active.is_none() && known_dungeon {
            // Regular dungeon, or a Mythic+ below the recording threshold.
            return;
        }
        if state.active.is_some() && !known_dungeon {
            // Active Mythic+ but not a dungeon encounter: abandon it and start
            // the raid encounter (abandoned key into raid pull).
            let active = state.active.take().expect("checked above");
            finish(
                active,
                at_ms,
                EndKind::Abandon(AbandonReason::Superseded),
                config,
                finished,
                actions,
            );
        }
        if state.active.is_none() {
            if config.current_raid_only && !CURRENT_RETAIL_ENCOUNTERS.contains(&encounter_id) {
                return;
            }
            let Some(info) = difficulty_info(difficulty_id) else {
                return;
            };
            let Some(actual) = info.order() else {
                return;
            };
            if actual < difficulty_order(&config.min_raid_difficulty) {
                return;
            }
            start_raid(
                state,
                rules,
                encounter_id,
                name,
                difficulty_id,
                at_ms,
                config,
                actions,
            );
            return;
        }
        let active = state.active.as_mut().expect("checked above");
        if !matches!(active.kind, ActiveKind::Challenge(_)) {
            return;
        }
        // Mythic+ boss encounter segment: close the open segment, then push a
        // boss segment labelled with the encounter name. The meter fight is
        // cut at the same transition.
        close_open_segment(active, at_ms, actions);
        let label = dungeon_encounter_name(encounter_id)
            .unwrap_or(name)
            .to_string();
        if let ActiveKind::Challenge(challenge) = &mut active.kind {
            challenge.segments.push(CmSegment {
                kind: TimelineKind::Encounter,
                start_ms: at_ms,
                end_ms: None,
                label: Some(label.clone()),
                result: None,
            });
        }
        active.meter.cut(at_ms, label);
        return;
    }

    // Classic/Era base handler.
    if state.active.is_some() {
        return;
    }
    start_raid(
        state,
        rules,
        encounter_id,
        name,
        difficulty_id,
        at_ms,
        config,
        actions,
    );
}

#[allow(clippy::too_many_arguments)]
fn start_raid(
    state: &mut FlavorState,
    rules: Rules,
    encounter_id: u32,
    name: &str,
    difficulty_id: u32,
    at_ms: i64,
    config: &ActivitySettings,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(info) = difficulty_info(difficulty_id) else {
        return;
    };
    if info.party != PartyType::Raid {
        return;
    }
    // Era activities record the Classic flavour.
    let flavor = match rules {
        Rules::Retail => GameFlavor::Retail,
        Rules::Classic | Rules::Era => GameFlavor::Classic,
    };
    let (boss_unit_name, boss_unit_active) = match encounter_id {
        BELOREN_ENCOUNTER_ID => (BELOREN_UNIT_NAME, false),
        ALLERIA_ENCOUNTER_ID => (ALLERIA_UNIT_NAME, true),
        _ => ("", true),
    };
    let active = ActiveActivity {
        id: RecordingId::new(),
        category: Category::Raids,
        flavor,
        started_at_ms: at_ms,
        overrun_ms: RAID_DEFAULT_OVERRUN_MS,
        combatants: Combatants::default(),
        player_guid: None,
        timeline: Vec::new(),
        meter: MeterAccumulator::new(at_ms, None),
        kind: ActiveKind::Raid(RaidState {
            encounter_id,
            encounter_name: name.to_string(),
            difficulty_id,
            current_hp: 1,
            max_hp: 1,
            boss_unit_name,
            boss_unit_active,
        }),
    };
    begin(state, active, at_ms, config, actions);
}

#[allow(clippy::too_many_arguments)]
fn handle_encounter_end(
    state: &mut FlavorState,
    rules: Rules,
    difficulty_id: u32,
    success: bool,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    if rules == Rules::Retail && matches!(active.kind, ActiveKind::Challenge(_)) {
        // Mythic+ boss encounter ended: record its result, close its span and
        // start a fresh trash segment, cutting the meter fight with it.
        if let ActiveKind::Challenge(challenge) = &mut active.kind
            && let Some(segment) = challenge.segments.last_mut()
        {
            segment.result = Some(success);
        }
        close_open_segment(active, at_ms, actions);
        if let ActiveKind::Challenge(challenge) = &mut active.kind {
            challenge.segments.push(CmSegment {
                kind: TimelineKind::Trash,
                start_ms: at_ms,
                end_ms: None,
                label: None,
                result: None,
            });
        }
        active.meter.cut(at_ms, "Trash".to_owned());
        return;
    }
    let Some(info) = difficulty_info(difficulty_id) else {
        return;
    };
    if info.party != PartyType::Raid {
        return;
    }
    if success {
        active.overrun_ms = u64::from(config.raid_overrun_seconds) * 1_000;
    }
    let outcome = if success { Outcome::Win } else { Outcome::Loss };
    let active = state.active.take().expect("checked above");
    finish(
        active,
        at_ms,
        EndKind::Complete(outcome),
        config,
        finished,
        actions,
    );
}

/// Close a currently open challenge segment, emitting its span update. Event
/// times are monotonic in practice; the span end is clamped defensively.
fn close_open_segment(active: &mut ActiveActivity, at_ms: i64, actions: &mut Vec<ActivityAction>) {
    let started_at_ms = active.started_at_ms;
    let item = {
        let ActiveKind::Challenge(challenge) = &mut active.kind else {
            return;
        };
        let Some(segment) = challenge.segments.last_mut() else {
            return;
        };
        if segment.end_ms.is_some() {
            return;
        }
        segment.end_ms = Some(at_ms);
        segment_item(started_at_ms, segment)
    };
    push_timeline(active, item, actions);
}

fn segment_item(started_at_ms: i64, segment: &CmSegment) -> TimelineItem {
    let start = relative_ms(started_at_ms, segment.start_ms);
    let end = relative_ms(started_at_ms, segment.end_ms.unwrap_or(segment.start_ms)).max(start);
    let outcome = segment
        .result
        .map(|result| if result { Outcome::Win } else { Outcome::Loss });
    TimelineItem::span(
        segment.kind.clone(),
        start,
        end,
        segment.label.clone(),
        outcome,
        None,
    )
    .expect("clamped span bounds")
}

// --- Challenge mode ---

#[allow(clippy::too_many_arguments)]
fn handle_challenge_start(
    state: &mut FlavorState,
    rules: Rules,
    zone_id: u32,
    map_id: u32,
    level: u32,
    affixes: &[u32],
    at_ms: i64,
    config: &ActivitySettings,
    actions: &mut Vec<ActivityAction>,
) {
    if rules == Rules::Era {
        return;
    }
    if state.active.is_some() {
        // A subsequent start for the in-flight dungeon is ignored, and a
        // challenge start over another category is not a recorded shape.
        // Either way the active activity stays.
        return;
    }
    match rules {
        Rules::Retail => {
            if !RETAIL_DUNGEON_MAP_IDS.contains(&map_id) || dungeon_timers(map_id).is_none() {
                return;
            }
            if level < config.min_keystone_level {
                return;
            }
        }
        Rules::Classic => {
            if mop_challenge_mode_name(map_id).is_none() || !config.record_challenge_modes {
                return;
            }
        }
        Rules::Era => return,
    }
    let flavor = match rules {
        Rules::Retail => GameFlavor::Retail,
        _ => GameFlavor::Classic,
    };
    // Classic challenge modes always record level 0 and no affixes, and have
    // no initial trash segment (one fight labelled by the activity title).
    let (level, affixes, segments, meter_label) = match rules {
        Rules::Retail => (
            level,
            affixes.to_vec(),
            vec![CmSegment {
                kind: TimelineKind::Trash,
                start_ms: at_ms,
                end_ms: None,
                label: None,
                result: None,
            }],
            Some("Trash".to_owned()),
        ),
        _ => (0, Vec::new(), Vec::new(), None),
    };
    let active = ActiveActivity {
        id: RecordingId::new(),
        category: Category::MythicPlus,
        flavor,
        started_at_ms: at_ms,
        overrun_ms: 0,
        combatants: Combatants::default(),
        player_guid: None,
        timeline: Vec::new(),
        meter: MeterAccumulator::new(at_ms, meter_label),
        kind: ActiveKind::Challenge(ChallengeState {
            zone_id,
            map_id,
            level,
            affixes,
            cm_duration_ms: None,
            segments,
        }),
    };
    begin(state, active, at_ms, config, actions);
}

#[allow(clippy::too_many_arguments)]
fn handle_challenge_end(
    state: &mut FlavorState,
    rules: Rules,
    success: bool,
    duration_ms: u64,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    if rules == Rules::Era {
        return;
    }
    let Some(active) = state.active.as_mut() else {
        return;
    };
    if !matches!(active.kind, ActiveKind::Challenge(_)) {
        return;
    }
    if success && rules == Rules::Retail {
        active.overrun_ms = u64::from(config.dungeon_overrun_seconds) * 1_000;
    }
    let started_at_ms = active.started_at_ms;
    let mut emitted = None;
    if let ActiveKind::Challenge(challenge) = &mut active.kind {
        // The classic handler always passes a zero challenge duration.
        challenge.cm_duration_ms = Some(if rules == Rules::Retail {
            duration_ms
        } else {
            0
        });
        // Close the last segment, then drop it when shorter than ten seconds.
        if let Some(last) = challenge.segments.last_mut() {
            last.end_ms = Some(at_ms);
        }
        if let Some(last) = challenge.segments.last() {
            let length = last.end_ms.unwrap_or(last.start_ms) - last.start_ms;
            if length < MIN_FINAL_SEGMENT_MS {
                challenge.segments.pop();
            } else {
                emitted = Some(segment_item(started_at_ms, last));
            }
        }
    }
    if let Some(item) = emitted {
        push_timeline(active, item, actions);
    }
    let outcome = match rules {
        Rules::Retail => {
            if success {
                Outcome::Complete
            } else {
                Outcome::Abandoned
            }
        }
        // Classic challenge modes always record success.
        _ => Outcome::Complete,
    };
    let active = state.active.take().expect("checked above");
    finish(
        active,
        at_ms,
        EndKind::Complete(outcome),
        config,
        finished,
        actions,
    );
}

// --- Arenas, battlegrounds, solo shuffle ---

#[allow(clippy::too_many_arguments)]
fn handle_arena_start(
    state: &mut FlavorState,
    rules: Rules,
    zone_id: u32,
    match_type: &str,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    if rules != Rules::Retail {
        return;
    }
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.category != Category::SoloShuffle)
    {
        // Arena start over a non-shuffle activity ends it (never a shuffle round).
        let active = state.active.take().expect("checked above");
        finish(
            active,
            at_ms,
            EndKind::Abandon(AbandonReason::Superseded),
            config,
            finished,
            actions,
        );
    }
    let category = match match_type {
        "Rated Solo Shuffle" => Category::SoloShuffle,
        "2v2" => Category::TwoVTwo,
        // 3v3 retail war games are logged as 5v5.
        "3v3" | "5v5" => Category::ThreeVThree,
        "Skirmish" => Category::Skirmish,
        _ => return,
    };
    if state.active.is_none() && category == Category::SoloShuffle {
        let active = ActiveActivity {
            id: RecordingId::new(),
            category: Category::SoloShuffle,
            flavor: GameFlavor::Retail,
            started_at_ms: at_ms,
            overrun_ms: PVP_DEFAULT_OVERRUN_MS,
            combatants: Combatants::default(),
            player_guid: None,
            timeline: Vec::new(),
            meter: MeterAccumulator::new(at_ms, Some("Round 1".to_owned())),
            kind: ActiveKind::SoloShuffle(ShuffleState {
                zone_id,
                rounds: vec![ShuffleRound::new(at_ms)],
            }),
        };
        begin(state, active, at_ms, config, actions);
    } else if state.active.is_some() && category == Category::SoloShuffle {
        // New round of the existing shuffle. A previous round that never ended
        // is emitted as an unended round point.
        let active = state.active.as_mut().expect("checked above");
        let started_at_ms = active.started_at_ms;
        let mut pending = None;
        let mut round_number = 0;
        if let ActiveKind::SoloShuffle(shuffle) = &mut active.kind {
            let index = shuffle.rounds.len() - 1;
            if let Some(round) = shuffle.rounds.last_mut()
                && round.end_ms.is_none()
                && !round.item_emitted
            {
                round.item_emitted = true;
                pending = Some(round_point(started_at_ms, index, round));
            }
            shuffle.rounds.push(ShuffleRound::new(at_ms));
            round_number = shuffle.rounds.len();
        }
        if let Some(item) = pending {
            push_timeline(active, item, actions);
        }
        // A new round cuts the meter fight at the existing round transition.
        active.meter.cut(at_ms, format!("Round {round_number}"));
    } else {
        let active = ActiveActivity {
            id: RecordingId::new(),
            category,
            flavor: GameFlavor::Retail,
            started_at_ms: at_ms,
            overrun_ms: PVP_DEFAULT_OVERRUN_MS,
            combatants: Combatants::default(),
            player_guid: None,
            timeline: Vec::new(),
            meter: MeterAccumulator::new(at_ms, None),
            kind: ActiveKind::Arena(ArenaState { zone_id }),
        };
        begin(state, active, at_ms, config, actions);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_arena_end(
    state: &mut FlavorState,
    rules: Rules,
    winning_team_id: u32,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    if rules != Rules::Retail {
        return;
    }
    let Some(active) = state.active.as_ref() else {
        return;
    };
    if matches!(active.kind, ActiveKind::SoloShuffle(_)) {
        // End of game always records a win; the round score is the detail.
        let active = state.active.take().expect("checked above");
        finish(
            active,
            at_ms,
            EndKind::Complete(Outcome::Win),
            config,
            finished,
            actions,
        );
        return;
    }
    let result = arena_result(active, winning_team_id);
    let outcome = if result { Outcome::Win } else { Outcome::Loss };
    let active = state.active.take().expect("checked above");
    finish(
        active,
        at_ms,
        EndKind::Complete(outcome),
        config,
        finished,
        actions,
    );
}

/// False when the player is unknown.
fn arena_result(active: &ActiveActivity, winning_team_id: u32) -> bool {
    let Some(guid) = current_player_guid(active) else {
        return false;
    };
    let Some(player) = active.combatants.get(guid) else {
        return false;
    };
    player
        .team_id
        .is_some_and(|team| u32::from(team) == winning_team_id)
}

fn handle_zone_change(
    state: &mut FlavorState,
    rules: Rules,
    zone_id: u32,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    match rules {
        Rules::Retail => retail_zone_change(state, zone_id, at_ms, config, finished, actions),
        Rules::Classic => classic_zone_change(state, zone_id, at_ms, config, finished, actions),
        // The Era handler does not subscribe to zone changes.
        Rules::Era => {}
    }
}

fn retail_zone_change(
    state: &mut FlavorState,
    zone_id: u32,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    let is_zone_bg = retail_battleground_name(zone_id).is_some();
    let Some(active_ref) = state.active.as_ref() else {
        if is_zone_bg {
            start_battleground(state, zone_id, GameFlavor::Retail, at_ms, config, actions);
        }
        return;
    };
    let is_activity_bg = matches!(active_ref.kind, ActiveKind::Battleground { .. });
    if is_zone_bg && is_activity_bg {
        // Internal battleground zone change.
        return;
    }
    if !is_zone_bg && is_activity_bg {
        end_battleground(state, at_ms, config, finished, actions);
        return;
    }
    if is_arena_category(&active_ref.category) {
        if Some(zone_id) == active_zone_id(active_ref) {
            return;
        }
        // Zone change out of arena/shuffle: loss outcome.
        let active = state.active.take().expect("checked above");
        finish(
            active,
            at_ms,
            EndKind::Complete(Outcome::Loss),
            config,
            finished,
            actions,
        );
        return;
    }
    if is_zone_bg {
        // Zoned into a battleground over another activity.
        let active = state.active.take().expect("checked above");
        finish(
            active,
            at_ms,
            EndKind::Abandon(AbandonReason::Superseded),
            config,
            finished,
            actions,
        );
        start_battleground(state, zone_id, GameFlavor::Retail, at_ms, config, actions);
    }
}

fn classic_zone_change(
    state: &mut FlavorState,
    zone_id: u32,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    if let Some(active_ref) = state.active.as_ref() {
        let activity_zone = active_zone_id(active_ref).unwrap_or(0);
        if matches!(active_ref.kind, ActiveKind::Arena(_)) && zone_id != activity_zone {
            end_classic_arena(state, at_ms, config, finished, actions);
            return;
        }
        if matches!(active_ref.kind, ActiveKind::Battleground { .. }) && zone_id != activity_zone {
            end_battleground(state, at_ms, config, finished, actions);
        }
        return;
    }
    if classic_battleground_name(zone_id).is_some() {
        start_battleground(state, zone_id, GameFlavor::Classic, at_ms, config, actions);
    } else if classic_arena_name(zone_id).is_some() {
        // Classic arenas start as 2v2; the roster size adjusts the category.
        let active = ActiveActivity {
            id: RecordingId::new(),
            category: Category::TwoVTwo,
            flavor: GameFlavor::Classic,
            started_at_ms: at_ms,
            overrun_ms: PVP_DEFAULT_OVERRUN_MS,
            combatants: Combatants::default(),
            player_guid: None,
            timeline: Vec::new(),
            meter: MeterAccumulator::new(at_ms, None),
            kind: ActiveKind::Arena(ArenaState { zone_id }),
        };
        begin(state, active, at_ms, config, actions);
    }
}

fn start_battleground(
    state: &mut FlavorState,
    zone_id: u32,
    flavor: GameFlavor,
    at_ms: i64,
    config: &ActivitySettings,
    actions: &mut Vec<ActivityAction>,
) {
    if state.active.is_some() {
        return;
    }
    let active = ActiveActivity {
        id: RecordingId::new(),
        category: Category::Battlegrounds,
        flavor,
        started_at_ms: at_ms,
        overrun_ms: PVP_DEFAULT_OVERRUN_MS,
        combatants: Combatants::default(),
        player_guid: None,
        timeline: Vec::new(),
        meter: MeterAccumulator::new(at_ms, None),
        kind: ActiveKind::Battleground { zone_id },
    };
    begin(state, active, at_ms, config, actions);
}

fn end_battleground(
    state: &mut FlavorState,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(active) = state.active.take() else {
        return;
    };
    // The recorded result is always the death-count estimate.
    let outcome = battleground_estimate(&active);
    finish(
        active,
        at_ms,
        EndKind::Complete(outcome),
        config,
        finished,
        actions,
    );
}

/// Winner is the team with the least deaths (best effort estimate).
fn battleground_estimate(active: &ActiveActivity) -> Outcome {
    let friends_dead = death_count(active, true);
    let enemies_dead = death_count(active, false);
    if friends_dead < enemies_dead {
        Outcome::Win
    } else {
        Outcome::Loss
    }
}

fn end_classic_arena(
    state: &mut FlavorState,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(active) = state.active.take() else {
        return;
    };
    // Classic decides the winner by counting deaths; the player is always
    // assigned team 1, so fewer friendly deaths is a win.
    let friends_dead = death_count(&active, true);
    let enemies_dead = death_count(&active, false);
    let outcome = if friends_dead < enemies_dead {
        Outcome::Win
    } else {
        Outcome::Loss
    };
    finish(
        active,
        at_ms,
        EndKind::Complete(outcome),
        config,
        finished,
        actions,
    );
}

/// Deaths are stored as timeline points: friendly deaths carry `Loss`, enemy
/// deaths carry `Win`.
fn death_count(active: &ActiveActivity, friendly: bool) -> usize {
    let want = if friendly {
        Outcome::Loss
    } else {
        Outcome::Win
    };
    active
        .timeline
        .iter()
        .filter(|item| matches!(item.kind(), TimelineKind::Death) && item.outcome() == Some(want))
        .count()
}

// --- Combatants, player observation, deaths, boss health ---

fn handle_combatant_info(
    state: &mut FlavorState,
    rules: Rules,
    guid: &str,
    team_id: Option<u8>,
    spec_id: Option<u16>,
) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    match rules {
        Rules::Retail => {
            let target = combatant_target(active);
            if target
                .get(guid)
                .is_some_and(CombatantState::is_fully_defined)
            {
                return;
            }
            target.upsert(CombatantState {
                guid: guid.to_string(),
                team_id,
                spec_id,
                ..CombatantState::default()
            });
        }
        Rules::Classic => {
            let target = combatant_target(active);
            if target.contains(guid) {
                return;
            }
            target.upsert(CombatantState {
                guid: guid.to_string(),
                ..CombatantState::default()
            });
        }
        Rules::Era => {
            combatant_target(active).upsert(CombatantState {
                guid: guid.to_string(),
                team_id,
                spec_id,
                ..CombatantState::default()
            });
        }
    }
    update_arena_category(active);
}

#[allow(clippy::too_many_arguments)]
fn handle_player_observed(
    state: &mut FlavorState,
    rules: Rules,
    kind: PlayerObservationKind,
    spell_id: u32,
    guid: &str,
    name: &str,
    flags: u64,
    target_guid: &str,
    target_name: &str,
    target_flags: u64,
    spell_name: &str,
    owner_guid: Option<&str>,
    at_ms: i64,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    if let Some(owner) = owner_guid {
        active.meter.record_owner(guid, owner, None);
    }
    if kind == PlayerObservationKind::CastSucceeded && is_bloodlust_spell(spell_id) {
        let start_ms = relative_ms(active.started_at_ms, at_ms);
        let duplicate = active
            .timeline
            .iter()
            .any(|item| item.kind() == &TimelineKind::Bloodlust && item.start_ms() == start_ms);
        if !duplicate {
            let item = TimelineItem::span(
                TimelineKind::Bloodlust,
                start_ms,
                start_ms.saturating_add(BLOODLUST_DURATION_MS),
                Some(spell_name.to_owned()),
                None,
                None,
            )
            .expect("bloodlust duration is positive");
            push_timeline(active, item, actions);
        }
    }
    match rules {
        Rules::Retail => {
            if kind == PlayerObservationKind::CastSucceeded
                && let ActiveKind::Raid(raid) = &mut active.kind
            {
                update_boss_status(raid, false, name, spell_name);
            }
            let allow_new =
                matches!(active.kind, ActiveKind::Battleground { .. }) || is_unit_self(flags);
            let mut player_guid = current_player_guid(active).cloned();
            let index = process_combatant(
                combatant_target(active),
                &mut player_guid,
                guid,
                name,
                flags,
                allow_new,
            );
            set_player_guid(active, player_guid);
            if kind == PlayerObservationKind::CastSucceeded
                && matches!(active.kind, ActiveKind::Battleground { .. })
                && let Some(combatant) =
                    index.and_then(|i| combatant_target(active).entries.get_mut(i))
                && combatant.spec_id.is_none()
                && let Some(spec) = retail_unique_spec(spell_name)
            {
                combatant.spec_id = Some(spec);
            }
        }
        Rules::Classic => {
            let already_know = combatant_target(active).contains(guid);
            let Some(index) = process_classic_combatant(
                active,
                guid,
                name,
                flags,
                target_guid,
                target_name,
                target_flags,
            ) else {
                return;
            };
            // First enemy spotted in an arena: the gates just opened, so the
            // activity start moves to this event.
            if matches!(active.kind, ActiveKind::Arena(_)) && !already_know {
                let target = combatant_target(active);
                let is_enemy = target
                    .entries
                    .get(index)
                    .is_some_and(|combatant| combatant.team_id == Some(0));
                if is_enemy {
                    let enemies = target
                        .iter()
                        .filter(|combatant| combatant.team_id == Some(0))
                        .count();
                    if enemies == 1 {
                        active.started_at_ms = at_ms;
                    }
                }
            }
            let combatant = &mut combatant_target(active).entries[index];
            if combatant.spec_id.is_none() {
                let spec = match kind {
                    PlayerObservationKind::AuraApplied => classic_unique_aura(spell_name),
                    PlayerObservationKind::CastSucceeded => classic_unique_spec(spell_name),
                };
                if spec.is_some() {
                    combatant.spec_id = spec;
                }
            }
        }
        Rules::Era => {
            let mut player_guid = current_player_guid(active).cloned();
            let index = process_combatant(
                combatant_target(active),
                &mut player_guid,
                guid,
                name,
                flags,
                false,
            );
            set_player_guid(active, player_guid);
            if kind == PlayerObservationKind::CastSucceeded
                && let Some(combatant) =
                    index.and_then(|i| combatant_target(active).entries.get_mut(i))
                && combatant.spec_id.is_none()
                && let Some(spec) = classic_unique_spec(spell_name)
            {
                combatant.spec_id = Some(spec);
            }
        }
    }
}

fn process_classic_combatant(
    active: &mut ActiveActivity,
    guid: &str,
    name: &str,
    flags: u64,
    target_guid: &str,
    target_name: &str,
    target_flags: u64,
) -> Option<usize> {
    let src_identified = combatant_target(active).contains(guid);
    let dest_identified = combatant_target(active).contains(target_guid);
    if matches!(active.kind, ActiveKind::Arena(_))
        && !is_unit_self(flags)
        && !src_identified
        && !dest_identified
    {
        // Arena combatants are only identified by interaction with an already
        // identified unit, crawling out from the player.
        return None;
    }
    let mut player_guid = current_player_guid(active).cloned();
    if src_identified && !dest_identified {
        process_combatant(
            combatant_target(active),
            &mut player_guid,
            target_guid,
            target_name,
            target_flags,
            true,
        );
    }
    let index = process_combatant(
        combatant_target(active),
        &mut player_guid,
        guid,
        name,
        flags,
        true,
    )?;
    set_player_guid(active, player_guid);
    // Classic has no team IDs; friendly units are assigned team 1.
    let team = if is_unit_friendly(flags) { 1 } else { 0 };
    if let Some(combatant) = combatant_target(active).entries.get_mut(index) {
        combatant.team_id = Some(team);
    }
    update_arena_category(active);
    Some(index)
}

#[allow(clippy::too_many_arguments)]
fn handle_unit_died(
    state: &mut FlavorState,
    rules: Rules,
    _guid: &str,
    name: &str,
    flags: u64,
    unconscious: bool,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    if !is_unit_player(flags) || unconscious {
        return;
    }
    let friendly = is_unit_friendly(flags);
    let relative = relative_ms(active.started_at_ms, at_ms - DEATH_MARKER_BACK_OFFSET_MS);
    let (plain_name, _, _) = ambiguate(name);
    let outcome = if friendly {
        Outcome::Loss
    } else {
        Outcome::Win
    };

    if matches!(active.kind, ActiveKind::SoloShuffle(_)) {
        let started_at_ms = active.started_at_ms;
        let mut items = Vec::new();
        if let ActiveKind::SoloShuffle(shuffle) = &mut active.kind {
            let round_index = shuffle.rounds.len().saturating_sub(1);
            let mut decided = false;
            if let Some(round) = shuffle.rounds.last_mut()
                && !round.has_death
            {
                // The first player death of a round decides it; later
                // deaths in the round are dropped entirely.
                let player_team = round
                    .player_guid
                    .as_ref()
                    .and_then(|guid| round.combatants.get(guid))
                    .and_then(|player| player.team_id);
                if let Some(player_team) = player_team {
                    let winning_team = if !friendly {
                        player_team
                    } else if player_team == 0 {
                        1
                    } else {
                        0
                    };
                    round.has_death = true;
                    round.end_ms = Some(at_ms);
                    round.result = player_team == winning_team;
                    round.item_emitted = true;
                    decided = true;
                }
            }
            if decided {
                if let Some(round) = shuffle.rounds.last() {
                    items.push(round_span(started_at_ms, round_index, round));
                }
                items.push(TimelineItem::point(
                    TimelineKind::Death,
                    relative,
                    Some(plain_name),
                    Some(outcome),
                    None,
                ));
            }
        }
        for item in items {
            push_timeline(active, item, actions);
        }
        return;
    }

    push_timeline(
        active,
        TimelineItem::point(
            TimelineKind::Death,
            relative,
            Some(plain_name),
            Some(outcome),
            None,
        ),
        actions,
    );

    if rules == Rules::Classic && matches!(active.kind, ActiveKind::Arena(_)) {
        process_classic_arena_death(state, at_ms, config, finished, actions);
    }
}

fn process_classic_arena_death(
    state: &mut FlavorState,
    at_ms: i64,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    let Some(active) = state.active.as_ref() else {
        return;
    };
    let mut total_friends = 0usize;
    let mut total_enemies = 0usize;
    for combatant in active.combatants.iter() {
        if combatant.team_id == Some(1) {
            total_friends += 1;
        } else {
            total_enemies += 1;
        }
    }
    let dead_friends = death_count(active, true);
    if total_friends.saturating_sub(dead_friends) < 1 {
        end_classic_arena(state, at_ms, config, finished, actions);
        return;
    }
    let dead_enemies = death_count(active, false);
    if total_enemies.saturating_sub(dead_enemies) < 1 {
        end_classic_arena(state, at_ms, config, finished, actions);
    }
}

fn handle_boss_health(
    state: &mut FlavorState,
    rules: Rules,
    name: &str,
    current: u64,
    maximum: u64,
) {
    if rules != Rules::Retail {
        return;
    }
    let Some(active) = state.active.as_mut() else {
        return;
    };
    let ActiveKind::Raid(raid) = &mut active.kind else {
        return;
    };
    if !raid.boss_unit_active {
        return;
    }
    if !raid.boss_unit_name.is_empty() {
        if name != raid.boss_unit_name {
            return;
        }
        raid.max_hp = maximum;
        raid.current_hp = current;
        return;
    }
    // Below 100M max HP the unit is assumed not to be a boss (retail only).
    if maximum < MIN_RETAIL_BOSS_HP {
        return;
    }
    if maximum < raid.max_hp {
        return;
    }
    raid.max_hp = maximum;
    raid.current_hp = current;
}

fn handle_boss_cast(state: &mut FlavorState, rules: Rules, source_name: &str, spell_name: &str) {
    if rules != Rules::Retail {
        return;
    }
    let Some(active) = state.active.as_mut() else {
        return;
    };
    let ActiveKind::Raid(raid) = &mut active.kind else {
        return;
    };
    update_boss_status(raid, true, source_name, spell_name);
}

/// Belo'ren (and future similar bosses) only count damage in the egg phase,
/// bracketed by `Rebirth` cast start/success.
fn update_boss_status(
    raid: &mut RaidState,
    cast_started: bool,
    source_name: &str,
    spell_name: &str,
) {
    if source_name == BELOREN_UNIT_NAME && spell_name == BELOREN_PHASE_SPELL {
        raid.boss_unit_active = cast_started;
    }
}

// --- Finishing ---

fn finish(
    mut active: ActiveActivity,
    ended_at_ms: i64,
    end: EndKind,
    config: &ActivitySettings,
    finished: &mut Vec<RecordingDraft>,
    actions: &mut Vec<ActivityAction>,
) {
    finalize_open_items(&mut active, actions);
    let outcome = match end {
        EndKind::Complete(outcome) => outcome,
        EndKind::Abandon(_) => abandon_outcome(&active),
    };
    if matches!(end, EndKind::Abandon(_)) {
        active.overrun_ms = 0;
    }

    // Metadata completeness: the recording player must be identified with a
    // named combatant, and zone-based activities need a nonzero zone;
    // otherwise metadata cannot be built and the video is dropped.
    let zone_ok = match &active.kind {
        ActiveKind::Arena(arena) => arena.zone_id != 0,
        ActiveKind::Battleground { zone_id } => *zone_id != 0,
        ActiveKind::SoloShuffle(shuffle) => shuffle.zone_id != 0,
        ActiveKind::Challenge(challenge) => challenge.zone_id != 0,
        ActiveKind::Raid(_) => true,
    };
    if player_summary(&active).is_none() || !zone_ok {
        let id = active.id.clone();
        finished.push(build_draft(active, outcome, ended_at_ms));
        actions.push(ActivityAction::Discard {
            id,
            reason: DiscardReason::IncompleteMetadata,
        });
        return;
    }

    let duration_ms = relative_ms(active.started_at_ms, ended_at_ms) + active.overrun_ms;
    if active.category == Category::Raids
        && (duration_ms as i64) < i64::from(config.min_raid_duration_seconds) * 1_000
    {
        let id = active.id.clone();
        finished.push(build_draft(active, outcome, ended_at_ms));
        actions.push(ActivityAction::Discard {
            id,
            reason: DiscardReason::BelowMinDuration,
        });
        return;
    }

    let id = active.id.clone();
    finished.push(build_draft(active, outcome, ended_at_ms));
    match end {
        EndKind::Complete(_) => actions.push(ActivityAction::Complete {
            id,
            outcome,
            ended_at_ms,
        }),
        EndKind::Abandon(reason) => actions.push(ActivityAction::Abandon {
            id,
            ended_at_ms,
            reason,
        }),
    }
}

fn abandon_outcome(active: &ActiveActivity) -> Outcome {
    match &active.kind {
        ActiveKind::Challenge(_) => Outcome::Abandoned,
        ActiveKind::Battleground { .. } => battleground_estimate(active),
        _ => Outcome::Loss,
    }
}

/// Close any open challenge segment as zero length and emit unstarted or
/// unended solo-shuffle rounds as points.
fn finalize_open_items(active: &mut ActiveActivity, actions: &mut Vec<ActivityAction>) {
    let started_at_ms = active.started_at_ms;
    let mut items = Vec::new();
    match &mut active.kind {
        ActiveKind::Challenge(challenge) => {
            if let Some(segment) = challenge.segments.last_mut()
                && segment.end_ms.is_none()
            {
                segment.end_ms = Some(segment.start_ms);
                items.push(segment_item(started_at_ms, segment));
            }
        }
        ActiveKind::SoloShuffle(shuffle) => {
            for (index, round) in shuffle.rounds.iter_mut().enumerate() {
                if !round.item_emitted {
                    round.item_emitted = true;
                    items.push(round_point(started_at_ms, index, round));
                }
            }
        }
        _ => {}
    }
    for item in items {
        push_timeline(active, item, actions);
    }
}

fn player_summary(active: &ActiveActivity) -> Option<PlayerSummary> {
    let (combatants, guid) = if let ActiveKind::SoloShuffle(shuffle) = &active.kind {
        let round = shuffle.rounds.last()?;
        (&round.combatants, round.player_guid.as_ref()?)
    } else {
        (&active.combatants, active.player_guid.as_ref()?)
    };
    let combatant = combatants.get(guid)?;
    Some(PlayerSummary {
        name: combatant.name.clone()?,
        realm: combatant.realm.clone(),
        guid: Some(combatant.guid.clone()),
        class_id: None,
        spec_id: combatant.spec_id,
    })
}

fn combatant_summaries(active: &ActiveActivity) -> Vec<CombatantSummary> {
    // Solo shuffle records only the combatants from the final round, and
    // battlegrounds record none at all (the player is still required).
    let combatants = match &active.kind {
        ActiveKind::SoloShuffle(shuffle) => shuffle
            .rounds
            .last()
            .map(|round| &round.combatants)
            .unwrap_or(&active.combatants),
        ActiveKind::Battleground { .. } => return Vec::new(),
        _ => &active.combatants,
    };
    combatants
        .iter()
        .map(|combatant| CombatantSummary {
            name: combatant.name.clone(),
            realm: combatant.realm.clone(),
            guid: Some(combatant.guid.clone()),
            region: combatant.region.clone(),
            class_id: None,
            spec_id: combatant.spec_id,
            team_id: combatant.team_id,
        })
        .collect()
}

fn build_draft(active: ActiveActivity, outcome: Outcome, ended_at_ms: i64) -> RecordingDraft {
    let duration_ms = relative_ms(active.started_at_ms, ended_at_ms) + active.overrun_ms;
    let player = player_summary(&active);
    let combatants = combatant_summaries(&active);
    let activity_hash = activity_hash(&active, outcome);
    let title = title_for(&active, outcome, player.as_ref());
    let details = build_details(&active);
    let mut timeline = active.timeline.clone();
    timeline.sort_by_key(|item| item.start_ms());
    // Draining resolves pet ownership and bounds rows; unlabelled fights
    // (raid/arena/battleground) take the activity title.
    let names = combatant_names(&active);
    let meter = active
        .meter
        .drain(ended_at_ms, active.started_at_ms, &title, &names);
    RecordingDraft {
        id: active.id.clone(),
        category: active.category.clone(),
        flavor: active.flavor.clone(),
        started_at_ms: active.started_at_ms,
        overrun_ms: active.overrun_ms,
        details,
        player,
        combatants,
        timeline,
        outcome: Some(outcome),
        ended_at_ms: Some(ended_at_ms),
        duration_ms: Some(duration_ms),
        title: Some(title),
        activity_hash: Some(activity_hash),
        meter,
    }
}

/// GUID-to-name map for pet-owner merge naming, from the same combatant map
/// the summaries use.
fn combatant_names(active: &ActiveActivity) -> HashMap<String, String> {
    let combatants = match &active.kind {
        ActiveKind::SoloShuffle(shuffle) => shuffle
            .rounds
            .last()
            .map(|round| &round.combatants)
            .unwrap_or(&active.combatants),
        _ => &active.combatants,
    };
    combatants
        .iter()
        .filter_map(|combatant| {
            combatant
                .name
                .clone()
                .map(|name| (combatant.guid.clone(), name))
        })
        .collect()
}

fn build_details(active: &ActiveActivity) -> ActivityDetails {
    match &active.kind {
        ActiveKind::Raid(raid) => {
            let boss_percent =
                ((100.0 * raid.current_hp as f64) / raid.max_hp as f64).round() as u8;
            ActivityDetails::Raid {
                zone_id: Some(raid_zone_id(raid.encounter_id)),
                zone_name: Some(raid_lookup(raid.encounter_id).short_name.to_string()),
                encounter_id: Some(raid.encounter_id),
                encounter_name: Some(raid.encounter_name.clone()),
                difficulty_id: Some(raid.difficulty_id),
                difficulty: difficulty_info(raid.difficulty_id).map(|info| info.short.to_string()),
                pull: None,
                boss_percent: Some(boss_percent),
            }
        }
        ActiveKind::Challenge(challenge) => ActivityDetails::Dungeon {
            zone_id: Some(challenge.zone_id),
            dungeon_name: Some(dungeon_name(
                &active.flavor,
                challenge.zone_id,
                challenge.map_id,
            )),
            map_id: Some(challenge.map_id),
            keystone_level: Some(challenge.level),
            affixes: challenge.affixes.clone(),
            upgrade_level: Some(upgrade_level(active, challenge)),
        },
        ActiveKind::Arena(arena) => ActivityDetails::ArenaOrBattleground {
            map_id: Some(arena.zone_id),
            map_name: arena_zone_name(&active.flavor, arena.zone_id),
            team_mmr: None,
        },
        ActiveKind::Battleground { zone_id } => ActivityDetails::ArenaOrBattleground {
            map_id: Some(*zone_id),
            map_name: Some(battleground_name(*zone_id).to_string()),
            team_mmr: None,
        },
        ActiveKind::SoloShuffle(shuffle) => {
            let started_at_ms = active.started_at_ms;
            let rounds: Vec<RoundSummary> = shuffle
                .rounds
                .iter()
                .enumerate()
                .map(|(index, round)| RoundSummary {
                    round: (index + 1) as u32,
                    outcome: if round.result {
                        Outcome::Win
                    } else {
                        Outcome::Loss
                    },
                    start_ms: relative_ms(started_at_ms, round.start_ms),
                    duration_ms: round
                        .end_ms
                        .map(|end| end.saturating_sub(round.start_ms).max(0) as u64),
                })
                .collect();
            let rounds_won = rounds
                .iter()
                .filter(|round| round.outcome == Outcome::Win)
                .count() as u8;
            ActivityDetails::SoloRounds {
                map_id: Some(shuffle.zone_id),
                map_name: arena_zone_name(&active.flavor, shuffle.zone_id),
                rounds_won: Some(rounds_won),
                rounds_played: Some(rounds.len() as u8),
                rounds,
            }
        }
    }
}

/// Keystone upgrade from the challenge duration, compared against the raw
/// table values: retail tables are seconds, classic MoP tables are minutes (so
/// a completed classic run always scores +3).
fn upgrade_level(active: &ActiveActivity, challenge: &ChallengeState) -> u8 {
    let timers = if active.flavor == GameFlavor::Classic {
        mop_challenge_mode_timers(challenge.map_id)
    } else {
        dungeon_timers(challenge.map_id)
    };
    let Some(timers) = timers else {
        return 0;
    };
    let cm_duration_ms = challenge.cm_duration_ms.unwrap_or(0);
    if cm_duration_ms == 0 && active.flavor == GameFlavor::Retail {
        // Run didn't complete (abandoned, not a deplete).
        return 0;
    }
    let mut effective_ms = cm_duration_ms as i64;
    if challenge.affixes.contains(&CHALLENGERS_PERIL_AFFIX) {
        effective_ms -= CHALLENGERS_PERIL_ADJUST_MS;
    }
    let duration_for_result = effective_ms as f64 / 1_000.0;
    for (index, timer) in timers.iter().enumerate().rev() {
        if duration_for_result <= *timer {
            return (index + 1) as u8;
        }
    }
    0
}

fn title_for(active: &ActiveActivity, outcome: Outcome, player: Option<&PlayerSummary>) -> String {
    let base = match &active.kind {
        ActiveKind::Raid(raid) => {
            let lookup = raid_lookup(raid.encounter_id);
            let difficulty = difficulty_info(raid.difficulty_id)
                .map(|info| info.short)
                .unwrap_or("");
            let result_text = if outcome == Outcome::Win {
                "Kill"
            } else {
                "Wipe"
            };
            let encounter = format!("{} [{}] ({})", raid.encounter_name, difficulty, result_text);
            if lookup.name == "Unknown Raid" {
                encounter
            } else {
                format!("{}, {}", lookup.name, encounter)
            }
        }
        ActiveKind::Challenge(challenge) => {
            let name = dungeon_name(&active.flavor, challenge.zone_id, challenge.map_id);
            let result_text = if outcome == Outcome::Complete {
                format!("+{}", upgrade_level(active, challenge))
            } else {
                "Abandoned".to_string()
            };
            format!("{} +{} ({})", name, challenge.level, result_text)
        }
        ActiveKind::Arena(arena) => {
            let category_text = match active.category {
                Category::TwoVTwo => "2v2",
                Category::ThreeVThree => "3v3",
                Category::FiveVFive => "5v5",
                _ => "Skirmish",
            };
            // An unknown zone interpolates as "undefined" in the title.
            let zone = arena_zone_name(&active.flavor, arena.zone_id)
                .unwrap_or_else(|| "undefined".to_string());
            let result_text = if outcome == Outcome::Win {
                "Win"
            } else {
                "Loss"
            };
            format!("{} {} ({})", category_text, zone, result_text)
        }
        ActiveKind::Battleground { zone_id } => {
            let result_text = if outcome == Outcome::Win {
                "Win"
            } else {
                "Loss"
            };
            format!("{} ({})", battleground_name(*zone_id), result_text)
        }
        ActiveKind::SoloShuffle(shuffle) => {
            let zone = arena_zone_name(&active.flavor, shuffle.zone_id)
                .unwrap_or_else(|| "undefined".to_string());
            let won = shuffle.rounds.iter().filter(|round| round.result).count();
            let lost = shuffle.rounds.len() - won;
            format!("Solo Shuffle {} ({}-{})", zone, won, lost)
        }
    };
    match player {
        Some(player) => format!("{} - {}", player.name, base),
        None => base,
    }
}

/// MD5 of category, flavour, result and the sorted combatant names,
/// concatenated without a separator before the names. Solo shuffle hashes no
/// names (its activity-level map stays empty).
fn activity_hash(active: &ActiveActivity, outcome: Outcome) -> String {
    let category = category_hash_name(&active.category);
    let flavor = match active.flavor {
        GameFlavor::Retail => "Retail",
        _ => "Classic",
    };
    let result = match outcome {
        Outcome::Win | Outcome::Complete => "true",
        _ => "false",
    };
    let mut names: Vec<String> = active
        .combatants
        .iter()
        .filter_map(|combatant| combatant.name.clone())
        .filter(|name| !name.is_empty())
        .collect();
    // JS default sort orders by UTF-16 code units.
    names.sort_by_key(|name| name.encode_utf16().collect::<Vec<u16>>());
    let input = format!("{} {} {}{}", category, flavor, result, names.join(" "));
    md5_hex(input.as_bytes())
}

fn category_hash_name(category: &Category) -> &'static str {
    match category {
        Category::TwoVTwo => "2v2",
        Category::ThreeVThree => "3v3",
        Category::FiveVFive => "5v5",
        Category::Skirmish => "Skirmish",
        Category::SoloShuffle => "Solo Shuffle",
        Category::MythicPlus => "Mythic+",
        Category::Raids => "Raids",
        Category::Battlegrounds => "Battlegrounds",
        Category::Manual => "Manual",
        Category::Clip => "Clips",
        Category::Unknown(_) => "Unknown",
    }
}

fn round_point(started_at_ms: i64, index: usize, round: &ShuffleRound) -> TimelineItem {
    TimelineItem::point(
        TimelineKind::Round,
        relative_ms(started_at_ms, round.start_ms),
        Some(format!("Round {}", index + 1)),
        Some(if round.result {
            Outcome::Win
        } else {
            Outcome::Loss
        }),
        None,
    )
}

fn round_span(started_at_ms: i64, index: usize, round: &ShuffleRound) -> TimelineItem {
    let start = relative_ms(started_at_ms, round.start_ms);
    let end = relative_ms(started_at_ms, round.end_ms.unwrap_or(round.start_ms)).max(start);
    TimelineItem::span(
        TimelineKind::Round,
        start,
        end,
        Some(format!("Round {}", index + 1)),
        Some(if round.result {
            Outcome::Win
        } else {
            Outcome::Loss
        }),
        None,
    )
    .expect("clamped span bounds")
}

// --- Factual data tables (ported from src/main/constants.ts) ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum PartyType {
    Party,
    Raid,
    Pvp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MappedDifficulty {
    Lfr,
    Normal,
    Heroic,
    Mythic,
    Pvp,
}

struct DifficultyInfo {
    mapped: MappedDifficulty,
    short: &'static str,
    party: PartyType,
}

impl DifficultyInfo {
    /// Ordered difficulty index; PvP has none and never meets a raid threshold.
    fn order(&self) -> Option<u8> {
        match self.mapped {
            MappedDifficulty::Lfr => Some(0),
            MappedDifficulty::Normal => Some(1),
            MappedDifficulty::Heroic => Some(2),
            MappedDifficulty::Mythic => Some(3),
            MappedDifficulty::Pvp => None,
        }
    }
}

fn difficulty_order(difficulty: &RaidDifficulty) -> u8 {
    match difficulty {
        RaidDifficulty::Lfr => 0,
        RaidDifficulty::Normal => 1,
        RaidDifficulty::Heroic => 2,
        RaidDifficulty::Mythic => 3,
    }
}

use MappedDifficulty::{Heroic, Lfr, Mythic, Normal, Pvp};
use PartyType::{Party, Pvp as PvpParty, Raid};

#[rustfmt::skip]
static INSTANCE_DIFFICULTY: &[(u32, DifficultyInfo)] = &[
    (1, DifficultyInfo { mapped: Normal, short: "N", party: Party }),
    (2, DifficultyInfo { mapped: Heroic, short: "HC", party: Party }),
    (3, DifficultyInfo { mapped: Normal, short: "10N", party: Raid }),
    (4, DifficultyInfo { mapped: Normal, short: "25N", party: Raid }),
    (5, DifficultyInfo { mapped: Heroic, short: "10HC", party: Raid }),
    (6, DifficultyInfo { mapped: Heroic, short: "25HC", party: Raid }),
    (7, DifficultyInfo { mapped: Lfr, short: "LFR", party: Raid }),
    (8, DifficultyInfo { mapped: Mythic, short: "Mythic Keystone", party: Party }),
    (9, DifficultyInfo { mapped: Normal, short: "40", party: Raid }),
    (14, DifficultyInfo { mapped: Normal, short: "N", party: Raid }),
    (15, DifficultyInfo { mapped: Heroic, short: "HC", party: Raid }),
    (16, DifficultyInfo { mapped: Mythic, short: "M", party: Raid }),
    (17, DifficultyInfo { mapped: Lfr, short: "LFR", party: Raid }),
    (23, DifficultyInfo { mapped: Mythic, short: "M", party: Party }),
    (24, DifficultyInfo { mapped: Normal, short: "T", party: Party }),
    (33, DifficultyInfo { mapped: Normal, short: "T", party: Raid }),
    (34, DifficultyInfo { mapped: Pvp, short: "PvP", party: PvpParty }),
    (150, DifficultyInfo { mapped: Normal, short: "N", party: Party }),
    (151, DifficultyInfo { mapped: Lfr, short: "T", party: Raid }),
    (175, DifficultyInfo { mapped: Normal, short: "10N", party: Raid }),
    (176, DifficultyInfo { mapped: Normal, short: "25N", party: Raid }),
    (185, DifficultyInfo { mapped: Normal, short: "N", party: Raid }),
    (186, DifficultyInfo { mapped: Normal, short: "N", party: Raid }),
    (193, DifficultyInfo { mapped: Heroic, short: "10HC", party: Raid }),
    (194, DifficultyInfo { mapped: Heroic, short: "25HC", party: Raid }),
    (198, DifficultyInfo { mapped: Normal, short: "10N", party: Raid }),
    (215, DifficultyInfo { mapped: Normal, short: "10N", party: Raid }),
    (226, DifficultyInfo { mapped: Normal, short: "N", party: Raid }),
    (233, DifficultyInfo { mapped: Mythic, short: "M", party: Raid }),
];

fn difficulty_info(id: u32) -> Option<&'static DifficultyInfo> {
    INSTANCE_DIFFICULTY
        .iter()
        .find(|(entry_id, _)| *entry_id == id)
        .map(|(_, info)| info)
}

struct RaidInstance {
    zone_id: u32,
    name: &'static str,
    short_name: &'static str,
    encounters: &'static [u32],
}

static UNKNOWN_RAID: RaidInstance = RaidInstance {
    zone_id: 0,
    name: "Unknown Raid",
    short_name: "Unknown Raid",
    encounters: &[],
};

#[rustfmt::skip]
static RAID_INSTANCES: &[RaidInstance] = &[
    RaidInstance { zone_id: 13224, name: "Castle Nathria", short_name: "Nathria", encounters: &[2398, 2418, 2402, 2405, 2383, 2406, 2412, 2399, 2417, 2407] },
    RaidInstance { zone_id: 13561, name: "Sanctum of Domination", short_name: "Sanctum", encounters: &[2523, 2433, 2429, 2432, 2434, 2430, 2436, 2431, 2422, 2435] },
    RaidInstance { zone_id: 13742, name: "Sepulcher of the First Ones", short_name: "Sepulcher", encounters: &[2537, 2512, 2529, 2539, 2540, 2542, 2543, 2544, 2546, 2549, 2553] },
    RaidInstance { zone_id: 14030, name: "Vault of the Incarnates", short_name: "Vault", encounters: &[2587, 2639, 2590, 2592, 2635, 2605, 2614, 2607] },
    RaidInstance { zone_id: 14663, name: "Aberrus, the Shadowed Crucible", short_name: "Aberrus", encounters: &[2688, 2687, 2693, 2682, 2680, 2689, 2683, 2684, 2685] },
    RaidInstance { zone_id: 16279, name: "Sporefall", short_name: "Sporefall", encounters: &[3159] },
    RaidInstance { zone_id: 3456, name: "Naxxramas", short_name: "Naxxramas", encounters: &[1107, 1110, 1116, 1118, 1117, 1112, 1115, 1113, 1109, 1121, 1119, 1120, 1114, 1111, 1108] },
    RaidInstance { zone_id: 4500, name: "Eye of Eternity", short_name: "EoE", encounters: &[734] },
    RaidInstance { zone_id: 4493, name: "Obsidian Sanctum", short_name: "OS", encounters: &[742] },
    RaidInstance { zone_id: 4603, name: "Vault of Archavon", short_name: "VoA", encounters: &[772] },
    RaidInstance { zone_id: 4273, name: "Ulduar", short_name: "Ulduar", encounters: &[744, 745, 746, 747, 748, 749, 750, 751, 752, 753, 754, 755, 756, 757] },
    RaidInstance { zone_id: 4722, name: "Trial of the Crusader", short_name: "ToC", encounters: &[629, 633, 637, 641, 645] },
];

fn raid_lookup(encounter_id: u32) -> &'static RaidInstance {
    RAID_INSTANCES
        .iter()
        .find(|raid| raid.encounters.contains(&encounter_id))
        .unwrap_or(&UNKNOWN_RAID)
}

fn raid_zone_id(encounter_id: u32) -> u32 {
    raid_lookup(encounter_id).zone_id
}

static CURRENT_RETAIL_ENCOUNTERS: &[u32] = &[
    3176, 3177, 3178, 3179, 3180, 3181, 3182, 3183, 3306, 3159, 3470, 3445, 3455, 3497, 3420, 3421,
    3429, 3492, 3379, 9999,
];

#[rustfmt::skip]
static DUNGEON_ENCOUNTERS: &[(u32, &str)] = &[
    (1715, "Rocketspark and Borka"), (1732, "Nitrogg Thundertower"), (1736, "Skylord Tovra"),
    (1748, "Grimrail Enforcers"), (1749, "Fleshrender Nok'gar"), (1750, "Oshir"), (1754, "Skulloc, Son of Gruul"),
    (1954, "Maiden of Virtue"), (1957, "Opera Hall"), (1960, "Attumen the Huntsman"), (1961, "Moroes"),
    (1964, "The Curator"), (1959, "Mana Devourer"), (1965, "Shade of Medivh"), (2017, "Viz'aduum the Watcher"),
    (2257, "Tussle Tonks"), (2258, "K.U.-J.0."), (2259, "Machinist's Garden"), (2260, "King Mechagon"),
    (2290, "King Gobbamak"), (2291, "HK-8 Aerial Oppression Unit"), (2292, "Gunker"), (2312, "Trixie & Naeno"),
    (2356, "Ventunax"), (2357, "Kin-Tara"), (2358, "Oryphrion"), (2359, "Devos, Paragon of Loyalty"),
    (2360, "Kryxis the Voracious"), (2361, "Executor Tarvold"), (2362, "Grand Proctor Beryllia"), (2363, "General Kaal"),
    (2364, "Kul'tharok"), (2365, "Gorechop"), (2366, "Xav the Unfallen"), (2391, "An Affront of Challengers"), (2404, "Mordretha"),
    (2380, "Echelon"), (2381, "Lord Chamberlain"), (2401, "Halkias, the Sin-Stained Goliath"), (2403, "High Adjudicator Aleez"),
    (2382, "Globgrog"), (2384, "Doctor Ickus"), (2385, "Domina Venomblade"), (2386, "Stradama Margrave"),
    (2387, "Blightbone"), (2388, "Amarth, The Harvester"), (2389, "Surgeon Stitchflesh"), (2390, "Nalthor the Rimebinder"),
    (2394, "The Manastorms"), (2395, "Hakkar, the Soulflayer"), (2396, "Mueh'zala"), (2400, "Dealer Xy'exa"),
    (2397, "Ingra Maloch"), (2392, "Mistcaller"), (2393, "Tred'ova"),
    (2419, "Timecap'n Hooktail"), (2426, "Hylbrande"), (2442, "So'leah"),
    (2424, "Mailroom Mayhem"), (2425, "Zo'phex the Sentinel"), (2441, "The Grand Menagerie"), (2437, "So'azmi"), (2440, "Myza's Oasis"),
    (2609, "Melidrussa Chillworn"), (2606, "Kokia Blazehoof"), (2623, "Kyrakka and Erhkard Stormvein"),
    (2637, "Granyth"), (2636, "The Raging Tempest"), (2581, "Teera and Maruuk"), (2580, "Balakar Khan"),
    (2582, "Leymor"), (2585, "Azureblade"), (2583, "Telash Greywing"), (2584, "Umbrelskul"),
    (2562, "Vexamus"), (2563, "Overgrown Ancient"), (2564, "Crawth"), (2565, "Echo of Doragosa"),
    (1805, "Hymdall"), (1806, "Hyrja"), (1807, "Fenryr"), (1808, "God-King Skovald"), (1809, "Odyn"),
    (1868, "Patrol Captain Gerdo"), (1869, "Talixae Flamewreath"), (1870, "Advisor Melandrus"),
    (1677, "Sadana Bloodfury"), (1688, "Nhallish"), (1679, "Bonemaw"), (1682, "Ner'zhul"),
    (1418, "Wise Mari"), (1417, "Lorewalker Stonestep"), (1416, "Liu Flameheart"), (1439, "Sha of Doubt"),
    (2570, "Hackclaw's War-Band"), (2567, "Gutshot"), (2568, "Treemouth"), (2569, "Decatriarch Wratheye"),
    (2615, "Watcher Irideus"), (2616, "Gulping Goliath"), (2617, "Khajin the Unyielding"), (2618, "Primal Tsunami"),
    (2555, "The Lost Dwarves"), (2556, "Bromach"), (2557, "Sentinel Talondras"), (2558, "Emberon"), (2559, "Chrono-Lord Deios"),
    (2610, "Magmatusk"), (2611, "Warlord Sargha"), (2612, "Forgemaster Gorek"), (2613, "Chargath, Bane of Scales"),
    (2093, "Skycap'n Kragg"), (2094, "Council o' Captains"), (2095, "Ring of Booty"), (2096, "Harlan Sweete"),
    (2111, "Elder Leaxa"), (2118, "Cragmaw the Infested"), (2112, "Sporecaller Zancha"), (2123, "Unbound Abomination"),
    (1790, "Rokmora"), (1791, "Ularogg Cragshaper"), (1792, "Naraxas"), (1793, "Dargrul the Underking"),
    (1041, "Altairus"), (1042, "Asaad, Caliph of Zephyrs"), (1043, "Grand Vizier Ertan"),
    (2113, "Heartsbane Triad"), (2114, "Soulbound Goliath"), (2115, "Raal the Gluttonous"), (2116, "Lord and Lady Waycrest"), (2117, "Gorak Tul"),
    (1832, "The Amalgam of Souls"), (1833, "Illysanna Ravencrest"), (1834, "Smashspite the Hateful"), (1835, "Lord Kur'talos Ravencrest"),
    (1045, "Lady Naz'jar"), (1044, "Commander Ulthok, the Festering Prince"), (1046, "Mindbender Ghur'sha"), (1047, "Ozumat"),
    (1746, "Witherbark"), (1757, "Ancient Protectors"), (1751, "Archmage Sol"), (1756, "Yalnu"),
    (2084, "Priestess Alun'za"), (2086, "Rezan"), (2085, "Vol'kaal"), (2087, "Yazma"),
    (2666, "Chronikar"), (2667, "Manifested Timeways"), (2668, "Blight of Galakrond"), (2669, "Iridikron the Stonescaled"),
    (2670, "Tyr, the Infinite Keeper"), (2671, "Morchie"), (2672, "Time-Lost Battlefield"), (2673, "Chrono-Lord Deios"),
    (1836, "Archdruid Glaidalis"), (1837, "Oakheart"), (1838, "Dresaron"), (1839, "Shade of Xavius"),
    (2854, "E.D.N.A"), (2880, "Skarmorak"), (2888, "Master Machinists"), (2883, "Void Speaker Eirich"),
    (2837, "Speaker Shadowcrown"), (2838, "Anub'ikkaj"), (2839, "Rasha'nan"),
    (2907, "Orator Krix'vizk"), (2908, "Fangs of the Queen"), (2905, "The Coaglamation"), (2909, "Izo, the Grand Splicer"),
    (2926, "Avanoxx"), (2906, "Anub'zekt"), (2901, "Ki'katal the Harvester"),
    (2098, "Chopper Redhook"), (2109, "Dread Captain Lockwood"), (2099, "Hadal Darkfathom"), (2100, "Viq'Goth"),
    (1051, "General Umbriss"), (1050, "Forgemaster Throngus"), (1048, "Drahga Shadowburner"), (1049, "Erudax, the Duke of Below"),
    (2847, "Captain Dailcry"), (2835, "Baron Braunpyke"), (2848, "Prioress Murrpray"),
    (2829, "Ol' Waxbeard"), (2826, "Blazikon"), (2787, "The Candle King"), (2788, "The Darkness"),
    (2816, "Kyrioss"), (2861, "Stormguard Gorren"), (2836, "Voidstone Monstrosity"),
    (2900, "Brew Master Aldryr"), (2929, "I'pa"), (2931, "Benk Buzzbee"), (2930, "Goldie Baronbottom"),
    (3020, "Big M.O.M.M.A."), (3054, "Geezle Gigazap"), (3053, "Swampface"), (3019, "Demolition Duo"),
    (2105, "Coin-Operated Crowd Pummeler"), (2106, "Azerokk"), (2107, "Rixxa Fluxflame"), (2108, "Mogul Razdunk"),
    (3107, "Azhiccar"), (3108, "Taah'bat and A'wazj"), (3109, "Soul-Scribe"),
    (3071, "Arcanotron Custos"), (3073, "Gemellus"), (3072, "Seranel Sunlash"), (3074, "Degentrius"),
    (3212, "Muro'jin and Nekraxx"), (3214, "Rak'tul, Vessel of Souls"), (3213, "Vordaza"),
    (3328, "Chief Corewright Kasreth"), (3332, "Corewarden Nysarra"), (3333, "Lothraxion"),
    (3058, "Commander Kroluk"), (3057, "Derelict Duo"), (3056, "Emberdawn"), (3059, "The Restless Heart"),
    (1999, "Forgemaster Garfrost"), (2001, "Ick and Krick"), (2000, "Scourgelord Tyrannus"),
    (2068, "L'ura"), (2066, "Saprish"), (2067, "Viceroy Nezhar"), (2065, "Zuraal the Ascended"),
    (1699, "Araknath"), (1701, "High Sage Viryx"), (1698, "Ranjit"), (1700, "Rukhran"),
    (3101, "Kystia Manaheart"), (3102, "Zaen Bladesorrow"), (3103, "Xathuux the Annihilator"), (3105, "Lithiel Cinderfury"),
    (3207, "The Hoardmonger"), (3208, "Sentinel of Winter"), (3209, "Nalorakk"),
    (3199, "Lightblossom Trinity"), (3200, "Ikuzz the Light Hunter"), (3201, "Lightwarden Ruia"), (3202, "Ziekket"),
    (3285, "Taz'Rah"), (3286, "Atroxus"), (3287, "Charonus"),
    (3456, "Rav'i"), (3457, "The Writhing Coil"), (3458, "Zul'jan"),
    (2124, "Adderis and Aspix"), (2125, "Merektha"), (2126, "Galvazzt"), (2127, "Avatar of Sethraliss"),
    (2139, "The Golden Serpent"), (2142, "Mchimba the Embalmer"), (2140, "The Council of Tribes"), (2143, "Dazar, The First King"),
];

fn dungeon_encounter_name(encounter_id: u32) -> Option<&'static str> {
    DUNGEON_ENCOUNTERS
        .iter()
        .find(|(id, _)| *id == encounter_id)
        .map(|(_, name)| *name)
}

/// Retail keystone map membership.
#[rustfmt::skip]
static RETAIL_DUNGEON_MAP_IDS: &[u32] = &[
    166, 169, 227, 234, 369, 370, 375, 376, 377, 378, 379, 380, 381, 382, 391, 392,
    399, 400, 401, 402, 200, 210, 165, 2,
    405, 406, 403, 404, 245, 251, 206, 438,
    463, 464, 248, 198, 199, 244, 168, 456,
    353, 501, 502, 503, 505, 507,
    499, 504, 500, 506, 247, 525,
    542,
    558, 560, 559, 557, 556, 239, 161,
    587, 586, 584, 585, 588, 250, 249,
];

/// Retail keystone timers in seconds `[one, two, three] chest`.
#[rustfmt::skip]
static DUNGEON_TIMERS: &[(u32, [f64; 3])] = &[
    (377, [2580.0, 2065.0, 1549.0]),
    (378, [1920.0, 1536.0, 1152.0]),
    (375, [1800.0, 1440.0, 1080.0]),
    (379, [2280.0, 1824.0, 1358.0]),
    (380, [2460.0, 1968.0, 1476.0]),
    (381, [2340.0, 1872.0, 1404.0]),
    (376, [1950.0, 1578.0, 1206.0]),
    (382, [2040.0, 1632.0, 1224.0]),
    (227, [2520.0, 2016.0, 1512.0]),
    (234, [2100.0, 1680.0, 1260.0]),
    (369, [2280.0, 1824.0, 1358.0]),
    (370, [1920.0, 1536.0, 1152.0]),
    (391, [2100.0, 1680.0, 1260.0]),
    (392, [1800.0, 1440.0, 1080.0]),
    (169, [1800.0, 1440.0, 1080.0]),
    (166, [1800.0, 1440.0, 1080.0]),
    (399, [1800.0, 1440.0, 1080.0]),
    (400, [2400.0, 1920.0, 1440.0]),
    (401, [2250.0, 1800.0, 1350.0]),
    (200, [2280.0, 1824.0, 1428.0]),
    (210, [1800.0, 1440.0, 1080.0]),
    (165, [1980.0, 1584.0, 1188.0]),
    (2, [1800.0, 1440.0, 1080.0]),
    (405, [2100.0, 1680.0, 1260.0]),
    (406, [2100.0, 1680.0, 1260.0]),
    (403, [2100.0, 1680.0, 1260.0]),
    (404, [1980.0, 1584.0, 1188.0]),
    (245, [1800.0, 1440.0, 1080.0]),
    (251, [1800.0, 1440.0, 1080.0]),
    (206, [1980.0, 1584.0, 1188.0]),
    (438, [1800.0, 1440.0, 1080.0]),
    (463, [2040.0, 1632.0, 1224.0]),
    (464, [2160.0, 1680.0, 1260.0]),
    (248, [2200.0, 1760.0, 1320.0]),
    (198, [1800.0, 1440.0, 1080.0]),
    (199, [2160.0, 1728.0, 1404.0]),
    (244, [1800.0, 1440.0, 1080.0]),
    (168, [1980.0, 1584.0, 1188.0]),
    (456, [2040.0, 1632.0, 1224.0]),
    (501, [1980.0, 1584.0, 1188.0]),
    (503, [1800.0, 1440.0, 1080.0]),
    (353, [1980.0, 1584.0, 1188.0]),
    (502, [2100.0, 1680.0, 1260.0]),
    (505, [1860.0, 1488.0, 1116.0]),
    (507, [2040.0, 1632.0, 1224.0]),
    (506, [1980.0, 1584.0, 1188.0]),
    (504, [1860.0, 1464.0, 1128.0]),
    (500, [1740.0, 1404.0, 1068.0]),
    (499, [1950.0, 1560.0, 1170.0]),
    (525, [1980.0, 1584.0, 1188.0]),
    (247, [1980.0, 1584.0, 1188.0]),
    (542, [1860.0, 1488.0, 1116.0]),
    (558, [2040.0, 1632.0, 1224.0]),
    (560, [1980.0, 1584.0, 1188.0]),
    (559, [1800.0, 1440.0, 1080.0]),
    (557, [2010.0, 1608.0, 1206.0]),
    (402, [1860.0, 1488.0, 1116.0]),
    (556, [1800.0, 1440.0, 1080.0]),
    (239, [2040.0, 1632.0, 1224.0]),
    (161, [1680.0, 1356.0, 1002.0]),
    (587, [1800.0, 1440.0, 1080.0]),
    (586, [1800.0, 1440.0, 1080.0]),
    (584, [1800.0, 1440.0, 1080.0]),
    (585, [1800.0, 1440.0, 1080.0]),
    (588, [1800.0, 1440.0, 1080.0]),
    (250, [1800.0, 1440.0, 1080.0]),
    (249, [1800.0, 1440.0, 1080.0]),
];

fn dungeon_timers(map_id: u32) -> Option<&'static [f64]> {
    DUNGEON_TIMERS
        .iter()
        .find(|(id, _)| *id == map_id)
        .map(|(_, timers)| &timers[..])
}

/// Dungeon zone names for Mythic+ metadata (`dungeonsByZoneId`).
#[rustfmt::skip]
static DUNGEONS_BY_ZONE_ID: &[(u32, &str)] = &[
    (1651, "Return to Karazhan"), (1208, "Grimrail Depot"), (1195, "Iron Docks"),
    (2097, "Operation: Mechagon"), (2291, "De Other Side"), (2287, "Halls of Atonement"),
    (2290, "Mists of Tirna Scithe"), (2289, "Plaguefall"), (2284, "Sanguine Depths"),
    (2285, "Spires of Ascension"), (2286, "The Necrotic Wake"), (2293, "Theater of Pain"),
    (2441, "Tazavesh the Veiled Market"),
    (2521, "Ruby Life Pools"), (2516, "The Nokhud Offensive"), (2515, "The Azure Vault"),
    (2526, "Algeth'ar Academy"), (1477, "Halls of Valor"), (1571, "Court of Stars"),
    (1176, "Shadowmoon Burial Grounds"), (960, "Temple of the Jade Serpent"),
    (2520, "Brackenhide Hollow"), (2527, "Halls of Infusion"), (2451, "Uldaman: Legacy of Tyr"),
    (2519, "Neltharus"), (1754, "Freehold"), (1841, "The Underrot"), (1458, "Neltharion's Lair"),
    (657, "The Vortex Pinnacle"),
    (2579, "Dawn of the Infinite"), (1862, "Waycrest Manor"), (1466, "Darkheart Thicket"),
    (1501, "Black Rook Hold"), (1763, "Atal'Dazar"), (1279, "The Everbloom"), (643, "Throne of the Tides"),
    (670, "Grim Batol"), (1822, "Siege of Boralus"), (2652, "The Stonevault"),
    (2660, "Ara-Kara, City of Echoes"), (2662, "The Dawnbreaker"), (2669, "City of Threads"),
    (2649, "Priory of the Sacred Flame"), (2651, "Darkflame Cleft"), (2648, "The Rookery"),
    (2661, "Cinderbrew Meadery"), (2773, "Operation: Floodgate"), (1594, "THE MOTHERLODE!!"),
    (2830, "Eco-Dome Al'Dani"),
    (2805, "Windrunner Spire"), (2811, "Magisters' Terrace"), (2874, "Maisara Caverns"),
    (2915, "Nexus-Point Xenas"), (658, "Pit of Saron"), (1209, "Skyreach"),
    (1753, "Seat of the Triumvirate"),
    (2813, "Murder Row"), (2825, "Den of Nalorakk"), (2859, "The Blinding Vale"),
    (2923, "Voidscar Arena"), (2993, "Altar of Fangs"), (1877, "Temple of Sethraliss"),
    (1762, "Kings' Rest"),
];

#[rustfmt::skip]
static RETAIL_BATTLEGROUNDS: &[(u32, &str)] = &[
    (30, "Alterac Valley"), (2107, "Arathi Basin"), (1681, "Arathi Basin"),
    (1105, "Deepwind Gorge"), (2245, "Deepwind Gorge"), (566, "Eye of the Storm"),
    (968, "Eye of the Storm"), (628, "Isle of Conquest"), (1803, "Seething Shore"),
    (727, "Silvershard Mines"), (998, "Temple of Kotmogu"), (761, "The Battle for Gilneas"),
    (726, "Twin Peaks"), (489, "Warsong Gulch"), (2106, "Warsong Gulch"),
    (2656, "Deephaul Ravine"), (2188, "Wintergrasp"),
];

#[rustfmt::skip]
static CLASSIC_BATTLEGROUNDS: &[(u32, &str)] = &[
    (30, "Alterac Valley"), (529, "Arathi Basin"), (566, "Eye of the Storm"),
    (607, "Strand of the Ancients"), (489, "Warsong Gulch"),
];

#[rustfmt::skip]
static RETAIL_ARENAS: &[(u32, &str)] = &[
    (1672, "Blade's Edge"), (617, "Dalaran Sewers"), (1505, "Nagrand"),
    (572, "Ruins of Lordaeron"), (2167, "Robodrome"), (1134, "Tiger's Peak"),
    (980, "Tol'viron"), (1504, "Black Rook"), (2373, "Empyrean Domain"),
    (1552, "Ashamane's Fall"), (1911, "Mugambala"), (1825, "Hook Point"),
    (2509, "Maldraxxus"), (2547, "Enigma Crucible"), (2563, "Nokhudon"),
    (2759, "Cage of Carnage"), (2923, "Voidscar Arena"),
];

#[rustfmt::skip]
static CLASSIC_ARENAS: &[(u32, &str)] = &[
    (572, "Ruins of Lordaeron"), (559, "Nagrand"), (617, "Dalaran"),
    (562, "Blade's Edge"), (1134, "Tiger's Peak"), (980, "Tol'viron"),
];

fn lookup(table: &[(u32, &'static str)], id: u32) -> Option<&'static str> {
    table
        .iter()
        .find(|(key, _)| *key == id)
        .map(|entry| entry.1)
}

fn retail_battleground_name(zone_id: u32) -> Option<&'static str> {
    lookup(RETAIL_BATTLEGROUNDS, zone_id)
}

fn classic_battleground_name(zone_id: u32) -> Option<&'static str> {
    lookup(CLASSIC_BATTLEGROUNDS, zone_id)
}

fn battleground_name(zone_id: u32) -> &'static str {
    retail_battleground_name(zone_id)
        .or_else(|| classic_battleground_name(zone_id))
        .unwrap_or("Unknown Battleground")
}

fn classic_arena_name(zone_id: u32) -> Option<&'static str> {
    lookup(CLASSIC_ARENAS, zone_id)
}

fn arena_zone_name(flavor: &GameFlavor, zone_id: u32) -> Option<String> {
    let name = match flavor {
        GameFlavor::Retail => lookup(RETAIL_ARENAS, zone_id),
        _ => lookup(CLASSIC_ARENAS, zone_id),
    };
    name.map(str::to_string)
}

/// Instance names by zone id (battlegrounds, arenas, dungeons) with the classic
/// MoP challenge-mode fallback, without a default. Crate-public because legacy
/// sidecars store only ids and storage restores display names from this table.
pub(crate) fn instance_name(flavor: &GameFlavor, zone_id: u32, map_id: u32) -> Option<String> {
    let merged = retail_battleground_name(zone_id)
        .or_else(|| classic_battleground_name(zone_id))
        .or_else(|| lookup(RETAIL_ARENAS, zone_id))
        .or_else(|| lookup(CLASSIC_ARENAS, zone_id))
        .or_else(|| lookup(DUNGEONS_BY_ZONE_ID, zone_id));
    if let Some(name) = merged {
        return Some(name.to_string());
    }
    if *flavor == GameFlavor::Classic {
        return mop_challenge_mode_name(map_id).map(str::to_string);
    }
    None
}

/// `instance_name` with the `Unknown Dungeon` default.
fn dungeon_name(flavor: &GameFlavor, zone_id: u32, map_id: u32) -> String {
    instance_name(flavor, zone_id, map_id).unwrap_or_else(|| "Unknown Dungeon".to_string())
}

fn mop_challenge_mode_name(map_id: u32) -> Option<&'static str> {
    lookup(MOP_CHALLENGE_MODES, map_id)
}

fn mop_challenge_mode_timers(map_id: u32) -> Option<&'static [f64]> {
    MOP_CHALLENGE_MODE_TIMERS
        .iter()
        .find(|(id, _)| *id == map_id)
        .map(|(_, timers)| &timers[..])
}

#[rustfmt::skip]
static MOP_CHALLENGE_MODES: &[(u32, &str)] = &[
    (2, "Temple of the Jade Serpent"), (56, "Stormstout Brewery"),
    (57, "Gate of the Setting Sun"), (58, "Shado-Pan Monastery"),
    (59, "Siege of Niuzao Temple"), (60, "Mogu'shan Palace"),
    (76, "Scholomance"), (77, "Scarlet Halls"), (78, "Scarlet Monastery"),
];

/// Gold/silver/bronze MoP challenge timers, in minutes.
#[rustfmt::skip]
static MOP_CHALLENGE_MODE_TIMERS: &[(u32, [f64; 3])] = &[
    (2, [45.0, 25.0, 15.0]),
    (56, [45.0, 21.0, 12.0]),
    (57, [45.0, 22.0, 13.0]),
    (58, [60.0, 35.0, 21.0]),
    (59, [50.0, 30.0, 17.5]),
    (60, [45.0, 21.0, 12.0]),
    (76, [55.0, 33.0, 19.0]),
    (77, [45.0, 25.0, 13.0]),
    (78, [45.0, 22.0, 13.0]),
];

#[rustfmt::skip]
static RETAIL_UNIQUE_SPEC_SPELLS: &[(&str, u16)] = &[
    ("Heart Strike", 250), ("Frost Strike", 251), ("Festering Strike", 252),
    ("Eye Beam", 577), ("Fel Devastation", 581), ("Starfall", 102),
    ("Tiger's Fury", 103), ("Maul", 104), ("Lifebloom", 105), ("Pyre", 1467),
    ("Echo", 1468), ("Ebon Might", 1473), ("Cobra Shot", 253), ("Aimed Shot", 254),
    ("Raptor Strike", 255), ("Arcane Barrage", 62), ("Pyroblast", 63),
    ("Ice Lance", 64), ("Keg Smash", 268), ("Fists of Fury", 269),
    ("Enveloping Mist", 270), ("Holy Shock", 65), ("Avenger's Shield", 66),
    ("Blade of Justice", 70), ("Penance", 256), ("Holy Word: Serenity", 257),
    ("Devouring Plague", 258), ("Mutilate", 259), ("Sinister Strike", 260),
    ("Shadow Dance", 261), ("Earth Shock", 262), ("Stormstrike", 263),
    ("Riptide", 264), ("Malefic Rapture", 265), ("Call Dreadstalkers", 266),
    ("Chaos Bolt", 267), ("Mortal Strike", 71), ("Bloodthirst", 72),
    ("Ignore Pain", 73),
];

#[rustfmt::skip]
static CLASSIC_UNIQUE_SPEC_SPELLS: &[(&str, u16)] = &[
    ("Heart Strike", 250), ("Howling Blast", 251), ("Summon Gargoyle", 252),
    ("Starfall", 102), ("Mangle", 103), ("Swiftmend", 105), ("Nourish", 105),
    ("Lifebloom", 105), ("Bestial Wrath", 253), ("Chimera Shot", 254),
    ("Explosive Shot", 255), ("Arcane Barrage", 62), ("Dragon's Breath", 63),
    ("Combustion", 63), ("Ice Barrier", 64), ("Deep Freeze", 64),
    ("Holy Shock", 65), ("Avenger's Shield", 66), ("Crusader Strike", 70),
    ("Penance", 256), ("Guardian Spirit", 257), ("Vampiric Touch", 258),
    ("Mutilate", 259), ("Killing Spree", 260), ("Shadowstep", 261),
    ("Lava Burst", 262), ("Thunderstorm", 262), ("Feral Spirit", 263),
    ("Riptide", 264), ("Haunt", 265), ("Metamorphosis", 266),
    ("Chaos Bolt", 267), ("Mortal Strike", 71), ("Bloodthirst", 72),
    ("Shockwave", 73),
];

#[rustfmt::skip]
static CLASSIC_UNIQUE_SPEC_AURAS: &[(&str, u16)] = &[
    ("Borrowed Time", 256), ("Unstable Affliction", 265), ("The Art of War", 70),
];

fn spell_lookup(table: &[(&'static str, u16)], name: &str) -> Option<u16> {
    table
        .iter()
        .find(|(spell, _)| *spell == name)
        .map(|(_, spec)| *spec)
}

fn retail_unique_spec(spell: &str) -> Option<u16> {
    spell_lookup(RETAIL_UNIQUE_SPEC_SPELLS, spell)
}

fn classic_unique_spec(spell: &str) -> Option<u16> {
    spell_lookup(CLASSIC_UNIQUE_SPEC_SPELLS, spell)
}

fn classic_unique_aura(spell: &str) -> Option<u16> {
    spell_lookup(CLASSIC_UNIQUE_SPEC_AURAS, spell)
}

// --- MD5 (RFC 1321) for the legacy-compatible activity hash ---

fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    let mut message = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[index * 4],
                chunk[index * 4 + 1],
                chunk[index * 4 + 2],
                chunk[index * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (mut f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(words[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            digest.push_str(&format!("{byte:02x}"));
        }
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    const SELF_FLAGS: u64 = 0x511;
    const FRIENDLY_FLAGS: u64 = 0x512;
    const ENEMY_FLAGS: u64 = 0x548;

    fn handle(
        engine: &mut ActivityEngine,
        flavor: GameFlavor,
        at_ms: i64,
        event: CombatEvent,
        config: &ActivitySettings,
    ) -> Vec<ActivityAction> {
        engine.handle(
            ParsedEvent {
                flavor,
                occurred_at_ms: at_ms,
                event,
            },
            config,
        )
    }

    fn encounter_start(encounter_id: u32, name: &str, difficulty_id: u32) -> CombatEvent {
        CombatEvent::EncounterStarted {
            encounter_id,
            name: name.to_string(),
            difficulty_id,
            group_size: 20,
            instance_id: 1,
        }
    }

    fn encounter_end(encounter_id: u32, difficulty_id: u32, success: bool) -> CombatEvent {
        CombatEvent::EncounterEnded {
            encounter_id,
            name: String::new(),
            difficulty_id,
            group_size: 20,
            success,
        }
    }

    fn combatant(guid: &str, team_id: Option<u8>, spec_id: Option<u16>) -> CombatEvent {
        CombatEvent::Combatant {
            guid: guid.to_string(),
            team_id,
            spec_id,
        }
    }

    fn cast(guid: &str, name: &str, flags: u64, spell: &str) -> CombatEvent {
        cast_at(guid, name, flags, EMPTY_GUID, "nil", 0, spell)
    }

    fn bloodlust_cast(guid: &str, name: &str, flags: u64) -> CombatEvent {
        let mut event = cast(guid, name, flags, "Fury of the Aspects");
        let CombatEvent::PlayerObserved { spell_id, .. } = &mut event else {
            unreachable!();
        };
        *spell_id = 390386;
        event
    }

    #[allow(clippy::too_many_arguments)]
    fn cast_at(
        guid: &str,
        name: &str,
        flags: u64,
        target_guid: &str,
        target_name: &str,
        target_flags: u64,
        spell: &str,
    ) -> CombatEvent {
        CombatEvent::PlayerObserved {
            kind: PlayerObservationKind::CastSucceeded,
            spell_id: 0,
            guid: guid.to_string(),
            name: name.to_string(),
            flags,
            target_guid: target_guid.to_string(),
            target_name: target_name.to_string(),
            target_flags,
            spell_name: spell.to_string(),
            owner_guid: None,
        }
    }

    fn died(guid: &str, name: &str, flags: u64) -> CombatEvent {
        CombatEvent::UnitDied {
            guid: guid.to_string(),
            name: name.to_string(),
            flags,
            unconscious: false,
        }
    }

    fn begins(actions: &[ActivityAction]) -> usize {
        actions
            .iter()
            .filter(|action| matches!(action, ActivityAction::Begin { .. }))
            .count()
    }

    #[test]
    fn bloodlust_cast_adds_one_40_second_span_to_the_active_recording() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let start = 100_000;
        let begin = handle(
            &mut engine,
            GameFlavor::Retail,
            start,
            CombatEvent::ChallengeStarted {
                name: "The Stonevault".to_owned(),
                zone_id: 2286,
                map_id: 377,
                level: 10,
                affixes: vec![],
            },
            &config,
        );
        let ActivityAction::Begin { draft, .. } = &begin[0] else {
            panic!("expected recording start");
        };
        let id = draft.id.clone();

        let actions = handle(
            &mut engine,
            GameFlavor::Retail,
            start + 12_345,
            bloodlust_cast("Player-1-A", "Evoker-Realm", FRIENDLY_FLAGS),
            &config,
        );
        assert_eq!(
            actions,
            vec![ActivityAction::Update {
                id,
                item: TimelineItem::span(
                    TimelineKind::Bloodlust,
                    12_345,
                    52_345,
                    Some("Fury of the Aspects".to_owned()),
                    None,
                    None,
                )
                .unwrap(),
            }]
        );
        let duplicate = handle(
            &mut engine,
            GameFlavor::Retail,
            start + 12_345,
            bloodlust_cast("Player-1-A", "Evoker-Realm", FRIENDLY_FLAGS),
            &config,
        );
        assert!(duplicate.is_empty());
    }

    #[test]
    fn retail_raid_kill_produces_golden_metadata() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;

        let actions = handle(
            &mut engine,
            retail.clone(),
            100_000,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin {
            draft,
            detected_at_ms,
        } = &actions[0]
        else {
            panic!("expected Begin");
        };
        assert_eq!(*detected_at_ms, 100_000);
        assert_eq!(draft.started_at_ms, 100_000);
        assert_eq!(draft.category, Category::Raids);
        assert_eq!(draft.overrun_ms, RAID_DEFAULT_OVERRUN_MS);
        assert!(draft.timeline.is_empty());
        let id = draft.id.clone();

        handle(
            &mut engine,
            retail.clone(),
            100_500,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            100_600,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let death = handle(
            &mut engine,
            retail.clone(),
            110_000,
            died("Player-2-A", "Beta-Realm", FRIENDLY_FLAGS),
            &config,
        );
        assert_eq!(
            death,
            vec![ActivityAction::Update {
                id: id.clone(),
                item: TimelineItem::point(
                    TimelineKind::Death,
                    9_998,
                    Some("Beta".to_string()),
                    Some(Outcome::Loss),
                    None,
                ),
            }]
        );

        let end = handle(
            &mut engine,
            retail,
            130_000,
            encounter_end(2587, 16, true),
            &config,
        );
        assert_eq!(
            end,
            vec![ActivityAction::Complete {
                id: id.clone(),
                outcome: Outcome::Win,
                ended_at_ms: 130_000,
            }]
        );

        let finished = engine.take_finished(&id).expect("finished draft");
        assert_eq!(finished.outcome, Some(Outcome::Win));
        assert_eq!(finished.ended_at_ms, Some(130_000));
        // 30 s of combat plus the configured 15 s kill overrun.
        assert_eq!(finished.overrun_ms, 15_000);
        assert_eq!(finished.duration_ms, Some(45_000));
        assert_eq!(
            finished.title.as_deref(),
            Some("Alpha - Vault of the Incarnates, Eranog [M] (Kill)")
        );
        assert_eq!(
            finished.activity_hash.as_deref(),
            Some("159d8df5e1ef99d5f039d31f01dd4706")
        );
        assert_eq!(
            finished.player,
            Some(PlayerSummary {
                name: "Alpha".to_string(),
                realm: Some("Realm".to_string()),
                guid: Some("Player-1-A".to_string()),
                class_id: None,
                spec_id: Some(71),
            })
        );
        assert_eq!(finished.combatants.len(), 1);
        assert_eq!(finished.timeline.len(), 1);
        assert_eq!(
            finished.details,
            ActivityDetails::Raid {
                zone_id: Some(14030),
                zone_name: Some("Vault".to_string()),
                encounter_id: Some(2587),
                encounter_name: Some("Eranog".to_string()),
                difficulty_id: Some(16),
                difficulty: Some("M".to_string()),
                pull: None,
                boss_percent: Some(100),
            }
        );
        assert!(engine.take_finished(&id).is_none());
    }

    #[test]
    fn short_raid_wipe_is_discarded() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;

        let actions = handle(
            &mut engine,
            retail.clone(),
            0,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        handle(
            &mut engine,
            retail.clone(),
            100,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        // 5 s wipe + 3 s default overrun = 8 s < the 15 s minimum.
        let end = handle(
            &mut engine,
            retail,
            5_000,
            encounter_end(2587, 16, false),
            &config,
        );
        assert_eq!(
            end,
            vec![ActivityAction::Discard {
                id: id.clone(),
                reason: DiscardReason::BelowMinDuration,
            }]
        );
        assert_eq!(
            engine.take_finished(&id).unwrap().outcome,
            Some(Outcome::Loss)
        );
    }

    #[test]
    fn raid_without_identified_player_is_discarded() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;
        handle(
            &mut engine,
            retail.clone(),
            0,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        let end = handle(
            &mut engine,
            retail,
            60_000,
            encounter_end(2587, 16, true),
            &config,
        );
        assert!(matches!(
            end.as_slice(),
            [ActivityAction::Discard {
                reason: DiscardReason::IncompleteMetadata,
                ..
            }]
        ));
    }

    #[test]
    fn raid_below_min_difficulty_is_ignored() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings {
            min_raid_difficulty: RaidDifficulty::Heroic,
            ..ActivitySettings::default()
        };
        let actions = handle(
            &mut engine,
            GameFlavor::Retail,
            0,
            encounter_start(2587, "Eranog", 17),
            &config,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn disabled_category_never_begins() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings {
            record_raids: false,
            ..ActivitySettings::default()
        };
        let start = handle(
            &mut engine,
            GameFlavor::Retail,
            0,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        let end = handle(
            &mut engine,
            GameFlavor::Retail,
            60_000,
            encounter_end(2587, 16, true),
            &config,
        );
        assert!(start.is_empty() && end.is_empty());
    }

    #[test]
    fn midnight_season_two_content_is_recordable() {
        let config = ActivitySettings {
            current_raid_only: true,
            ..ActivitySettings::default()
        };

        for (zone_id, map_id) in [
            (2521, 399),
            (2813, 587),
            (2825, 586),
            (2859, 584),
            (2923, 585),
            (2993, 588),
            (1877, 250),
            (1762, 249),
        ] {
            let actions = handle(
                &mut ActivityEngine::new(),
                GameFlavor::Retail,
                0,
                CombatEvent::ChallengeStarted {
                    name: "Midnight Season 2".to_string(),
                    zone_id,
                    map_id,
                    level: 10,
                    affixes: Vec::new(),
                },
                &config,
            );
            assert_eq!(begins(&actions), 1, "map {map_id} was not recordable");
        }

        for encounter_id in [3470, 3445, 3455, 3497, 3420, 3421, 3429, 3492, 3379] {
            let actions = handle(
                &mut ActivityEngine::new(),
                GameFlavor::Retail,
                0,
                encounter_start(encounter_id, "Midnight Season 2", 16),
                &config,
            );
            assert_eq!(
                begins(&actions),
                1,
                "raid encounter {encounter_id} was not recordable"
            );
        }

        for encounter_id in [
            3101, 3102, 3103, 3105, 3207, 3208, 3209, 3199, 3200, 3201, 3202, 3285, 3286, 3287,
            3456, 3457, 3458, 2124, 2125, 2126, 2127, 2139, 2142, 2140, 2143,
        ] {
            assert!(dungeon_encounter_name(encounter_id).is_some());
        }
    }

    #[test]
    fn mythic_plus_completion_builds_segments_and_upgrade() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;

        let actions = handle(
            &mut engine,
            retail.clone(),
            0,
            CombatEvent::ChallengeStarted {
                name: "Algeth'ar Academy".to_string(),
                zone_id: 2526,
                map_id: 402,
                level: 10,
                affixes: vec![9, 152],
            },
            &config,
        );
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        assert_eq!(draft.category, Category::MythicPlus);

        handle(
            &mut engine,
            retail.clone(),
            1_000,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            1_100,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );

        // Boss pull at 60 s closes the opening trash segment.
        let boss = handle(
            &mut engine,
            retail.clone(),
            60_000,
            encounter_start(2562, "Vexamus", 8),
            &config,
        );
        assert_eq!(
            boss,
            vec![ActivityAction::Update {
                id: id.clone(),
                item: TimelineItem::span(TimelineKind::Trash, 0, 60_000, None, None, None).unwrap(),
            }]
        );
        let boss_end = handle(
            &mut engine,
            retail.clone(),
            120_000,
            encounter_end(2562, 8, true),
            &config,
        );
        assert_eq!(
            boss_end,
            vec![ActivityAction::Update {
                id: id.clone(),
                item: TimelineItem::span(
                    TimelineKind::Encounter,
                    60_000,
                    120_000,
                    Some("Vexamus".to_string()),
                    Some(Outcome::Win),
                    None
                )
                .unwrap(),
            }]
        );

        // End 125 s later: the trailing 5 s trash segment is dropped.
        let end = handle(
            &mut engine,
            retail,
            125_000,
            CombatEvent::ChallengeEnded {
                zone_id: 2526,
                success: true,
                duration_ms: 1_400_000,
            },
            &config,
        );
        assert_eq!(
            end,
            vec![ActivityAction::Complete {
                id: id.clone(),
                outcome: Outcome::Complete,
                ended_at_ms: 125_000,
            }]
        );
        let finished = engine.take_finished(&id).unwrap();
        assert_eq!(finished.overrun_ms, 5_000);
        assert_eq!(finished.timeline.len(), 2);
        // 1400 s minus the 90 s Challenger's Peril adjustment beats the 1488 s
        // two-chest timer for map 402.
        assert_eq!(
            finished.details,
            ActivityDetails::Dungeon {
                zone_id: Some(2526),
                dungeon_name: Some("Algeth'ar Academy".to_string()),
                map_id: Some(402),
                keystone_level: Some(10),
                affixes: vec![9, 152],
                upgrade_level: Some(2),
            }
        );
        assert_eq!(
            finished.title.as_deref(),
            Some("Alpha - Algeth'ar Academy +10 (+2)")
        );
    }

    #[test]
    fn mythic_plus_abandon_records_abandoned_outcome() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;
        handle(
            &mut engine,
            retail.clone(),
            0,
            CombatEvent::ChallengeStarted {
                name: "Algeth'ar Academy".to_string(),
                zone_id: 2526,
                map_id: 402,
                level: 10,
                affixes: vec![9],
            },
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            100,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let end = handle(
            &mut engine,
            retail,
            600_000,
            CombatEvent::ChallengeEnded {
                zone_id: 2526,
                success: false,
                duration_ms: 0,
            },
            &config,
        );
        let [
            ActivityAction::Update { .. },
            ActivityAction::Complete { id, outcome, .. },
        ] = end.as_slice()
        else {
            panic!("expected trailing segment update then Complete, got {end:?}");
        };
        assert_eq!(*outcome, Outcome::Abandoned);
        let finished = engine.take_finished(id).unwrap();
        assert_eq!(finished.overrun_ms, 0);
        assert!(matches!(
            finished.details,
            ActivityDetails::Dungeon {
                upgrade_level: Some(0),
                ..
            }
        ));
        assert_eq!(
            finished.title.as_deref(),
            Some("Alpha - Algeth'ar Academy +10 (Abandoned)")
        );
    }

    #[test]
    fn raid_encounter_over_mythic_plus_hands_off() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;
        handle(
            &mut engine,
            retail.clone(),
            0,
            CombatEvent::ChallengeStarted {
                name: "Algeth'ar Academy".to_string(),
                zone_id: 2526,
                map_id: 402,
                level: 10,
                affixes: vec![9],
            },
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            100,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let handoff = handle(
            &mut engine,
            retail,
            60_000,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        let [
            ActivityAction::Update { .. },
            ActivityAction::Abandon { id, reason, .. },
            ActivityAction::Begin { draft, .. },
        ] = handoff.as_slice()
        else {
            panic!("expected Update, Abandon, Begin, got {handoff:?}");
        };
        assert_eq!(*reason, AbandonReason::Superseded);
        assert_eq!(draft.category, Category::Raids);
        assert_eq!(
            engine.take_finished(id).unwrap().outcome,
            Some(Outcome::Abandoned)
        );
    }

    #[test]
    fn retail_arena_win_and_loss() {
        for (winning_team, expected) in [(0u32, Outcome::Win), (1u32, Outcome::Loss)] {
            let mut engine = ActivityEngine::new();
            let config = ActivitySettings::default();
            let retail = GameFlavor::Retail;
            let actions = handle(
                &mut engine,
                retail.clone(),
                0,
                CombatEvent::ArenaStarted {
                    zone_id: 1672,
                    match_type: "2v2".to_string(),
                },
                &config,
            );
            let ActivityAction::Begin { draft, .. } = &actions[0] else {
                panic!("expected Begin");
            };
            let id = draft.id.clone();
            assert_eq!(draft.category, Category::TwoVTwo);
            handle(
                &mut engine,
                retail.clone(),
                100,
                combatant("Player-1-A", Some(0), Some(71)),
                &config,
            );
            handle(
                &mut engine,
                retail.clone(),
                200,
                cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
                &config,
            );
            let end = handle(
                &mut engine,
                retail,
                240_000,
                CombatEvent::ArenaEnded {
                    winning_team_id: winning_team,
                    team_0_mmr: 1_500,
                    team_1_mmr: 1_500,
                },
                &config,
            );
            assert_eq!(
                end,
                vec![ActivityAction::Complete {
                    id: id.clone(),
                    outcome: expected,
                    ended_at_ms: 240_000,
                }]
            );
            let result_text = if expected == Outcome::Win {
                "Win"
            } else {
                "Loss"
            };
            assert_eq!(
                engine.take_finished(&id).unwrap().title.as_deref(),
                Some(format!("Alpha - 2v2 Blade's Edge ({result_text})").as_str())
            );
        }
    }

    #[test]
    fn solo_shuffle_rounds_and_completion() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;
        let start = CombatEvent::ArenaStarted {
            zone_id: 1672,
            match_type: "Rated Solo Shuffle".to_string(),
        };

        let actions = handle(&mut engine, retail.clone(), 0, start.clone(), &config);
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();

        handle(
            &mut engine,
            retail.clone(),
            100,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        // Enemy death decides round one as a win: round span plus death point.
        let decided = handle(
            &mut engine,
            retail.clone(),
            30_000,
            died("Player-9-B", "Foe-Realm", ENEMY_FLAGS),
            &config,
        );
        assert_eq!(
            decided,
            vec![
                ActivityAction::Update {
                    id: id.clone(),
                    item: TimelineItem::span(
                        TimelineKind::Round,
                        0,
                        30_000,
                        Some("Round 1".to_string()),
                        Some(Outcome::Win),
                        None
                    )
                    .unwrap(),
                },
                ActivityAction::Update {
                    id: id.clone(),
                    item: TimelineItem::point(
                        TimelineKind::Death,
                        29_998,
                        Some("Foe".to_string()),
                        Some(Outcome::Win),
                        None
                    ),
                },
            ]
        );
        // A second death in the same round is dropped entirely.
        assert!(
            handle(
                &mut engine,
                retail.clone(),
                31_000,
                died("Player-8-B", "Ally-Realm", FRIENDLY_FLAGS),
                &config,
            )
            .is_empty()
        );

        // Round two: no duplicate Begin, fresh round roster.
        let round_two = handle(&mut engine, retail.clone(), 60_000, start, &config);
        assert_eq!(begins(&round_two), 0);
        handle(
            &mut engine,
            retail.clone(),
            60_100,
            combatant("Player-1-A", Some(1), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail.clone(),
            60_200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );

        let end = handle(
            &mut engine,
            retail,
            90_000,
            CombatEvent::ArenaEnded {
                winning_team_id: 0,
                team_0_mmr: 1_500,
                team_1_mmr: 1_500,
            },
            &config,
        );
        // The undecided round two is emitted as a point, then the game
        // completes as a win.
        let [
            ActivityAction::Update { item, .. },
            ActivityAction::Complete { outcome, .. },
        ] = end.as_slice()
        else {
            panic!("expected round point then Complete, got {end:?}");
        };
        assert_eq!(item.kind(), &TimelineKind::Round);
        assert_eq!(item.label(), Some("Round 2"));
        assert_eq!(*outcome, Outcome::Win);

        let finished = engine.take_finished(&id).unwrap();
        assert_eq!(
            finished.title.as_deref(),
            Some("Alpha - Solo Shuffle Blade's Edge (1-1)")
        );
        let ActivityDetails::SoloRounds {
            rounds_won,
            rounds_played,
            rounds,
            ..
        } = &finished.details
        else {
            panic!("expected SoloRounds");
        };
        assert_eq!(*rounds_won, Some(1));
        assert_eq!(*rounds_played, Some(2));
        assert_eq!(
            rounds
                .iter()
                .map(|round| (round.round, round.outcome))
                .collect::<Vec<_>>(),
            vec![(1, Outcome::Win), (2, Outcome::Loss)]
        );
    }

    #[test]
    fn retail_battleground_estimates_result_from_deaths() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;
        let actions = handle(
            &mut engine,
            retail.clone(),
            0,
            CombatEvent::ZoneChanged {
                zone_id: 30,
                name: "Alterac Valley".to_string(),
                instance_id: 30,
            },
            &config,
        );
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        assert_eq!(draft.category, Category::Battlegrounds);

        handle(
            &mut engine,
            retail.clone(),
            100,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        for (at_ms, guid, name, flags) in [
            (10_000, "Player-2-A", "Beta-Realm", FRIENDLY_FLAGS),
            (11_000, "Player-3-A", "Gamma-Realm", FRIENDLY_FLAGS),
            (12_000, "Player-9-B", "Foe-Realm", ENEMY_FLAGS),
        ] {
            handle(
                &mut engine,
                retail.clone(),
                at_ms,
                died(guid, name, flags),
                &config,
            );
        }
        let end = handle(
            &mut engine,
            retail,
            600_000,
            CombatEvent::ZoneChanged {
                zone_id: 1,
                name: "Durotar".to_string(),
                instance_id: 1,
            },
            &config,
        );
        assert_eq!(
            end,
            vec![ActivityAction::Complete {
                id: id.clone(),
                outcome: Outcome::Loss,
                ended_at_ms: 600_000,
            }]
        );
        let finished = engine.take_finished(&id).unwrap();
        assert!(finished.combatants.is_empty());
        assert_eq!(finished.player.as_ref().map(|p| p.spec_id), Some(Some(71)));
        assert_eq!(
            finished.title.as_deref(),
            Some("Alpha - Alterac Valley (Loss)")
        );
    }

    #[test]
    fn classic_raid_kill() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let classic = GameFlavor::Classic;
        let actions = handle(
            &mut engine,
            classic.clone(),
            0,
            encounter_start(1107, "Anub'Rekhan", 9),
            &config,
        );
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        handle(
            &mut engine,
            classic.clone(),
            100,
            combatant("Player-1-A", None, None),
            &config,
        );
        handle(
            &mut engine,
            classic.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let end = handle(
            &mut engine,
            classic,
            60_000,
            encounter_end(1107, 9, true),
            &config,
        );
        assert!(matches!(
            end.as_slice(),
            [ActivityAction::Complete {
                outcome: Outcome::Win,
                ..
            }]
        ));
        let finished = engine.take_finished(&id).unwrap();
        assert_eq!(finished.flavor, GameFlavor::Classic);
        assert_eq!(finished.player.as_ref().map(|p| p.spec_id), Some(Some(71)));
        assert_eq!(
            finished.title.as_deref(),
            Some("Alpha - Naxxramas, Anub'Rekhan [40] (Kill)")
        );
    }

    #[test]
    fn classic_arena_death_driven_end() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let classic = GameFlavor::Classic;
        let actions = handle(
            &mut engine,
            classic.clone(),
            0,
            CombatEvent::ZoneChanged {
                zone_id: 559,
                name: "Nagrand Arena".to_string(),
                instance_id: 559,
            },
            &config,
        );
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        assert_eq!(draft.category, Category::TwoVTwo);

        handle(
            &mut engine,
            classic.clone(),
            1_000,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        // Enemies become known by interacting with the identified player.
        for (at_ms, guid, name) in [
            (5_000, "Player-9-B", "Foe-Realm"),
            (6_000, "Player-8-B", "Bane-Realm"),
        ] {
            handle(
                &mut engine,
                classic.clone(),
                at_ms,
                cast_at(
                    guid,
                    name,
                    ENEMY_FLAGS,
                    "Player-1-A",
                    "Alpha-Realm",
                    SELF_FLAGS,
                    "Mortal Strike",
                ),
                &config,
            );
        }
        handle(
            &mut engine,
            classic.clone(),
            30_000,
            died("Player-9-B", "Foe-Realm", ENEMY_FLAGS),
            &config,
        );
        let end = handle(
            &mut engine,
            classic,
            40_000,
            died("Player-8-B", "Bane-Realm", ENEMY_FLAGS),
            &config,
        );
        // Second enemy death empties their team: death marker then Complete.
        let [
            ActivityAction::Update { .. },
            ActivityAction::Complete {
                outcome,
                ended_at_ms,
                ..
            },
        ] = end.as_slice()
        else {
            panic!("expected Update then Complete, got {end:?}");
        };
        assert_eq!(*outcome, Outcome::Win);
        assert_eq!(*ended_at_ms, 40_000);
        let finished = engine.take_finished(&id).unwrap();
        // First enemy sighting at 5 s restarted the activity clock.
        assert_eq!(finished.started_at_ms, 5_000);
        assert_eq!(finished.combatants.len(), 3);
    }

    #[test]
    fn classic_challenge_mode_completes() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let classic = GameFlavor::Classic;
        let actions = handle(
            &mut engine,
            classic.clone(),
            0,
            CombatEvent::ChallengeStarted {
                name: "Mogu'shan Palace".to_string(),
                zone_id: 994,
                map_id: 60,
                level: 1,
                affixes: Vec::new(),
            },
            &config,
        );
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        handle(
            &mut engine,
            classic.clone(),
            100,
            combatant("Player-1-A", None, None),
            &config,
        );
        handle(
            &mut engine,
            classic.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let end = handle(
            &mut engine,
            classic,
            900_000,
            CombatEvent::ChallengeEnded {
                zone_id: 994,
                success: false,
                duration_ms: 900_000,
            },
            &config,
        );
        assert!(matches!(
            end.as_slice(),
            [ActivityAction::Complete {
                outcome: Outcome::Complete,
                ..
            }]
        ));
        let finished = engine.take_finished(&id).unwrap();
        assert_eq!(
            finished.details,
            ActivityDetails::Dungeon {
                zone_id: Some(994),
                dungeon_name: Some("Mogu'shan Palace".to_string()),
                map_id: Some(60),
                keystone_level: Some(0),
                affixes: Vec::new(),
                upgrade_level: Some(3),
            }
        );
    }

    #[test]
    fn era_raid_records_classic_flavor() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let era = GameFlavor::Era;
        let actions = handle(
            &mut engine,
            era.clone(),
            0,
            encounter_start(1107, "Anub'Rekhan", 9),
            &config,
        );
        assert_eq!(begins(&actions), 1);
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        assert_eq!(draft.flavor, GameFlavor::Classic);
        handle(
            &mut engine,
            era.clone(),
            100,
            combatant("Player-1-A", None, None),
            &config,
        );
        handle(
            &mut engine,
            era.clone(),
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let end = handle(
            &mut engine,
            era,
            60_000,
            encounter_end(1107, 9, true),
            &config,
        );
        assert!(matches!(
            end.as_slice(),
            [ActivityAction::Complete {
                outcome: Outcome::Win,
                ..
            }]
        ));
        assert_eq!(
            engine.take_finished(&id).unwrap().flavor,
            GameFlavor::Classic
        );
    }

    #[test]
    fn interleaved_flavors_stay_independent() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        handle(
            &mut engine,
            GameFlavor::Retail,
            0,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        // A classic battleground begins its own activity.
        let classic = handle(
            &mut engine,
            GameFlavor::Classic,
            1_000,
            CombatEvent::ZoneChanged {
                zone_id: 30,
                name: "Alterac Valley".to_string(),
                instance_id: 30,
            },
            &config,
        );
        assert_eq!(begins(&classic), 1);
        let unknown = handle(
            &mut engine,
            GameFlavor::Unknown("ptr_x".to_string()),
            2_000,
            encounter_end(2587, 16, true),
            &config,
        );
        assert!(unknown.is_empty());
        // The retail raid is still in flight and only retail can end it.
        assert!(engine.force_end(GameFlavor::Era, 3_000).is_empty());
        let ended = engine.force_end(GameFlavor::Retail, 120_000);
        assert!(matches!(
            ended.as_slice(),
            [ActivityAction::Discard {
                reason: DiscardReason::IncompleteMetadata,
                ..
            }]
        ));
    }

    #[test]
    fn force_end_emits_final_action_once() {
        let mut engine = ActivityEngine::new();
        let config = ActivitySettings::default();
        let retail = GameFlavor::Retail;
        assert!(engine.force_end(GameFlavor::Retail, 0).is_empty());
        let actions = handle(
            &mut engine,
            retail.clone(),
            0,
            encounter_start(2587, "Eranog", 16),
            &config,
        );
        let ActivityAction::Begin { draft, .. } = &actions[0] else {
            panic!("expected Begin");
        };
        let id = draft.id.clone();
        handle(
            &mut engine,
            retail.clone(),
            100,
            combatant("Player-1-A", Some(0), Some(71)),
            &config,
        );
        handle(
            &mut engine,
            retail,
            200,
            cast("Player-1-A", "Alpha-Realm", SELF_FLAGS, "Mortal Strike"),
            &config,
        );
        let ended = engine.force_end(GameFlavor::Retail, 120_000);
        assert_eq!(
            ended,
            vec![ActivityAction::Abandon {
                id: id.clone(),
                ended_at_ms: 120_000,
                reason: AbandonReason::ForceEnd,
            }]
        );
        let finished = engine.take_finished(&id).unwrap();
        assert_eq!(finished.outcome, Some(Outcome::Loss));
        assert_eq!(finished.overrun_ms, 0);
        assert_eq!(finished.duration_ms, Some(120_000));
        assert!(engine.force_end(GameFlavor::Retail, 130_000).is_empty());
    }

    #[test]
    fn md5_matches_reference_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }
}
