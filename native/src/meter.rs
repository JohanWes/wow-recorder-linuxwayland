// SPDX-License-Identifier: GPL-3.0-or-later

//! Damage-meter aggregation for one automatic activity.
//!
//! Rows are keyed by the raw source GUID; pet-to-owner attribution is resolved
//! once at `drain`, so ownership discovered after a pet's first damage still
//! merges retroactively. The ownership map is activity-scoped and dropped with
//! the accumulator. No spell IDs are involved anywhere.

use std::collections::{BTreeMap, HashMap};

use crate::activity::{is_player_controlled_friendly, is_unit_friendly, is_unit_self, relative_ms};
use crate::domain::{MeterActor, MeterData, MeterEntry, MeterFight, MeterMetric, MeterSample};

/// Maximum spell or target rows kept per actor per metric; the remainder folds
/// into a single "Other" row so totals stay exact.
const MAX_BREAKDOWN_ROWS: usize = 16;
/// Persisted playback resolution for meter samples and UI refreshes.
pub const SAMPLE_INTERVAL_MS: u64 = 500;

/// Total hostile-damage silence separating Mythic+ trash pulls. Chain pulls
/// inside this window remain one fight, matching the host staying in combat.
const PULL_GAP_MS: i64 = 6_000;
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
    /// Mythic+ trash is split into host-current and ambient group fights.
    segmented: bool,
    /// Lingering MINE effects must not re-engage a dead host in the same pull.
    host_dead: bool,
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
    last_bucket_end_ms: i64,
    ambient: bool,
    actors: Vec<RawActor>,
    actor_index: HashMap<String, usize>,
    recent: HashMap<(String, String, MeterMetric), RecentRecord>,
}

struct RecentRecord {
    spell_key: (MeterMetric, String),
    target_key: (MeterMetric, String, u8),
    at_ms: i64,
    bucket_end_ms: i64,
    remaining: u64,
    remaining_overheal: u64,
}

impl RawFight {
    fn new(start_ms: i64, label: Option<String>, ambient: bool) -> Self {
        Self {
            label,
            start_ms,
            end_ms: start_ms,
            first_event_ms: None,
            last_event_ms: None,
            last_bucket_end_ms: start_ms,
            ambient,
            actors: Vec::new(),
            actor_index: HashMap::new(),
            recent: HashMap::new(),
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

    fn add_transfer(&mut self, amount: u64, overheal: u64, bucket_end_ms: i64) {
        self.total.amount += amount;
        self.total.overheal += overheal;
        let sample = self.samples.entry(bucket_end_ms).or_default();
        sample.amount += amount;
        sample.overheal += overheal;
    }

    fn subtract_transfer(&mut self, amount: u64, overheal: u64, bucket_end_ms: i64) {
        self.total.amount = self.total.amount.saturating_sub(amount);
        self.total.overheal = self.total.overheal.saturating_sub(overheal);
        if let Some(sample) = self.samples.get_mut(&bucket_end_ms) {
            sample.amount = sample.amount.saturating_sub(amount);
            sample.overheal = sample.overheal.saturating_sub(overheal);
        }
    }
}

impl MeterAccumulator {
    pub(crate) fn new(start_ms: i64, initial_label: Option<String>) -> Self {
        Self {
            started_at_ms: start_ms,
            fights: vec![RawFight::new(start_ms, initial_label, false)],
            owners: HashMap::new(),
            segmented: false,
            host_dead: false,
        }
    }

    /// Retail Mythic+ starts outside the capturing player's combat while still
    /// retaining the group's opening damage for Overall.
    pub(crate) fn trash(start_ms: i64) -> Self {
        Self {
            started_at_ms: start_ms,
            fights: vec![RawFight::new(start_ms, Some("Trash".to_owned()), true)],
            owners: HashMap::new(),
            segmented: true,
            host_dead: false,
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
        self.observe_damage(at_ms, source_flags, dest_flags);
        // Known summons include CONTROL_NPC guardians whose flags are not
        // friendly. Their owner was accepted only from a friendly
        // player-controlled summoner.
        if (!is_player_controlled_friendly(source_flags) && !self.owners.contains_key(source_guid))
            || is_unit_friendly(dest_flags)
        {
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
        self.observe_heal(at_ms, source_flags);
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

    /// Move support-contributed damage or effective healing from the actor who
    /// produced the base event to the supporting player. Support rows follow
    /// their base event in the combat log; a missing base is ignored.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn support(
        &mut self,
        metric: MeterMetric,
        supporter_guid: &str,
        source_guid: &str,
        dest_name: &str,
        marker: u8,
        support_spell: &str,
        amount: u64,
        overheal: u64,
        at_ms: i64,
    ) {
        if supporter_guid.is_empty() || supporter_guid == source_guid {
            return;
        }
        let effective = if metric == MeterMetric::Healing {
            amount.saturating_sub(overheal)
        } else {
            amount
        };
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        let key = (source_guid.to_owned(), dest_name.to_owned(), metric);
        let Some(&source_index) = fight.actor_index.get(source_guid) else {
            return;
        };
        let Some(recent) = fight.recent.get_mut(&key) else {
            return;
        };
        if at_ms.saturating_sub(recent.at_ms) > 1_000 {
            return;
        }
        let transferred = effective.min(recent.remaining);
        let transferred_overheal = overheal.min(recent.remaining_overheal);
        if transferred == 0 && transferred_overheal == 0 {
            return;
        }
        recent.remaining -= transferred;
        recent.remaining_overheal -= transferred_overheal;
        let bucket_end_ms = recent.bucket_end_ms;
        {
            let source = &mut fight.actors[source_index];
            if let Some(entry) = source.spells.get_mut(&recent.spell_key) {
                entry.subtract_transfer(transferred, transferred_overheal, bucket_end_ms);
            }
            if let Some(entry) = source.targets.get_mut(&recent.target_key) {
                entry.subtract_transfer(transferred, transferred_overheal, bucket_end_ms);
            }
        }
        let supporter = fight.actor(supporter_guid, "");
        supporter
            .spells
            .entry((metric, support_spell.to_owned()))
            .or_default()
            .add_transfer(transferred, transferred_overheal, bucket_end_ms);
        supporter
            .targets
            .entry((metric, dest_name.to_owned(), marker))
            .or_default()
            .add_transfer(transferred, transferred_overheal, bucket_end_ms);
    }

    /// Close the open fight and start a fixed Current segment such as a boss
    /// encounter or arena round.
    pub(crate) fn cut(&mut self, at_ms: i64, new_label: String) {
        self.segmented = false;
        self.host_dead = false;
        self.begin_fight(at_ms, Some(new_label), false);
    }

    /// A boss encounter ended; group damage remains in Overall until the host
    /// joins the next trash pull.
    pub(crate) fn cut_to_trash(&mut self, at_ms: i64) {
        self.segmented = true;
        self.host_dead = false;
        self.begin_fight(at_ms, Some("Trash".to_owned()), true);
    }

    /// Host death is an exact player-POV combat boundary for trash.
    pub(crate) fn host_died(&mut self, at_ms: i64) {
        if self.segmented {
            if self.fights.last().is_some_and(|fight| !fight.ambient) {
                self.begin_fight(at_ms, Some("Trash".to_owned()), true);
            }
            self.host_dead = true;
        }
    }

    fn begin_fight(&mut self, at_ms: i64, label: Option<String>, ambient: bool) {
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        fight.end_ms = at_ms.max(fight.last_bucket_end_ms);
        self.fights.push(RawFight::new(at_ms, label, ambient));
    }

    /// Hostile damage is the only reliable combat-state signal in the saved
    /// log. Group-wide silence splits pulls; MINE on either side starts Current.
    fn observe_damage(&mut self, at_ms: i64, source_flags: u64, dest_flags: u64) {
        if !self.segmented || is_unit_friendly(source_flags) == is_unit_friendly(dest_flags) {
            return;
        }
        let (ambient, separated) = {
            let fight = self
                .fights
                .last()
                .expect("the open fight is always present");
            (
                fight.ambient,
                fight
                    .last_event_ms
                    .is_some_and(|last| at_ms.saturating_sub(last) > PULL_GAP_MS),
            )
        };
        // A new pull or an enemy hitting the host proves that stale effects
        // from the death boundary no longer describe the host's combat state.
        if separated || is_unit_self(dest_flags) {
            self.host_dead = false;
        }
        let mine = !self.host_dead && (is_unit_self(source_flags) || is_unit_self(dest_flags));
        if separated || mine && ambient {
            self.begin_fight(at_ms, Some("Trash".to_owned()), !mine);
        }
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        fight.first_event_ms = Some(fight.first_event_ms.map_or(at_ms, |first| first.min(at_ms)));
        fight.last_event_ms = Some(fight.last_event_ms.map_or(at_ms, |last| last.max(at_ms)));
    }

    /// Healing a party member already fighting engages a healer host. Healing
    /// alone never opens or extends a pull, avoiding downtime inflation.
    fn observe_heal(&mut self, at_ms: i64, source_flags: u64) {
        if !self.segmented || self.host_dead || !is_unit_self(source_flags) {
            return;
        }
        let last_event_ms = self
            .fights
            .last()
            .filter(|fight| fight.ambient)
            .and_then(|fight| fight.last_event_ms)
            .filter(|last| at_ms.saturating_sub(*last) <= PULL_GAP_MS);
        if let Some(last_event_ms) = last_event_ms {
            self.begin_fight(at_ms, Some("Trash".to_owned()), false);
            self.fights
                .last_mut()
                .expect("the open fight is always present")
                .last_event_ms = Some(last_event_ms);
        }
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
        fight.end_ms = ended_at_ms.max(fight.last_bucket_end_ms);
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
        let interval_ms = SAMPLE_INTERVAL_MS as i64;
        let elapsed_ms = at_ms.saturating_sub(self.started_at_ms).max(1);
        let bucket_end_ms = self.started_at_ms
            + elapsed_ms.saturating_add(interval_ms - 1) / interval_ms * interval_ms;
        let fight = self
            .fights
            .last_mut()
            .expect("the open fight is always present");
        if !self.segmented {
            fight.first_event_ms =
                Some(fight.first_event_ms.map_or(at_ms, |first| first.min(at_ms)));
            fight.last_event_ms = Some(fight.last_event_ms.map_or(at_ms, |last| last.max(at_ms)));
        }
        fight.last_bucket_end_ms = fight.last_bucket_end_ms.max(bucket_end_ms);
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
        if matches!(metric, MeterMetric::Damage | MeterMetric::Healing) {
            fight.recent.insert(
                (source_guid.to_owned(), dest_name.to_owned(), metric),
                RecentRecord {
                    spell_key: (metric, spell_name.to_owned()),
                    target_key: (metric, dest_name.to_owned(), marker),
                    at_ms,
                    bucket_end_ms,
                    remaining: amount,
                    remaining_overheal: overheal,
                },
            );
        }
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
            ambient: self.ambient,
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
            let name = if resolved != raw.guid {
                owners
                    .get(&raw.guid)
                    .and_then(|owned| owned.name.clone())
                    .or_else(|| names.get(resolved).cloned())
                    .unwrap_or_else(|| raw.name.clone())
            } else if raw.name.is_empty() {
                names.get(resolved).cloned().unwrap_or_default()
            } else {
                raw.name.clone()
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
        .rposition(|fight| !fight.ambient && fight.start_ms <= position_ms)
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
            (projected.hits > 0 || projected.amount > 0 || projected.overheal > 0)
                .then_some(projected)
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
    use crate::activity::{AFFILIATION_MINE, CONTROL_PLAYER, REACTION_FRIENDLY};

    const PLAYER: u64 = CONTROL_PLAYER | REACTION_FRIENDLY;
    const SELF: u64 = PLAYER | AFFILIATION_MINE;
    const ALLY: u64 = PLAYER | 0x2;
    const MOB: u64 = 0x10a48;
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
    fn owned_control_npc_damage_merges_into_player() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.record_owner("Creature-0-GHOUL", "Player-0-A", Some("Death Knight"));
        meter.damage(
            "Creature-0-GHOUL",
            "Lesser Ghoul",
            0xa28,
            "Boss",
            0,
            0,
            "Sweeping Claws",
            75,
            1_000,
        );
        let data = meter.drain(2_000, 0, "Fight", &HashMap::new());
        let actor = &data.fights[0].actors[0];
        assert_eq!(actor.guid, "Player-0-A");
        assert_eq!(actor.name, "Death Knight");
        assert_eq!(actor.spells[0].amount, 75);
    }

    #[test]
    fn support_transfer_preserves_total_and_samples() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.damage(
            "Player-0-A",
            "Dealer",
            PLAYER,
            "Boss",
            0,
            0,
            "Strike",
            100,
            1_200,
        );
        meter.support(
            MeterMetric::Damage,
            "Player-0-B",
            "Player-0-A",
            "Boss",
            0,
            "Ebon Might",
            30,
            0,
            1_200,
        );
        meter.support(
            MeterMetric::Damage,
            "Player-0-B",
            "Player-0-A",
            "Boss",
            0,
            "Prescience",
            10,
            0,
            1_200,
        );
        meter.support(
            MeterMetric::Damage,
            "Player-0-B",
            "Player-0-A",
            "Boss",
            0,
            "Stale support",
            10,
            0,
            2_201,
        );
        let mut names = HashMap::new();
        names.insert("Player-0-B".to_owned(), "Supporter".to_owned());
        let data = meter.drain(3_000, 0, "Fight", &names);
        let fight = &data.fights[0];
        let dealer = fight
            .actors
            .iter()
            .find(|actor| actor.guid == "Player-0-A")
            .unwrap();
        let supporter = fight
            .actors
            .iter()
            .find(|actor| actor.guid == "Player-0-B")
            .unwrap();
        assert_eq!(supporter.name, "Supporter");
        assert_eq!(
            dealer.spells.iter().map(|entry| entry.amount).sum::<u64>(),
            60
        );
        assert_eq!(
            supporter
                .spells
                .iter()
                .map(|entry| entry.amount)
                .sum::<u64>(),
            40
        );
        assert_eq!(
            fight
                .actors
                .iter()
                .flat_map(|actor| &actor.spells)
                .map(|entry| entry.amount)
                .sum::<u64>(),
            100
        );
        let projected = project_current(&data.fights, 2_000).unwrap();
        assert_eq!(
            projected
                .actors
                .iter()
                .flat_map(|actor| &actor.spells)
                .map(|entry| entry.amount)
                .sum::<u64>(),
            100
        );
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
    fn ally_trash_stays_ambient_until_the_host_engages() {
        let mut meter = MeterAccumulator::trash(0);
        meter.damage(
            "Player-0-ALLY",
            "Qeld",
            ALLY,
            "Trash",
            MOB,
            0,
            "Thrash",
            245_000,
            1_000,
        );
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Trash",
            MOB,
            0,
            "Strike",
            100_000,
            31_000,
        );
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Trash",
            MOB,
            0,
            "Strike",
            100_000,
            35_000,
        );

        let data = meter.drain(40_000, 0, "Dungeon", &HashMap::new());
        assert_eq!(data.fights.len(), 2);
        assert!(data.fights[0].ambient);
        assert!(!data.fights[1].ambient);
        assert_eq!(data.fights[1].first_event_ms, Some(31_000));
        assert!(project_current(&data.fights, 30_000).is_none());
        let current = project_current(&data.fights, 40_000).unwrap();
        assert_eq!(current.elapsed_ms, 4_000);
        assert_eq!(current.actors[0].guid, "Player-0-HOST");
        let overall = project_overall(&data.fights, 40_000);
        assert_eq!(
            overall
                .actors
                .iter()
                .flat_map(|actor| &actor.spells)
                .map(|entry| entry.amount)
                .sum::<u64>(),
            445_000
        );
    }

    #[test]
    fn a_pull_gap_freezes_current_and_keeps_later_ally_damage_overall() {
        let mut meter = MeterAccumulator::trash(0);
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Mob",
            MOB,
            0,
            "Hit",
            10,
            1_000,
        );
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Mob",
            MOB,
            0,
            "Hit",
            20,
            3_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            30,
            30_000,
        );

        let data = meter.drain(40_000, 0, "Dungeon", &HashMap::new());
        assert_eq!(data.fights.len(), 3);
        assert!(!data.fights[1].ambient);
        assert!(data.fights[2].ambient);
        let current = project_current(&data.fights, 40_000).unwrap();
        assert_eq!(current.elapsed_ms, 2_000);
        assert_eq!(current.actors[0].guid, "Player-0-HOST");
        assert_eq!(current.actors[0].spells[0].amount, 30);
        let overall = project_overall(&data.fights, 40_000);
        assert_eq!(
            overall
                .actors
                .iter()
                .flat_map(|actor| &actor.spells)
                .map(|entry| entry.amount)
                .sum::<u64>(),
            60
        );
    }

    #[test]
    fn damage_taken_by_the_host_starts_current() {
        let mut meter = MeterAccumulator::trash(0);
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            10,
            1_000,
        );
        meter.damage(
            "Creature-0-MOB",
            "Mob",
            MOB,
            "Host",
            SELF,
            0,
            "Hit",
            50,
            4_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            20,
            5_000,
        );

        let data = meter.drain(10_000, 0, "Dungeon", &HashMap::new());
        assert_eq!(data.fights.len(), 2);
        assert_eq!(data.fights[1].first_event_ms, Some(4_000));
        assert_eq!(
            project_current(&data.fights, 10_000).unwrap().elapsed_ms,
            1_000
        );
    }

    #[test]
    fn healer_joined_pull_still_splits_after_damage_silence() {
        let mut meter = MeterAccumulator::trash(0);
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            10,
            100_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            10,
            110_000,
        );
        meter.heal(
            "Player-0-HOST",
            "Host",
            SELF,
            "Ally",
            0,
            "Heal",
            100,
            0,
            112_000,
        );
        meter.heal(
            "Player-0-HOST",
            "Host",
            SELF,
            "Ally",
            0,
            "Heal",
            100,
            0,
            129_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob 2",
            MOB,
            0,
            "Hit",
            10,
            130_000,
        );
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Mob 2",
            MOB,
            0,
            "Hit",
            10,
            131_000,
        );
        meter.heal(
            "Player-0-HOST",
            "Host",
            SELF,
            "Host",
            0,
            "Heal",
            50,
            0,
            132_000,
        );

        let data = meter.drain(140_000, 0, "Dungeon", &HashMap::new());
        let current = project_current(&data.fights, 140_000).unwrap();
        assert_eq!(current.elapsed_ms, 0);
        assert_eq!(
            current
                .actors
                .iter()
                .flat_map(|actor| &actor.spells)
                .filter(|entry| entry.metric == MeterMetric::Healing)
                .map(|entry| entry.amount)
                .sum::<u64>(),
            50
        );
    }

    #[test]
    fn host_death_returns_trash_to_ambient() {
        let mut meter = MeterAccumulator::trash(0);
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Mob",
            MOB,
            0,
            "Hit",
            10,
            1_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            20,
            3_000,
        );
        meter.host_died(4_000);
        // Lingering host effects keep MINE after death and must not re-promote.
        meter.damage(
            "Player-0-HOST",
            "Host",
            SELF,
            "Mob",
            MOB,
            0,
            "DoT",
            5,
            5_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            30,
            7_000,
        );

        let data = meter.drain(10_000, 0, "Dungeon", &HashMap::new());
        assert_eq!(data.fights.len(), 3);
        assert!(!data.fights[1].ambient);
        assert!(data.fights[2].ambient);
        let current = project_current(&data.fights, 10_000).unwrap();
        assert_eq!(current.elapsed_ms, 2_000);
        assert_eq!(current.actors.len(), 2);
        assert_eq!(
            current
                .actors
                .iter()
                .flat_map(|actor| &actor.spells)
                .map(|entry| entry.amount)
                .sum::<u64>(),
            30
        );
    }

    #[test]
    fn boss_cuts_override_host_state_then_return_to_ambient_trash() {
        let mut meter = MeterAccumulator::trash(0);
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Mob",
            MOB,
            0,
            "Hit",
            10,
            1_000,
        );
        meter.cut(10_000, "Boss".to_owned());
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Boss",
            MOB,
            0,
            "Hit",
            20,
            11_000,
        );
        meter.damage(
            "Player-0-ALLY",
            "Ally",
            ALLY,
            "Boss",
            MOB,
            0,
            "Hit",
            30,
            30_000,
        );
        meter.cut_to_trash(40_000);

        let data = meter.drain(50_000, 0, "Dungeon", &HashMap::new());
        assert_eq!(data.fights.len(), 3);
        assert!(data.fights[0].ambient);
        assert!(!data.fights[1].ambient);
        assert_eq!(data.fights[1].label, "Boss");
        assert_eq!(data.fights[1].active_ms, 19_000);
        assert!(data.fights[2].ambient);
    }

    #[test]
    fn projections_advance_on_half_second_samples() {
        let mut meter = MeterAccumulator::new(0, None);
        meter.damage("Player-0-A", "A", PLAYER, "Boss", MOB, 0, "Hit", 10, 100);
        meter.damage("Player-0-A", "A", PLAYER, "Boss", MOB, 0, "Hit", 20, 600);
        let data = meter.drain(1_500, 0, "Fight", &HashMap::new());

        assert!(
            project_current(&data.fights, 499)
                .unwrap()
                .actors
                .is_empty()
        );
        assert_eq!(
            project_current(&data.fights, 500).unwrap().actors[0].spells[0].amount,
            10
        );
        assert_eq!(
            project_current(&data.fights, 999).unwrap().actors[0].spells[0].amount,
            10
        );
        assert_eq!(
            project_current(&data.fights, 1_000).unwrap().actors[0].spells[0].amount,
            30
        );
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
            vec![(1_000, 10), (1_500, 5)]
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
