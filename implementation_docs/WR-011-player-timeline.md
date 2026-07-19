# WR-011: Player, combat timeline, drawing, clips, and local multi-POV

## Goal

Integrate the WR-002 Clapper backend into the persistent player pane and preserve current transport, timeline, drawing, clipping, multi-view playback, and kill-video editing with the least custom media code possible.

## Dependencies

WR-002, WR-007, WR-008, and WR-009 must be `DONE`.

## Owned files

- `native/src/ui/player.rs`
- `native/src/ui/timeline.rs`
- `native/src/ui/drawing.rs`
- `native/src/ui/multipov.rs`
- `native/src/ui/kill_video.rs`
- `native/src/ui/mod.rs` module wiring
- edits to `native/src/ui/player_backend.rs` only for proven product operations missing from WR-002
- focused player/timeline UI tests
- `implementation_docs/reports/wr-011-player-evidence.md`

## Single-view player

1. Embed `ClapperGtkVideo` in the top pane and use its concrete Player directly through WR-002's thin backend. Do not build another trait/backend, poll GStreamer buses yourself, or reproduce loading/seek state already exposed by Clapper.
2. When table selection changes, stop the old item, clear drawings/clip mode, load the preferred/default local POV, and seek to zero (or the retained same-activity progress when changing POV). Empty/invalid selection shows the placeholder; playback failure shows one recovery action and leaves the library usable.
3. Implement the UI-BRIEF player controls: play/pause, seek, time text, volume/mute, fullscreen, speed 0.25×/0.5×/1×/2×, marker visibility controls, clip mode/Create, drawing toggle/exposed tools, Reveal, and POV mode/selection. The kill-video dialog opens from WR-010's Raid Creator action, not a new player control.
4. Persist marker visibility through WR-003 config. Volume/mute and the divider remain shared session state across selected videos, matching current behavior. Speed, position, drawings, clip range, and current multi-view selection are also session-only unless WR-000 proves otherwise.
5. Seeking is asynchronous. While a seek is pending, keep the newest requested target and update presentation from the player position; do not create a seek queue or promise/callback framework. Ten rapid seeks must settle near the final requested target. Acceptance is directional/usable, not exact frame/timing equality.
6. Keyboard/pointer behavior matches UI-BRIEF and WR-000. Ignore player shortcuts while focus is in an editable field/dialog. Comma uses the current approximate previous-frame behavior (known FPS when present, otherwise WR-000's 30 fps assumption); Period uses Clapper frame advance while paused. Do not promise codec-independent frame accuracy beyond current behavior.

## Timeline and drawing

- Implement one custom `GtkWidget`/`GtkDrawingArea` for the seek track because combat spans/markers and clip handles are product-specific. Draw directly from `TimelineItem`s; no canvas/rendering crate.
- Convert offsets to pixels at draw/hit-test time. Show current position, activity spans, deaths, encounter/round markers, and clip range in stable lanes/colors. Hover/focus label and clicking/keyboard seeking must work.
- Visibility preferences dispatch `SetMarkerVisibility` and filter drawn items only; they do not mutate stored metadata.
- Clip mode adds start/current/end handles constrained to `0 ≤ start < end ≤ duration`, initialized to the current baseline range. `Create` sends one command and exits mode only after accepted; progress/errors come from snapshot.
- Drawing overlay stores a small tagged item list in normalized video coordinates. Implement exactly WR-000's exposed selection/move, freehand, line/arrow, rectangle/diamond/ellipse, text, eraser, stroke controls, undo/redo, and clear set using Cairo/Pango/GTK input; omit any tool the baseline proves hidden. Keep a simple bounded undo/redo stack for this session only. Pointer editing occurs only when enabled; playback controls receive input otherwise. Toggle-off/media change follows the baseline clear/retain behavior. No image import, files, export, collaboration, scene format, or third-party drawing library.

## Multi-POV playback

Preserve the current local behavior for a correlated activity:

1. Single-view selector lists each distinct local POV with player/spec and remembers the preferred player for subsequent rows during the current process, matching the current `preferredViewpoint` session state.
2. Grid mode allows two to four distinct local POVs, laid out 2 columns × 1 row for two and 2 × 2 for three/four. Do not support arbitrary counts/layouts.
3. The first selected POV is master for position, controls, timeline, and audio; other players are muted. Play/pause/seek/speed commands go directly to all active players.
4. On entering grid mode choose the first two baseline-sorted POVs. Pause during seeks until all players report ready, then resume only if previously playing. Correct drift only when observed difference exceeds WR-000's tolerance, by seeking lagging players to master; one periodic GTK timeout is enough. Do not build clock synchronization, a media server, or background sync thread.
5. Clip and drawing are disabled in grid mode exactly as current behavior; fullscreen remains available.

## Kill-video editor

When at least two correlated local POVs exist, open one native dialog matching WR-000:

- source list and a single ordered segment track initialized with the current equal/default allocation;
- embed one reused Clapper preview player: play/pause, mute, ruler/playhead scrubbing, and switch/seek to the active segment's source without rendering a composed preview;
- reorder sources, resize adjacent boundaries while redistributing duration, allow removal only above two sources, and disallow gaps/overlap/out-of-range segments; Reset restores the complete initial source list and defaults—there is no arbitrary Add source control;
- toggle single audio track and choose its source; when disabled, audio follows video segments;
- current FPS choices (10/20/30/60), current resolution choices, Reset, Render/Cancel, CPU-intensive warning, progress, and automatically named completion appearing in Clips.

The editor only builds `CreateKillVideo` payloads; FFmpeg argv/work remains in WR-007/008. The single-source preview is not a rendered montage preview. Do not add transition choices, quality presets, composed preview rendering, saved projects, undo history, or parallel jobs.

## Acceptance criteria

- WR-000 real H.264 and AV1 files play with audio, seek, mute/volume, all speed values, approximate previous/next frame, fullscreen, and reopen inside Flatpak.
- All keyboard/pointer shortcuts and Reveal work; focus in search/tag/name prevents transport shortcuts.
- Timeline markers/spans/visibility/navigation and clip boundaries match legacy sidecar goldens; created clip is playable and indexed.
- Drawing remains aligned after resize/fullscreen and clears on media change.
- Two/four POV grid layout, master-only audio, coordinated transport, seek pause/readiness, drift correction, and return to single POV match current behavior.
- Raid Creator gating plus kill-video preview play/mute/scrub/source switching, segment editing/removal/Reset, output choices, progress, and WR-007 two/three-source Clips result match baseline.
- No custom GStreamer pipeline/bus state machine, media HTTP server, drawing dependency, generic sync layer, or exact-time/flaky performance assertion exists.

## Tests and evidence

Pure tests cover timeline hit-testing/visibility, clip bounds, normalized drawing item hit-testing/undo, POV selection/layout, and kill-video segment/output-option validation. Use one thin action-routing UI test. Manual evidence covers the real-media matrix, all controls/shortcuts, every retained drawing tool through resize/fullscreen, 2/4 POV sync, clip, and montage including Reset/FPS/resolution/audio. Do not test every shape/color combination, mock Clapper internals, or repeat WR-002 codec proof as unit tests.

## Complexity check

At review, report production lines in these files. Above 1,800 lines requires a deletion/simplification pass and explanation; splitting files or adding abstractions does not satisfy the check.

## Not in scope

Cloud playback/download, arbitrary video layouts, subtitles, playlists, persisted annotations, frame-perfect editing, new montage effects/formats, or background playback/tray controls.
