# WR-003: Domain model and config migration

## Goal

Define one compact GTK-free model that can represent every retained recording, timeline, category row, recorder option, and preference, then implement validated atomic config persistence and one-way import of the retained legacy keys.

## Dependencies

WR-002 must be `DONE`. Use WR-000's parity, config, sidecar, and license reports as the source of truth.

## Owned files

- `native/src/domain.rs`
- `native/src/config.rs`
- `native/src/lib.rs` module exports
- `native/Cargo.toml`, `native/Cargo.lock`, `flatpak/cargo-sources.json` for `uuid`
- `tests/native/fixtures/legacy/config*.json`
- config-specific tests in the owned modules

## Domain model

Use concrete structs/enums with `serde` derives. Store absolute instants as Unix milliseconds (`i64`) and media offsets/durations as integer milliseconds (`u64`); do not add `chrono`. Define opaque `RecordingId(String)`. Add the small `uuid` crate with v4 support only to generate new ID strings: current recording/queue/sidecar identity already uses UUIDs and the standard library has no UUID generator. Preserve a valid legacy UUID string; when old metadata has none, use the normalized relative media filename as the stable legacy-only ID. Resolve the rare duplicate by appending the sidecar filename, not by inventing a hash framework.

Define at minimum:

- `GameFlavor`: exactly the retained WR-000 flavours plus `Unknown(String)` for legacy display;
- `Category`: exactly retained automatic categories plus `Clip`, `Manual`, and `Unknown(String)`; do not represent Manual twice through both a special enum and data-driven category;
- `Outcome`: `Win`, `Loss`, `Complete`, `Abandoned`, `Unknown`;
- `Codec`, `ReplayStorage`, and marker-visibility enums required by config;
- `StorageLimit`: `Unlimited` or `Gib(NonZeroU64)`. Legacy numeric zero maps to Unlimited; a native config never represents a zero-byte eviction limit;
- `PlayerSummary`: name, realm, GUID when retained, class/spec IDs;
- `CombatantSummary`: only fields displayed or used by search/details;
- `ActivityDetails`: a tagged enum for raid, dungeon/keystone, arena/battleground, solo rounds, clip (including source recording/category), manual, and unknown legacy details. Include the exact category-specific fields from WR-000 such as encounter/map/difficulty/pull/level/affixes/round result/boss percent where present;
- `TimelineItem`: `Point` or `Span`, kind, start offset, optional end offset, optional label/result/player reference. Reject end-before-start;
- `LibraryEntry`: ID, media/sidecar paths, category/flavour, title, start/duration, outcome, protected, optional tag, activity hash, player/combatant summaries, details, timeline, and media facts needed by playback such as FPS when known. It must render every WR-000 table/player field without rereading raw JSON on the GTK thread;
- `CorrelatedActivity`: one primary `LibraryEntry` plus local POV entry IDs, derived by storage rather than serialized into every entry;
- small user-facing `RecorderStatus`, `WorkProgress`, and `Problem` value types shared with snapshots. Recorder status covers the WR-000 waiting/reconfiguring/ready/recording/overrun/finalizing/fatal distinctions. Add microphone status only if WR-000 proves a reachable Linux state. `Problem` contains summary, optional safe detail, occurrence time, and recovery action identifier—not an arbitrary error object.

Do not carry cloud URLs/flags, account IDs, upload state, chat, Electron window state, platform unions, translated string keys, or a generic metadata map into new native sidecars. Legacy parsing may privately ignore such fields.

## Config

Create one versioned `Config` whose fields exactly match WR-000's `KEEP` config table. It must include these known groups when confirmed by that report:

- WoW log directories/enabled flavours and validation state input paths;
- per-activity recording toggles and retained thresholds;
- storage directory, optional distinct replay-buffer directory, and `StorageLimit`;
- FPS, codec, bitrate/quality, replay-buffer seconds, extra lead-in seconds, RAM/disk replay storage, cursor, output-audio device, and optional input-audio device;
- manual-recording enabled/sound behavior;
- hide-empty categories, marker visibility defaults, selected category, minimize-to-tray, close-to-tray, and start-minimized/autostart only where WR-000 records reachable Linux behavior/defaults;
- optional reusable capture-target token and any portal-authorized replacement paths as internal fields, not additional preference screens.

Do not add output resolution, force-mono, notification, thumbnail, cache, telemetry, or speculative advanced fields unless WR-000 marks an existing observable setting `KEEP`.

Implementation requirements:

1. `Config::default()` matches current Linux defaults from WR-000.
2. `Config::validate()` checks only actionable constraints: required/authorized directories where enabled, documented numeric ranges, storage/buffer relationship supported by GSR, at least one enabled activity where required, and supported IDs/enums. `StorageLimit::Gib` is nonzero by type. The legacy importer maps numeric zero to Unlimited, rejects negative, and maps positive values to `Gib`. Return all validation problems in stable field order.
3. Resolve the config path with standard environment/path logic: nonempty `XDG_CONFIG_HOME`, otherwise nonempty `HOME` plus `.config`, then app ID/config filename. An unresolved home is an actionable error. Do not add a directories crate or import GLib into core config. Tests inject an explicit path.
4. Save as pretty JSON to a sibling temp file, flush/sync as required by the recorded durability expectation, then rename over the destination. Set restrictive user permissions on Unix. Keep at most one `.bak` only if WR-000 proves the current app exposes recovery; otherwise no backup rotation. The whole pattern is standard library only:

   ```rust
   let tmp = path.with_file_name("config.json.tmp");
   let _ = fs::remove_file(&tmp); // stale leftover from a crashed save
   let mut f = fs::OpenOptions::new()
       .write(true).create_new(true).mode(0o600).open(&tmp)?;
   f.write_all(pretty_json.as_bytes())?;
   f.sync_all()?;
   fs::rename(&tmp, path)?;
   fs::File::open(path.parent().expect("config path has parent"))?.sync_all()?;
   ```
5. On startup: read native config if present; otherwise read the exact known Electron config location through WR-002's narrow read-only permission, import WR-000 keys/defaults, and leave it byte-identical. Imported absolute log/storage/buffer paths are desired values, not authorization. Probe before any scan/arm/write/eviction; preserve inaccessible text for display, mark setup incomplete, and require WR-012 chooser replacement. Save native config after import so it is one-time. Unknown/disabled keys are ignored; native-config existence is the marker.
6. Keep errors typed enough to distinguish not-found, invalid JSON, validation, and I/O for user-facing recovery. Use standard error traits; do not add a generic error dependency.

## Acceptance criteria

- Every `KEEP` config key has exactly one new field/mapping and every new field is justified by a current behavior.
- Every category/table/player value in WR-000's representative sidecars fits the model without `serde_json::Value` escape hatches.
- New config round-trips without information loss and writes atomically.
- The full anonymized legacy config migrates to the recorded golden; the legacy file is byte-identical afterward.
- Inaccessible imported paths remain visible but inactive and cannot cause storage eviction/capture until replaced with WR-002-proven authorization.
- Invalid settings return field-specific English messages and do not overwrite the last valid native file.
- Core files compile without GTK widget imports and no time/directory/error dependency was added for a stdlib/GLib capability.
- `uuid` is the only direct dependency added by this ticket and Flatpak Cargo sources are regenerated.

## Tests

- one complete default/new-config round trip;
- one table-driven validation test covering each independent constraint;
- one successful full legacy migration golden;
- one explicit `maxStorage: 0` migration/round trip proving it remains unlimited;
- one invalid native JSON/load error and one simulated failed save proving the existing file survives;
- domain invariants for timeline bounds and category/detail pairing.

Do not test every optional-field combination or serde implementation detail.

## Not in scope

Legacy recording-sidecar scanning, UI widgets, recorder argv, parser behavior, automatic config watching, multi-version migration framework, or settings added for future use.
