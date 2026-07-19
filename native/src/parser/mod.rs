// SPDX-License-Identifier: GPL-3.0-or-later

//! Combat-log parsing into small facts consumed by the activity engine.

use crate::domain::GameFlavor;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedEvent {
    pub flavor: GameFlavor,
    pub occurred_at_ms: i64,
    pub event: CombatEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CombatEvent {
    ZoneChanged {
        zone_id: u32,
        name: String,
        instance_id: u32,
    },
    EncounterStarted {
        encounter_id: u32,
        name: String,
        difficulty_id: u32,
        group_size: u32,
        instance_id: u32,
    },
    EncounterEnded {
        encounter_id: u32,
        name: String,
        difficulty_id: u32,
        group_size: u32,
        success: bool,
    },
    ChallengeStarted {
        name: String,
        zone_id: u32,
        map_id: u32,
        level: u32,
        affixes: Vec<u32>,
    },
    ChallengeEnded {
        zone_id: u32,
        success: bool,
        duration_ms: u64,
    },
    ArenaStarted {
        zone_id: u32,
        match_type: String,
    },
    ArenaEnded {
        winning_team_id: u32,
        team_0_mmr: u32,
        team_1_mmr: u32,
    },
    Combatant {
        guid: String,
        team_id: Option<u8>,
        spec_id: Option<u16>,
    },
    PlayerObserved {
        kind: PlayerObservationKind,
        guid: String,
        name: String,
        flags: u64,
        target_guid: String,
        target_name: String,
        target_flags: u64,
        spell_name: String,
    },
    UnitDied {
        guid: String,
        name: String,
        flags: u64,
        unconscious: bool,
    },
    BossHealth {
        name: String,
        current: u64,
        maximum: u64,
    },
    BossCast {
        started: bool,
        source_name: String,
        spell_name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerObservationKind {
    AuraApplied,
    CastSucceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseTimeContext {
    pub year: i32,
    pub utc_offset_minutes: i32,
}

impl ParseTimeContext {
    pub const fn new(year: i32, utc_offset_minutes: i32) -> Self {
        Self {
            year,
            utc_offset_minutes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFailure {
    MalformedTimestamp,
    MalformedRetainedEvent,
}

pub fn parse_line(
    flavor: GameFlavor,
    context: ParseTimeContext,
    line: &str,
) -> Result<Option<ParsedEvent>, ParseFailure> {
    let Some((timestamp, payload)) = line.split_once("  ") else {
        return Ok(None);
    };
    let raw_event_name = payload.split_once(',').map_or(payload, |(name, _)| name);
    if !is_retained(raw_event_name) {
        return Ok(None);
    }
    let occurred_at_ms = parse_timestamp(timestamp, context)?;
    let fields = split_fields(payload).map_err(|()| ParseFailure::MalformedRetainedEvent)?;
    let Some(event_name) = fields.first().map(String::as_str) else {
        return Ok(None);
    };

    let event = match event_name {
        "ZONE_CHANGE" => CombatEvent::ZoneChanged {
            zone_id: number(&fields, 1)?,
            name: text(&fields, 2)?.to_owned(),
            instance_id: number(&fields, 3)?,
        },
        "ENCOUNTER_START" => CombatEvent::EncounterStarted {
            encounter_id: number(&fields, 1)?,
            name: text(&fields, 2)?.to_owned(),
            difficulty_id: number(&fields, 3)?,
            group_size: number(&fields, 4)?,
            instance_id: number(&fields, 5)?,
        },
        "ENCOUNTER_END" => CombatEvent::EncounterEnded {
            encounter_id: number(&fields, 1)?,
            name: text(&fields, 2)?.to_owned(),
            difficulty_id: number(&fields, 3)?,
            group_size: number(&fields, 4)?,
            success: number::<u8>(&fields, 5)? != 0,
        },
        "CHALLENGE_MODE_START" => CombatEvent::ChallengeStarted {
            name: text(&fields, 1)?.to_owned(),
            zone_id: number(&fields, 2)?,
            map_id: number(&fields, 3)?,
            level: number(&fields, 4)?,
            affixes: integer_list(text(&fields, 5)?)?,
        },
        "CHALLENGE_MODE_END" => CombatEvent::ChallengeEnded {
            zone_id: number(&fields, 1)?,
            success: number::<u8>(&fields, 2)? != 0,
            duration_ms: number(&fields, 4)?,
        },
        "ARENA_MATCH_START" => CombatEvent::ArenaStarted {
            zone_id: number(&fields, 1)?,
            match_type: text(&fields, 3)?.to_owned(),
        },
        "ARENA_MATCH_END" => CombatEvent::ArenaEnded {
            winning_team_id: number(&fields, 1)?,
            team_0_mmr: number(&fields, 3)?,
            team_1_mmr: number(&fields, 4)?,
        },
        "COMBATANT_INFO" => {
            let (team_id, spec_id) = if matches!(flavor, GameFlavor::Retail) {
                (
                    Some(number(&fields, 2)?),
                    Some(number(&fields, 25)?).filter(|value| *value != 0),
                )
            } else {
                (optional_number(&fields, 2)?, None)
            };
            CombatEvent::Combatant {
                guid: text(&fields, 1)?.to_owned(),
                team_id,
                spec_id,
            }
        }
        "UNIT_DIED" => CombatEvent::UnitDied {
            guid: text(&fields, 5)?.to_owned(),
            name: text(&fields, 6)?.to_owned(),
            flags: hexadecimal(&fields, 7)?,
            unconscious: optional_number::<u8>(&fields, 9)?.is_some_and(|value| value != 0),
        },
        "SPELL_AURA_APPLIED" | "SPELL_CAST_SUCCESS" => CombatEvent::PlayerObserved {
            kind: if event_name == "SPELL_AURA_APPLIED" {
                PlayerObservationKind::AuraApplied
            } else {
                PlayerObservationKind::CastSucceeded
            },
            guid: text(&fields, 1)?.to_owned(),
            name: text(&fields, 2)?.to_owned(),
            flags: hexadecimal(&fields, 3)?,
            target_guid: text(&fields, 5)?.to_owned(),
            target_name: text(&fields, 6)?.to_owned(),
            target_flags: hexadecimal(&fields, 7)?,
            spell_name: text(&fields, 10)?.to_owned(),
        },
        "SPELL_DAMAGE" => CombatEvent::BossHealth {
            name: text(&fields, 6)?.to_owned(),
            current: number(&fields, 14)?,
            maximum: number(&fields, 15)?,
        },
        "SPELL_CAST_START" => CombatEvent::BossCast {
            started: true,
            source_name: text(&fields, 2)?.to_owned(),
            spell_name: text(&fields, 10)?.to_owned(),
        },
        _ => return Ok(None),
    };

    Ok(Some(ParsedEvent {
        flavor,
        occurred_at_ms,
        event,
    }))
}

pub(crate) fn event_name(line: &str) -> Option<&str> {
    let payload = line.split_once("  ")?.1;
    Some(payload.split_once(',').map_or(payload, |(name, _)| name))
}

fn is_retained(name: &str) -> bool {
    matches!(
        name,
        "ZONE_CHANGE"
            | "ENCOUNTER_START"
            | "ENCOUNTER_END"
            | "CHALLENGE_MODE_START"
            | "CHALLENGE_MODE_END"
            | "ARENA_MATCH_START"
            | "ARENA_MATCH_END"
            | "COMBATANT_INFO"
            | "UNIT_DIED"
            | "SPELL_AURA_APPLIED"
            | "SPELL_CAST_START"
            | "SPELL_CAST_SUCCESS"
            | "SPELL_DAMAGE"
    )
}

fn text(fields: &[String], index: usize) -> Result<&str, ParseFailure> {
    fields
        .get(index)
        .map(String::as_str)
        .ok_or(ParseFailure::MalformedRetainedEvent)
}

fn number<T: std::str::FromStr>(fields: &[String], index: usize) -> Result<T, ParseFailure> {
    text(fields, index)?
        .parse()
        .map_err(|_| ParseFailure::MalformedRetainedEvent)
}

fn optional_number<T: std::str::FromStr>(
    fields: &[String],
    index: usize,
) -> Result<Option<T>, ParseFailure> {
    fields
        .get(index)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse()
                .map_err(|_| ParseFailure::MalformedRetainedEvent)
        })
        .transpose()
}

fn hexadecimal(fields: &[String], index: usize) -> Result<u64, ParseFailure> {
    let value = text(fields, index)?;
    u64::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16)
        .map_err(|_| ParseFailure::MalformedRetainedEvent)
}

fn integer_list(value: &str) -> Result<Vec<u32>, ParseFailure> {
    let inner = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or(ParseFailure::MalformedRetainedEvent)?;
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|value| {
            value
                .parse()
                .map_err(|_| ParseFailure::MalformedRetainedEvent)
        })
        .collect()
}

fn split_fields(payload: &str) -> Result<Vec<String>, ()> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = payload.chars().peekable();
    let mut quoted = false;
    let mut escaped = false;
    let mut nesting = 0_u32;

    while let Some(character) = chars.next() {
        if escaped {
            field.push(character);
            escaped = false;
            continue;
        }
        if quoted {
            match character {
                '\\' => escaped = true,
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(character),
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '[' | '(' => {
                nesting += 1;
                field.push(character);
            }
            ']' | ')' => {
                nesting = nesting.checked_sub(1).ok_or(())?;
                field.push(character);
            }
            ',' if nesting == 0 => {
                fields.push(std::mem::take(&mut field));
            }
            _ => field.push(character),
        }
    }
    if quoted || escaped || nesting != 0 {
        return Err(());
    }
    fields.push(field);
    Ok(fields)
}

fn parse_timestamp(value: &str, context: ParseTimeContext) -> Result<i64, ParseFailure> {
    let (date, time) = value
        .split_once(' ')
        .ok_or(ParseFailure::MalformedTimestamp)?;
    let date_parts = date
        .split('/')
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ParseFailure::MalformedTimestamp)?;
    let (year, month, day) = match date_parts.as_slice() {
        [month, day] => (context.year, *month, *day),
        [month, day, year] => (*year, *month, *day),
        _ => return Err(ParseFailure::MalformedTimestamp),
    };
    if !(1..=9999).contains(&year) {
        return Err(ParseFailure::MalformedTimestamp);
    }
    let (whole_time, fraction) = time
        .split_once('.')
        .ok_or(ParseFailure::MalformedTimestamp)?;
    if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ParseFailure::MalformedTimestamp);
    }
    let time_parts = whole_time
        .split(':')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ParseFailure::MalformedTimestamp)?;
    let [hour, minute, second] = time_parts.as_slice() else {
        return Err(ParseFailure::MalformedTimestamp);
    };
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || !(0..=23).contains(hour)
        || !(0..=59).contains(minute)
        || !(0..=59).contains(second)
    {
        return Err(ParseFailure::MalformedTimestamp);
    }
    let milliseconds = fraction
        .chars()
        .take(3)
        .chain(std::iter::repeat('0'))
        .take(3)
        .collect::<String>()
        .parse::<i64>()
        .map_err(|_| ParseFailure::MalformedTimestamp)?;
    let days = days_from_civil(year, month, day);
    Ok((days * 86_400 + hour * 3_600 + minute * 60 + second
        - i64::from(context.utc_offset_minutes) * 60)
        * 1_000
        + milliseconds)
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

// Howard Hinnant's proleptic Gregorian conversion, expressed directly for std-only parsing.
fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::from(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: ParseTimeContext = ParseTimeContext::new(2026, 120);

    // Each entry maps to both tests/logs/<entry>.txt and tests/src/<entry>.py. The latter is
    // the legacy expected-behavior group; test.py is its shared harness. Keeping paths here,
    // rather than copying raw fixtures, audits complete corpus coverage without retaining names.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum GoldenGroup {
        Retail,
        Classic,
        Era,
    }

    const SOURCE_BEHAVIOR_GROUPS: &[(&str, GoldenGroup)] = &[
        ("retail/beloren_boss_hp", GoldenGroup::Retail),
        ("retail/mythic_plus", GoldenGroup::Retail),
        ("retail/mythic_plus_ditch_into_raid", GoldenGroup::Retail),
        ("retail/mythic_plus_drop_go", GoldenGroup::Retail),
        ("retail/mythic_plus_no_boss", GoldenGroup::Retail),
        ("retail/mythic_plus_repair", GoldenGroup::Retail),
        ("retail/raid_reset", GoldenGroup::Retail),
        ("retail/raid_unknown_encounter", GoldenGroup::Retail),
        ("retail/raid_wipe", GoldenGroup::Retail),
        ("retail/rated_2v2", GoldenGroup::Retail),
        ("retail/rated_2v2_afk_out", GoldenGroup::Retail),
        ("retail/rated_3v3", GoldenGroup::Retail),
        ("retail/rated_battleground", GoldenGroup::Retail),
        ("retail/rated_solo_shuffle", GoldenGroup::Retail),
        ("retail/skirmish", GoldenGroup::Retail),
        ("retail/wargame_3v3", GoldenGroup::Retail),
        ("retail/zone_changes", GoldenGroup::Retail),
        ("classic/battleground", GoldenGroup::Classic),
        ("classic/mop_challenge_mode", GoldenGroup::Classic),
        ("classic/raid", GoldenGroup::Classic),
        ("classic/rated_2v2", GoldenGroup::Classic),
        ("classic/rated_2v2_double", GoldenGroup::Classic),
        ("classic/rated_2v2_extra_units", GoldenGroup::Classic),
        ("classic/rated_2v2_feign_death", GoldenGroup::Classic),
        ("classic/rated_3v3", GoldenGroup::Classic),
        ("classic/rated_3v3_force_stop", GoldenGroup::Classic),
        ("classic/rated_5v5", GoldenGroup::Classic),
        ("era/raid", GoldenGroup::Era),
    ];

    #[test]
    fn parses_anonymized_retained_variants_in_order() {
        assert_eq!(SOURCE_BEHAVIOR_GROUPS.len(), 28);
        assert_eq!(
            SOURCE_BEHAVIOR_GROUPS
                .iter()
                .filter(|(_, group)| *group == GoldenGroup::Retail)
                .count(),
            17
        );
        assert_eq!(
            SOURCE_BEHAVIOR_GROUPS
                .iter()
                .filter(|(_, group)| *group == GoldenGroup::Classic)
                .count(),
            10
        );
        assert_eq!(SOURCE_BEHAVIOR_GROUPS[27].1, GoldenGroup::Era);
        let fixture = [
            "4/9 19:27:13.200  ZONE_CHANGE,2652,\"The Stonevault\",23",
            "4/9 19:27:14.200  CHALLENGE_MODE_START,\"The Stonevault\",2652,501,10,[158,10,152,9]",
            "4/9 19:27:15.200  ENCOUNTER_START,9999,\"Training Construct\",8,5,2652,1",
            "4/9 19:27:16.200  ENCOUNTER_END,9999,\"Training Construct\",8,5,1",
            "4/9 19:27:17.200  CHALLENGE_MODE_END,2652,1,3,123456,0,0",
            "4/9 19:27:18.200  ARENA_MATCH_START,1134,34,\"Rated, Solo Shuffle\",0",
            "4/9 19:27:19.200  ARENA_MATCH_END,1,8,1600,1700",
            "4/9 19:27:20.200  UNIT_DIED,0,nil,0x0,0x0,Player-0-AAAA,\"Player One\",0x511,0x0,0",
            "4/9 19:27:21.200  COMBATANT_INFO,Player-0-AAAA,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1234,256,[]",
            "4/9 19:27:22.200  SPELL_AURA_APPLIED,Player-0-AAAA,\"Player One\",0x511,0x0,Player-0-BBBB,\"Player Two\",0x512,0x0,123,\"Aura, Tested\",0x1",
            "4/9 19:27:23.200  SPELL_DAMAGE,Player-0-AAAA,\"Player One\",0x511,0x0,Creature-0-BOSS,\"Training Boss\",0x10a48,0x0,123,Hit,0x1,Creature-0-BOSS,0,400,1000",
            "4/9 19:27:24.200  SPELL_CAST_START,Creature-0-BOSS,\"Training Boss\",0x10a48,0x0,0,nil,0x0,0x0,456,\"Rebirth\",0x1",
        ];
        let events = fixture
            .iter()
            .map(|line| {
                parse_line(GameFlavor::Retail, CONTEXT, line)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let golden = vec![
            CombatEvent::ZoneChanged {
                zone_id: 2652,
                name: "The Stonevault".into(),
                instance_id: 23,
            },
            CombatEvent::ChallengeStarted {
                name: "The Stonevault".into(),
                zone_id: 2652,
                map_id: 501,
                level: 10,
                affixes: vec![158, 10, 152, 9],
            },
            CombatEvent::EncounterStarted {
                encounter_id: 9999,
                name: "Training Construct".into(),
                difficulty_id: 8,
                group_size: 5,
                instance_id: 2652,
            },
            CombatEvent::EncounterEnded {
                encounter_id: 9999,
                name: "Training Construct".into(),
                difficulty_id: 8,
                group_size: 5,
                success: true,
            },
            CombatEvent::ChallengeEnded {
                zone_id: 2652,
                success: true,
                duration_ms: 123_456,
            },
            CombatEvent::ArenaStarted {
                zone_id: 1134,
                match_type: "Rated, Solo Shuffle".into(),
            },
            CombatEvent::ArenaEnded {
                winning_team_id: 1,
                team_0_mmr: 1600,
                team_1_mmr: 1700,
            },
            CombatEvent::UnitDied {
                guid: "Player-0-AAAA".into(),
                name: "Player One".into(),
                flags: 0x511,
                unconscious: false,
            },
            CombatEvent::Combatant {
                guid: "Player-0-AAAA".into(),
                team_id: Some(1),
                spec_id: Some(256),
            },
            CombatEvent::PlayerObserved {
                kind: PlayerObservationKind::AuraApplied,
                guid: "Player-0-AAAA".into(),
                name: "Player One".into(),
                flags: 0x511,
                target_guid: "Player-0-BBBB".into(),
                target_name: "Player Two".into(),
                target_flags: 0x512,
                spell_name: "Aura, Tested".into(),
            },
            CombatEvent::BossHealth {
                name: "Training Boss".into(),
                current: 400,
                maximum: 1000,
            },
            CombatEvent::BossCast {
                started: true,
                source_name: "Training Boss".into(),
                spell_name: "Rebirth".into(),
            },
        ];
        assert_eq!(
            events.iter().map(|event| &event.event).collect::<Vec<_>>(),
            golden.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.occurred_at_ms)
                .collect::<Vec<_>>(),
            (0..fixture.len())
                .map(|index| 1_775_755_633_200 + index as i64 * 1_000)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn classic_shapes_allow_missing_combatant_details_and_short_deaths() {
        let fixture = [
            "10/16 12:44:44.182  ZONE_CHANGE,562,\"Blade's Edge Arena\",0",
            "10/16 12:44:45.182  COMBATANT_INFO,Player-0-CLASSIC",
            "10/16 12:44:46.182  SPELL_CAST_SUCCESS,Player-0-CLASSIC,\"Fighter-One\",0x511,0x0,Player-0-RIVAL,\"Fighter-Two\",0x548,0x0,123,\"Mortal Strike\",0x1",
            "10/16 12:44:47.182  UNIT_DIED,0,nil,0x0,0x0,Player-0-RIVAL,\"Fighter Two\",0x548,0x0",
            "10/16 12:44:48.182  ZONE_CHANGE,571,\"Dalaran\",0",
        ];
        let events = fixture
            .iter()
            .map(|line| {
                parse_line(GameFlavor::Classic, CONTEXT, line)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let golden = vec![
            CombatEvent::ZoneChanged {
                zone_id: 562,
                name: "Blade's Edge Arena".into(),
                instance_id: 0,
            },
            CombatEvent::Combatant {
                guid: "Player-0-CLASSIC".into(),
                team_id: None,
                spec_id: None,
            },
            CombatEvent::PlayerObserved {
                kind: PlayerObservationKind::CastSucceeded,
                guid: "Player-0-CLASSIC".into(),
                name: "Fighter-One".into(),
                flags: 0x511,
                target_guid: "Player-0-RIVAL".into(),
                target_name: "Fighter-Two".into(),
                target_flags: 0x548,
                spell_name: "Mortal Strike".into(),
            },
            CombatEvent::UnitDied {
                guid: "Player-0-RIVAL".into(),
                name: "Fighter Two".into(),
                flags: 0x548,
                unconscious: false,
            },
            CombatEvent::ZoneChanged {
                zone_id: 571,
                name: "Dalaran".into(),
                instance_id: 0,
            },
        ];
        let golden = golden
            .into_iter()
            .enumerate()
            .map(|(index, event)| ParsedEvent {
                flavor: GameFlavor::Classic,
                occurred_at_ms: 1_792_147_484_182 + index as i64 * 1_000,
                event,
            })
            .collect::<Vec<_>>();
        assert_eq!(events, golden);
    }

    #[test]
    fn era_family_shapes_parse_explicit_year_and_encounter_sequence() {
        let fixture = [
            "3/24/2026 19:54:49.5171  ENCOUNTER_START,2940,\"Clockwork Keeper\",198,10,90,2",
            "3/24/2026 19:54:49.5171  COMBATANT_INFO,Player-0-ERA,0",
            "3/24/2026 19:54:50.5171  SPELL_CAST_SUCCESS,Player-0-ERA,\"Raider-One\",0x511,0x0,Creature-0-BOSS,\"Clockwork Keeper\",0x10a48,0x0,321,\"Storm Strike\",0x1",
            "3/24/2026 19:54:51.5171  UNIT_DIED,0,nil,0x0,0x0,Player-0-ERA,\"Raider One\",0x511,0x0,0",
            "3/24/2026 19:54:52.5171  ENCOUNTER_END,2940,\"Clockwork Keeper\",198,10,1",
        ];
        let era_context = ParseTimeContext::new(1999, -300);
        let events = fixture
            .iter()
            .map(|line| {
                parse_line(GameFlavor::Classic, era_context, line)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let golden = vec![
            CombatEvent::EncounterStarted {
                encounter_id: 2940,
                name: "Clockwork Keeper".into(),
                difficulty_id: 198,
                group_size: 10,
                instance_id: 90,
            },
            CombatEvent::Combatant {
                guid: "Player-0-ERA".into(),
                team_id: Some(0),
                spec_id: None,
            },
            CombatEvent::PlayerObserved {
                kind: PlayerObservationKind::CastSucceeded,
                guid: "Player-0-ERA".into(),
                name: "Raider-One".into(),
                flags: 0x511,
                target_guid: "Creature-0-BOSS".into(),
                target_name: "Clockwork Keeper".into(),
                target_flags: 0x10a48,
                spell_name: "Storm Strike".into(),
            },
            CombatEvent::UnitDied {
                guid: "Player-0-ERA".into(),
                name: "Raider One".into(),
                flags: 0x511,
                unconscious: false,
            },
            CombatEvent::EncounterEnded {
                encounter_id: 2940,
                name: "Clockwork Keeper".into(),
                difficulty_id: 198,
                group_size: 10,
                success: true,
            },
        ];
        let timestamps = [
            1_774_400_089_517,
            1_774_400_089_517,
            1_774_400_090_517,
            1_774_400_091_517,
            1_774_400_092_517,
        ];
        let golden = golden
            .into_iter()
            .zip(timestamps)
            .map(|(event, occurred_at_ms)| ParsedEvent {
                flavor: GameFlavor::Classic,
                occurred_at_ms,
                event,
            })
            .collect::<Vec<_>>();
        assert_eq!(events, golden);
    }

    #[test]
    fn required_retail_combatant_fields_and_optional_death_flag_are_strict() {
        let prefix = "4/9 19:27:13.200  ";
        assert_eq!(
            parse_line(
                GameFlavor::Retail,
                CONTEXT,
                &format!("{prefix}COMBATANT_INFO,Player-0-A,not-a-team")
            ),
            Err(ParseFailure::MalformedRetainedEvent)
        );
        let mut combatant = vec!["0"; 26];
        combatant[0] = "COMBATANT_INFO";
        combatant[1] = "Player-0-A";
        combatant[2] = "1";
        combatant[25] = "not-a-spec";
        assert_eq!(
            parse_line(
                GameFlavor::Retail,
                CONTEXT,
                &format!("{prefix}{}", combatant.join(","))
            ),
            Err(ParseFailure::MalformedRetainedEvent)
        );
        assert!(matches!(
            parse_line(
                GameFlavor::Classic,
                CONTEXT,
                &format!("{prefix}COMBATANT_INFO,Player-0-A")
            )
            .unwrap()
            .unwrap()
            .event,
            CombatEvent::Combatant {
                team_id: None,
                spec_id: None,
                ..
            }
        ));
        assert!(matches!(
            parse_line(
                GameFlavor::Classic,
                CONTEXT,
                &format!("{prefix}UNIT_DIED,0,nil,0x0,0x0,Player-0-A,Name,0x511,0x0")
            )
            .unwrap()
            .unwrap()
            .event,
            CombatEvent::UnitDied {
                unconscious: false,
                ..
            }
        ));
        assert_eq!(
            parse_line(
                GameFlavor::Classic,
                CONTEXT,
                &format!("{prefix}UNIT_DIED,0,nil,0x0,0x0,Player-0-A,Name,0x511,0x0,invalid")
            ),
            Err(ParseFailure::MalformedRetainedEvent)
        );
    }

    #[test]
    fn timestamp_with_year_and_four_fraction_digits_is_deterministic() {
        let event = parse_line(
            GameFlavor::Classic,
            ParseTimeContext::new(1999, 0),
            "7/30/2025 23:35:05.5863  ZONE_CHANGE,1007,\"Scholomance\",0",
        )
        .unwrap()
        .unwrap();
        assert_eq!(event.occurred_at_ms, 1_753_918_505_586);
    }

    #[test]
    fn timestamp_rejects_invalid_calendar_dates_and_leap_seconds() {
        for timestamp in [
            "2/29/2025 12:00:00.000",
            "4/31/2026 12:00:00.000",
            "13/1/2026 12:00:00.000",
            "2/28/2026 12:00:60.000",
            "2/28/2026 12:00:00",
            "2/28/2026 12:00:00.",
            "2/28/2026 12:00:00.123garbage",
            "2/28/0 12:00:00.000",
            "2/28/10000 12:00:00.000",
            "2/28/999999999999999999999 12:00:00.000",
        ] {
            assert_eq!(
                parse_line(
                    GameFlavor::Retail,
                    CONTEXT,
                    &format!("{timestamp}  ZONE_CHANGE,1,Zone,0")
                ),
                Err(ParseFailure::MalformedTimestamp)
            );
        }
        assert!(
            parse_line(
                GameFlavor::Retail,
                CONTEXT,
                "2/29/2024 12:00:59.000  ZONE_CHANGE,1,Zone,0"
            )
            .is_ok()
        );
        assert_eq!(
            parse_line(
                GameFlavor::Retail,
                ParseTimeContext::new(i32::MAX, 0),
                "2/28 12:00:00.000  ZONE_CHANGE,1,Zone,0"
            ),
            Err(ParseFailure::MalformedTimestamp)
        );
    }

    #[test]
    fn quoted_and_nested_commas_do_not_split_top_level_fields() {
        let fields = split_fields("EVENT,\"one, two\",[1,(2,3)],\"a\\\"b\",\"c\"\"d\"").unwrap();
        assert_eq!(fields, ["EVENT", "one, two", "[1,(2,3)]", "a\"b", "c\"d"]);
    }

    #[test]
    fn unknown_event_is_ignored() {
        assert_eq!(
            parse_line(
                GameFlavor::Retail,
                CONTEXT,
                "4/9 19:27:13.200  SPELL_HEAL,irrelevant"
            ),
            Ok(None)
        );
        assert_eq!(
            parse_line(
                GameFlavor::Retail,
                CONTEXT,
                "not a timestamp  SPELL_HEAL,\"unterminated"
            ),
            Ok(None)
        );
        assert_eq!(
            parse_line(GameFlavor::Retail, CONTEXT, "irrelevant"),
            Ok(None)
        );
    }
}
