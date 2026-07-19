# ADR-001: Native Rust, GTK, Clapper, and Flatpak

- Status: accepted for implementation
- Decision owners: maintainers
- Scope: Linux/Wayland rewrite

## Context

The current application ships an Electron/React/Node stack around a local recorder. That stack is expensive in installed size, startup time, idle memory, packaging surface, and maintenance code. Previous webview/native experiments either retained compatibility layers or rebuilt media behavior with too much custom code.

The replacement must retain every reachable local-Linux function except localization, preserve the product's recognizable player-plus-library workflow, and substantially reduce code and runtime weight.

## Decision

Build one Rust application using:

- GTK4 and libadwaita for the window, responsive navigation, settings, native list/table widgets, dialogs, accessibility, and theme integration;
- `GtkColumnView`/GTK selection, filter, and sort models for the virtualized category library;
- `GtkPaned` for the persistent resizable player-above-table workflow;
- Clapper and ClapperGtk as the native GTK/GStreamer playback layer;
- `gpu-screen-recorder` as one supervised child process for Wayland capture and replay buffering;
- one minimal pinned FFmpeg executable, called with `std::process::Command`, for retained stream-copy clips and multi-source kill-video transforms;
- GLib/GIO/GTK launchers and file dialogs for native/portal-backed open and selection operations;
- a minimal freedesktop StatusNotifierItem implementation for the current tray/background recording lifecycle on desktops with a watcher;
- JSON sidecars plus the filesystem as the library source of truth;
- Flatpak as the only native release format after the final AppImage migration release.

The Rust package remains under `native/`. Core domain code is GTK-free. One coordinator thread owns mutable domain state; one serialized worker performs media finalization, clipping, and kill-video jobs. A narrowly owned tray service thread sends only bounded Open/Quit events. Typed standard-library channels connect all three to the GTK thread; `main` owns and joins the coordinator and tray handles.

## Why Clapper instead of `gtk::Video` or a custom GStreamer pipeline

The current product exposes playback speed, seeking, keyboard transport, and frame stepping. GTK's basic media widget does not expose all of those controls. Dropping them violates parity; building a custom GStreamer bus/state/seek layer recreates the complexity this rewrite is meant to remove.

Clapper already provides a GTK video widget and player API for playback speed, seeking, and frame advance while remaining on the native GStreamer stack. Project code owns only Warcraft Recorder's controls, marker timeline, drawing overlay, and multi-POV selection. It must not wrap Clapper in a generic player abstraction or duplicate its internal state machine.

WR-002 is a hard platform gate: it pins compatible Clapper/ClapperGtk libraries and Rust bindings, verifies their licenses against the canonical project license, bundles them in the Flatpak, and exercises real legacy H.264 and AV1 recordings, audio, rapid seeking, 0.25x/2x speed, and frame advance. It also proves a minimal FFmpeg build can perform the current clip and transition/audio-filter montage operations within the payload budget. If either proof fails, implementation stops for an ADR revision; agents do not invent a fallback player/editor.

Primary API references to pin in WR-002:

- Clapper Player: <https://rafostar.github.io/clapper/doc/clapper/class.Player.html>
- ClapperGtk: <https://rafostar.github.io/clapper/doc/clapper-gtk/>

## Why GTK/libadwaita

GTK supplies the table virtualization, selection, filtering/sorting models, keyboard focus, accessibility, theming, file dialogs, launchers, split navigation, and resizable panes the product needs. Using those native facilities removes React component infrastructure and avoids custom widget libraries.

The refreshed UI deliberately preserves the existing category sidebar, prominent recording status, resizable player above a dense sortable table, dark product theme, class/spec imagery, and colored combat timeline. It does not adopt a generic dashboard, thumbnail-card grid, or a new information architecture.

## Why filesystem JSON, not a database

Existing recordings already use media plus sidecar JSON. The library is small enough for one background scan and an in-memory index. A database would add migration, synchronization, and recovery code without a measured requirement. Writes use temp-file-plus-rename; WR-007 covers only states those writes and media finalization can actually produce.

## Why Flatpak only

Capture and playback depend on native libraries, binaries, and permissions. A pinned Flatpak manifest makes those dependencies reproducible and provides the intended install/update path. The final Electron/AppImage release carries the migration notice before old packaging is removed; the native application does not contain an AppImage updater or package-manager abstraction.

## Rejected alternatives

- Electron/Tauri/webviews: retain a browser engine, frontend toolchain, or bridge layer.
- A localhost range server: unnecessary with native playback.
- Raw `gtk::Video`: cannot meet the retained player-control contract.
- A project-owned GStreamer pipeline/state machine: too much code and failure surface when Clapper provides the platform feature.
- A Rust FFmpeg/media-editing framework: the retained transforms need one deterministic argv builder and serialized process, not a second media architecture.
- SQLite: no measured library scale or query requirement justifies it.
- Tokio/async workers/event bus: the workload has a small, known set of owners and blocking OS operations.
- Thumbnail extraction/cache: not present in the current workflow and adds media work, storage, invalidation, and UI complexity.
- Root-level Cargo workspace or multiple crates/binaries: no independent deliverable needs them.

## Consequences and guardrails

- Flatpak must bundle pinned Clapper/ClapperGtk, `gpu-screen-recorder`, and the minimal FFmpeg features not supplied by the runtime.
- Direct dependencies need license and payload review. No dependency is approved merely because it appears in an example.
- Long filesystem/media/process work never runs on the GTK thread.
- Domain snapshots and commands are concrete product types, not a generic message envelope.
- Multi-POV correlation and kill-video creation remain because they are reachable for multiple local recordings. Disabled cloud paths are removed and must not shape the new model.
- Tray Open/Quit and hide-to-background remain because current defaults rely on them to keep automatic capture running. WR-002 must prove the smallest StatusNotifierItem binding/permissions and a safe no-watcher fallback; GTK4 has no tray API.
- English strings live next to their UI; no localization service survives.
- Performance is measured at the baseline and final gate, not asserted through fragile timing unit tests.

## Revisit conditions

Revisit this ADR only if WR-002 proves one of the chosen platform contracts cannot work inside the supported Flatpak runtime, the canonical license is incompatible with a required component, or WR-015 proves a release budget cannot be met after optimizing the measured hot path.
