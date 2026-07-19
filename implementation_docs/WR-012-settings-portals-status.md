# WR-012: Settings, native choosers/launchers, manual/test controls, and status

## Goal

Expose every retained setting and operational action through compact native UI, using GTK/GIO/Flatpak facilities already present. Do not add a portal wrapper, notification subsystem, or speculative preference. WR-009 owns the retained tray/background lifecycle.

## Dependencies

WR-002, WR-003, WR-006, WR-008, and WR-009 must be `DONE`.

## Owned files

- `native/src/ui/settings.rs`
- `native/src/ui/operational_actions.rs`
- `native/src/ui/mod.rs` module wiring
- edits to `native/src/ui/sidebar.rs`, `status.rs`, and window menu wiring
- focused settings/action tests
- `implementation_docs/reports/wr-012-settings-evidence.md`

## Settings window

Implement one current `AdwPreferencesDialog` with the four pages and ordering in UI-BRIEF. Every field maps one-to-one to WR-003 `Config`/WR-000; omit any proposal not in that mapping. Do not use deprecated `AdwPreferencesWindow`.

### Capture

- codec values supported/proven by GSR (H.264/HEVC/AV1 as retained), FPS, bitrate/quality, replay-buffer duration, extra lead-in, cursor, RAM/disk replay storage;
- current capture-target label/status and `Reselect capture target`;
- inline warning for unsupported codec/hardware combinations returned by the recorder, not a duplicated compatibility table.

### Audio

- output and optional input device dropdowns populated from `Recorder::audio_devices` with stable ID/label;
- selected unavailable device remains visible as unavailable and requires user choice; do not silently pick another while editing.

### Activities

- enabled log path/validation for each retained flavour;
- all retained activity toggles and thresholds in WR-000 order/dependencies;
- the advanced-combat-logging warning/action recorded by WR-000;
- `Test recording…` opening the retained category choice.

### Storage & interface

- recording directory, optional distinct replay-buffer directory, storage limit and current usage/protected warning. The numeric UI preserves `0 = Unlimited` visibly and maps zero to `StorageLimit::Unlimited`; positive GiB maps to `Gib(NonZeroU64)`;
- hide-empty categories. Death/encounter/round visibility remains in the player controls and saves the same config fields there; do not duplicate it in Settings.

Use native spin rows, switches, dropdowns, and entry rows. Numeric bounds/units come from config validation and appear next to the control. Changing a dependent switch disables its children but does not erase their values.

Editing occurs in one draft `Config`. Apply validates and sends `SaveConfig`; display all field/runtime preparation problems inline, keep the window open, and preserve the last valid disk/runtime config on failure per WR-008. Cancel discards the draft. Show Saved/close only after the snapshot confirms runtime and disk match; do not autosave every keystroke.

Disable Apply and recorder/path controls while the snapshot reports a state WR-000 marks unsafe for reconfiguration (active recording, overrun, or relevant finalization). Explain why inline; do not hide the fields.

## Native chooser and launch operations

- Folder fields use `GtkFileDialog::select_folder` (or the current nondeprecated GTK equivalent) with the current value as initial folder. Cancellation changes nothing; inaccessible choice shows the returned actionable error.
- Reveal media/log folder uses `GtkFileLauncher::open_containing_folder`/`open`; links use `GtkUriLauncher`. Do not call deprecated `gtk_show_uri`, shell out to `xdg-open`, or add `ashpd` when GTK/GIO already supplies portal-backed behavior.
- Persist the path granted by the chooser and rely on the Flatpak permission model proven in WR-002. Do not create a bookmark/token database.
- On first native start, every imported log/storage/replay path that fails the authorization probe is shown with `Permission required` and `Select this folder…`. Explain that Flatpak needs the user to reselect it; initialize the chooser near the legacy path when permitted. Replacement updates only native config. Do not scan, arm, transcode, or enforce a limit against the inaccessible text path.
- After selection, probe read/write requirements appropriate to the field, Apply, restart the app, and re-probe before reporting setup Ready. Child access is covered by WR-002; a grant that Rust can access but GSR/FFmpeg cannot is invalid.

## Manual, test, force-end, and reselection

- Manual category toolbar contains `Start recording` when Ready/Armed and `Stop recording` with elapsed time while active. Respect retained manual enabled/sound/timeout behavior. Do not add a Linux global or in-window hotkey unless WR-000 finds an existing reachable one.
- Window menu `Test recording…` opens a small category chooser matching WR-000, explains duration/behavior, and sends `RunTest`. Status card shows Test recording/progress and cancellation/force behavior exactly as baseline.
- Status-card `Force end` confirms if WR-000 does, then sends `ForceEnd`; it never calls Recorder directly.
- Reselect shows the platform prompt explanation, sends `ReselectCaptureTarget`, handles cancel without destroying a usable selection, and displays success/error from snapshot.
- Disable mutually exclusive actions from snapshot state. Busy channel/full media worker yields the common Busy problem, not duplicated local state.

## Logs and About

- `Open logs` launches the native log directory. Logging uses `tracing-subscriber` plus a bounded/rotated file policy implemented with the standard library or the smallest already-installed facility; do not add `tracing-appender` solely for rotation. Keep the current number/size from WR-000, or one current log if no retention behavior is user-visible.
- Updates are platform-owned. The Flatpak remote plus the desktop software center deliver update discovery, download, progress, and app restart; the app ships accurate AppStream release notes (WR-014) and performs no network I/O. Do not implement an update checker, update dialog, dismissed-tag state, libsoup dependency, host `flatpak-spawn`, or the obsolete AppImage downloader. Migration from the final AppImage is WR-013's script, not native app code.
- About uses `AdwAboutDialog` with canonical license, version, website, credits, and English-only status.
- Do not send desktop notifications or register a global shortcut daemon. Do not duplicate WR-009's tray/background backend in Settings.

## Acceptance criteria

- Every retained config field appears once, with correct default/current value, bounds, dependency sensitivity, Apply/Cancel, and validation mapping; no dead/cloud/platform/localization field appears.
- Folder selection, cancel, denied access, reveal, log open, and URL open work inside Flatpak using GTK/GIO facilities.
- Legacy custom/default paths require reauthorization when inaccessible; selection persists across restart and legacy config remains unchanged.
- Audio devices, capture reselection, automatic/manual/test/force-end state gating, shortcuts, progress, failures, and recovery actions match WR-000.
- The app performs no network I/O; update delivery is proven through the Flatpak remote/software-center path in WR-014/WR-015, not app code.
- No chooser abstraction, ashpd, notification center, second tray/background service, settings search, import/export, update checker, or autosave machinery exists.

## Tests and evidence

Use one table-driven config-to-row/value mapping test, one validation/apply failure test, and action enabling/payload tests. Do not unit-test GTK widget internals or every dropdown value combination. Manually exercise every page/field group, chooser/denial, audio refresh, reselect cancel/success, manual/test/force-end, logs, and About inside Flatpak with screenshots/results.

## Not in scope

Localization selector, cloud/account settings, desktop notifications, global shortcuts, settings profiles/search/import-export, any in-app update checking or project-owned update downloads, or capture options absent from WR-000.
