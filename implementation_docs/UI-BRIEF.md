# UI brief: Warcraft Recorder, native and faster

This brief is binding for WR-009 through WR-012. It preserves the current product's identity and working information architecture while replacing browser UI machinery with native GTK widgets. Do not add screens, cards, navigation items, animations, thumbnail systems, or settings unless required below or by WR-000's `KEEP` matrix.

## Product character

Warcraft Recorder is a focused desktop tool for recording and reviewing WoW activities. It should feel technical, calm, and game-specific—not like a generic analytics dashboard. The signature composition is the resizable video player directly above a dense activity table, with a compact category rail and a colored combat timeline linking the two.

Preserve these identity cues:

- deep charcoal surfaces with a restrained warm red/orange accent derived from the current app;
- existing class/spec/category/affix imagery where its license and provenance are approved by WR-000;
- dense, legible activity rows rather than card grids;
- success/failure colors used for outcomes and timeline events, not decoration;
- the current workflow: choose category, filter/table-select an activity, review it immediately above the table.

Use libadwaita's spacing, typography, focus, disabled states, and adaptive behavior. One compact project CSS resource may define colors, row density, timeline geometry, and drawing-overlay appearance. Do not recreate a design system.

## Main window

```text
┌──────────────────┬──────────────────────────────────────────────────────┐
│ Warcraft Recorder│ category title       search / filters       ⋮ menu  │
│ recording status ├──────────────────────────────────────────────────────┤
│                  │                                                      │
│ Raids            │               selected video / placeholder           │
│ Mythic+          │                                                      │
│ Arena            │ controls ───── colored combat timeline ────────────  │
│ ...retained cats  ├──────────── draggable horizontal divider ───────────┤
│ Clips            │ active filter chips      date range     clear        │
│ Manual           │ sortable GtkColumnView                            ↕  │
│                  │ [ ] ★ details | encounter/map | result | duration…   │
│ Settings         │ [ ]   …                                               │
│                  │ multiselect action bar when rows are selected        │
└──────────────────┴──────────────────────────────────────────────────────┘
```

- Use `AdwNavigationSplitView` for sidebar/content and a vertical `GtkPaned` inside the content.
- The player and table remain visible together on normal desktop sizes. Keep the divider position while the process runs, matching the current app; do not add persistent layout state.
- On narrow windows, libadwaita may collapse the sidebar; do not invent a mobile-specific layout.
- Do not add a speculative Home, Dashboard, or Recent destination. Selecting a category opens its newest matching recording by default, as today.
- Category order and visibility come from WR-000. `hide_empty_categories` hides only categories with zero entries; Manual and Clips follow the current behavior recorded there.

## Sidebar and status

- Top: product name/mark and a compact status card.
- Status card states: Setup required, Ready, Buffering/Armed, Recording with elapsed time and activity title, Finalizing, Test recording, Manual recording, and Error.
- When an automatic recording is active, the card exposes `Force end`. When manual recording is allowed, the Manual category exposes `Start recording`/`Stop recording`.
- Category rows use the approved existing category assets; generic actions use stock symbolic icons.
- Bottom: Settings. Put `Test recording…`, `Open logs`, and `About` in the primary window menu rather than adding permanent navigation destinations.
- There is no update UI: updates are delivered automatically by the Flatpak remote and the desktop's software center.

## Library table

Use `GtkColumnView` backed by GTK list/filter/sort/selection models. Do not make a thumbnail row or thumbnail cache. Rows remain compact and virtualized.

Shared row behavior:

- single click selects and loads the recording in the player;
- checkbox/multiselect mode supports bulk protection and deletion;
- star/protection toggle and tag edit are accessible without opening a detail page;
- context menu: Protect/Unprotect, Edit tag, Reveal in folder, Delete;
- all library entries remain reachable through scrolling/virtualization; pagination controls are not required.

Required category columns, matching the current table's useful information:

| Category family | Columns after selection/protection |
|---|---|
| Raid | Details, Encounter, Result, Pull, Difficulty, Duration, Date, Viewpoints, Creator |
| Mythic+ | Details, Dungeon, Result, Level, Affixes, Duration, Date, Viewpoints |
| Arena/Battleground/Solo Shuffle | Details, Map, Result, Duration, Date, Viewpoints |
| Clips | Details, Type, Source activity, Duration, Date, Viewpoints |
| Manual/unknown | Details, Type, Duration, Date |

WR-010 may omit a value when legacy metadata lacks it; it may not omit the column or fabricate a value. Clicking a sortable header cycles ascending/descending with a visible indicator. Default is newest first.

## Search, date filter, and bulk actions

Preserve the current suggestion-chip search rather than inventing free text or a query language.

- Typing narrows the exact metadata-derived suggestions recorded by WR-000 (player/class/spec, encounter/dungeon/map, tag, result, difficulty/level/affix, etc. where present) but does not filter rows until the user selects a suggestion. Tab accepts the active suggestion as today.
- A selected suggestion becomes a removable colored/icon chip. Rows match only when every selected encoded suggestion occurs in the entry or one of its correlated POVs (AND semantics). Do not add `field:value` parsing, arbitrary free-text substring matching, OR/groups, or invalid-query states.
- A native date-range popover supplies the same paired start/end range. Apply date filtering only when both endpoints exist and use the current inclusive date semantics; rely on the native control to keep range ordering valid.
- Show selected chips and the paired date control in the toolbar; each chip remains individually removable as today.
- With selected rows, replace the passive footer with a compact bar showing the count and Protect/Unprotect/Delete. Deletion always confirms the exact count and notes that media plus sidecars are permanent.

## Player and combat timeline

Use ClapperGtk's video widget with Warcraft Recorder's own compact control row. Required controls:

- play/pause, seek, elapsed/total time, mute, volume, fullscreen;
- speed menu/cycle containing 0.25×, 0.5×, 1×, and 2×;
- marker visibility toggles for deaths, encounter segments, and round boundaries;
- clip-range start/end and Create clip;
- drawing toggle and the WR-000-exposed edit tools;
- Reveal in folder;
- viewpoint selector when correlated local POVs exist;

The timeline is the primary distinctive visual element. It uses thin colored lanes/markers over one shared seek track: activity segments, deaths, encounter boundaries, round boundaries, and the current clip range. Hover/focus reveals a short label and timestamp. Clicking seeks. Visibility preferences only hide presentation; marker data remains stored.

Drawing remains an in-player analysis overlay but drops the Excalidraw dependency. Implement the tools WR-000 proves are exposed in the current build, using one `GtkDrawingArea` plus Cairo/Pango. Known minimum is selection/move, freehand, line/arrow, rectangle, diamond, ellipse, text, eraser, stroke color/width, undo/redo, and clear when those controls are present in the baseline. Items use normalized video coordinates and are session-only; they clear when the selected recording changes and are not loaded/saved/exported.

Multi-POV behavior remains local-only. Correlate entries with the same approved activity hash and start-time tolerance from WR-000. The selector shows player/spec and chooses one or multiple synchronized viewpoints according to the existing behavior. WR-011 must use one shared logical position and no generic synchronization framework.

The Raid table's Creator cell enables the kill-video editor only when at least two local correlated POVs exist, matching the current entry point. The editor retains the existing controls with a simpler native surface: one playable/scrubbable preview that switches to the source under the playhead, preview mute, ordered source segments on one duration track, drag/reorder, adjustable adjacent boundaries, removal only while more than two sources remain, FPS choices (10/20/30/60), current resolution choices, single-audio-track toggle/source, Reset restoring all initial sources/settings, Render, and cancel/progress. Output naming remains automatic. It creates a Clips entry through the one media worker. No source browser, transitions, or options beyond the baseline are added.

## Keyboard and pointer contract

When focus is not in a text field:

| Input | Action |
|---|---|
| Space or K | Play/pause |
| J / Left | Seek backward by current baseline interval |
| L / Right | Seek forward by current baseline interval |
| Comma / Period | Previous/next frame-equivalent step while paused |
| Double click video | Toggle fullscreen |

Do not require hover for an action. Tooltips supplement visible/accessibility labels; they do not replace them.

## Settings

Use one `AdwPreferencesDialog` with only pages justified by retained configuration:

1. Capture: codec, FPS, bitrate/quality, replay buffer, extra lead-in, cursor, RAM/disk replay storage, capture-target status and Reselect.
2. Audio: output and input device selectors populated by the recorder adapter.
3. Activities: retained category toggles and thresholds, log directories, validation state, test-recording action.
4. Storage & interface: recording directory, optional replay-buffer directory, storage limit, and hide-empty setting. Marker visibility stays beside the player where it is used.

Use `GtkFileDialog` folder selection and GIO/GTK file/URI launchers. No custom chooser, portal abstraction, background notification subsystem, or global shortcut daemon.

## Tray and background behavior

- Register the WR-002 StatusNotifierItem backend where a watcher exists. Menu: Open and Quit; primary activation opens/presents the window.
- Current minimize-to-tray and close-to-tray settings hide/pause player presentation while coordinator/GSR continue. Keep the application held while hidden.
- Quit is an explicit graceful shutdown. Closing/hiding is not Quit when the retained setting is enabled.
- If no watcher is available, never hide the only window: minimize normally and make close perform/confirm graceful quit according to baseline. Show one nonpersistent explanation in Settings/status; do not add notifications.

## Empty, setup, progress, and errors

- First run/setup missing: show one banner with `Open Settings`; do not populate the main area with onboarding cards.
- Empty category: `No recordings in this category`; filtered-empty copy says the selected chips/date removed all matches. The Manual category may show its retained start action.
- Finalization/clip/kill-video progress appears in the status card or a single compact progress row, not a notification center.
- Errors state the failed operation, relevant path/device when safe, and one recovery action. Expandable technical detail may include the logged error; never dump raw logs into the main UI.

## Accessibility and visual acceptance

- Every icon-only action has an accessible name and tooltip.
- Full keyboard traversal follows visual order and visible focus is never removed.
- Outcome and marker meaning is conveyed by label/icon as well as color.
- Body text and interactive states meet WCAG AA contrast in light and dark system themes.
- Honor reduced-motion; only native state transitions are allowed and no decorative animation is added.
- Long English labels and 200% scaling must not overlap or hide primary controls.

WR-009 records dark and light screenshots of the shell at 1440×900. WR-015 records the same views with data and compares them with the current application for workflow/identity, not pixel matching.
