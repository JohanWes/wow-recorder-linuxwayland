# WR-002: Development Flatpak and platform proofs

## Goal

Create the reproducible development Flatpak early and prove the two highest-risk native contracts—capture and feature-complete playback—inside the sandbox before domain/UI work expands.

## Dependencies

WR-001 must be `DONE`. WR-000 supplies the app ID, license, media samples, and capture contract.

## Owned files

- `flatpak/<app-id>.Devel.yml`
- `flatpak/cargo-sources.json`
- `flatpak/modules/` only for pinned component manifests/patches that cannot be expressed inline
- `data/<app-id>.desktop`
- `data/<app-id>.metainfo.xml`
- `data/icons/`
- `native/Cargo.toml`, `native/Cargo.lock`
- `native/src/ui/player_backend.rs`
- `native/src/ui/tray_backend.rs`
- the smallest temporary/probe wiring in `native/src/main.rs`; retain only code reused by WR-011
- `implementation_docs/reports/wr-002-platform-proofs.md`

## Implementation

### Manifest

1. Pin one supported GNOME runtime/SDK and Rust extension. Record exact versions and end-of-life date in the report.
2. Build the Cargo package offline from `cargo-sources.json`.
3. Bundle pinned `gpu-screen-recorder`, the exact Clapper/ClapperGtk native libraries/bindings selected below, and a minimal FFmpeg executable when the runtime does not supply compatible versions. The FFmpeg build enables only the demuxers/muxers/codecs/filters required by WR-000's existing clip and kill-video commands (including their H.264/AAC and transition/audio behavior). Do not bundle a second GStreamer stack or general-purpose codec suite.
4. Add only permissions proven by WR-000: Wayland, required GPU/device access, PipeWire/audio, StatusNotifier session-bus names, the folder-access mechanism, and one exact read-only `~/.config/<legacy-app-directory>` permission needed to import the known Electron config. There is no network permission: the native app performs no network I/O (updates are Flatpak-owned). Prefer chooser grants over broad home access. Explain every `finish-args` line; the legacy-config permission is removed only in a later release after the documented migration support window, not during this rewrite.
5. Install desktop, metainfo, and icon files with the canonical app ID and license. Devel naming must not collide with release installations.

### Tray/background platform gate

1. Pin the smallest maintained StatusNotifierItem binding that works without an application-wide async runtime. First candidate is `ksni` with default features disabled and its blocking API; verify current API/license/dependency tree rather than assuming the candidate. It may own one joinable service thread. Do not enable Tokio or bundle GTK3/libappindicator.
2. Implement a concrete `TrayBackend` exposing only availability, Open activation, Quit activation, status/title update if baseline uses it, and shutdown. Menu contains Open and Quit; activation/double-click equivalent opens the window.
3. Inside Flatpak, prove registration/activation/menu with a StatusNotifierWatcher and the exact narrow session-bus permissions. Prove watcher absence/offline is a soft state: the main window must remain recoverable and close/minimize must not hide it.
4. Hide the probe window while a real GSR buffer continues, reopen from tray, then Quit and verify GSR plus tray thread exit. Record payload/dependency cost. If the only working implementation requires a broad bus wildcard, GTK3 stack, or async runtime, stop for ADR/maintainer scope review rather than silently removing tray behavior.

### Playback platform gate

1. Pin mutually compatible Clapper, ClapperGtk, and Rust bindings. Verify license identifiers from their canonical source, not a crate-index summary, and record compatibility with WR-000's project license.
2. Add a thin concrete `PlayerBackend` in `ui/player_backend.rs`. It owns the Clapper player/video objects needed by GTK and directly exposes only the operations used by this product: open local URI, play/pause, position/duration, seek, set volume/mute, set speed, advance one frame, and stop. Do not define a trait, generic backend, bus thread, retry queue, or custom playback state machine.
3. Wire a development-only probe window or command-line switch that embeds the real ClapperGtk video widget and invokes those operations. Ensure production builds do not expose a mystery user-facing menu item; the wrapper itself remains for WR-011.
4. Inside the Flatpak, test WR-000's real legacy H.264 and AV1 media with audio: open/play, seek near start/middle/end, ten rapid seeks, mute/volume, 0.25×/1×/2× speed, pause and frame advance, close/reopen. Record results and any codec modules added.
5. If speed/frame APIs, real codec playback, or GTK embedding fails, stop and mark the ticket blocked for ADR review. Do not fall back to `gtk::Video`, an external player, or project-owned GStreamer pipeline.

### Capture platform gate

Inside the Flatpak, exercise WR-000's exact `gpu-screen-recorder` arm/buffer, save replay, stop, audio, capture-target selection token, reselection, and shutdown behavior using a disposable output directory. This may initially be a documented shell invocation; do not implement WR-006 early. Verify both permission denial and successful capture produce understandable evidence.

### Folder authorization gate

Prove the exact read-only legacy config permission can import but not modify that file. Use `GtkFileDialog` to select representative WoW log, recording, and separate replay directories outside private data. Store the authorized path, restart, then prove Rust plus sandboxed GSR/FFmpeg access. Also prove an imported absolute data path never selected is denied and replaceable without modifying legacy config. If document-portal folder grants are not persistent/child-visible, choose the narrowest evidence-backed static permissions for actual WoW/default roots and require chooser reauthorization for arbitrary custom roots; never use blanket `home`/`host`.

### Media-transform platform gate

Using the same minimal FFmpeg build that will ship, run the current stream-copy/re-encode clip case and one short two-source kill-video command containing the retained trim/transition/audio filters. Verify playable output and record executable/library payload size, enabled build flags, license, argv, input/output hashes, and probe results. If a required filter/codec or the README payload budget cannot be satisfied, stop for ADR review; do not defer discovery to WR-007.

### Dependency source update

Document one exact command that regenerates `flatpak/cargo-sources.json` from `native/Cargo.lock`. Later dependency-adding tickets must use it.

## Acceptance criteria

- `flatpak-builder --force-clean` succeeds from a clean checkout with downloads permitted at build preparation and Cargo offline during the build.
- The installed Devel app launches on Wayland and its app ID/desktop/metainfo validate.
- All manifest permissions have a behavior/evidence row; no blanket `home`, host filesystem, X11 fallback, session-bus wildcard, or network permission exists.
- Selected folder grants persist across app restart and work for Rust plus GSR/FFmpeg; inaccessible imported paths require explicit replacement and cannot trigger scan/eviction.
- Real H.264 and AV1 playback, audio, speed, seeking, and frame advance pass inside the sandbox.
- A real sandboxed replay capture is playable and capture-target reselection is proven.
- The minimal FFmpeg build creates a real clip and two-source transitioned montage inside the sandbox and its payload/license are recorded.
- Tray Open/Quit/background capture works with a watcher; the no-watcher fallback cannot strand an invisible process.
- The report records exact versions, licenses, commands, sample hashes, and results; no speculative fallback code is committed.

## Verification

Run the standard Rust checks, `flatpak-builder-lint manifest`, `flatpak-builder-lint appstream`, a clean Flatpak build/install/run, and the manual playback/capture matrix above.

## Not in scope

Release signing/remote publication, production recorder orchestration, full player controls/timeline, new codecs/options, notifications, AppImage migration, or performance tuning.
