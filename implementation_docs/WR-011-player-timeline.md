# WR-011: Player, combat timeline, drawing, clips, and viewpoint selection

## Goal

Integrate the WR-002 Clapper backend into the persistent player pane and preserve current transport, timeline, drawing, clipping, and viewpoint selection with the least custom media code possible.

> **Scope change (maintainer, 2026-07-22):** multi-POV grid playback
> (synchronized 2–4 player grid, master audio, drift correction) is removed
> from the product — recorded as `REMOVE_OBSOLETE` in the WR-000 parity
> matrix. The single-view viewpoint selector, POV correlation, and the
> local single-view viewpoint selection remains. Kill-video montage was also removed by maintainer decision.

## Dependencies

WR-002, WR-007, WR-008, and WR-009 must be `DONE`.

## Owned files

- `native/src/ui/player.rs`
- `native/src/ui/timeline.rs`
- `native/src/ui/drawing.rs`
- `native/src/ui/multipov.rs`
- `native/src/ui/mod.rs` module wiring
- edits to `native/src/ui/player_backend.rs` only for proven product operations missing from WR-002
- focused player/timeline UI tests
- `implementation_docs/reports/wr-011-player-evidence.md`

## Single-view player

1. Embed `ClapperGtkVideo` in the top pane and use its concrete Player directly through WR-002's thin backend. Do not build another trait/backend, poll GStreamer buses yourself, or reproduce loading/seek state already exposed by Clapper.
2. When table selection changes, stop the old item, clear drawings/clip mode, load the preferred/default local POV, and seek to zero (or the retained same-activity progress when changing POV). Empty/invalid selection shows the placeholder; playback failure shows one recovery action and leaves the library usable.
3. Implement the UI-BRIEF player controls: play/pause, seek, time text, volume/mute, fullscreen, speed 0.25×/0.5×/1×/2×, marker visibility controls, clip mode/Create, drawing toggle/exposed tools, Reveal, and POV mode/selection.
4. Persist marker visibility through WR-003 config. Volume/mute and the divider remain shared session state across selected videos, matching current behavior. Speed, position, drawings, clip range, and current multi-view selection are also session-only unless WR-000 proves otherwise.
5. Seeking is asynchronous. While a seek is pending, keep the newest requested target and update presentation from the player position; do not create a seek queue or promise/callback framework. Ten rapid seeks must settle near the final requested target. Acceptance is directional/usable, not exact frame/timing equality.
6. Keyboard/pointer behavior matches UI-BRIEF and WR-000. Ignore player shortcuts while focus is in an editable field/dialog. Comma uses the current approximate previous-frame behavior (known FPS when present, otherwise WR-000's 30 fps assumption); Period uses Clapper frame advance while paused. Do not promise codec-independent frame accuracy beyond current behavior.

## Timeline and drawing

- Implement one custom `GtkWidget`/`GtkDrawingArea` for the seek track because combat spans/markers and clip handles are product-specific. Draw directly from `TimelineItem`s; no canvas/rendering crate.
- Convert offsets to pixels at draw/hit-test time. Show current position, activity spans, deaths, encounter/round markers, and clip range in stable lanes/colors. Hover/focus label and clicking/keyboard seeking must work.
- Visibility preferences dispatch `SetMarkerVisibility` and filter drawn items only; they do not mutate stored metadata.
- Clip mode adds start/current/end handles constrained to `0 ≤ start < end ≤ duration`, initialized to the current baseline range. `Create` sends one command and exits mode only after accepted; progress/errors come from snapshot.
- Drawing overlay stores a small tagged item list in normalized video coordinates. Implement exactly WR-000's exposed selection/move, freehand, line/arrow, rectangle/diamond/ellipse, text, eraser, stroke controls, undo/redo, and clear set using Cairo/Pango/GTK input; omit any tool the baseline proves hidden. Keep a simple bounded undo/redo stack for this session only. Pointer editing occurs only when enabled; playback controls receive input otherwise. Toggle-off/media change follows the baseline clear/retain behavior. No image import, files, export, collaboration, scene format, or third-party drawing library.

## Viewpoint selection

The single-view selector lists each distinct local POV with player/spec and remembers the preferred player for subsequent rows during the current process, matching the current `preferredViewpoint` session state. Grid/synchronized multi-POV playback is removed (see the scope change above); do not build it.

## Acceptance criteria

- WR-000 real H.264 and AV1 files play with audio, seek, mute/volume, all speed values, approximate previous/next frame, fullscreen, and reopen inside Flatpak.
- All keyboard/pointer shortcuts and Reveal work; focus in search/tag/name prevents transport shortcuts.
- Timeline markers/spans/visibility/navigation and clip boundaries match legacy sidecar goldens; created clip is playable and indexed.
- Drawing remains aligned after resize/fullscreen and clears on media change.
- The viewpoint selector opens the chosen POV, retains same-activity progress, and remembers the preferred player for the session.
- No custom GStreamer pipeline/bus state machine, media HTTP server, drawing dependency, generic sync layer, or exact-time/flaky performance assertion exists.

## Tests and evidence

Pure tests cover timeline hit-testing/visibility, clip bounds, normalized drawing item hit-testing/undo, and POV selection. Use one thin action-routing UI test. Manual evidence covers the real-media matrix, all controls/shortcuts, every retained drawing tool through resize/fullscreen, viewpoint switching, and clips. Do not test every shape/color combination, mock Clapper internals, or repeat WR-002 codec proof as unit tests.

## Complexity check

At review, report production lines in these files. Above 1,800 lines requires a deletion/simplification pass and explanation; splitting files or adding abstractions does not satisfy the check.

## Not in scope

Cloud playback/download, arbitrary video layouts, subtitles, playlists, persisted annotations, frame-perfect editing, new montage effects/formats, or background playback/tray controls.
