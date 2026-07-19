# WR-000: legacy config/import contract

## Source and path

`electron-store` opens store name `config-v3` (`src/config/ConfigService.ts:59-64`), i.e. `<Electron app userData>/config-v3.json`. Import is one-way: read this file if present, validate/coerce only as explicitly stated below, write the native config, never modify the legacy file.

## Legacy key disposition and retained mapping

Every row with a concrete native field below is `KEEP`. A `none` destination is not retained and carries the explicit removal classification in its note. This status rule avoids implying that an OBS-only key is retained merely because its legacy default is documented.

| Legacy key | Type / default | Native field | Notes |
|---|---|---|---|
| `storagePath` | string / `""` | `recording_dir` | required setup path |
| `separateBufferPath`, `bufferStoragePath` | bool/false, string/`""` | `separate_buffer_dir`, `buffer_dir` | retained UI setting; current GSR argv does not consume the path |
| `retailLogPath`, `retailPtrLogPath`, `classicLogPath`, `classicPtrLogPath`, `eraLogPath` | string / `""` | five flavour log-dir fields | Era uses Classic flavour metadata/parser family |
| `recordRetail`, `recordRetailPtr`, `recordClassic`, `recordClassicPtr`, `recordEra` | bool / false each | five flavour enabled fields | |
| `maxStorage` | integer / 50, min 0 | `max_storage_gib` | 0 is permitted |
| `selectedCategory` | integer / 1 | `selected_category` | index in exact SideMenu order |
| `minEncounterDuration` | integer / 15, max 10000 | `min_raid_duration_seconds` | no schema minimum |
| `obsFPS` | integer / 60, schema 15..60 | `fps` | GSR uses directly. Linux UI currently declares max 240 (`LinuxCaptureSettings.tsx:176-185`), contradicting the store maximum; migration honors the schema until maintainer resolves it |
| `obsQuality` | string / `Moderate` | none | `REMOVE_UNREACHABLE`: OBS-only, not used by Linux recorder |
| `obsRecEncoder` | string / `obs_x264` | none | `REMOVE_UNREACHABLE`: written to sidecar but GSR codec has its own setting; do not configure native capture from it |
| `recordRaids`, `recordDungeons`, `recordTwoVTwo`, `recordThreeVThree`, `recordFiveVFive`, `recordSkirmish`, `recordSoloShuffle`, `recordBattlegrounds`, `recordChallengeModes` | bool / true each | corresponding activity toggles | 5v5 remains reachable Classic behavior |
| `minKeystoneLevel` | integer / 2 | `min_keystone_level` | |
| `minRaidDifficulty` | string / `LFR` | `min_raid_difficulty` | valid observed values are LFR/Normal/Heroic/Mythic, case-insensitive in parser |
| `recordCurrentRaidEncountersOnly` | bool / false | `current_raid_only` | |
| `raidOverrun`, `dungeonOverrun` | integer / 15, 5; each 0..60 | `raid_overrun_seconds`, `dungeon_overrun_seconds` | arena/BG/shuffle fixed 3 s in activities |
| `captureCursor` | bool / false | `capture_cursor` | |
| `minimizeOnQuit`, `minimizeToTray` | bool / true each | `close_to_tray`, `minimize_to_tray` | native no-watcher fallback applies |
| `startMinimized` | bool / false | `start_minimized` | |
| `deathMarkers` | integer / 1 | `death_markers` | display mode, not boolean |
| `encounterMarkers`, `roundMarkers` | bool / true | `show_encounter_markers`, `show_round_markers` | schema mistakenly declares encounter `type: integer`; accept only a JSON boolean or use default |
| `hideEmptyCategories` | bool / false | `hide_empty_categories` | |
| `manualRecord` | bool / false | `manual_record_enabled` | |
| `manualRecordHotKey` | integer / -1 | `manual_record_keycode` | |
| `manualRecordHotKeyModifiers` | string / `""` | `manual_record_modifiers` | substrings ctrl/win/shift/alt in legacy |
| `manualRecordSoundAlert` | bool / true | `manual_record_sound` | |
| `validateLogPaths` | bool / true | `validate_log_paths` | |
| `firstTimeSetup` | bool / true | `first_time_setup_complete = !value` | inverted meaning |
| `linuxGsrBufferSeconds` | integer / 180, 30..600 | `replay_buffer_seconds` | |
| `linuxGsrCodec` | string / `h264` | `capture_codec` | UI offers h264/hevc/av1 |
| `linuxGsrBitrateKbps` | integer / 20000, 1000..200000 | `bitrate_kbps` | |
| `linuxGsrAudioOutput` | string / `default_output` | `audio_output` | if absent, import legacy `linuxGsrAudio`; do not import it otherwise |
| `linuxGsrAudioInput` | string / `""` | `audio_input` | empty disables mic track |
| `linuxGsrReplayStorage` | string / `ram` | `replay_storage` | UI values ram/disk |
| `linuxGsrLeadInSeconds` | integer / 0, 0..30 | `extra_lead_in_seconds` | added to measured detection delay |

Defaults and bounds are source-backed by `src/config/configSchema.ts:99-557`. `ConfigService.get` returns defaults through electron-store; number/string/path accessors are at `src/config/ConfigService.ts:76-126`.

## Deliberately not imported

- `REMOVE_OBSOLETE` localization: `language`.
- `REMOVE_OBSOLETE` native update implementation state: `dismissedUpdateVersion`.
- `REMOVE_DISABLED` cloud/chat/pro: `chatOverlay*`, `chatUserNameAgreed`, `manualRecordUpload`, `uploadCurrentRaidEncountersOnly`.
- `REMOVE_UNREACHABLE` Windows/OBS-only: `monitorIndex`, `audioSources`, `obsOutputResolution`, `obsForceMono`, `obsQuality`, `obsCaptureMode`, `obsRecEncoder`, `pushToTalk*`, `obsAudioSuppression`, `hardwareAcceleration`, `forceSdr`, `videoSourceScale`, `videoSourceXPosition`, `videoSourceYPosition`.
- Proposed `REMOVE_UNREACHABLE`, pending maintainer approval: `startUp`. It has a generic Electron listener but no renderer control; unlike the platform removals above, its omission is not pre-approved by the document set.
- Compatibility-only `linuxGsrAudio` is used solely as fallback when the new split output key is absent.

## Validation/fallback

- For missing keys use the table default. For wrong JSON type, non-finite number, or numeric value outside documented bounds, use the default and report the key once; do not silently clamp because current electron-store schema rejects invalid writes rather than documenting clamping.
- For enum-like strings not recognized by the retained native UI (`capture_codec`, replay storage, difficulty, resolution), use the default and report it.
- No real legacy config was available in this checkout, so “invalid values seen in real configs” and a collected full-config fixture cannot be truthfully supplied. No substitute file is presented as user evidence.

## Player UI preferences

- `selectedCategory`, marker visibility and hide-empty are stored.
- Mute/volume are process-global only (`src/main/main.ts:577-593`) and are deliberately not imported/persisted.
- Player height/progress, playback rate, filters, date range, selection, multi-player mode and drawings are session-only.

## Blocker

Manual collection of one anonymized full `config-v3.json` plus expected import is required before this report satisfies acceptance. No personal config location/content was guessed.

## Skipped (YAGNI)

- No migration framework/version graph: WR-003 needs one guarded one-way importer only.
