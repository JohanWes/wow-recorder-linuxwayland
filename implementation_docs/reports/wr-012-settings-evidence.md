# WR-012 evidence: settings, native choosers/launchers, manual/test controls, and status

Status: **code complete and verified on host** (fmt/clippy/tests/release).
In-Flatpak manual acceptance (chooser/portal/audio/reselect flows with a real
GSR session) follows the same WR-000 source-traced, owner-executed deferral
used by WR-009–WR-011 (see "Known limitations").

## Work log: acceptance criteria copied before coding

- Every retained config field appears once, with correct default/current value, bounds, dependency sensitivity, Apply/Cancel, and validation mapping; no dead/cloud/platform/localization field appears.
- Folder selection, cancel, denied access, reveal, log open, and URL open work inside Flatpak using GTK/GIO facilities.
- Legacy custom/default paths require reauthorization when inaccessible; selection persists across restart and legacy config remains unchanged.
- Audio devices, capture reselection, automatic/manual/test/force-end state gating, shortcuts, progress, failures, and recovery actions match WR-000.
- The app performs no network I/O; update delivery is proven through the Flatpak remote/software-center path in WR-014/WR-015, not app code.
- No chooser abstraction, ashpd, notification center, second tray/background service, settings search, import/export, update checker, or autosave machinery exists.

## Environment

- commit: refactor/native-non-frontend (WR-012 working tree, on top of `983efae8`)
- OS/kernel/session: CachyOS, Linux 7.1.4-1-cachyos, Wayland
- toolchain: cargo/rustc stable (edition 2024); gtk4 0.11.4 (v4_10), libadwaita 0.9.2 (v1_6)

## What was built

- `ui/settings.rs` — the one settings dialog. Rule-8 deviation recorded in the
  ticket and UI-BRIEF: `AdwPreferencesDialog` exposes no Apply/Cancel actions,
  so the dialog is an `AdwDialog` hosting an `AdwViewSwitcher` over four
  `AdwPreferencesPage`s (Capture, Audio, Activities, Storage & interface) with
  Cancel/Apply in its header bar. All rows are built from static spec tables
  (`CAPTURE_*`, `ACTIVITY_*`, `STORAGE_*`, `INTERFACE_*`, `PATHS`) mapping
  one-to-one onto WR-003 `Config` fields; the coverage test locks the exact
  field set (41 fields) so no dead/cloud/localization field can appear.
  - Editing mutates one draft `Config`. Apply runs `Config::validate`, shows
    every problem inline (list + per-row error class/tooltip), and only then
    sends `SaveConfig`. "Saved." appears only after a snapshot whose config
    equals the sent draft arrives; runtime-preparation problems that arrive
    with it are shown instead. Cancel/close discards the draft. No autosave.
  - Apply and recorder/path controls are disabled with an inline explanation
    while the snapshot reports Recording/Overrunning/Finalizing or queued
    media work (`unsafe_reason`); fields stay visible.
  - Capture page: codec (H.264/HEVC/AV1), FPS, bitrate, replay buffer, extra
    lead-in, cursor, RAM/disk replay storage, capture-target status from the
    saved portal token, Reselect (explanation dialog → `ReselectCaptureTarget`),
    and the recorder's own "capture settings are not usable" problem shown
    inline — no duplicated compatibility table.
  - Audio page: output/input dropdowns filled from `Recorder::audio_devices`
    on GIO's blocking pool (`gio::spawn_blocking`), with a Refresh action.
    A selected device missing from the list is appended as "… — Unavailable"
    and stays selected until the user chooses (`audio_model`).
  - Activities page: five flavour rows (enable switch + Logs folder chooser),
    `validate_log_paths`, the nine WR-000 activity toggles in order, keystone/
    difficulty/duration/overrun thresholds with dependency sensitivity
    (`row_sensitive` greys children without erasing values), the per-flavour
    advanced-combat-logging warnings from the snapshot, manual enabled/sound,
    and `Test recording…`.
  - Storage & interface: recording dir, separate-buffer switch + dir, storage
    limit spin with visible `0 = Unlimited` ↔ `StorageLimit::Unlimited`
    mapping, live usage line and protected-over-limit warning from the
    snapshot, hide-empty, minimize/close-to-tray, start-minimized, and the
    no-tray explanation label.
  - Folder fields use `GtkFileDialog::select_folder` seeded with the current
    (or imported legacy) path. Cancel changes nothing. A selection is stored
    as `AuthorizedPath::authorized` only after a field-appropriate probe
    (read for log dirs, read+write for storage dirs) passes on the blocking
    pool; failures show the returned error on the row. Imported-inactive
    paths render "Permission required — Flatpak needs you to select this
    folder again: <path>" with `Select this folder…`. Persistence relies on
    the WR-002 Flatpak permission model; no bookmark/token database.
- `ui/operational_actions.rs` — the Manual-category toolbar (WR-000's
  approved native entry): `Start recording` sensitive only on `Ready`,
  `Stop recording` with a live elapsed label while a manual recording runs
  (`manual_view` is the tested gating function). The legacy manual-recording
  MP3s were rejected for native redistribution by the WR-000 assets/licenses
  report, so the retained `manual.sound` setting plays the display bell on
  start/stop/failed-start transitions instead of bundling audio. Also the
  test-recording chooser (`AdwAlertDialog` with the exact six WR-000
  categories, the 5 s/20 s duration explanation, Start disabled when the
  recorder is not ready) and the capture-reselection explanation dialog
  (cancel keeps the previous usable selection per the WR-006 token contract).
- `ui/window.rs` — sink wiring for OpenSettings/TestRecording, the one open
  Settings instance fed by every snapshot, and the manual bar mounted above
  the player/table pane. `ui/mod.rs` — module wiring, `Test recording…` as a
  single menu item opening the chooser (replacing the placeholder submenu).
- `main.rs` — logging now writes `app.log` into the recorder diagnostics
  directory that `Open logs` reveals, via a stdlib `RotatingLog` writer
  (4 MiB cap, one `.old` predecessor; no `tracing-appender`), with stderr
  fallback. WR-000 recorded no user-visible legacy retention count, so one
  current log plus one rotation satisfies the ticket's bounded policy.
- Status card, About, Open logs, and tray behavior needed no changes: Force
  end has no legacy confirmation (`Status.tsx:156-174` is a direct button),
  test/manual states already render, and About/`GtkFileLauncher` landed in
  WR-009. No `sidebar.rs`/`status.rs` edits were required.

## Automated verification (host)

All from repo root, all passing:

- `cargo fmt --manifest-path native/Cargo.toml --check`
- `cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings`
- `cargo test --manifest-path native/Cargo.toml --all-targets` — 129 tests
  (76 bin + 46 lib + 7 integration), including the new WR-012 tests:
  - one table-driven mapping test: exact retained-field set, defaults inside
    declared bounds, get/set round-trips, dependency sensitivity;
  - validation/apply tests: `apply_outcome` blocks unsafe states, reports
    field problems (fps/path), marks setup complete on success;
  - `unsafe_reason` over recording/overrun/finalizing/media-work states;
  - path/audio helpers: reauthorization subtitles, unavailable-device
    retention, probe read/write behavior, `0 = Unlimited` round-trip,
    storage usage formatting;
  - action tests: manual bar gating table, elapsed anchor, exact test-dialog
    category order, failed-start problem recognition.
- `cargo build --manifest-path native/Cargo.toml --release`

No GTK widget internals are unit-tested; widgets are thin consumers of the
tested pure functions, per the ticket's test policy.

## Lean/scope notes

- No ashpd, chooser abstraction, notification subsystem, global shortcuts,
  settings search/import/export, update checker, or autosave machinery was
  added; `rg -n "ashpd|libsoup|flatpak-spawn|xdg-open|show_uri" native/src`
  returns nothing. No network I/O exists in the native process.
- No new Cargo dependency; `Cargo.lock`/`flatpak/cargo-sources.json`
  unchanged.
- Restart-and-reprobe after reauthorizing a legacy path is inherent: paths
  persist as `authorized` in native config only, the coordinator re-validates
  and probes on every startup/save, and the legacy file is never written.

## Known limitations and deferred owner-executed acceptance

Under the same maintainer-approved deviation as WR-009–WR-011, the
in-sandbox manual pass — every page/field group, chooser cancel/denial
against a restricted host path, audio refresh against a real GSR install,
portal reselect cancel/success, manual/test/force-end runs, log reveal, and
About, with screenshots — is deferred to the owner-executed Flatpak session
and rolls up into WR-014/WR-015's release evidence. Host-side automated
checks above are the evidence this session can truthfully produce.
