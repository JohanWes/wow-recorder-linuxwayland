# WR-001: Minimal Rust package scaffold

## Goal

Create the smallest buildable Rust/GTK application skeleton in its permanent `native/` location, with CI-quality formatting/lint/test commands and no premature architecture.

## Dependencies

WR-000 must be `DONE`, including the canonical license.

## Owned files

- `native/Cargo.toml`
- `native/Cargo.lock`
- `native/src/main.rs`
- `native/src/lib.rs`
- empty module files named in the target tree only when Rust compilation requires them
- root `.gitignore`
- existing CI workflow file, or one new native-check workflow if none can be extended cleanly

## Implementation

1. Create one Cargo package, edition 2024, with one binary and one library target. Do not create a workspace, build script, procedural macro, example binary, benchmark crate, or feature matrix.
2. Add only the direct dependencies needed by this scaffold:
   - `gtk4` and `libadwaita` versions compatible with WR-002's chosen GNOME runtime;
   - `serde` with derive and `serde_json` for contracts used by later modules;
   - `tracing` and `tracing-subscriber` for application logging.
3. Do not add Tokio, async-channel, anyhow, thiserror, chrono, uuid, directories, clap, a DI container, or test/mocking frameworks. Later tickets add an approved dependency only when their concrete behavior needs it. Standard `SystemTime`, `PathBuf`, `std::error::Error`, and channels are the defaults.
4. `main.rs` initializes tracing, GTK/libadwaita, creates one `AdwApplication` using the app ID recorded by WR-000, opens a simple titled `AdwApplicationWindow`, and runs. Keep setup direct; do not introduce `Application`, `Service`, or builder abstractions.
5. `lib.rs` declares the domain/core modules but contains no GTK import. Module files may contain only a module-level purpose comment until their ticket owns implementation.
6. Add narrow ignore rules for generated artifacts actually created by this work: `/target/`, `/native/target/`, `/spikes/**/target/`, `/spikes/**/work/`, and Flatpak build/export directories chosen by WR-002. Do not ignore all of `spikes/` or other user-owned source/evidence.
7. Extend CI to run the standard verification commands on Linux with the required GTK development packages. Reuse the existing workflow/job structure when it can express this in a few steps.
8. Add the canonical SPDX header/package metadata decided in WR-000. Do not copy conflicting legacy metadata.

## Acceptance criteria

- The native package stays under `native/`; no later ticket is expected to move it.
- Starting the binary opens one native window and cleanly exits when it closes.
- `native/src/lib.rs` compiles without GTK/libadwaita imports.
- Direct dependencies are limited to the six named packages (GTK and libadwaita bindings count separately).
- No unused abstraction, generic error layer, async runtime, or compatibility code exists.
- Generated `target`/Flatpak work directories no longer appear in `git status`, while source under `spikes/` remains visible.

## Verification

Run all standard commands from the root, then launch the debug binary once on Wayland and record the command/result.

## Not in scope

Flatpak packaging, real UI layout, coordinator threads/channels, config persistence, playback, capture, application icons beyond a placeholder supplied by packaging, or legacy code deletion.
