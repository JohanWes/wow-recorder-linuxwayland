// SPDX-License-Identifier: GPL-3.0-or-later

//! Native configuration persistence and legacy import.

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::domain::{
    Category, Codec, DeathMarkerVisibility, MarkerVisibility, RaidDifficulty, ReplayStorage,
    StorageLimit,
};

pub const CONFIG_VERSION: u32 = 1;
pub const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder";
pub const CONFIG_FILENAME: &str = "config.json";
pub const LEGACY_CONFIG_DIR: &str = "WarcraftRecorder";
pub const LEGACY_CONFIG_FILENAME: &str = "config-v3.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathAuthorization {
    Unset,
    ImportedInactive,
    Authorized,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedPath {
    pub path: PathBuf,
    pub authorization: PathAuthorization,
}

impl AuthorizedPath {
    pub fn unset() -> Self {
        Self {
            path: PathBuf::new(),
            authorization: PathAuthorization::Unset,
        }
    }

    pub fn imported(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let authorization = if path.as_os_str().is_empty() {
            PathAuthorization::Unset
        } else {
            PathAuthorization::ImportedInactive
        };
        Self {
            path,
            authorization,
        }
    }

    pub fn authorized(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            authorization: PathAuthorization::Authorized,
        }
    }

    pub fn is_authorized(&self) -> bool {
        self.authorization == PathAuthorization::Authorized && !self.path.as_os_str().is_empty()
    }
}

impl Default for AuthorizedPath {
    fn default() -> Self {
        Self::unset()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlavorConfig {
    pub enabled: bool,
    pub log_dir: AuthorizedPath,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FlavorSettings {
    pub retail: FlavorConfig,
    pub retail_ptr: FlavorConfig,
    pub classic: FlavorConfig,
    pub classic_ptr: FlavorConfig,
    pub era: FlavorConfig,
}

impl FlavorSettings {
    fn in_field_order(&self) -> [(&'static str, &FlavorConfig); 5] {
        [
            ("flavors.retail", &self.retail),
            ("flavors.retail_ptr", &self.retail_ptr),
            ("flavors.classic", &self.classic),
            ("flavors.classic_ptr", &self.classic_ptr),
            ("flavors.era", &self.era),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySettings {
    pub record_raids: bool,
    pub record_dungeons: bool,
    pub record_two_v_two: bool,
    pub record_three_v_three: bool,
    pub record_five_v_five: bool,
    pub record_skirmish: bool,
    pub record_solo_shuffle: bool,
    pub record_battlegrounds: bool,
    pub record_challenge_modes: bool,
    pub min_keystone_level: u32,
    pub min_raid_difficulty: RaidDifficulty,
    pub min_raid_duration_seconds: i32,
    pub current_raid_only: bool,
    pub raid_overrun_seconds: u32,
    pub dungeon_overrun_seconds: u32,
}

impl Default for ActivitySettings {
    fn default() -> Self {
        Self {
            record_raids: true,
            record_dungeons: true,
            record_two_v_two: true,
            record_three_v_three: true,
            record_five_v_five: true,
            record_skirmish: true,
            record_solo_shuffle: true,
            record_battlegrounds: true,
            record_challenge_modes: true,
            min_keystone_level: 2,
            min_raid_difficulty: RaidDifficulty::Lfr,
            min_raid_duration_seconds: 15,
            current_raid_only: false,
            raid_overrun_seconds: 15,
            dungeon_overrun_seconds: 5,
        }
    }
}

impl ActivitySettings {
    fn any_enabled(&self) -> bool {
        self.record_raids
            || self.record_dungeons
            || self.record_two_v_two
            || self.record_three_v_three
            || self.record_five_v_five
            || self.record_skirmish
            || self.record_solo_shuffle
            || self.record_battlegrounds
            || self.record_challenge_modes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSettings {
    pub recording_dir: AuthorizedPath,
    pub separate_buffer_dir: bool,
    pub buffer_dir: AuthorizedPath,
    pub limit: StorageLimit,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            recording_dir: AuthorizedPath::unset(),
            separate_buffer_dir: false,
            buffer_dir: AuthorizedPath::unset(),
            limit: StorageLimit::Gib(NonZeroU64::new(50).expect("50 is nonzero")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSettings {
    pub fps: u32,
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub replay_buffer_seconds: u32,
    pub extra_lead_in_seconds: u32,
    pub replay_storage: ReplayStorage,
    pub capture_cursor: bool,
    pub audio_output: String,
    pub audio_input: Option<String>,
    pub capture_target_token: Option<String>,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            fps: 60,
            codec: Codec::H264,
            bitrate_kbps: 20_000,
            replay_buffer_seconds: 180,
            extra_lead_in_seconds: 0,
            replay_storage: ReplayStorage::Ram,
            capture_cursor: false,
            audio_output: "default_output".to_owned(),
            audio_input: None,
            capture_target_token: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualSettings {
    pub enabled: bool,
    pub sound: bool,
}

impl Default for ManualSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            sound: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceSettings {
    pub hide_empty_categories: bool,
    pub death_markers: DeathMarkerVisibility,
    pub encounter_markers: MarkerVisibility,
    pub round_markers: MarkerVisibility,
    pub selected_category: Category,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub start_minimized: bool,
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        Self {
            hide_empty_categories: false,
            death_markers: DeathMarkerVisibility::Own,
            encounter_markers: MarkerVisibility::Visible,
            round_markers: MarkerVisibility::Visible,
            selected_category: Category::ThreeVThree,
            minimize_to_tray: true,
            close_to_tray: true,
            start_minimized: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub flavors: FlavorSettings,
    pub activities: ActivitySettings,
    pub storage: StorageSettings,
    pub capture: CaptureSettings,
    pub manual: ManualSettings,
    pub interface: InterfaceSettings,
    pub validate_log_paths: bool,
    pub first_time_setup_complete: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            flavors: FlavorSettings::default(),
            activities: ActivitySettings::default(),
            storage: StorageSettings::default(),
            capture: CaptureSettings::default(),
            manual: ManualSettings::default(),
            interface: InterfaceSettings::default(),
            validate_log_paths: true,
            first_time_setup_complete: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationProblem {
    pub field: &'static str,
    pub message: String,
}

impl ValidationProblem {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Vec<ValidationProblem> {
        let mut problems = self.persistence_problems();

        validate_active_path(
            &mut problems,
            "storage.recording_dir",
            "Choose a recording directory.",
            "Choose the recording directory again to authorize access.",
            &self.storage.recording_dir,
        );

        if self.storage.separate_buffer_dir {
            validate_active_path(
                &mut problems,
                "storage.buffer_dir",
                "Choose a replay-buffer directory.",
                "Choose the replay-buffer directory again to authorize access.",
                &self.storage.buffer_dir,
            );
        }

        if !self.any_flavor_enabled() {
            problems.push(ValidationProblem::new(
                "flavors",
                "Enable at least one World of Warcraft flavor.",
            ));
        }

        for (field, flavor) in self.flavors.in_field_order() {
            validate_flavor(&mut problems, field, flavor, self.validate_log_paths);
        }

        if !self.activities.any_enabled() {
            problems.push(ValidationProblem::new(
                "activities",
                "Enable at least one automatic activity type.",
            ));
        }

        problems
    }

    fn any_flavor_enabled(&self) -> bool {
        self.flavors.retail.enabled
            || self.flavors.retail_ptr.enabled
            || self.flavors.classic.enabled
            || self.flavors.classic_ptr.enabled
            || self.flavors.era.enabled
    }

    fn persistence_problems(&self) -> Vec<ValidationProblem> {
        let mut problems = Vec::new();

        if self.version != CONFIG_VERSION {
            problems.push(ValidationProblem::new(
                "version",
                format!("Unsupported config version {}.", self.version),
            ));
        }

        for (field, path) in [
            ("storage.recording_dir", &self.storage.recording_dir),
            ("storage.buffer_dir", &self.storage.buffer_dir),
        ] {
            validate_path_state(&mut problems, field, path);
        }
        for (field, flavor) in self.flavors.in_field_order() {
            validate_path_state(&mut problems, field, &flavor.log_dir);
        }
        if !(15..=60).contains(&self.capture.fps) {
            problems.push(ValidationProblem::new(
                "capture.fps",
                "FPS must be between 15 and 60.",
            ));
        }
        if !(1_000..=200_000).contains(&self.capture.bitrate_kbps) {
            problems.push(ValidationProblem::new(
                "capture.bitrate_kbps",
                "Bitrate must be between 1000 and 200000 Kbps.",
            ));
        }
        if !(30..=600).contains(&self.capture.replay_buffer_seconds) {
            problems.push(ValidationProblem::new(
                "capture.replay_buffer_seconds",
                "Replay buffer must be between 30 and 600 seconds.",
            ));
        }
        if self.capture.extra_lead_in_seconds > 30 {
            problems.push(ValidationProblem::new(
                "capture.extra_lead_in_seconds",
                "Extra lead-in must be between 0 and 30 seconds.",
            ));
        }
        if self.capture.audio_output.trim().is_empty() {
            problems.push(ValidationProblem::new(
                "capture.audio_output",
                "Choose an output-audio device.",
            ));
        }
        if self
            .capture
            .audio_input
            .as_ref()
            .is_some_and(|input| input.trim().is_empty())
        {
            problems.push(ValidationProblem::new(
                "capture.audio_input",
                "Disable input audio or choose an input-audio device.",
            ));
        }
        if self
            .capture
            .capture_target_token
            .as_ref()
            .is_some_and(|token| token.is_empty())
        {
            problems.push(ValidationProblem::new(
                "capture.capture_target_token",
                "The capture-target token cannot be empty.",
            ));
        }
        if self.activities.min_raid_duration_seconds > 10_000 {
            problems.push(ValidationProblem::new(
                "activities.min_raid_duration_seconds",
                "Minimum raid duration cannot exceed 10000 seconds.",
            ));
        }
        if self.activities.raid_overrun_seconds > 60 {
            problems.push(ValidationProblem::new(
                "activities.raid_overrun_seconds",
                "Raid overrun must be between 0 and 60 seconds.",
            ));
        }
        if self.activities.dungeon_overrun_seconds > 60 {
            problems.push(ValidationProblem::new(
                "activities.dungeon_overrun_seconds",
                "Dungeon overrun must be between 0 and 60 seconds.",
            ));
        }
        if matches!(self.interface.selected_category, Category::Unknown(_)) {
            problems.push(ValidationProblem::new(
                "interface.selected_category",
                "Choose a supported recording category.",
            ));
        }

        problems
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = fs::read(path).map_err(|source| map_read_error(path, source))?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::InvalidJson {
                path: path.to_owned(),
                source,
            })?;
        let problems = config.persistence_problems();
        if problems.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError::Validation(problems))
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let problems = self.persistence_problems();
        if !problems.is_empty() {
            return Err(ConfigError::Validation(problems));
        }
        write_atomic(self, path)
    }
}

fn validate_path_state(
    problems: &mut Vec<ValidationProblem>,
    field: &'static str,
    path: &AuthorizedPath,
) {
    let empty = path.path.as_os_str().is_empty();
    let consistent = matches!(
        (empty, path.authorization),
        (true, PathAuthorization::Unset)
            | (
                false,
                PathAuthorization::ImportedInactive | PathAuthorization::Authorized
            )
    );
    if !consistent {
        problems.push(ValidationProblem::new(
            field,
            "The saved path and its authorization state are inconsistent.",
        ));
    }
    if path.authorization == PathAuthorization::Authorized && !path.path.is_absolute() {
        problems.push(ValidationProblem::new(
            field,
            "Choose an absolute directory path.",
        ));
    }
}

fn validate_active_path(
    problems: &mut Vec<ValidationProblem>,
    field: &'static str,
    missing_message: &'static str,
    unauthorized_message: &'static str,
    path: &AuthorizedPath,
) {
    if path.path.as_os_str().is_empty() {
        problems.push(ValidationProblem::new(field, missing_message));
    } else if !path.is_authorized() {
        problems.push(ValidationProblem::new(field, unauthorized_message));
    }
}

fn validate_flavor(
    problems: &mut Vec<ValidationProblem>,
    field: &'static str,
    flavor: &FlavorConfig,
    validate_log_paths: bool,
) {
    if !flavor.enabled {
        return;
    }
    if flavor.log_dir.path.as_os_str().is_empty() {
        problems.push(ValidationProblem::new(
            field,
            "Choose the enabled flavor's Logs directory.",
        ));
    } else if !flavor.log_dir.is_authorized() {
        problems.push(ValidationProblem::new(
            field,
            "Choose the enabled flavor's Logs directory again to authorize access.",
        ));
    } else if validate_log_paths && flavor.log_dir.path.file_name() != Some(OsStr::new("Logs")) {
        problems.push(ValidationProblem::new(
            field,
            "Choose this flavor's World of Warcraft Logs directory.",
        ));
    }
}

fn write_atomic(config: &Config, path: &Path) -> Result<(), ConfigError> {
    let parent = path.parent().ok_or_else(|| ConfigError::Io {
        operation: "resolve config parent",
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
        operation: "create config directory",
        path: parent.to_owned(),
        source,
    })?;

    let pretty =
        serde_json::to_string_pretty(config).map_err(|source| ConfigError::InvalidJson {
            path: path.to_owned(),
            source,
        })?;
    let filename = path.file_name().ok_or_else(|| ConfigError::Io {
        operation: "resolve config filename",
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "config path has no filename"),
    })?;
    let mut temporary_name = filename.to_os_string();
    temporary_name.push(".tmp");
    let temporary_path = path.with_file_name(temporary_name);
    let _ = fs::remove_file(&temporary_path);

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary_path)
            .map_err(|source| ConfigError::Io {
                operation: "create temporary config",
                path: temporary_path.clone(),
                source,
            })?;
        file.write_all(pretty.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|source| ConfigError::Io {
                operation: "write temporary config",
                path: temporary_path.clone(),
                source,
            })?;
        file.sync_all().map_err(|source| ConfigError::Io {
            operation: "sync temporary config",
            path: temporary_path.clone(),
            source,
        })?;
        fs::rename(&temporary_path, path).map_err(|source| ConfigError::Io {
            operation: "replace config",
            path: path.to_owned(),
            source,
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| ConfigError::Io {
                operation: SYNC_DIRECTORY,
                path: parent.to_owned(),
                source,
            })
    })();

    if result.is_err() {
        let _ = fs::remove_file(temporary_path);
    }
    result
}

pub fn config_path_from_environment() -> Result<PathBuf, ConfigError> {
    config_path_from_values(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

pub fn legacy_config_path_from_environment() -> Result<PathBuf, ConfigError> {
    legacy_config_path_from_values(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn config_path_from_values(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    Ok(config_root(xdg_config_home, home)?
        .join(APP_ID)
        .join(CONFIG_FILENAME))
}

fn legacy_config_path_from_values(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    Ok(config_root(xdg_config_home, home)?
        .join(LEGACY_CONFIG_DIR)
        .join(LEGACY_CONFIG_FILENAME))
}

fn config_root(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    if let Some(path) = nonempty_os(xdg_config_home) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = nonempty_os(home) {
        return Ok(PathBuf::from(path).join(".config"));
    }
    Err(ConfigError::UnresolvedHome)
}

fn nonempty_os(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportWarning {
    pub key: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigOrigin {
    Native,
    LegacyImported,
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: Config,
    pub origin: ConfigOrigin,
    pub import_warnings: Vec<ImportWarning>,
}

pub fn load_or_import(native_path: &Path, legacy_path: &Path) -> Result<LoadedConfig, ConfigError> {
    match Config::load(native_path) {
        Ok(config) => {
            return Ok(LoadedConfig {
                config,
                origin: ConfigOrigin::Native,
                import_warnings: Vec::new(),
            });
        }
        Err(ConfigError::NotFound(_)) => {}
        Err(error) => return Err(error),
    }

    let legacy_bytes = match fs::read(legacy_path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedConfig {
                config: Config::default(),
                origin: ConfigOrigin::Default,
                import_warnings: Vec::new(),
            });
        }
        Err(source) => {
            return Err(ConfigError::Io {
                operation: "read legacy config",
                path: legacy_path.to_owned(),
                source,
            });
        }
    };

    let value: Value =
        serde_json::from_slice(&legacy_bytes).map_err(|source| ConfigError::InvalidJson {
            path: legacy_path.to_owned(),
            source,
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| ConfigError::LegacyRootNotObject {
            path: legacy_path.to_owned(),
        })?;
    let imported = import_legacy(object);
    write_atomic(&imported.config, native_path)?;
    Ok(LoadedConfig {
        config: imported.config,
        origin: ConfigOrigin::LegacyImported,
        import_warnings: imported.warnings,
    })
}

struct LegacyImport {
    config: Config,
    warnings: Vec<ImportWarning>,
}

fn import_legacy(values: &Map<String, Value>) -> LegacyImport {
    let mut reader = LegacyReader::new(values);

    let max_storage = reader.integer("maxStorage", 50, Some(0), None);
    let selected_category =
        category_from_index(reader.integer("selectedCategory", 1, Some(0), Some(9)));
    let death_markers = match reader.integer("deathMarkers", 1, Some(0), Some(2)) {
        0 => DeathMarkerVisibility::None,
        1 => DeathMarkerVisibility::Own,
        2 => DeathMarkerVisibility::All,
        _ => unreachable!("legacy integer helper enforces death marker range"),
    };
    let output_audio = if values.contains_key("linuxGsrAudioOutput") {
        reader.string("linuxGsrAudioOutput", "default_output")
    } else {
        reader.string("linuxGsrAudio", "default_output")
    };
    let input_audio = reader.string("linuxGsrAudioInput", "");

    let mut config = Config {
        version: CONFIG_VERSION,
        flavors: FlavorSettings {
            retail: FlavorConfig {
                enabled: reader.boolean("recordRetail", false),
                log_dir: AuthorizedPath::imported(reader.string("retailLogPath", "")),
            },
            retail_ptr: FlavorConfig {
                enabled: reader.boolean("recordRetailPtr", false),
                log_dir: AuthorizedPath::imported(reader.string("retailPtrLogPath", "")),
            },
            classic: FlavorConfig {
                enabled: reader.boolean("recordClassic", false),
                log_dir: AuthorizedPath::imported(reader.string("classicLogPath", "")),
            },
            classic_ptr: FlavorConfig {
                enabled: reader.boolean("recordClassicPtr", false),
                log_dir: AuthorizedPath::imported(reader.string("classicPtrLogPath", "")),
            },
            era: FlavorConfig {
                enabled: reader.boolean("recordEra", false),
                log_dir: AuthorizedPath::imported(reader.string("eraLogPath", "")),
            },
        },
        activities: ActivitySettings {
            record_raids: reader.boolean("recordRaids", true),
            record_dungeons: reader.boolean("recordDungeons", true),
            record_two_v_two: reader.boolean("recordTwoVTwo", true),
            record_three_v_three: reader.boolean("recordThreeVThree", true),
            record_five_v_five: reader.boolean("recordFiveVFive", true),
            record_skirmish: reader.boolean("recordSkirmish", true),
            record_solo_shuffle: reader.boolean("recordSoloShuffle", true),
            record_battlegrounds: reader.boolean("recordBattlegrounds", true),
            record_challenge_modes: reader.boolean("recordChallengeModes", true),
            min_keystone_level: reader.integer("minKeystoneLevel", 2, Some(0), None) as u32,
            min_raid_difficulty: reader.raid_difficulty("minRaidDifficulty"),
            min_raid_duration_seconds: reader.integer(
                "minEncounterDuration",
                15,
                Some(i32::MIN.into()),
                Some(10_000),
            ) as i32,
            current_raid_only: reader.boolean("recordCurrentRaidEncountersOnly", false),
            raid_overrun_seconds: reader.integer("raidOverrun", 15, Some(0), Some(60)) as u32,
            dungeon_overrun_seconds: reader.integer("dungeonOverrun", 5, Some(0), Some(60)) as u32,
        },
        storage: StorageSettings {
            recording_dir: AuthorizedPath::imported(reader.string("storagePath", "")),
            separate_buffer_dir: reader.boolean("separateBufferPath", false),
            buffer_dir: AuthorizedPath::imported(reader.string("bufferStoragePath", "")),
            limit: if max_storage == 0 {
                StorageLimit::Unlimited
            } else {
                StorageLimit::Gib(
                    NonZeroU64::new(max_storage as u64)
                        .expect("positive legacy storage limit is nonzero"),
                )
            },
        },
        capture: CaptureSettings {
            fps: reader.integer("obsFPS", 60, Some(15), Some(60)) as u32,
            codec: reader.codec("linuxGsrCodec"),
            bitrate_kbps: reader.integer("linuxGsrBitrateKbps", 20_000, Some(1_000), Some(200_000))
                as u32,
            replay_buffer_seconds: reader.integer("linuxGsrBufferSeconds", 180, Some(30), Some(600))
                as u32,
            extra_lead_in_seconds: reader.integer("linuxGsrLeadInSeconds", 0, Some(0), Some(30))
                as u32,
            replay_storage: reader.replay_storage("linuxGsrReplayStorage"),
            capture_cursor: reader.boolean("captureCursor", false),
            audio_output: output_audio,
            audio_input: (!input_audio.is_empty()).then_some(input_audio),
            capture_target_token: None,
        },
        manual: ManualSettings {
            enabled: reader.boolean("manualRecord", false),
            sound: reader.boolean("manualRecordSoundAlert", true),
        },
        interface: InterfaceSettings {
            hide_empty_categories: reader.boolean("hideEmptyCategories", false),
            death_markers,
            encounter_markers: visibility(reader.boolean("encounterMarkers", true)),
            round_markers: visibility(reader.boolean("roundMarkers", true)),
            selected_category,
            minimize_to_tray: reader.boolean("minimizeToTray", true),
            close_to_tray: reader.boolean("minimizeOnQuit", true),
            start_minimized: reader.boolean("startMinimized", false),
        },
        validate_log_paths: reader.boolean("validateLogPaths", true),
        first_time_setup_complete: !reader.boolean("firstTimeSetup", true),
    };

    if !config.validate().is_empty() {
        config.first_time_setup_complete = false;
    }

    LegacyImport {
        config,
        warnings: reader.warnings,
    }
}

fn visibility(value: bool) -> MarkerVisibility {
    if value {
        MarkerVisibility::Visible
    } else {
        MarkerVisibility::Hidden
    }
}

fn category_from_index(index: i64) -> Category {
    match index {
        0 => Category::TwoVTwo,
        1 => Category::ThreeVThree,
        2 => Category::FiveVFive,
        3 => Category::Skirmish,
        4 => Category::SoloShuffle,
        5 => Category::MythicPlus,
        6 => Category::Raids,
        7 => Category::Battlegrounds,
        8 => Category::Manual,
        9 => Category::Clip,
        _ => unreachable!("legacy selected-category index is range checked"),
    }
}

struct LegacyReader<'a> {
    values: &'a Map<String, Value>,
    warned: BTreeSet<String>,
    warnings: Vec<ImportWarning>,
}

impl<'a> LegacyReader<'a> {
    fn new(values: &'a Map<String, Value>) -> Self {
        Self {
            values,
            warned: BTreeSet::new(),
            warnings: Vec::new(),
        }
    }

    fn boolean(&mut self, key: &str, default: bool) -> bool {
        match self.values.get(key) {
            None => default,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                self.warn(key, "Expected a boolean; used the legacy default.");
                default
            }
        }
    }

    fn string(&mut self, key: &str, default: &str) -> String {
        match self.values.get(key) {
            None => default.to_owned(),
            Some(Value::String(value)) => value.clone(),
            Some(_) => {
                self.warn(key, "Expected a string; used the legacy default.");
                default.to_owned()
            }
        }
    }

    fn integer(
        &mut self,
        key: &str,
        default: i64,
        minimum: Option<i64>,
        maximum: Option<i64>,
    ) -> i64 {
        let Some(value) = self.values.get(key) else {
            return default;
        };
        let Some(value) = value.as_i64() else {
            self.warn(key, "Expected an integer; used the legacy default.");
            return default;
        };
        if minimum.is_some_and(|minimum| value < minimum)
            || maximum.is_some_and(|maximum| value > maximum)
        {
            self.warn(
                key,
                "Value was outside its supported range; used the legacy default.",
            );
            default
        } else {
            value
        }
    }

    fn codec(&mut self, key: &str) -> Codec {
        match self.string(key, "h264").to_ascii_lowercase().as_str() {
            "h264" => Codec::H264,
            "hevc" => Codec::Hevc,
            "av1" => Codec::Av1,
            _ => {
                self.warn(key, "Unsupported codec; used h264.");
                Codec::H264
            }
        }
    }

    fn replay_storage(&mut self, key: &str) -> ReplayStorage {
        match self.string(key, "ram").to_ascii_lowercase().as_str() {
            "ram" => ReplayStorage::Ram,
            "disk" => ReplayStorage::Disk,
            _ => {
                self.warn(key, "Unsupported replay storage; used ram.");
                ReplayStorage::Ram
            }
        }
    }

    fn raid_difficulty(&mut self, key: &str) -> RaidDifficulty {
        match self.string(key, "LFR").to_ascii_lowercase().as_str() {
            "lfr" => RaidDifficulty::Lfr,
            "normal" => RaidDifficulty::Normal,
            "heroic" => RaidDifficulty::Heroic,
            "mythic" => RaidDifficulty::Mythic,
            _ => {
                self.warn(key, "Unsupported raid difficulty; used LFR.");
                RaidDifficulty::Lfr
            }
        }
    }

    fn warn(&mut self, key: &str, message: &str) {
        if self.warned.insert(key.to_owned()) {
            self.warnings.push(ImportWarning {
                key: key.to_owned(),
                message: message.to_owned(),
            });
        }
    }
}

fn map_read_error(path: &Path, source: io::Error) -> ConfigError {
    if source.kind() == io::ErrorKind::NotFound {
        ConfigError::NotFound(path.to_owned())
    } else {
        ConfigError::Io {
            operation: "read config",
            path: path.to_owned(),
            source,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    NotFound(PathBuf),
    InvalidJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    LegacyRootNotObject {
        path: PathBuf,
    },
    Validation(Vec<ValidationProblem>),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    UnresolvedHome,
}

/// Naming the one post-rename step lets a caller tell "the write never
/// happened" from "the write is visible but its durability is unconfirmed".
const SYNC_DIRECTORY: &str = "sync config directory";

impl ConfigError {
    /// True when the new config is already on disk despite the error, so the
    /// caller must keep the value it just wrote rather than roll it back.
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Io { operation, .. } if *operation == SYNC_DIRECTORY)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(formatter, "config not found: {}", path.display()),
            Self::InvalidJson { path, .. } => {
                write!(formatter, "invalid JSON in {}", path.display())
            }
            Self::LegacyRootNotObject { path } => {
                write!(
                    formatter,
                    "legacy config is not an object: {}",
                    path.display()
                )
            }
            Self::Validation(problems) => {
                write!(
                    formatter,
                    "config has {} validation problem(s)",
                    problems.len()
                )
            }
            Self::Io {
                operation, path, ..
            } => write!(formatter, "failed to {operation}: {}", path.display()),
            Self::UnresolvedHome => formatter.write_str(
                "Cannot find the config directory because XDG_CONFIG_HOME and HOME are unset.",
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let path =
            env::temp_dir().join(format!("warcraft-recorder-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn ready_config() -> Config {
        let mut config = Config::default();
        config.storage.recording_dir = AuthorizedPath::authorized("/recordings");
        config.flavors.retail = FlavorConfig {
            enabled: true,
            log_dir: AuthorizedPath::authorized("/games/wow/_retail_/Logs"),
        };
        config.first_time_setup_complete = true;
        config
    }

    #[test]
    fn default_config_round_trips_atomically_with_private_permissions() {
        let directory = temporary_directory("config-round-trip");
        let path = directory.join(CONFIG_FILENAME);
        let config = Config::default();

        config.save(&path).expect("save default config");
        assert_eq!(Config::load(&path).expect("load default config"), config);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path)
                .expect("config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!directory.join("config.json.tmp").exists());

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn validation_reports_every_constraint_in_stable_field_order() {
        // Rules the ordered assertion below does not already exercise.
        type Mutation = Box<dyn Fn(&mut Config)>;
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "storage.recording_dir",
                Box::new(|config| config.storage.recording_dir = AuthorizedPath::unset()),
            ),
            (
                "storage.recording_dir",
                Box::new(|config| {
                    config.storage.recording_dir.authorization = PathAuthorization::Unset
                }),
            ),
            (
                "storage.recording_dir",
                Box::new(|config| {
                    config.storage.recording_dir = AuthorizedPath::authorized("recordings")
                }),
            ),
            (
                "storage.buffer_dir",
                Box::new(|config| {
                    config.storage.separate_buffer_dir = true;
                    config.storage.buffer_dir = AuthorizedPath::unset();
                }),
            ),
            (
                "flavors.retail",
                Box::new(|config| config.flavors.retail.log_dir = AuthorizedPath::unset()),
            ),
            (
                "flavors.retail",
                Box::new(|config| {
                    config.flavors.retail.log_dir = AuthorizedPath::authorized("/wow/Data")
                }),
            ),
            (
                "capture.audio_input",
                Box::new(|config| config.capture.audio_input = Some(String::new())),
            ),
            (
                "capture.capture_target_token",
                Box::new(|config| config.capture.capture_target_token = Some(String::new())),
            ),
            (
                "interface.selected_category",
                Box::new(|config| {
                    config.interface.selected_category = Category::Unknown("future".to_owned())
                }),
            ),
        ];

        for (expected_field, mutate) in cases {
            let mut config = ready_config();
            mutate(&mut config);
            let problems = config.validate();
            assert!(
                problems
                    .iter()
                    .any(|problem| problem.field == expected_field && !problem.message.is_empty()),
                "missing field-specific validation for {expected_field}: {problems:?}"
            );
        }

        let mut config = Config {
            version: 99,
            ..Config::default()
        };
        config.storage.recording_dir = AuthorizedPath::imported("/recordings#old");
        config.storage.separate_buffer_dir = true;
        config.storage.buffer_dir = AuthorizedPath::imported("/recordings#old");
        config.capture.fps = 14;
        config.capture.bitrate_kbps = 999;
        config.capture.replay_buffer_seconds = 29;
        config.capture.extra_lead_in_seconds = 31;
        config.capture.audio_output.clear();
        config.activities.min_raid_duration_seconds = 10_001;
        config.activities.raid_overrun_seconds = 61;
        config.activities.dungeon_overrun_seconds = 61;
        config.flavors.retail = FlavorConfig {
            enabled: true,
            log_dir: AuthorizedPath::imported("/games/wow/_retail_/Logs"),
        };
        disable_automatic_activities(&mut config);

        let fields: Vec<_> = config
            .validate()
            .into_iter()
            .map(|problem| problem.field)
            .collect();
        assert_eq!(
            fields,
            [
                "version",
                "capture.fps",
                "capture.bitrate_kbps",
                "capture.replay_buffer_seconds",
                "capture.extra_lead_in_seconds",
                "capture.audio_output",
                "activities.min_raid_duration_seconds",
                "activities.raid_overrun_seconds",
                "activities.dungeon_overrun_seconds",
                "storage.recording_dir",
                "storage.buffer_dir",
                "flavors.retail",
                "activities",
            ]
        );

        let mut single_path_problem = ready_config();
        single_path_problem.flavors.classic.log_dir = AuthorizedPath {
            path: PathBuf::from("/games/wow/_classic_/Logs"),
            authorization: PathAuthorization::Unset,
        };
        assert_eq!(
            single_path_problem
                .persistence_problems()
                .iter()
                .filter(|problem| problem.field == "flavors.classic")
                .count(),
            1,
            "each saved flavor path state is validated exactly once"
        );
    }

    #[test]
    fn full_legacy_config_migrates_to_golden_without_touching_source() {
        let directory = temporary_directory("legacy-import");
        let native_path = directory.join("native/config.json");
        let legacy_path = directory.join("legacy/config-v3.json");
        fs::create_dir_all(legacy_path.parent().expect("legacy parent"))
            .expect("create legacy parent");
        let legacy = include_bytes!("../../tests/native/fixtures/legacy/config-full.json");
        fs::write(&legacy_path, legacy).expect("write legacy fixture");

        let loaded = load_or_import(&native_path, &legacy_path).expect("import legacy config");
        assert_eq!(loaded.origin, ConfigOrigin::LegacyImported);
        assert!(loaded.import_warnings.is_empty());
        assert_eq!(
            fs::read(&legacy_path).expect("read legacy after import"),
            legacy
        );
        assert_eq!(
            serde_json::to_string_pretty(&loaded.config).expect("serialize imported config"),
            include_str!("../../tests/native/fixtures/legacy/config-full.expected.json").trim_end()
        );
        assert_eq!(
            loaded.config.storage.recording_dir.authorization,
            PathAuthorization::ImportedInactive
        );
        assert!(loaded.config.validate().iter().any(|problem| {
            problem.field == "storage.recording_dir" && problem.message.contains("authorize access")
        }));

        fs::write(&legacy_path, b"not json anymore").expect("change legacy marker file");
        assert_eq!(
            load_or_import(&native_path, &legacy_path)
                .expect("native config is one-way marker")
                .origin,
            ConfigOrigin::Native
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn equal_imported_hash_paths_reload_as_inactive_setup_values() {
        let directory = temporary_directory("legacy-equal-hash-paths");
        let native_path = directory.join("native/config.json");
        let legacy_path = directory.join("legacy/config-v3.json");
        fs::create_dir_all(legacy_path.parent().expect("legacy parent"))
            .expect("create legacy parent");
        fs::write(
            &legacy_path,
            br##"{
                "storagePath":"/portal/#shared",
                "separateBufferPath":true,
                "bufferStoragePath":"/portal/#shared",
                "firstTimeSetup":false
            }"##,
        )
        .expect("write equal-path legacy config");

        let imported = load_or_import(&native_path, &legacy_path).expect("import equal paths");
        assert_eq!(imported.origin, ConfigOrigin::LegacyImported);
        assert_eq!(
            imported.config.storage.recording_dir,
            AuthorizedPath::imported("/portal/#shared")
        );
        assert_eq!(
            imported.config.storage.buffer_dir,
            AuthorizedPath::imported("/portal/#shared")
        );
        assert!(!imported.config.first_time_setup_complete);
        assert!(imported.config.validate().iter().any(|problem| {
            problem.field == "storage.recording_dir" && problem.message.contains("authorize")
        }));

        assert_eq!(
            Config::load(&native_path).expect("reload imported native marker"),
            imported.config
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn zero_legacy_storage_limit_stays_unlimited_after_native_round_trip() {
        let value: Value = serde_json::from_str(include_str!(
            "../../tests/native/fixtures/legacy/config-zero-storage.json"
        ))
        .expect("parse zero storage fixture");
        let imported = import_legacy(value.as_object().expect("fixture object"));
        assert_eq!(imported.config.storage.limit, StorageLimit::Unlimited);
        let encoded = serde_json::to_string(&imported.config).expect("serialize native config");
        let decoded: Config = serde_json::from_str(&encoded).expect("deserialize native config");
        assert_eq!(decoded.storage.limit, StorageLimit::Unlimited);

        let negative: Value =
            serde_json::from_str(r#"{"maxStorage":-1}"#).expect("parse negative storage value");
        let rejected = import_legacy(negative.as_object().expect("negative storage object"));
        assert_eq!(
            rejected.config.storage.limit,
            StorageLimit::Gib(NonZeroU64::new(50).expect("50 is nonzero"))
        );
        assert_eq!(rejected.warnings[0].key, "maxStorage");
    }

    #[test]
    fn legacy_keystone_level_bounds_are_mapped() {
        for level in [0, 1] {
            let value: Value = serde_json::from_value(serde_json::json!({
                "minKeystoneLevel": level
            }))
            .expect("build valid keystone level");
            let imported = import_legacy(value.as_object().expect("legacy object"));

            assert_eq!(imported.config.activities.min_keystone_level, level);
            assert!(imported.warnings.is_empty());
            assert!(
                !imported
                    .config
                    .validate()
                    .iter()
                    .any(|problem| problem.field == "activities.min_keystone_level")
            );

            let encoded = serde_json::to_string(&imported.config).expect("serialize native config");
            let decoded: Config =
                serde_json::from_str(&encoded).expect("deserialize native config");
            assert_eq!(
                decoded.activities.min_keystone_level,
                imported.config.activities.min_keystone_level
            );
        }

        let value: Value = serde_json::from_value(serde_json::json!({
            "minKeystoneLevel": -1
        }))
        .expect("build negative keystone level");
        let imported = import_legacy(value.as_object().expect("legacy object"));
        assert_eq!(imported.config.activities.min_keystone_level, 2);
        assert_eq!(
            imported.warnings,
            [ImportWarning {
                key: "minKeystoneLevel".to_owned(),
                message: "Value was outside its supported range; used the legacy default."
                    .to_owned(),
            }]
        );
    }

    #[test]
    fn invalid_native_json_and_failed_save_preserve_existing_file() {
        let directory = temporary_directory("config-failure");
        let invalid_path = directory.join("invalid.json");
        fs::write(&invalid_path, b"{not-json").expect("write invalid config");
        assert!(matches!(
            Config::load(&invalid_path),
            Err(ConfigError::InvalidJson { .. })
        ));

        let path = directory.join(CONFIG_FILENAME);
        let existing = ready_config();
        existing.save(&path).expect("write existing valid config");
        let existing_bytes = fs::read(&path).expect("read existing config");
        fs::create_dir(directory.join("config.json.tmp")).expect("block temporary file creation");
        let mut changed = existing;
        changed.capture.fps = 30;
        assert!(matches!(changed.save(&path), Err(ConfigError::Io { .. })));
        assert_eq!(
            fs::read(&path).expect("read preserved config"),
            existing_bytes
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn invalid_values_do_not_replace_a_valid_native_file() {
        let directory = temporary_directory("invalid-save");
        let path = directory.join(CONFIG_FILENAME);
        let valid = ready_config();
        valid.save(&path).expect("save valid config");
        let bytes = fs::read(&path).expect("read valid config");

        let mut invalid = valid;
        invalid.capture.fps = 240;
        let error = invalid.save(&path).expect_err("invalid save must fail");
        let ConfigError::Validation(problems) = error else {
            panic!("unexpected save error")
        };
        assert_eq!(problems[0].field, "capture.fps");
        assert_eq!(fs::read(&path).expect("read preserved config"), bytes);

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn config_paths_follow_nonempty_xdg_then_home() {
        assert_eq!(
            config_path_from_values(
                Some(OsString::from("/xdg")),
                Some(OsString::from("/home/a"))
            )
            .expect("xdg config path"),
            PathBuf::from("/xdg").join(APP_ID).join(CONFIG_FILENAME)
        );
        assert_eq!(
            legacy_config_path_from_values(Some(OsString::new()), Some(OsString::from("/home/a")))
                .expect("home config path"),
            PathBuf::from("/home/a/.config/WarcraftRecorder/config-v3.json")
        );
        assert!(matches!(
            config_path_from_values(None, None),
            Err(ConfigError::UnresolvedHome)
        ));
    }

    #[test]
    fn wrong_legacy_values_fall_back_once_per_key() {
        let value: Value = serde_json::from_str(
            r#"{
                "obsFPS": 240,
                "minEncounterDuration": -5,
                "linuxGsrCodec": "vp9",
                "encounterMarkers": 1,
                "linuxGsrAudioOutput": false,
                "linuxGsrAudio": "must-not-be-used"
            }"#,
        )
        .expect("parse invalid legacy values");
        let imported = import_legacy(value.as_object().expect("legacy object"));
        assert_eq!(imported.config.capture.fps, 60);
        assert_eq!(imported.config.activities.min_raid_duration_seconds, -5);
        assert_eq!(imported.config.capture.codec, Codec::H264);
        assert_eq!(
            imported.config.interface.encounter_markers,
            MarkerVisibility::Visible
        );
        assert_eq!(imported.config.capture.audio_output, "default_output");
        let keys: Vec<_> = imported
            .warnings
            .iter()
            .map(|warning| warning.key.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "linuxGsrAudioOutput",
                "obsFPS",
                "linuxGsrCodec",
                "encounterMarkers"
            ]
        );

        let fallback_value: Value =
            serde_json::from_str(r#"{"linuxGsrAudio":"legacy-output.monitor"}"#)
                .expect("parse legacy audio fallback");
        let fallback = import_legacy(fallback_value.as_object().expect("fallback object"));
        assert_eq!(
            fallback.config.capture.audio_output,
            "legacy-output.monitor"
        );
        assert!(fallback.warnings.is_empty());
    }

    fn disable_automatic_activities(config: &mut Config) {
        config.activities.record_raids = false;
        config.activities.record_dungeons = false;
        config.activities.record_two_v_two = false;
        config.activities.record_three_v_three = false;
        config.activities.record_five_v_five = false;
        config.activities.record_skirmish = false;
        config.activities.record_solo_shuffle = false;
        config.activities.record_battlegrounds = false;
        config.activities.record_challenge_modes = false;
    }
}
