# Warcraft Recorder native migration

This directory is the implementation contract for replacing the Electron application with a lean native Linux application. The target is one Rust process with a GTK4/libadwaita UI, Clapper/GStreamer playback, `gpu-screen-recorder` capture, and Flatpak distribution.

Read this file, [ADR-001](ADR-001-native-rust-gtk-flatpak.md), [UI-BRIEF](UI-BRIEF.md), WR-000's completed parity matrix, and the complete assigned ticket before editing code. If they disagree, WR-000's recorded observable behavior wins and the documents must be reconciled before implementation continues.

Measurement tickets use the [evidence report template](reports/README.md).

## Outcome

The native application is the same product, not a smaller replacement product. The migration is complete when:

- every reachable local-Linux function recorded by WR-000 works in the native application, except localization/language selection;
- the UI remains recognizably Warcraft Recorder while becoming native, faster, and less visually dense;
- the normal install and update path is Flatpak;
- Electron, Chromium, Node, React, webpack, Tailwind, AppImage packaging, disabled cloud/account code, and non-Linux platform code are absent after cutover;
- a release build meets the absolute size/startup/memory budgets below.

This is a rewrite, not an Electron API emulation. Do not add Tauri, a webview, a localhost media server, a generic IPC layer, a JavaScript compatibility bridge, or a second copy of legacy business logic.

## Scope rule: parity first, deletion by evidence

Localization and its language selector are the only pre-approved user-facing removals. English copy remains in normal source/UI files; do not create a one-language localization framework.

WR-000 must classify every legacy feature as one of:

- `KEEP`: reachable in the current Linux application and therefore required;
- `REMOVE_DISABLED`: present in source but deliberately disabled in this fork, such as cloud upload/download;
- `REMOVE_UNREACHABLE`: no Linux UI or runtime path can invoke it, with file/line evidence;
- `REMOVE_OBSOLETE`: depends on a service, platform, or game mode that no longer exists, with maintainer approval.

An implementation ticket may not silently downgrade a `KEEP` feature. If its native implementation is missing from the tickets, stop and fix the relevant ticket before coding. Known `KEEP` scope includes automatic and manual recording, test recording, force-end, capture-target reselection, background recording with tray Open/Quit and current close/minimize behavior, suggestion-chip/paired-date filtering, sortable category tables, multiselect/bulk actions, tagging/protection, reveal/delete, playback speed, keyboard controls, frame stepping, marker visibility controls, drawing, clipping, and local viewpoint-selection/kill-video behavior (multi-POV grid playback was removed by maintainer decision 2026-07-22; see the WR-000 parity matrix). The user's Update outcome is preserved outside app code: the final AppImage's updater installs the Flatpak (WR-013), and the Flatpak remote/software center owns updates thereafter; the in-app checker is not rebuilt (WR-000 records the approved `REMOVE_OBSOLETE`).

Cloud/account/upload/chat/pro paths are already disabled in this fork and are not to be revived. Windows, macOS, OBS/libobs, NSIS, notarization, AppImage after the migration release, telemetry, plugins, and a database are outside scope.

Classic/Era support is not silently deleted. WR-000 records exactly which flavours and activity types are reachable and fixture-backed. Any product-scope reduction other than localization needs a maintainer decision in the parity matrix.

The repository has conflicting license metadata (`LICENSE` contains GPLv2 text while `package.json` declares a Creative Commons noncommercial license). WR-000 records the maintainer's canonical SPDX choice before dependencies are selected for shipping or Flatpak metadata is published. Agents must not guess or relicense the project.

## Lean implementation decision order

These are judgment prompts, not rules to game mechanically. Apply them for each proposed type, helper, abstraction, dependency, test, recovery branch, and UI component:

1. Does it need to exist for a `KEEP` behavior or measured release gate? If the need is speculative, skip it and record that in one line.
2. Does an equivalent already exist in the new Rust code or as reusable factual data/assets in the current app? Reuse it.
3. Does Rust's standard library cover it clearly? Use that.
4. Does GTK, GLib/GIO, Clapper, GStreamer, Flatpak, or the filesystem cover it natively? Use the platform facility.
5. Does an already-approved dependency cover it without adding an architecture? Use it.
6. Prefer the smallest direct expression over a helper used once.
7. Only then write the minimum project-specific code.

Do not turn this order into compressed or obscure code. A few explicit lines are better than a clever abstraction, and a named domain type is justified when it prevents invalid state.

## Target architecture and tree

- One GTK process. The GTK main thread owns widgets only.
- One coordinator thread owns mutable application/domain state, log polling, and recorder control.
- One background media/storage worker serializes finalization, clipping, and kill-video work. No generic worker pool.
- One minimal StatusNotifierItem service thread is permitted solely for the retained tray/background lifecycle after WR-002 proves it; it is not a general worker. `main` owns this and the coordinator join handle, and bounded tray events are polled on GTK's thread.
- `std::sync::mpsc`/`sync_channel` carries typed commands and snapshots. No async runtime or event-bus framework.
- Clapper/ClapperGtk supplies playback, speed control, seeking, and forward frame stepping over GStreamer. Project code supplies product controls and the combat timeline, not a custom media state machine.
- `gpu-screen-recorder` remains the capture engine.
- One narrowly configured FFmpeg executable supplies the already-existing clip and local kill-video transforms through `std::process`; no Rust media-editing framework is added.
- JSON sidecars remain the library source of truth; no database or generated thumbnail cache.

The Rust package remains in `native/` before and after cutover. Moving it to the repository root would create churn without product value.

```text
native/
  Cargo.toml
  src/
    lib.rs
    domain.rs
    config.rs
    parser/
    activity.rs
    logwatch.rs
    recorder.rs
    process.rs
    storage.rs
    media_jobs.rs
    coordinator.rs
    ui/
flatpak/
data/
tests/native/{fixtures,golden}/
implementation_docs/
```

There is one Cargo package, not a workspace. `native/src/lib.rs` must not depend on GTK. The GUI is a thin consumer of coordinator snapshots and commands.

## Dependency order and status

Statuses are `TODO`, `IN PROGRESS`, `BLOCKED(reason)`, and `DONE`. Start only when every dependency is `DONE`.

| ID | Ticket | Depends on | Status |
|---|---|---|---|
| WR-000 | [Baseline and parity contract](WR-000-baseline-contract.md) | — | DONE |
| WR-001 | [Rust package scaffold](WR-001-rust-scaffold.md) | WR-000 | DONE |
| WR-002 | [Development Flatpak and platform proofs](WR-002-development-flatpak.md) | WR-001 | DONE |
| WR-003 | [Domain model and config](WR-003-domain-config.md) | WR-002 | DONE |
| WR-004 | [Log reader and parser](WR-004-log-reader-parser.md) | WR-000, WR-003 | DONE |
| WR-005 | [Activity state machine](WR-005-activity-state-machine.md) | WR-003, WR-004 | DONE |
| WR-006 | [Recorder adapter](WR-006-recorder-adapter.md) | WR-002, WR-003 | DONE |
| WR-007 | [Storage and library index](WR-007-storage-library.md) | WR-003, WR-006 | DONE |
| WR-008 | [Coordinator and vertical slice](WR-008-coordinator-vertical-slice.md) | WR-004, WR-005, WR-006, WR-007 | DONE |
| WR-009 | [Native shell and UI system](WR-009-native-shell.md) | WR-008 | DONE |
| WR-010 | [Library view and local actions](WR-010-library-view.md) | WR-007, WR-009 | DONE |
| WR-011 | [Player, timeline, drawing, and viewpoints](WR-011-player-timeline.md) | WR-002, WR-007, WR-008, WR-009 | DONE |
| WR-012 | [Settings, native choosers, and app status](WR-012-settings-portals-status.md) | WR-002, WR-003, WR-006, WR-008, WR-009 | DONE |
| WR-014 | [Flatpak release-candidate pipeline and permanent remote](WR-014-flatpak-release.md) | WR-002, WR-010, WR-011, WR-012 | BLOCKED (WR-015 fixes superseded the disposable signed candidate; project signing environment/key absent) |
| WR-013 | [Migration release and Electron cutover](WR-013-electron-dead-code-cutover.md) | WR-014 | BLOCKED (final AppImage publication and existing-install verification are absent; cutover candidate is ready) |
| WR-015 | [Parity/lean gates and stable publication](WR-015-release-gates.md) | WR-013, WR-014 | BLOCKED (local measurable gates pass; signing, manual duration/UI gates, maintainer sign-off, and WR-013 publication remain) |

Safe parallel work: WR-004 and WR-006 after WR-003; WR-010 and WR-011 after WR-009; WR-012 when its dependencies are done. Ticket numbers do not imply execution order: the dependency table intentionally runs WR-014's release pipeline before WR-013 cutover, then WR-015 publishes stable only after final gates pass.

WR-009 and WR-010 are complete. Their implementation commits are `9ddb52a7` and `5d489296`, respectively; the standard native checks pass, including the UI and vertical-slice test suites.

## Rules for implementation agents

1. Change only ticket-owned files plus the explicitly allowed nearby tests/mechanical lockfiles.
2. Copy the ticket acceptance criteria into the work log or PR description before coding.
3. Consult current TypeScript to establish behavior and reuse factual tables/assets; do not port its architecture line by line.
4. Search the new code before adding a helper, type, asset, or dependency.
5. Do not add dependencies outside the approved list without documenting the missing capability, rejected native/stdlib alternatives, license, payload effect, and maintainer approval.
6. The GTK thread performs no recursive scan, process wait, media probe, log parsing, transcoding, or blocking file copy.
7. A ticket is `DONE` only when its named automated checks and manual acceptance checks pass inside the Flatpak when applicable.
8. If a real API differs from a snippet, use the smallest current equivalent and update the ticket. Do not build a compatibility wrapper solely to preserve the snippet.
9. Preserve unrelated working-tree changes and research artifacts.
10. After WR-002, Cargo dependency changes also update `native/Cargo.lock` and `flatpak/cargo-sources.json` using WR-002's command.
11. Visual decisions not covered by `UI-BRIEF.md` are added there in the same change, not improvised in widget code.
12. Tests cover observable contracts and realistic failure boundaries. Do not add combinatorial tests, mocks of GTK internals, exact timing assertions, or recovery for states this architecture cannot create.

## Complexity budget

- production Rust plus UI definitions: target 12,000 lines; mandatory deletion/design review above 18,000;
- direct Cargo dependencies: target 14; mandatory review above 16;
- no second binary, daemon, webview, async runtime, generic repository layer, service locator, plugin API, worker pool, or home-grown media pipeline;
- no thumbnail extraction/cache, speculative notification system, or compatibility layer;
- one coordinator thread, one serialized media/storage worker, and the narrowly owned tray service thread required by StatusNotifierItem.

Crossing a threshold requires a maintainer-approved explanation and a deletion pass before WR-013. Transitive crates are reviewed for licensing and payload but are not counted as direct dependencies.

## Standard verification

Run from the repository root:

```bash
cargo fmt --manifest-path native/Cargo.toml --check
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path native/Cargo.toml --all-targets
cargo build --manifest-path native/Cargo.toml --release
```

Flatpak-facing tickets also build and exercise the relevant behavior inside the sandbox. Host-only success is insufficient for capture, playback, choosers, file launching, or packaging.

## Performance budgets

WR-000 defines the deterministic 2,000-sidecar corpus; WR-015 measures on one named machine. Release gates are absolute — no comparative Electron baseline is measured:

- installed application payload, excluding the shared runtime: below 100 MiB;
- idle RSS after loading 2,000 entries: below 150 MiB;
- cold start to interactive library with 2,000 generated sidecars: below 1.0 second on the gate machine;
- filter/sort update over 2,000 entries: below 50 ms;
- no GTK-main-thread stall above 100 ms during library load or background media work.

Measure only in WR-015 and optimize only a demonstrated failing path. These budgets are not permission to add caches or frameworks pre-emptively.
