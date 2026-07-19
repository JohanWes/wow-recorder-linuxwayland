# WR-000: Baseline and parity contract

## Goal

Produce the evidence package that makes “the same app, except localization” implementable and testable. This ticket changes no product code. Its completed reports are authoritative inputs for every later ticket.

## Owned files

- `implementation_docs/reports/wr-000-feature-parity.md`
- `implementation_docs/reports/wr-000-fixtures.md`
- `implementation_docs/reports/wr-000-capture-contract.md`
- `implementation_docs/reports/wr-000-config-contract.md`
- `implementation_docs/reports/wr-000-assets-licenses.md`
- `implementation_docs/reports/wr-000-performance-corpus.md`
- anonymized files under `tests/native/fixtures/legacy/` and `tests/native/golden/`

Do not change application code, dependencies, or packaging.

## Required work

### 1. Build the feature-parity matrix

Start the current Linux application from the default branch/worktree and trace each item from visible UI to its main-process implementation. For every item record:

- feature and exact user entry point;
- source file/line for UI and implementation;
- prerequisites needed to make it reachable;
- one concise observable-behavior example;
- classification: `KEEP`, `REMOVE_DISABLED`, `REMOVE_UNREACHABLE`, or `REMOVE_OBSOLETE`;
- evidence and maintainer approval for any classification other than `KEEP`.

Localization/language selection is pre-approved as `REMOVE_OBSOLETE` for this rewrite. Do not use that exception to remove English copy or accessibility labels.

At minimum inventory:

- first-run/setup validation and actionable failures;
- system tray creation, Open/Quit/activation, minimize-to-tray, close-to-tray, player pause on hide, continued background capture, start-minimized/autostart fields, defaults, and no-tray-host behavior;
- combat-log directory selection and validation for every supported WoW flavour;
- category visibility, order, recording toggles, thresholds, and hide-empty behavior;
- automatic recording, replay-buffer lead-in, stop/abandon handling, force-end, and finalization;
- manual recording, keyboard entry point, configured sound behavior, and test recording category picker;
- capture target initial selection/reselection, output/input audio devices, cursor, codec, FPS, bitrate, buffer duration/location, and storage limit;
- category-specific sortable columns, default sort, structured-search grammar/suggestions, date range, filter combination semantics, empty/filtered states;
- row selection, multiselect, protection, tags, bulk protection/deletion, delete confirmation, reveal in folder;
- player controls, volume/mute scope (shared for the process but not saved across restart unless evidence differs), speed values, seeking intervals, frame-step behavior, fullscreen, marker visibility, every exposed drawing tool/edit operation, and clip creation;
- local multi-POV correlation/viewpoint playback and kill-video segment editing, reset, output resolution/FPS, audio mode/source, queue/progress/output;
- waiting/reconfiguring/ready/recording/overrun/finalizing/fatal status details, microphone indication reachability on the Linux recorder, recovered recorder error reports, advanced-combat-logging warnings, logs, update check plus Install/progress/relaunch action, version/about, and status-card actions.

Cloud/account/upload/download/chat/pro code in this fork currently logs that it is disabled. Prove that there is no successful local user path, then classify it `REMOVE_DISABLED`; do not port inert controls or metadata fields solely for cloud compatibility. Do the same evidence check for Windows/macOS/OBS and obsolete modes such as 5v5. Multi-POV and kill video must not be grouped with cloud: they are reachable for two or more correlated local files and default to `KEEP`.

Split updating into two matrix rows, both preserved outside native app code. First, the migration: the final AppImage's existing updater fetches `install.sh` from `main` at run time and runs it; WR-013 turns that script into the Flatpak installer, so the user's one-click Update action itself performs the migration. Second, ongoing updates: after migration, the Flatpak remote plus the desktop software center own update discovery, download, progress, and restart. The native app therefore ships no update checker/dialog; classify the in-app checker `REMOVE_OBSOLETE` (maintainer-approved by this document set). Record the current checker/updater behavior as evidence — GitHub `releases/latest` comparison, dismissed tag, and the exact `[install]` progress-marker lines parsed by `updateService.ts` — because WR-013's migration script must stay compatible with the shipped parser.

### 2. Capture parser/activity fixtures and goldens

For every `KEEP` activity type and WoW flavour, save the smallest anonymized combat-log excerpt that includes start, meaningful timeline events, and every completion/abandon form used by the current detector. Preserve chunk boundaries only where they expose parser behavior.

For each fixture record a golden containing:

- category/flavour;
- recording start and stop action timestamps;
- calculated replay lead-in/detection delay;
- title, result/outcome including `Abandoned`, start time, duration, activity hash, and category-specific details;
- ordered timeline points/spans and player/combatant summary that the UI displays;
- whether the current app would keep or discard the recording.

Do not create a combinatorial fixture matrix. One minimal fixture per distinct retained state-machine path is enough; reuse it across parser, activity, and integration tests.

### 3. Record the recorder/capture contract

On a supported Wayland session, record the exact `gpu-screen-recorder` version, argv, environment, portal/capture token lifecycle, selected monitor/window behavior, audio-device discovery/selection, signals, child exit handling, replay-save behavior, output naming, and the current retry policy. Include examples for:

- arm/buffer;
- automatic start with observed detection delay plus configured extra lead-in;
- automatic stop and force-end;
- manual start/stop;
- test recording;
- reselect target;
- graceful app shutdown.

Replace user paths/device IDs with tokens while keeping argument order and quoting semantics.

### 4. Record exact config migration

List every current Linux setting with old key/path, type/default, `KEEP` status, and new `Config` field. Include stored player UI preferences. Identify invalid or missing values seen in real configs and the current fallback. Do not migrate disabled cloud/platform keys.

Capture one anonymized full legacy config and the expected migrated config. Migration is one-way read/import; it does not alter the legacy file.

### 5. Record legacy media/metadata and assets

- Collect anonymized sidecars for each retained category, a clip, a manual recording, an abandoned result, protected/tagged data, old optional/missing fields, and two local correlated POVs. Record exact mapping to the new model.
- Identify representative real legacy H.264 and AV1 recordings with audio for WR-002/WR-011. Small redistributable samples may be committed; otherwise record local test paths and hashes in the private/manual evidence without committing personal media.
- Inventory product/category/spec/class/affix assets used by retained UI. Record source, license, whether redistribution/modification is allowed, and exact reused files. Reject unproven assets instead of redrawing a speculative replacement set.
- Resolve and record the canonical project SPDX/license with a maintainer. This is a hard block for WR-002 dependency shipping.

### 6. Define the performance corpus and reference views

Define the deterministic 2,000-sidecar corpus (documented seed and schema) that WR-015 uses for its absolute performance gates, and capture reference screenshots of each main view/category at 1440×900 for WR-015's identity comparison. Do not measure the Electron baseline: the release gates are absolute (README performance budgets), so a comparative Electron measurement campaign adds no decision value.

## Acceptance criteria

- Every visible/local runtime feature is present in the matrix and every removal beyond localization has evidence plus maintainer approval.
- Every downstream `KEEP` behavior has either a fixture/golden, a concrete manual scenario, or both.
- Capture and config reports are precise enough to reproduce argv, replay timing, migration, and defaults without consulting TypeScript.
- The fixture set is minimal: no two fixtures exist solely to test the same behavior with different incidental data.
- The license decision and dependency-compatible SPDX are recorded; otherwise mark WR-000 `BLOCKED(license decision)`.
- The performance-corpus seed/schema and the reference screenshots are recorded.
- Each speculative idea encountered is either omitted or listed once under `Skipped (YAGNI)` with a one-line reason.

## Verification

Review the reports against current source and exercise each reachable UI path once. A maintainer signs the feature-parity and license sections before the ticket is `DONE`.

## Not in scope

Implementing fixes, choosing new product features, redesigning behavior, adding exhaustive malformed-log fixtures, or any performance benchmarking (WR-015 owns measurement against the absolute budgets).
