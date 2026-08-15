// SPDX-License-Identifier: GPL-3.0-or-later

//! Damage-meter aggregation for one automatic activity.
//!
//! Rows are keyed by the raw source GUID; pet-to-owner attribution is resolved
//! once at `drain`, so ownership discovered after a pet's first damage still
//! merges retroactively. The ownership map is activity-scoped and dropped with
//! the accumulator. No spell IDs are involved anywhere.

use std::collections::{BTreeMap, HashMap};

use crate::activity::{is_player_controlled_friendly, is_unit_friendly, relative_ms};
use crate::domain::{MeterActor, MeterData, MeterEntry, MeterFight, MeterMetric, MeterSample};

/// Maximum spell or target rows kept per actor per metric; the remainder folds
/// into a single "Other" row so totals stay exact.
const MAX_BREAKDOWN_ROWS: usize = 16;
const OTHER_KEY: &str = "Other";
const METRICS: [MeterMetric; 4] = [
    MeterMetric::Damage,
    MeterMetric::Healing,
    MeterMetric::Interrupts,
    MeterMetric::Dispels,
];

pub(crate) struct MeterAccumulator {
    started_at_ms: i64,
    fights: Vec<RawFight>,
    owners: HashMap<String, OwnedBy>,
}

struct OwnedBy {
    guid: String,
    /// Owner display name when the ownership signal carries one (summons).
    name: Option<String>,
}

struct RawFight {
    label: Option<String>,
    start_ms: i64,
    end_ms: i64,
    first_event_ms: Option<i64>,
    last_event_ms: Option<i64>,
    actors: Vec<RawActor>,
    actor_index: HashMap<String, usize>,
}

impl RawFight {
    fn new(start_ms: i64, label: Option<String>) -> Self {
        Self {
            label,
            start_ms,
            end_ms: start_ms,
            first_event_ms: None,
            last_event_ms: None,
            actors: Vec::new(),
            actor_index: HashMap::new(),
        }
    }

    fn actor(&mut self, guid: &str, name: &str) -> &mut RawActor {
        if let Some(position) = self.actor_index.get(guid) {
            return &mut self.actors[*position];
        }
        self.actor_index.insert(guid.to_owned(), self.actors.len());
        self.actors.push(RawActor {
            guid: guid.to_owned(),
            name: name.to_owned(),
            spells: HashMap::new(),
            targets: HashMap::new(),
        });
        self.actors.last_mut().expect("just pushed")
    }
}

struct RawActor {
    guid: String,
    name: String,
    spells: HashMap<(MeterMetric, String), RawEntry>,
    targets: HashMap<(MeterMetric, String, u8), RawEntry>,
}

#[derive(Clone, Default)]
struct RawValue {
    amount: u64,
    hits: u32,
    overheal: u64,
}

#[derive(Default)]
struct RawEntry {
    total: RawValue,
    samples: BTreeMap<i64, RawValue>,
}

impl RawEntry {
    fn add(&mut self, amount: u64, overheal: u64, bucket_end_ms: i64) {
        for value in [
            &mut self.total,
            self.samples.entry(bucket_end_ms).or_default(),
        ] {
            value.amount += amount;
            value.hits += 1;
            value.overheal += overheal;
        }
    }
}

impl MeterAccumulator {
    pub(crate) fn new(start_ms: i64, initial_label: Option<String>) -> Self {
        Self {
            started_at_ms: start_ms,
            fights: vec![RawFight::new(start_ms, initial_label)],
            owners: HashMap::new(),
        }
    }

    /// Record pet-to-owner attribution. `owner_name` is carried when the
    /// signal names it (summons); otherwise the owner is named at drain from
    /// the combatant map.
    pub(crate) fn record_owner(
        &mut self,
        pet_guid: &str,
        owner_guid: &str,
        owner_name: Option<&str>,
    ) {
        if pet_guid.is_empty() || owner_guid.is_empty() || pet_guid == owner_guid {
            return;
        }
        self.owners.insert(
            pet_guid.to_owned(),
            OwnedBy {
                guid: owner_guid.to_owned(),
                name: owner_name.map(str::to_owned),
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn damage(
        &mut self,
        source_guid: &str,
        source_name: &str,
        source_flags: u64,
        dest_name: &str,
        dest_flags: u64,
        marker: u8,
        spell_name: &str,
        amount: u64,
        at_ms: i64,
    ) {
        // Damage Done accepts friendly player-controlled sources (players,
        // pets, guardians) against hostile destinations; friendly fire is
        // excluded.
        if !is_player_controlled_friendly(source_flags) || is_unit_friendly(dest_flags) {
            return;
        }
        self.record(
            source_guid,
            source_name,
            MeterMetric::Damage,
            spell_name,
            dest_name,
            marker,
            amount,
            0,
            at_ms,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn heal(
        &mut self,
        source_guid: &str,
        source_name: &str,
        source_flags: u64,
        dest_name: &str,
        marker: u8,
        spell_name: &str,
        amount: u64,
        overheal: u64,
        at_ms: i64,
    ) {
        if !is_player_controlled_friendly(source_flags) {
            return;
        }
        // Healing Done is effective healing; overheal is retained separately.
        self.record(
            source_guid,
            source_name,
            MeterMetric::Healing,
            spell_name,
            dest_name,
            marker,
            amount.saturating_sub(overheal),
            overheal,
            at_ms,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn interrupt(
        &mut self,
        source_guid: &str,
        source_name: &str,
        source_flags: u64,
        dest_name: &str,
        marker: u8,
        spell_name: &str,
        at_ms: i64,
    ) {
        if !is_player_controlled_friendly(source_flags) {
            return;
        }
        self.record(
            source_guid,
            source_name,
            MeterMetric::Interrupts,
            spell_name,
            dest_name,
            marker,
            1,
            0,
            at_ms,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispel(
        &mut self,
        source_guid: &str,
        source_name: &str,
        source_flags: u64,
        dest_name: &str,
        marker: u8,
        spell_name: &str,
        at_ms: i64,
    ) {
        if !is_player_controlled_friendly(source_flags) {
            return;
        }
        self.record(
            source_guid,
            source_name,
            MeterMetric::Dispels,
            spell_name,
            dest_name,
            marker,
            1,
            0,
            at_ms,
        );
    }

    /// Close the open fight and start a new one labelled for the next segment.
    pub(crate) fn cut(&mut self, at_ms: i64, new_label: String) {
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        fight.end_ms = at_ms;
        self.fights.push(RawFight::new(at_ms, Some(new_label)));
    }

    /// Close the open fight at the activity end, resolve ownership, bound the
    /// breakdown rows, and produce the persisted shape. `names` maps GUIDs to
    /// combatant names for owners that never appear as sources themselves.
    pub(crate) fn drain(
        mut self,
        ended_at_ms: i64,
        started_at_ms: i64,
        fallback_label: &str,
        names: &HashMap<String, String>,
    ) -> MeterData {
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        fight.end_ms = ended_at_ms;
        MeterData {
            fights: self
                .fights
                .iter()
                .map(|fight| fight.finish(started_at_ms, fallback_label, &self.owners, names))
                .collect(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        source_guid: &str,
        source_name: &str,
        metric: MeterMetric,
        spell_name: &str,
        dest_name: &str,
        marker: u8,
        amount: u64,
        overheal: u64,
        at_ms: i64,
    ) {
        let elapsed_ms = at_ms.saturating_sub(self.started_at_ms).max(1);
        let bucket_end_ms = self.started_at_ms + elapsed_ms.saturating_add(999) / 1_000 * 1_000;
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        fight.first_event_ms = Some(fight.first_event_ms.map_or(at_ms, |first| first.min(at_ms)));
        fight.last_event_ms = Some(fight.last_event_ms.map_or(at_ms, |last| last.max(at_ms)));
        let actor = fight.actor(source_guid, source_name);
        actor
            .spells
            .entry((metric, spell_name.to_owned()))
            .or_default()
            .add(amount, overheal, bucket_end_ms);
        actor
            .targets
            .entry((metric, dest_name.to_owned(), marker))
            .or_default()
            .add(amount, overheal, bucket_end_ms);
    }
}

impl RawFight {
    fn finish(
        &self,
        started_at_ms: i64,
        fallback_label: &str,
        owners: &HashMap<String, OwnedBy>,
        names: &HashMap<String, String>,
    ) -> MeterFight {
        let start = relative_ms(started_at_ms, self.start_ms);
        let end = relative_ms(started_at_ms, self.end_ms).max(start);
        let active_ms = match (self.first_event_ms, self.last_event_ms) {
            (Some(first), Some(last)) => (last - first).max(0) as u64,
            _ => 0,
        };
        MeterFight {
            label: self
                .label
                .clone()
                .unwrap_or_else(|| fallback_label.to_owned()),
            start_ms: start,
            end_ms: end,
            first_event_ms: self
                .first_event_ms
                .map(|at_ms| relative_ms(started_at_ms, at_ms)),
            active_ms,
            actors: merged_actors(self, owners, names, started_at_ms),
        }
    }
}

/// Follow the ownership chain (pet of a pet) to the top-level owner.
fn owner_of<'a>(guid: &'a str, owners: &'a HashMap<String, OwnedBy>) -> &'a str {
    let mut current = guid;
    for _ in 0..owners.len() {
        match owners.get(current) {
            Some(owned) if owned.guid != current => current = &owned.guid,
            _ => return current,
        }
    }
    current
}

/// Resolve raw rows to owners and merge, in first-appearance order. Unknown
/// pets (no ownership on record) stay their own rows.
fn merged_actors(
    fight: &RawFight,
    owners: &HashMap<String, OwnedBy>,
    names: &HashMap<String, String>,
    started_at_ms: i64,
) -> Vec<MeterActor> {
    let mut actors: Vec<MeterActor> = Vec::new();
    for raw in &fight.actors {
        let resolved = owner_of(&raw.guid, owners);
        if let Some(actor) = actors.iter_mut().find(|actor| actor.guid == resolved) {
            append_entries(actor, raw, started_at_ms);
        } else {
            let name = if resolved == raw.guid {
                raw.name.clone()
            } else if let Some(owned) = owners.get(&raw.guid)
                && let Some(name) = &owned.name
            {
                name.clone()
            } else {
                names
                    .get(resolved)
                    .cloned()
                    .unwrap_or_else(|| raw.name.clone())
            };
            actors.push(MeterActor {
                guid: resolved.to_owned(),
                name,
                spells: entries(&raw.spells, started_at_ms),
                targets: entries_targets(&raw.targets, started_at_ms),
            });
        }
    }
    for actor in &mut actors {
        actor.spells = bounded(std::mem::take(&mut actor.spells));
        actor.targets = bounded(std::mem::take(&mut actor.targets));
    }
    actors
}

/// Collect raw spell entries into `MeterEntry`s; spell rows never carry a
/// marker.
fn entries(raw: &HashMap<(MeterMetric, String), RawEntry>, started_at_ms: i64) -> Vec<MeterEntry> {
    raw.iter()
        .map(|((metric, key), entry)| meter_entry(*metric, key.clone(), 0, entry, started_at_ms))
        .collect()
}

/// Collect raw target entries, keyed by `(name, marker)`.
fn entries_targets(
    raw: &HashMap<(MeterMetric, String, u8), RawEntry>,
    started_at_ms: i64,
) -> Vec<MeterEntry> {
    raw.iter()
        .map(|((metric, key, marker), entry)| {
            meter_entry(*metric, key.clone(), *marker, entry, started_at_ms)
        })
        .collect()
}

fn meter_entry(
    metric: MeterMetric,
    key: String,
    marker: u8,
    entry: &RawEntry,
    started_at_ms: i64,
) -> MeterEntry {
    MeterEntry {
        metric,
        key,
        marker,
        amount: entry.total.amount,
        hits: entry.total.hits,
        overheal: entry.total.overheal,
        samples: entry
            .samples
            .iter()
            .map(|(at_ms, value)| MeterSample {
                at_ms: relative_ms(started_at_ms, *at_ms),
                amount: value.amount,
                hits: value.hits,
                overheal: value.overheal,
            })
            .collect(),
    }
}

fn append_entries(actor: &mut MeterActor, raw: &RawActor, started_at_ms: i64) {
    for ((metric, key), entry) in &raw.spells {
        append(
            &mut actor.spells,
            meter_entry(*metric, key.clone(), 0, entry, started_at_ms),
        );
    }
    for ((metric, key, marker), entry) in &raw.targets {
        append(
            &mut actor.targets,
            meter_entry(*metric, key.clone(), *marker, entry, started_at_ms),
        );
    }
}

fn append(entries: &mut Vec<MeterEntry>, entry: MeterEntry) {
    if let Some(existing) = entries.iter_mut().find(|existing| {
        existing.metric == entry.metric
            && existing.key == entry.key
            && existing.marker == entry.marker
    }) {
        existing.amount += entry.amount;
        existing.hits += entry.hits;
        existing.overheal += entry.overheal;
        merge_samples(&mut existing.samples, &entry.samples);
    } else {
        entries.push(entry);
    }
}

fn merge_samples(into: &mut Vec<MeterSample>, from: &[MeterSample]) {
    for sample in from {
        if let Some(existing) = into.iter_mut().find(|item| item.at_ms == sample.at_ms) {
            existing.amount += sample.amount;
            existing.hits += sample.hits;
            existing.overheal += sample.overheal;
        } else {
            into.push(sample.clone());
        }
    }
    into.sort_by_key(|sample| sample.at_ms);
}

/// Bound each metric's rows to `MAX_BREAKDOWN_ROWS` largest contributors,
/// folding the remainder into one "Other" row so totals stay exact.
fn bounded(entries: Vec<MeterEntry>) -> Vec<MeterEntry> {
    let mut result = Vec::new();
    for metric in METRICS {
        let mut group: Vec<MeterEntry> = entries
            .iter()
            .filter(|entry| entry.metric == metric)
            .cloned()
            .collect();
        group.sort_by(|a, b| {
            b.amount
                .cmp(&a.amount)
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| a.marker.cmp(&b.marker))
        });
        // Keep the top MAX_BREAKDOWN_ROWS - 1 rows and fold the remainder
        // into one "Other" row, so the list never exceeds MAX_BREAKDOWN_ROWS
        // rows while totals stay exact.
        if group.len() > MAX_BREAKDOWN_ROWS {
            let rest = group.split_off(MAX_BREAKDOWN_ROWS - 1);
            let mut other = MeterEntry {
                metric,
                key: OTHER_KEY.to_owned(),
                marker: 0,
                amount: 0,
                hits: 0,
                overheal: 0,
                samples: Vec::new(),
            };
            for entry in rest {
                other.amount += entry.amount;
                other.hits += entry.hits;
                other.overheal += entry.overheal;
                merge_samples(&mut other.samples, &entry.samples);
            }
            group.push(other);
        }
        result.extend(group);
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeterProjection {
    pub label: String,
    pub elapsed_ms: u64,
    pub actors: Vec<ProjectedActor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedActor {
    pub guid: String,
    pub name: String,
    pub spells: Vec<ProjectedEntry>,
    pub targets: Vec<ProjectedEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedEntry {
    pub metric: MeterMetric,
    pub key: String,
    pub marker: u8,
    pub amount: u64,
    pub hits: u32,
    pub overheal: u64,
}

pub fn fight_index_at(fights: &[MeterFight], position_ms: u64) -> Option<usize> {
    fights
        .iter()
        .position(|fight| position_ms >= fight.start_ms && position_ms < fight.end_ms)
        .or_else(|| fights.iter().rposition(|fight| fight.end_ms <= position_ms))
        .or((!fights.is_empty()).then_some(0))
}

pub fn project_current(fights: &[MeterFight], position_ms: u64) -> Option<MeterProjection> {
    project_fight(
        fights.get(fight_index_at(fights, position_ms)?)?,
        position_ms,
    )
}

pub fn project_overall(fights: &[MeterFight], position_ms: u64) -> MeterProjection {
    let mut projection = MeterProjection {
        label: String::new(),
        elapsed_ms: 0,
        actors: Vec::new(),
    };
    for fight in fights.iter().filter(|fight| fight.start_ms <= position_ms) {
        let Some(partial) = project_fight(fight, position_ms) else {
            continue;
        };
        projection.elapsed_ms += partial.elapsed_ms;
        for actor in partial.actors {
            if let Some(existing) = projection
                .actors
                .iter_mut()
                .find(|existing| existing.guid == actor.guid)
            {
                merge_projected_entries(&mut existing.spells, actor.spells);
                merge_projected_entries(&mut existing.targets, actor.targets);
            } else {
                projection.actors.push(actor);
            }
        }
    }
    projection
}

pub fn has_untimed_totals(fights: &[MeterFight]) -> bool {
    fights
        .iter()
        .flat_map(|fight| &fight.actors)
        .flat_map(|actor| actor.spells.iter().chain(&actor.targets))
        .any(|entry| entry.amount > 0 && entry.samples.is_empty())
}

fn project_fight(fight: &MeterFight, position_ms: u64) -> Option<MeterProjection> {
    let limit = position_ms.min(fight.end_ms);
    let elapsed_ms = fight.first_event_ms.map_or(0, |first| {
        limit
            .clamp(first, first.saturating_add(fight.active_ms))
            .saturating_sub(first)
    });
    let actors = fight
        .actors
        .iter()
        .filter_map(|actor| {
            let spells = project_entries(&actor.spells, limit);
            let targets = project_entries(&actor.targets, limit);
            (!spells.is_empty() || !targets.is_empty()).then(|| ProjectedActor {
                guid: actor.guid.clone(),
                name: actor.name.clone(),
                spells,
                targets,
            })
        })
        .collect();
    Some(MeterProjection {
        label: fight.label.clone(),
        elapsed_ms,
        actors,
    })
}

fn project_entries(entries: &[MeterEntry], position_ms: u64) -> Vec<ProjectedEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            let mut projected = ProjectedEntry {
                metric: entry.metric,
                key: entry.key.clone(),
                marker: entry.marker,
                amount: 0,
                hits: 0,
                overheal: 0,
            };
            for sample in entry
                .samples
                .iter()
                .take_while(|sample| sample.at_ms <= position_ms)
            {
                projected.amount += sample.amount;
                projected.hits += sample.hits;
                projected.overheal += sample.overheal;
            }
            (projected.hits > 0).then_some(projected)
        })
        .collect()
}

fn merge_projected_entries(into: &mut Vec<ProjectedEntry>, from: Vec<ProjectedEntry>) {
    for entry in from {
        if let Some(existing) = into.iter_mut().find(|existing| {
            existing.metric == entry.metric
                && existing.key == entry.key
                && existing.marker == entry.marker
        }) {
            existing.amount += entry.amount;
            existing.hits += entry.hits;
            existing.overheal += entry.overheal;
        } else {
            into.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{CONTROL_PLAYER, REACTION_FRIENDLY};

    const PLAYER: u64 = CONTROL_PLAYER | REACTION_FRIENDLY;

    #[test]
    fn pet_damage_before_summon_merges_into_owner() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.damage(
            "Pet-0-1", "Imp", PLAYER, "Boss", 0, 0, "Firebolt", 100, 5_000,
        );
        meter.record_owner("Pet-0-1", "Player-0-A", None);
        let mut names = HashMap::new();
        names.insert("Player-0-A".to_owned(), "Warlock".to_owned());
        let data = meter.drain(10_000, 0, "Fight", &names);
        let fight = &data.fights[0];
        assert_eq!(fight.actors.len(), 1);
        assert_eq!(fight.actors[0].guid, "Player-0-A");
        assert_eq!(fight.actors[0].name, "Warlock");
        assert_eq!(fight.actors[0].spells.len(), 1);
        assert_eq!(fight.actors[0].spells[0].key, "Firebolt");
        assert_eq!(fight.actors[0].spells[0].amount, 100);
    }

    #[test]
    fn friendly_fire_damage_is_excluded() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.damage(
            "Player-0-A",
            "A",
            PLAYER,
            "Player-0-B",
            REACTION_FRIENDLY,
            0,
            "Hit",
            50,
            1_000,
        );
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0, "Hit", 80, 2_000);
        let data = meter.drain(3_000, 0, "Fight", &HashMap::new());
        let fight = &data.fights[0];
        assert_eq!(fight.actors.len(), 1);
        assert_eq!(fight.actors[0].spells.len(), 1);
        assert_eq!(fight.actors[0].spells[0].amount, 80);
    }

    #[test]
    fn target_rows_key_on_name_and_marker() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0x80, "Hit", 10, 1_000);
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0, "Hit", 20, 2_000);
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0x80, "Hit", 30, 3_000);
        let data = meter.drain(4_000, 0, "Fight", &HashMap::new());
        let targets = &data.fights[0].actors[0].targets;
        assert_eq!(targets.len(), 2);
        let skull = targets.iter().find(|target| target.marker == 0x80).unwrap();
        assert_eq!(skull.key, "Boss");
        assert_eq!(skull.amount, 40);
        let unmarked = targets.iter().find(|target| target.marker == 0).unwrap();
        assert_eq!(unmarked.key, "Boss");
        assert_eq!(unmarked.amount, 20);
    }

    #[test]
    fn other_folding_preserves_totals() {
        let mut meter = MeterAccumulator::new(0, None);
        let mut total = 0;
        for index in 0..20i64 {
            let amount = (index + 1) as u64;
            total += amount;
            meter.damage(
                "Player-0-A",
                "A",
                PLAYER,
                "Boss",
                0,
                0,
                &format!("Spell {index}"),
                amount,
                1_000 + index,
            );
            meter.damage(
                "Pet-0-1",
                "Imp",
                PLAYER,
                "Boss",
                0,
                0,
                &format!("Pet Spell {index}"),
                amount,
                1_000 + index,
            );
        }
        meter.record_owner("Pet-0-1", "Player-0-A", None);
        let data = meter.drain(5_000, 0, "Fight", &HashMap::new());
        let spells = &data.fights[0].actors[0].spells;
        assert_eq!(spells.len(), MAX_BREAKDOWN_ROWS);
        let sum: u64 = spells.iter().map(|spell| spell.amount).sum();
        assert_eq!(sum, total * 2);
        assert_eq!(spells.last().unwrap().key, "Other");
    }

    #[test]
    fn healing_aggregates_the_effective_amount() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.heal(
            "Player-0-A",
            "A",
            PLAYER,
            "Tank",
            0,
            "Flash Heal",
            1000,
            400,
            1_000,
        );
        let data = meter.drain(2_000, 0, "Fight", &HashMap::new());
        let spells = &data.fights[0].actors[0].spells;
        assert_eq!(spells.len(), 1);
        assert_eq!(spells[0].metric, MeterMetric::Healing);
        assert_eq!(spells[0].amount, 600);
        assert_eq!(spells[0].overheal, 400);
    }

    #[test]
    fn cut_splits_fights_at_segment_boundaries() {
        let mut meter = MeterAccumulator::new(0, Some("Trash".to_owned()));
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0, "Hit", 10, 1_000);
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0, "Hit", 5, 1_500);
        meter.cut(2_000, "Boss One".to_owned());
        meter.damage("Player-0-A", "A", PLAYER, "Boss", 0, 0, "Hit", 20, 3_000);
        let data = meter.drain(4_000, 0, "Fight", &HashMap::new());
        assert_eq!(data.fights.len(), 2);
        assert_eq!(data.fights[0].label, "Trash");
        assert_eq!(data.fights[0].start_ms, 0);
        assert_eq!(data.fights[0].end_ms, 2_000);
        assert_eq!(data.fights[0].active_ms, 500);
        assert_eq!(data.fights[0].actors[0].spells[0].amount, 15);
        assert_eq!(
            data.fights[0].actors[0].spells[0]
                .samples
                .iter()
                .map(|sample| (sample.at_ms, sample.amount))
                .collect::<Vec<_>>(),
            vec![(1_000, 10), (2_000, 5)]
        );
        assert_eq!(data.fights[1].label, "Boss One");
        assert_eq!(data.fights[1].start_ms, 2_000);
        assert_eq!(data.fights[1].end_ms, 4_000);
        assert_eq!(data.fights[1].active_ms, 0);
        assert_eq!(data.fights[1].actors[0].spells[0].amount, 20);
        let current = project_current(&data.fights, 1_000).unwrap();
        assert_eq!(current.actors[0].spells[0].amount, 10);
        let current = project_current(&data.fights, 3_000).unwrap();
        assert_eq!(current.actors[0].spells[0].amount, 20);
        let overall = project_overall(&data.fights, 3_000);
        assert_eq!(overall.actors[0].spells[0].amount, 35);
        assert_eq!(overall.elapsed_ms, 500);
    }

    #[test]
    fn unknown_pets_stay_their_own_row_and_uncontrolled_sources_are_excluded() {
        let mut meter = MeterAccumulator::new(0, None);
        // Hostile NPC source: not player-controlled, excluded entirely.
        meter.damage("Creature-0-X", "Mob", 0, "Boss", 0, 0, "Hit", 999, 1_000);
        // Player-controlled pet without an ownership record stays its own row.
        meter.damage(
            "Pet-0-1", "Imp", PLAYER, "Boss", 0, 0, "Firebolt", 100, 2_000,
        );
        let data = meter.drain(3_000, 0, "Fight", &HashMap::new());
        let actors = &data.fights[0].actors;
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].guid, "Pet-0-1");
        assert_eq!(actors[0].name, "Imp");
    }

    #[test]
    fn unlabelled_fights_take_the_activity_title() {
        let meter = MeterAccumulator::new(0, None);
        let data = meter.drain(5_000, 0, "Alpha - Raid", &HashMap::new());
        assert_eq!(data.fights.len(), 1);
        assert_eq!(data.fights[0].label, "Alpha - Raid");
        assert_eq!(data.fights[0].start_ms, 0);
        assert_eq!(data.fights[0].end_ms, 5_000);
        assert_eq!(data.fights[0].active_ms, 0);
    }
}
