# WR-013: Final AppImage migration release and Electron cutover

## Goal

Provide one tested migration path for existing AppImage users, then remove the legacy application and its disabled/platform dead code without moving or rearchitecting the completed native package.

## Dependencies

WR-014 must be `DONE`: native features are complete and a release-candidate build is installable from WR-014's pipeline. Do not delete Electron until the final AppImage artifact below is actually published and its migration path is verified.

## Owned files

- legacy source/package/build/release files being changed or deleted
- root README/contribution/build instructions
- native startup migration code only where this ticket's one-time import needs a correction
- `implementation_docs/reports/wr-013-cutover.md`
- release notes/version metadata for the final AppImage migration release

The Rust package stays under `native/`; do not move it to root or convert the project into a workspace.

## Phase A: freeze and build the migration release

1. Record the exact legacy commit/version being replaced and create the maintainer-approved release/tag plan before deletions.
2. Make the existing update feature perform the migration. The shipped updater already fetches `install.sh` from `main` at run time and runs it; write the migration version of that script so the user's normal one-click Update installs the native app:
   - check for the `flatpak` binary; when present, add WR-014's permanent remote and install the app with `flatpak --user` (no root, no privilege escalation), then offer to launch it;
   - when `flatpak` is absent or a step fails, print the permanent installation page URL and manual instructions instead;
   - keep the script's `[install]` progress-marker lines compatible with the parser in the shipped `updateService.ts` so existing installations display progress correctly;
   - the script goes live only when WR-015 publishes stable, because the updater fetches it from `main` at run time. Until then it stays unmerged or behind the release branch plan recorded here.
3. Also update the release-notes surface with a concise notice: native releases are Flatpak-only; existing local recordings/config remain in place and are imported by the native app; rollback means running this final AppImage against the untouched legacy config/data. Do not add format auto-detection services or a package-manager abstraction beyond this one script.
4. Build the final AppImage from a clean checkout using the existing release pipeline. Exercise the updater end to end against a release-candidate build: the migration script installs the Flatpak user-level, the native app starts and imports, and rollback still works. Archive artifact hash, signature, release notes, and logs in the report.
5. Obtain release authority and actually publish the final AppImage/update notice. Verify an existing installation receives it and that the migration flow works. If publication is not authorized or fails, mark this ticket blocked and do not begin Phase C. A locally publishable artifact is not sufficient.
6. The notice must say the final AppImage remains usable until WR-015 announces stable Flatpak availability; it must not claim stable is already published. The migration script must not be reachable by ordinary users before WR-015 flips it live.

## Phase B: native migration rehearsal

Using copies of real anonymized user data, test:

- default and custom legacy storage directories;
- separate replay-buffer directory;
- retained legacy config keys imported exactly once without modifying the old config;
- all WR-000 representative legacy sidecars including protected/tagged, clips/manual, missing optional fields, and correlated POVs;
- invalid legacy config and inaccessible/missing paths with actionable recovery;
- Flatpak reauthorization of every inaccessible imported log/storage/replay path, including restart persistence and GSR/FFmpeg child access;
- starting the final AppImage after native use still sees its original files (rollback rehearsal).

The native app writes only its own config/new sidecars. It may update a legacy sidecar only after a deliberate user tag/protection action and only in the compatible/atomic form defined by WR-007.

## Phase C: delete the legacy stack

After Phase A publication and Phase B success, delete rather than preserve:

- Electron/React/Node/TypeScript renderer and main-process code;
- localization files and language assets/framework;
- cloud/account/upload/download/chat/pro code;
- Windows/macOS/OBS/platform adapters and packaging;
- webpack/Tailwind/storybook/frontend tests and package-manager lockfiles;
- AppImage/electron-builder packaging and old update-downloader code;
- unused assets, scripts, fixtures, patches, and dependencies that no retained native code/report needs.

Do not retain a `legacy/` copy, generated transpiled output, IPC shim, migration server, or commented port. Git history is the archive.

Update root documentation to make Flatpak/native development the only normal path. Keep historical reports/fixtures required to prove parity and migration.

## Phase D: post-cutover release candidate

Run WR-014's release pipeline on the exact post-deletion commit to produce the release-candidate artifact, and verify a fresh install from it. WR-015 tests and publishes this exact candidate; the pre-cutover build used for migration rehearsal is never published.

## Lean audit

From repository root, produce:

- tracked-file list grouped by top-level purpose;
- production native LOC and direct dependency count versus README thresholds;
- `rg` results for Electron/Node/React/webview/IPC/cloud/OBS/Windows/macOS/localization/AppImage terms, classifying only legitimate historical documentation/legacy-parser references;
- unused Cargo dependencies/assets/modules check;
- clean `git status` after generated artifacts are removed/ignored (never delete unrelated user research artifacts).

Remove unused abstractions/dependencies discovered. Threshold violations require maintainer approval before `DONE`.

## Full cutover smoke

Inside Flatpak, exercise one continuous scenario:

1. fresh setup and custom path selection;
2. capture-target selection/reselection and audio settings;
3. automatic recording with lead-in, force-end/abandon, finalization, and restart scan;
4. manual and test recording;
5. every category table family, structured search/chips/date/sort, multiselect protection/tag/delete/reveal;
6. H.264/AV1 playback, speed, keyboard/frame controls, timeline/visibility, drawing, clipping;
7. two/four local POV playback and kill-video creation;
8. logs, About, storage limit, and graceful shutdown.

## Acceptance criteria

- The final AppImage migration artifact/update notice was actually published and verified by an existing installation before deletion.
- Native import/restart/rollback scenarios preserve user recordings, tags, protection, config, and correlations.
- No shipped Electron/web stack, disabled product code, non-Linux code, localization framework, or AppImage builder remains.
- `native/` was not moved and no compatibility architecture was added.
- A release candidate from the exact post-cutover commit is available for WR-015; nothing is published to the permanent remote yet.
- Standard checks and full Flatpak smoke pass after deletion.
- Root docs, tree/LOC/dependency/search evidence, artifact hashes, and maintainer decisions are recorded.

## Not in scope

Publishing the native stable Flatpak (WR-015), supporting both applications indefinitely, converting cloud data, keeping AppImage builds after the migration release, or unrelated code/style cleanup not made obsolete by cutover.
