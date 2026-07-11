use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub category: VideoCategory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_category: Option<VideoCategory>,
    pub duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipped_at: Option<i64>,
    pub result: bool,
    pub flavour: Flavour,
    #[serde(rename = "zoneID", skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<i64>,
    #[serde(rename = "encounterID", skip_serializing_if = "Option::is_none")]
    pub encounter_id: Option<i64>,
    #[serde(rename = "mapID", skip_serializing_if = "Option::is_none")]
    pub map_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encounter_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique_hash: Option<String>,
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererVideo {
    #[serde(flatten)]
    pub metadata: Metadata,
    pub video_name: String,
    pub mtime: i64,
    pub video_source: String,
    pub is_protected: bool,
    pub cloud: bool,
    #[serde(default)]
    pub multi_pov: Vec<RendererVideo>,
    #[serde(default)]
    pub unique_id: String,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum RecStatus {
    WaitingForWoW,
    Recording,
    InvalidConfig,
    ReadyToRecord,
    FatalError,
    Overrunning,
    Reconfiguring,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum MicStatus {
    NONE,
    MUTED,
    LISTENING,
}

macro_rules! numeric_enum_serde {
    ($type:ty, { $($number:literal => $variant:ident),+ $(,)? }) => {
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_u8(*self as u8)
            }
        }
        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                match u8::deserialize(deserializer)? {
                    $($number => Ok(Self::$variant),)+
                    value => Err(serde::de::Error::custom(format!("invalid {} value: {value}", stringify!($type)))),
                }
            }
        }
    };
}

numeric_enum_serde!(RecStatus, {
    0 => WaitingForWoW,
    1 => Recording,
    2 => InvalidConfig,
    3 => ReadyToRecord,
    4 => FatalError,
    5 => Overrunning,
    6 => Reconfiguring,
});
numeric_enum_serde!(MicStatus, { 0 => NONE, 1 => MUTED, 2 => LISTENING });

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStatus {
    pub category: VideoCategory,
    pub start: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskStatus {
    pub usage: f64,
    pub limit: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SoundAlerts {
    #[serde(rename = "manual-recording-error")]
    ManualRecordingError,
    #[serde(rename = "manual-recording-start")]
    ManualRecordingStart,
    #[serde(rename = "manual-recording-stop")]
    ManualRecordingStop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Flavour {
    Retail,
    Classic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum VideoCategory {
    #[serde(rename = "2v2")]
    TwoVTwo,
    #[serde(rename = "3v3")]
    ThreeVThree,
    #[serde(rename = "5v5")]
    FiveVFive,
    Skirmish,
    #[serde(rename = "Solo Shuffle")]
    SoloShuffle,
    #[serde(rename = "Mythic+")]
    MythicPlus,
    Raids,
    Battlegrounds,
    Clips,
    Manual,
}
