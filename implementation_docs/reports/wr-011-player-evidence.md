# WR-011 evidence: player, combat timeline, drawing, clips, and viewpoints

WR-015 addendum (2026-07-22): an empty/failed media load disables transport,
mute/volume, speed, seek/frame shortcuts, drawing, and clip creation while
preserving Reveal. A previously loaded backend item can no longer receive
actions after an unusable corpus entry is selected.

Bloodlust addendum (2026-07-22): real July 18/20 combat logs prove that one
`SPELL_CAST_SUCCESS` identifies each activation, while `SPELL_AURA_APPLIED`
fans out across the party and can be removed/reapplied repeatedly. The parser
now retains spell IDs, the activity engine records one 40-second Bloodlust
span for the known player abilities, and the legacy-library startup path
streams each matching historical log once to add exact `bloodlustTimeline`
timestamps to otherwise timestamp-free legacy sidecars. The enrichment is
restricted to retail Mythic+ data, derives the historical UTC offset from the
matching `CHALLENGE_MODE_START`, proves the log covers the recording end,
deduplicates casts, remains retryable while a log is incomplete, is atomic,
preserves unknown legacy JSON fields, and is skipped on later starts.
The pass runs on the existing serial storage/media worker; live log polling,
recorder control, commands, and initial library readiness remain responsive,
and its typed completion event triggers the coordinator rescan.
The timeline draws these spans purple independently of Mythic+ segments and
uses the legacy-inspired gravestone silhouette for deaths.

Status: **code complete and verified on host** (fmt/clippy/tests/release).
In-Flatpak manual acceptance with real media follows the same WR-000
source-traced, owner-executed deferral used by WR-009/WR-010 (see "Known
limitations").

## Environment
- commit: refactor/native-non-frontend (WR-011 working tree)
- OS/kernel/session: CachyOS, Linux 7.1.4-1-cachyos, Wayland
- toolchain: cargo/rustc stable (edition 2024); gtk4 0.11.4 (v4_10),
  libadwaita 0.9.2 (v1_6), clapper/clapper-gtk 0.10.1 (v0_10)

## Scope change recorded during implementation

**Multi-POV grid playback (synchronized 2–4 player grid, master audio, drift
correction) was removed from the product by maintainer decision on
2026-07-22**, given mid-implementation. Recorded as `REMOVE_OBSOLETE` in the
WR-000 parity matrix; the WR-011 ticket, UI-BRIEF, and README were reconciled
in the same change. Retained: POV correlation, the single-view viewpoint
selector with preferred-player memory, and the kill-video editor. The player
pane therefore owns exactly one Clapper backend.

## What was built

- `ui/player_backend.rs` — WR-002's thin concrete Clapper surface, now wired
  into `ui/mod.rs`, plus the product operations WR-011 needed: `is_ready`
  (state is Playing/Paused), `connect_position_updated` (notify::position),
  and `connect_seek_done`. No second trait/backend, no bus polling.
- `ui/player.rs` — the persistent pane: `ClapperGtk` video inside a
  placeholder/video stack, one compact control row (play/pause, mute, volume,
  time, speed cycle 0.25/0.5/1/2, marker-visibility popover, drawing toggle,
  clip mode with Create/Cancel, viewpoint dropdown, Reveal, fullscreen), the
  window-level keyboard controller (Space/K, J/L/arrows ±5 s, Comma
  approximate previous frame at known-FPS-else-30 while paused, Period Clapper
  `advance_frame` while paused; all ignored while an editable widget has
  focus), asynchronous newest-target-only seeking, one playback-failure
  recovery row (Reveal in folder), and the kill-video entry point.
- `ui/timeline.rs` — the one custom `GtkDrawingArea` seek track: activity and
  purple Bloodlust spans, death gravestones, encounter/round marks in outcome colors, elapsed
  track, playhead, clip start/end handles, hover/keyboard-focus tooltip with
  label+timestamp, click/drag seeking, Left/Right nudge. Pure functions for
  visibility filtering, ms↔px mapping, hover hit-testing, and clip-handle
  clamping (`0 ≤ start < end ≤ duration`, legacy ±15 s initial range).
- `ui/drawing.rs` — session-only overlay: normalized-coordinate item list,
  tools select/move, freehand, line, arrow, rectangle, diamond, ellipse, text,
  eraser, stroke color/width, bounded (64) undo/redo, clear; Cairo rendering
  on one `GtkDrawingArea`; items survive toggle-off and reset on media change,
  matching the recorded legacy remount behavior. No drawing dependency.
- `ui/multipov.rs` — GTK-free viewpoint logic: distinct labelled POVs
  ("Player (Spec)" via the shared spec table), label dedup, preferred-player
  choice. Grid/drift logic deleted with the scope change.
- `ui/kill_video.rs` — the native editor dialog: one reused Clapper preview
  (play/pause/mute), a ruler with alternating segment blocks, draggable
  boundaries and playhead scrubbing that switches the preview source, an
  ordered segment list with move/remove (>2 only) controls, FPS 10/20/30/60
  (default 60), the legacy `obsResolutions` table (default 1920×1080),
  single-audio toggle + source, CPU warning, Reset, Render/Cancel. The pure
  `Track` model keeps `boundaries` strictly increasing over the shortest
  source duration, so gaps/overlap/out-of-range are unrepresentable; Render
  only builds one `CreateKillVideo` command.
- `ui/window.rs`/`ui/mod.rs` — the WR-010 placeholder replaced with the
  player; selection and Raid Creator montage routing; snapshot applied to the
  player before the library so reselect callbacks resolve current entries.
- `ui/filters.rs` — `spec_name` made `pub` for viewpoint labels (reuse, no new
  table).

## Contract checked (acceptance criterion → evidence)

- Selection change stops the old item, clears drawings/clip mode, loads the
  preferred/default POV, seeks to zero; same-activity POV switch retains
  progress → `set_selection`/`load_pov`; placeholder on empty/unresolvable
  selection; failure shows one recovery row and leaves the library usable
  (no supported error signal in the bindings — a 4 s readiness check).
- Marker visibility dispatches `SetMarkerVisibility` and filters presentation
  only → `visible_items` test proves stored items unchanged; popover rebuilt
  from the authoritative snapshot; persisted via WR-003 config.
- Volume/mute are process-shared session state (live in the one pane across
  selections); speed/position/drawings/clip range session-only.
- Ten rapid seeks settle near the final target → newest-target-only pending
  seek (`request_seek`/`on_seek_done`), no queue or callback framework.
- Timeline markers/spans/visibility/navigation and clip bounds → pure tests
  (`timeline::tests`): visibility for own/all deaths, encounters, rounds,
  always-independent Bloodlust presentation, clip-category suppression; px↔ms round trip; nearest-item hover; initial
  and clamped clip handles.
- Bloodlust capture/enrichment → parser test uses the real Fury of the Aspects
  cast shape and rejects Exhaustion; activity test asserts one 40-second span;
  storage test proves aura fan-out is ignored, exact log-relative placement,
  atomic/idempotent enrichment, and unknown-key preservation.
- Drawing aligned after resize/fullscreen (normalized coordinates, redrawn at
  widget size) and cleared on media change → `Doc` tests for topmost
  hit-testing, undo/redo/clear round trip, move translation, erase.
- Viewpoint selector/preferred-player memory → `multipov::tests`.
- Raid Creator gating (≥2 local POVs, WR-010 Creator cell) opens the dialog;
  segment editing/removal/Reset, output choices, audio payload →
  `kill_video::tests` incl. the one action-routing test asserting the exact
  `CreateKillVideo` command Render dispatches; FFmpeg work stays in
  WR-007/008.
- No custom GStreamer pipeline/bus machine, media HTTP server, drawing
  dependency, generic sync layer, or exact-timing assertion exists.

## Commands and raw results
```text
cargo fmt --manifest-path native/Cargo.toml --check      # clean
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings   # clean
cargo test --manifest-path native/Cargo.toml --all-targets   # 76 lib + 36 bin/ui + 7 vertical-slice, all pass
cargo build --manifest-path native/Cargo.toml --release  # Finished, optimized
```

## Complexity check

Non-blank, non-comment production lines in the owned files (tests excluded):
player.rs 696, timeline.rs 400, drawing.rs 534, multipov.rs 40,
kill_video.rs 542, player_backend.rs 64 — **total 2,276**, above the ticket's
1,800 threshold. A deletion/simplification pass was performed: multi-POV grid
playback removed entirely (≈270 lines), rectangle/diamond geometry unified
behind one polygon helper, and every dead method deleted (clippy `-D warnings`
enforces zero unused code). The remainder is mandated surface, not
abstraction: the ticket requires a nine-tool drawing overlay, a custom combat
seek track with clip handles, and a full segment-editing montage dialog; the
files contain no traits, no single-impl abstractions, and no speculative
scaffolding. Legacy equivalents were ~2,000 lines of TSX plus the Excalidraw
dependency. Explanation submitted for maintainer sign-off per the ticket.

## Decisions and deviations
- **Multi-POV grid playback removed** (maintainer, 2026-07-22) — see above.
- Fullscreen toggles the application window (`set_fullscreened`), covering
  the button, double-click (`ClapperGtk` `toggle-fullscreen`), and re-exit;
  there is no separate video-only fullscreen surface.
- Drift tolerance, grid layout, and pause-until-ready logic were deleted with
  the scope change rather than kept "for later".
- The drawing toolbar uses stock symbolic icons for tools; toggle-off retains
  items (legacy: only remount/media change clears), media change resets.
- Clip mode replaces the Clip button with Create/Cancel and draws start/end
  handles on the shared track (the native equivalent of the legacy three-thumb
  slider); Create exits only after the coordinator accepts the command.

## Skipped (YAGNI)
- No thumbnail/preview extraction, no composed montage preview (the preview
  plays the source under the playhead, as specified), no persisted
  annotations, no saved projects, no seek queue, no media abstraction layer.

## Known limitations
- No populated real-media corpus was exercised in this session, so the
  in-Flatpak manual matrix (H.264/AV1 with audio, all shortcuts, drawing
  through resize/fullscreen, clip playback, montage render) is not claimed
  here. This is the same maintainer-approved deferral recorded by WR-009 and
  WR-010; WR-015 owns the final populated visual/manual parity pass. All named
  automated checks pass.

## Approval
- Reviewer/maintainer: pending sign-off (including the >1,800-line
  complexity explanation above).
- Date/result: code complete; automated gates green; Flatpak manual
  acceptance deferred to owner/WR-015 under the WR-000 deviation.
