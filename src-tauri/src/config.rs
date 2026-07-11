use std::{fs, path::PathBuf, sync::RwLock};

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};

/// Flat, forward-compatible config store. Values unknown to this port remain intact.
pub struct ConfigState {
    path: PathBuf,
    values: RwLock<Map<String, Value>>,
}

impl ConfigState {
    pub fn load(app: &AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_config_dir()
            .map_err(|error| format!("could not find app config directory: {error}"))?;
        fs::create_dir_all(&dir).map_err(|error| {
            format!(
                "could not create config directory {}: {error}",
                dir.display()
            )
        })?;
        let path = dir.join("config-v3.json");
        let mut values = defaults();

        if path.exists() {
            let content = fs::read_to_string(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            match serde_json::from_str::<Value>(&content) {
                Ok(Value::Object(on_disk)) => values.extend(on_disk),
                Ok(_) => eprintln!("config-v3.json is not a JSON object; using defaults"),
                Err(error) => eprintln!("could not parse config-v3.json; using defaults: {error}"),
            }
        }

        Ok(Self {
            path,
            values: RwLock::new(values),
        })
    }

    pub fn all(&self) -> Result<Value, String> {
        let values = self.values.read().map_err(|_| "config lock poisoned")?;
        Ok(Value::Object(values.clone()))
    }

    pub fn set(&self, key: String, value: Value) -> Result<(), String> {
        self.set_values(Map::from_iter([(key, value)]))
    }

    pub fn set_values(&self, updates: Map<String, Value>) -> Result<(), String> {
        let snapshot = {
            let mut values = self.values.write().map_err(|_| "config lock poisoned")?;
            values.extend(updates);
            Value::Object(values.clone())
        };
        let data = serde_json::to_vec_pretty(&snapshot)
            .map_err(|error| format!("could not serialize config: {error}"))?;
        fs::write(&self.path, data)
            .map_err(|error| format!("could not write {}: {error}", self.path.display()))
    }
}

fn defaults() -> Map<String, Value> {
    let entries = [
        ("storagePath", json!("")),
        ("bufferStoragePath", json!("")),
        ("separateBufferPath", json!(false)),
        ("retailLogPath", json!("")),
        ("retailPtrLogPath", json!("")),
        ("classicLogPath", json!("")),
        ("classicPtrLogPath", json!("")),
        ("eraLogPath", json!("")),
        ("maxStorage", json!(50)),
        ("monitorIndex", json!(0)),
        ("selectedCategory", json!(1)),
        ("minEncounterDuration", json!(15)),
        ("startUp", json!(false)),
        ("startMinimized", json!(false)),
        ("obsOutputResolution", json!("1920x1080")),
        ("obsFPS", json!(60)),
        ("obsForceMono", json!(true)),
        ("obsQuality", json!("Moderate")),
        ("obsCaptureMode", json!("window_capture")),
        ("obsRecEncoder", json!("obs_x264")),
        ("recordRetail", json!(false)),
        ("recordRetailPtr", json!(false)),
        ("recordClassic", json!(false)),
        ("recordClassicPtr", json!(false)),
        ("recordEra", json!(false)),
        ("recordRaids", json!(true)),
        ("recordDungeons", json!(true)),
        ("recordTwoVTwo", json!(true)),
        ("recordThreeVThree", json!(true)),
        ("recordFiveVFive", json!(true)),
        ("recordSkirmish", json!(true)),
        ("recordSoloShuffle", json!(true)),
        ("recordBattlegrounds", json!(true)),
        ("captureCursor", json!(false)),
        ("minKeystoneLevel", json!(2)),
        ("recordChallengeModes", json!(true)),
        ("minRaidDifficulty", json!("LFR")),
        ("minimizeOnQuit", json!(true)),
        ("minimizeToTray", json!(true)),
        ("chatOverlayEnabled", json!(false)),
        ("chatOverlayOwnImage", json!(false)),
        ("chatOverlayOwnImagePath", json!("")),
        ("chatOverlayScale", json!(1)),
        ("chatOverlayXPosition", json!(0)),
        ("chatOverlayYPosition", json!(0)),
        ("chatOverlayCropX", json!(0)),
        ("chatOverlayCropY", json!(0)),
        ("deathMarkers", json!(1)),
        ("encounterMarkers", json!(true)),
        ("roundMarkers", json!(true)),
        ("pushToTalk", json!(false)),
        ("pushToTalkKey", json!(-1)),
        ("pushToTalkMouseButton", json!(-1)),
        ("pushToTalkModifiers", json!("")),
        ("pushToTalkReleaseDelay", json!(0)),
        ("obsAudioSuppression", json!(true)),
        ("raidOverrun", json!(15)),
        ("dungeonOverrun", json!(5)),
        ("language", json!("English")),
        ("hideEmptyCategories", json!(false)),
        ("hardwareAcceleration", json!(false)),
        ("recordCurrentRaidEncountersOnly", json!(false)),
        ("uploadCurrentRaidEncountersOnly", json!(false)),
        ("forceSdr", json!(false)),
        ("videoSourceScale", json!(1)),
        ("videoSourceXPosition", json!(0)),
        ("videoSourceYPosition", json!(0)),
        ("manualRecord", json!(false)),
        ("manualRecordHotKey", json!(-1)),
        ("manualRecordHotKeyModifiers", json!("")),
        ("manualRecordSoundAlert", json!(true)),
        ("manualRecordUpload", json!(true)),
        ("firstTimeSetup", json!(true)),
        ("chatUserNameAgreed", json!("")),
        ("validateLogPaths", json!(true)),
        ("dismissedUpdateVersion", json!("")),
        ("videoOverrun", json!(0)),
        ("linuxGsrBufferSeconds", json!(180)),
        ("linuxGsrCodec", json!("h264")),
        ("linuxGsrBitrateKbps", json!(20000)),
        ("linuxGsrAudioOutput", json!("default_output")),
        ("linuxGsrAudioInput", json!("")),
        ("linuxGsrAudio", json!("default_output")),
        ("linuxGsrReplayStorage", json!("ram")),
        ("linuxGsrLeadInSeconds", json!(0)),
        (
            "audioSources",
            json!([
              {"id":"WCR Audio Source 1","friendly":"default","device":"default","volume":1,"type":"wasapi_output_capture"},
              {"id":"WCR Audio Source 2","friendly":"default","device":"default","volume":1,"type":"wasapi_input_capture"}
            ]),
        ),
    ];
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}
