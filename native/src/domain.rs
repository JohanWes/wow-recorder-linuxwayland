// SPDX-License-Identifier: GPL-3.0-or-later

//! GTK-free application domain types.

use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordingId(String);

impl RecordingId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_legacy(value: Option<&str>, relative_media_path: &Path) -> Self {
        if let Some(value) = value.filter(|value| Uuid::parse_str(value).is_ok()) {
            return Self(value.to_owned());
        }

        Self(normalized_relative_path(relative_media_path))
    }

    pub fn with_legacy_duplicate_suffix(&self, sidecar_filename: &Path) -> Self {
        let filename = sidecar_filename
            .file_name()
            .unwrap_or(sidecar_filename.as_os_str());
        Self(format!("{}#{}", self.0, filename.to_string_lossy()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RecordingId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RecordingId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

fn normalized_relative_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy()),
            std::path::Component::ParentDir => {
                if parts.last().is_some_and(|part| part != "..") {
                    parts.pop();
                } else {
                    parts.push("..".into());
                }
            }
            _ => {}
        }
    }
    parts.join("/")
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameFlavor {
    Retail,
    Classic,
    /// Classic Era log source. Only used to tag parsed events and key per-flavour
    /// engine state; Era recordings store `Classic` in their metadata, matching
    /// the legacy `Flavour.Classic` written by `EraLogHandler`.
    Era,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    TwoVTwo,
    ThreeVThree,
    FiveVFive,
    Skirmish,
    SoloShuffle,
    MythicPlus,
    Raids,
    Battlegrounds,
    Manual,
    Clip,
    Unknown(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Win,
    Loss,
    Complete,
    Abandoned,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStorage {
    Ram,
    Disk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathMarkerVisibility {
    None,
    Own,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerVisibility {
    Hidden,
    Visible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "gib", rename_all = "snake_case")]
pub enum StorageLimit {
    Unlimited,
    Gib(NonZeroU64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerSummary {
    pub name: String,
    pub realm: Option<String>,
    pub guid: Option<String>,
    pub class_id: Option<u16>,
    pub spec_id: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatantSummary {
    pub name: Option<String>,
    pub realm: Option<String>,
    pub guid: Option<String>,
    pub region: Option<String>,
    pub class_id: Option<u16>,
    pub spec_id: Option<u16>,
    pub team_id: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaidDifficulty {
    Lfr,
    Normal,
    Heroic,
    Mythic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundSummary {
    pub round: u32,
    pub outcome: Outcome,
    pub start_ms: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivityDetails {
    Raid {
        zone_id: Option<u32>,
        zone_name: Option<String>,
        encounter_id: Option<u32>,
        encounter_name: Option<String>,
        difficulty_id: Option<u32>,
        difficulty: Option<String>,
        pull: Option<u32>,
        boss_percent: Option<u8>,
    },
    Dungeon {
        zone_id: Option<u32>,
        dungeon_name: Option<String>,
        map_id: Option<u32>,
        keystone_level: Option<u32>,
        affixes: Vec<u32>,
        upgrade_level: Option<u8>,
    },
    ArenaOrBattleground {
        map_id: Option<u32>,
        map_name: Option<String>,
        team_mmr: Option<u32>,
    },
    SoloRounds {
        map_id: Option<u32>,
        map_name: Option<String>,
        rounds_won: Option<u8>,
        rounds_played: Option<u8>,
        rounds: Vec<RoundSummary>,
    },
    Clip {
        source_recording: RecordingId,
        source_category: Category,
        source_title: Option<String>,
    },
    Manual,
    UnknownLegacy {
        description: Option<String>,
    },
}

impl ActivityDetails {
    pub fn matches_category(&self, category: &Category) -> bool {
        matches!(
            (category, self),
            (Category::Raids, Self::Raid { .. })
                | (Category::MythicPlus, Self::Dungeon { .. })
                | (
                    Category::TwoVTwo
                        | Category::ThreeVThree
                        | Category::FiveVFive
                        | Category::Skirmish
                        | Category::Battlegrounds,
                    Self::ArenaOrBattleground { .. }
                )
                | (Category::SoloShuffle, Self::SoloRounds { .. })
                | (Category::Clip, Self::Clip { .. })
                | (Category::Manual, Self::Manual)
                | (Category::Unknown(_), Self::UnknownLegacy { .. })
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineKind {
    Death,
    Encounter,
    Trash,
    Round,
    Activity,
    Unknown(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineShape {
    Point,
    Span,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TimelineItem {
    shape: TimelineShape,
    kind: TimelineKind,
    start_ms: u64,
    end_ms: Option<u64>,
    label: Option<String>,
    outcome: Option<Outcome>,
    player_reference: Option<String>,
}

#[derive(Deserialize)]
struct TimelineItemData {
    shape: TimelineShape,
    kind: TimelineKind,
    start_ms: u64,
    end_ms: Option<u64>,
    label: Option<String>,
    outcome: Option<Outcome>,
    player_reference: Option<String>,
}

impl TimelineItem {
    pub fn point(
        kind: TimelineKind,
        start_ms: u64,
        label: Option<String>,
        outcome: Option<Outcome>,
        player_reference: Option<String>,
    ) -> Self {
        Self {
            shape: TimelineShape::Point,
            kind,
            start_ms,
            end_ms: None,
            label,
            outcome,
            player_reference,
        }
    }

    pub fn span(
        kind: TimelineKind,
        start_ms: u64,
        end_ms: u64,
        label: Option<String>,
        outcome: Option<Outcome>,
        player_reference: Option<String>,
    ) -> Result<Self, DomainError> {
        if end_ms < start_ms {
            return Err(DomainError::TimelineEndBeforeStart { start_ms, end_ms });
        }

        Ok(Self {
            shape: TimelineShape::Span,
            kind,
            start_ms,
            end_ms: Some(end_ms),
            label,
            outcome,
            player_reference,
        })
    }

    pub fn shape(&self) -> TimelineShape {
        self.shape
    }

    pub fn kind(&self) -> &TimelineKind {
        &self.kind
    }

    pub fn start_ms(&self) -> u64 {
        self.start_ms
    }

    pub fn end_ms(&self) -> Option<u64> {
        self.end_ms
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    pub fn player_reference(&self) -> Option<&str> {
        self.player_reference.as_deref()
    }

    fn try_from_data(data: TimelineItemData) -> Result<Self, DomainError> {
        match (data.shape, data.end_ms) {
            (TimelineShape::Point, None) => Ok(Self::point(
                data.kind,
                data.start_ms,
                data.label,
                data.outcome,
                data.player_reference,
            )),
            (TimelineShape::Span, Some(end_ms)) => Self::span(
                data.kind,
                data.start_ms,
                end_ms,
                data.label,
                data.outcome,
                data.player_reference,
            ),
            (TimelineShape::Point, Some(_)) => Err(DomainError::PointHasEnd),
            (TimelineShape::Span, None) => Err(DomainError::SpanMissingEnd),
        }
    }
}

impl<'de> Deserialize<'de> for TimelineItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = TimelineItemData::deserialize(deserializer)?;
        Self::try_from_data(data).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFacts {
    pub fps: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<Codec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub id: RecordingId,
    pub media_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub category: Category,
    pub flavor: GameFlavor,
    pub title: String,
    pub start_unix_ms: i64,
    pub duration_ms: u64,
    pub outcome: Outcome,
    pub protected: bool,
    pub tag: Option<String>,
    pub activity_hash: Option<String>,
    pub player: Option<PlayerSummary>,
    pub combatants: Vec<CombatantSummary>,
    pub details: ActivityDetails,
    pub timeline: Vec<TimelineItem>,
    pub media: MediaFacts,
}

impl LibraryEntry {
    pub fn validate(&self) -> Result<(), DomainError> {
        if !self.details.matches_category(&self.category) {
            return Err(DomainError::CategoryDetailsMismatch {
                category: self.category.clone(),
            });
        }

        if let Some(offset_ms) = self.timeline.iter().find_map(|item| {
            (item.start_ms() > self.duration_ms)
                .then_some(item.start_ms())
                .or_else(|| item.end_ms().filter(|end| *end > self.duration_ms))
        }) {
            return Err(DomainError::TimelineOutsideDuration {
                offset_ms,
                duration_ms: self.duration_ms,
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrelatedActivity {
    pub primary: LibraryEntry,
    pub local_pov_ids: Vec<RecordingId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecorderStatus {
    SetupRequired,
    WaitingForWow,
    Reconfiguring,
    Ready,
    Buffering,
    Recording {
        category: Category,
        title: String,
        started_unix_ms: i64,
        manual: bool,
        test: bool,
    },
    Overrunning {
        title: String,
        started_unix_ms: i64,
    },
    Finalizing {
        title: String,
    },
    Fatal {
        problem: Problem,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Finalize,
    Clip,
    KillVideo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkProgress {
    pub kind: WorkKind,
    pub completed: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    OpenSettings,
    ReselectCaptureTarget,
    Retry,
    OpenLogs,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub summary: String,
    pub safe_detail: Option<String>,
    pub occurred_unix_ms: i64,
    pub recovery_action: Option<RecoveryAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    TimelineEndBeforeStart { start_ms: u64, end_ms: u64 },
    PointHasEnd,
    SpanMissingEnd,
    CategoryDetailsMismatch { category: Category },
    TimelineOutsideDuration { offset_ms: u64, duration_ms: u64 },
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimelineEndBeforeStart { start_ms, end_ms } => write!(
                formatter,
                "timeline end {end_ms} ms is before start {start_ms} ms"
            ),
            Self::PointHasEnd => formatter.write_str("timeline point cannot have an end"),
            Self::SpanMissingEnd => formatter.write_str("timeline span requires an end"),
            Self::CategoryDetailsMismatch { category } => {
                write!(
                    formatter,
                    "activity details do not match category {category:?}"
                )
            }
            Self::TimelineOutsideDuration {
                offset_ms,
                duration_ms,
            } => write!(
                formatter,
                "timeline offset {offset_ms} ms exceeds media duration {duration_ms} ms"
            ),
        }
    }
}

impl std::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(category: Category, details: ActivityDetails) -> LibraryEntry {
        LibraryEntry {
            id: RecordingId::from_legacy(None, Path::new("Raids/example.mkv")),
            media_path: PathBuf::from("/recordings/example.mkv"),
            sidecar_path: PathBuf::from("/recordings/example.json"),
            category,
            flavor: GameFlavor::Retail,
            title: "Example".to_owned(),
            start_unix_ms: 1_700_000_000_000,
            duration_ms: 60_000,
            outcome: Outcome::Unknown,
            protected: false,
            tag: None,
            activity_hash: Some("activity".to_owned()),
            player: None,
            combatants: Vec::new(),
            details,
            timeline: Vec::new(),
            media: MediaFacts {
                fps: Some(60),
                width: Some(1920),
                height: Some(1080),
                codec: Some(Codec::H264),
            },
        }
    }

    #[test]
    fn timeline_rejects_invalid_bounds_and_invalid_json_shapes() {
        assert_eq!(
            TimelineItem::span(TimelineKind::Encounter, 200, 100, None, None, None),
            Err(DomainError::TimelineEndBeforeStart {
                start_ms: 200,
                end_ms: 100,
            })
        );

        let invalid_span = r#"{
            "shape":"span","kind":"round","start_ms":10,"end_ms":null,
            "label":null,"outcome":null,"player_reference":null
        }"#;
        assert!(serde_json::from_str::<TimelineItem>(invalid_span).is_err());

        let valid = TimelineItem::span(
            TimelineKind::Round,
            10,
            20,
            Some("Round 1".to_owned()),
            Some(Outcome::Win),
            None,
        )
        .expect("valid span");
        let encoded = serde_json::to_string(&valid).expect("serialize span");
        assert_eq!(
            serde_json::from_str::<TimelineItem>(&encoded).expect("deserialize span"),
            valid
        );
    }

    #[test]
    fn category_and_details_must_match() {
        let raid = ActivityDetails::Raid {
            zone_id: Some(1),
            zone_name: Some("Example Raid".to_owned()),
            encounter_id: Some(2),
            encounter_name: Some("Example Boss".to_owned()),
            difficulty_id: Some(16),
            difficulty: Some("Mythic".to_owned()),
            pull: Some(3),
            boss_percent: Some(42),
        };

        assert!(entry(Category::Raids, raid.clone()).validate().is_ok());
        assert_eq!(
            entry(Category::MythicPlus, raid).validate(),
            Err(DomainError::CategoryDetailsMismatch {
                category: Category::MythicPlus,
            })
        );

        let mut outside = entry(Category::Manual, ActivityDetails::Manual);
        outside.timeline.push(TimelineItem::point(
            TimelineKind::Death,
            60_001,
            None,
            None,
            None,
        ));
        assert_eq!(
            outside.validate(),
            Err(DomainError::TimelineOutsideDuration {
                offset_ms: 60_001,
                duration_ms: 60_000,
            })
        );
    }

    #[test]
    fn retained_categories_have_concrete_detail_variants() {
        let flavors = [
            GameFlavor::Retail,
            GameFlavor::Classic,
            GameFlavor::Unknown("Legacy flavor".to_owned()),
        ];
        for flavor in flavors {
            let encoded = serde_json::to_string(&flavor).expect("serialize flavor");
            assert_eq!(
                serde_json::from_str::<GameFlavor>(&encoded).expect("deserialize flavor"),
                flavor
            );
        }

        let pvp = || ActivityDetails::ArenaOrBattleground {
            map_id: Some(1),
            map_name: Some("Example Map".to_owned()),
            team_mmr: Some(1_800),
        };
        let cases = [
            (Category::TwoVTwo, pvp()),
            (Category::ThreeVThree, pvp()),
            (Category::FiveVFive, pvp()),
            (Category::Skirmish, pvp()),
            (Category::Battlegrounds, pvp()),
            (
                Category::SoloShuffle,
                ActivityDetails::SoloRounds {
                    map_id: Some(2),
                    map_name: Some("Shuffle Map".to_owned()),
                    rounds_won: Some(4),
                    rounds_played: Some(6),
                    rounds: vec![RoundSummary {
                        round: 1,
                        outcome: Outcome::Win,
                        start_ms: 0,
                        duration_ms: Some(30_000),
                    }],
                },
            ),
            (
                Category::MythicPlus,
                ActivityDetails::Dungeon {
                    zone_id: Some(3),
                    dungeon_name: Some("Example Dungeon".to_owned()),
                    map_id: Some(4),
                    keystone_level: Some(12),
                    affixes: vec![9, 10],
                    upgrade_level: Some(2),
                },
            ),
            (
                Category::Raids,
                ActivityDetails::Raid {
                    zone_id: Some(5),
                    zone_name: Some("Example Raid".to_owned()),
                    encounter_id: Some(6),
                    encounter_name: Some("Example Boss".to_owned()),
                    difficulty_id: Some(16),
                    difficulty: Some("Mythic".to_owned()),
                    pull: Some(7),
                    boss_percent: Some(8),
                },
            ),
            (Category::Manual, ActivityDetails::Manual),
            (
                Category::Clip,
                ActivityDetails::Clip {
                    source_recording: RecordingId::from_legacy(None, Path::new("source.mkv")),
                    source_category: Category::Raids,
                    source_title: Some("Example Boss".to_owned()),
                },
            ),
            (
                Category::Unknown("Legacy Category".to_owned()),
                ActivityDetails::UnknownLegacy {
                    description: Some("Legacy details".to_owned()),
                },
            ),
        ];

        for (category, details) in cases {
            assert!(details.matches_category(&category), "{category:?}");
        }
    }

    #[test]
    fn legacy_recording_ids_are_stable_and_uuid_values_are_preserved() {
        assert_eq!(
            Uuid::parse_str(RecordingId::new().as_str())
                .expect("new recording UUID")
                .get_version_num(),
            4
        );
        let legacy_uuid = "6BA7B810-9DAD-11D1-80B4-00C04FD430C8";
        assert_eq!(
            RecordingId::from_legacy(Some(legacy_uuid), Path::new("ignored.mkv")).as_str(),
            legacy_uuid
        );
        let fallback = RecordingId::from_legacy(None, Path::new("./Raids/../pull.mkv"));
        assert_eq!(fallback.as_str(), "pull.mkv");
        assert_eq!(
            fallback
                .with_legacy_duplicate_suffix(Path::new("pull.json"))
                .as_str(),
            "pull.mkv#pull.json"
        );
    }
}
