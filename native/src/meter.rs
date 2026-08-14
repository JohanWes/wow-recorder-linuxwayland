// SPDX-License-Identifier: GPL-3.0-or-later

//! Damage-meter aggregation for one automatic activity.
//!
//! Rows are keyed by the raw source GUID; pet-to-owner attribution is resolved
//! once at `drain`, so ownership discovered after a pet's first damage still
//! merges retroactively. The ownership map is activity-scoped and dropped with
//! the accumulator. No spell IDs are involved anywhere.

use std::collections::HashMap;

use crate::activity::{is_player_controlled_friendly, is_unit_friendly, relative_ms};
use crate::domain::{MeterActor, MeterData, MeterEntry, MeterFight, MeterMetric};

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

#[derive(Default)]
struct RawEntry {
    amount: u64,
    hits: u32,
    overheal: u64,
}

impl RawEntry {
    fn add(&mut self, amount: u64, overheal: u64) {
        self.amount += amount;
        self.hits += 1;
        self.overheal += overheal;
    }
}

impl MeterAccumulator {
    pub(crate) fn new(start_ms: i64, initial_label: Option<String>) -> Self {
        Self {
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
            .add(amount, overheal);
        actor
            .targets
            .entry((metric, dest_name.to_owned(), marker))
            .or_default()
            .add(amount, overheal);
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
            active_ms,
            actors: merged_actors(self, owners, names),
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
) -> Vec<MeterActor> {
    let mut actors: Vec<MeterActor> = Vec::new();
    for raw in &fight.actors {
        let resolved = owner_of(&raw.guid, owners);
        if let Some(actor) = actors.iter_mut().find(|actor| actor.guid == resolved) {
            append_entries(actor, raw);
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
                spells: entries(&raw.spells),
                targets: entries_targets(&raw.targets),
            });
        }
    }
    // Bound after merging: rows appended by pets may push the actor over the
    // cap, so the fold runs on the final lists.
    for actor in &mut actors {
        actor.spells = bounded(std::mem::take(&mut actor.spells));
        actor.targets = bounded(std::mem::take(&mut actor.targets));
    }
    actors
}

/// Collect raw spell entries into `MeterEntry`s; spell rows never carry a
/// marker.
fn entries(raw: &HashMap<(MeterMetric, String), RawEntry>) -> Vec<MeterEntry> {
    raw.iter()
        .map(|((metric, key), entry)| MeterEntry {
            metric: *metric,
            key: key.clone(),
            marker: 0,
            amount: entry.amount,
            hits: entry.hits,
            overheal: entry.overheal,
        })
        .collect()
}

/// Collect raw target entries, keyed by `(name, marker)`.
fn entries_targets(raw: &HashMap<(MeterMetric, String, u8), RawEntry>) -> Vec<MeterEntry> {
    raw.iter()
        .map(|((metric, key, marker), entry)| MeterEntry {
            metric: *metric,
            key: key.clone(),
            marker: *marker,
            amount: entry.amount,
            hits: entry.hits,
            overheal: entry.overheal,
        })
        .collect()
}

fn append_entries(actor: &mut MeterActor, raw: &RawActor) {
    for ((metric, key), entry) in &raw.spells {
        append(
            &mut actor.spells,
            *metric,
            key.clone(),
            0,
            entry.amount,
            entry.hits,
            entry.overheal,
        );
    }
    for ((metric, key, marker), entry) in &raw.targets {
        append(
            &mut actor.targets,
            *metric,
            key.clone(),
            *marker,
            entry.amount,
            entry.hits,
            entry.overheal,
        );
    }
}

fn append(
    entries: &mut Vec<MeterEntry>,
    metric: MeterMetric,
    key: String,
    marker: u8,
    amount: u64,
    hits: u32,
    overheal: u64,
) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|entry| entry.metric == metric && entry.key == key && entry.marker == marker)
    {
        existing.amount += amount;
        existing.hits += hits;
        existing.overheal += overheal;
    } else {
        entries.push(MeterEntry {
            metric,
            key,
            marker,
            amount,
            hits,
            overheal,
        });
    }
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
            group.push(MeterEntry {
                metric,
                key: OTHER_KEY.to_owned(),
                marker: 0,
                amount: rest.iter().map(|entry| entry.amount).sum(),
                hits: rest.iter().map(|entry| entry.hits).sum(),
                overheal: rest.iter().map(|entry| entry.overheal).sum(),
            });
        }
        result.extend(group);
    }
    result
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
        assert_eq!(data.fights[1].label, "Boss One");
        assert_eq!(data.fights[1].start_ms, 2_000);
        assert_eq!(data.fights[1].end_ms, 4_000);
        assert_eq!(data.fights[1].active_ms, 0);
        assert_eq!(data.fights[1].actors[0].spells[0].amount, 20);
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
