# WR-010 evidence: library view, filters, and local actions

Status: **code complete and verified on host** (fmt/clippy/tests/release).
In-Flatpak manual acceptance of each category/filter/action follows the
WR-000 source-traced, owner-executed deferral (see "Known limitations").

## Environment
- commit: refactor/native-non-frontend (WR-010 working tree)
- OS/kernel/session: CachyOS, Linux 7.1.2-3-cachyos, Wayland
- toolchain: cargo/rustc stable (edition 2024); gtk4 0.11.4 (v4_10),
  libadwaita 0.9.2 (v1_6)

## What was built

- `ui/filters.rs` — GTK-free suggestion-chip and date-range logic ported from
  the legacy `VideoFilter`/`VideoTag` (WR-000 rows 48–49). A `Chip` is
  `(group, label)`, the identity a legacy `VideoTag.encode()` compares. Public
  API: `suggestions_for_entry`, `combined_suggestions` (union over primary +
  POVs), `narrow` (typing narrows only; excludes selected), `within_range`
  (inclusive, both-endpoints-only), `row_matches` (AND across POVs + date).
  Reuses the factual `specializationById` and `dungeonAffixesById` tables from
  `src/main/constants.ts` as compact id→name arrays for spec/affix suggestions;
  all other suggestion text comes from strings already in the sidecar.
- `ui/library.rs` — the `Library` widget: the native model pipeline
  `gio::ListStore` (one `BoxedAnyObject`-wrapped `Rc<RowModel>` per correlated
  activity of the selected category, newest first) → `FilterListModel`
  (chips + date `CustomFilter`) → `SortListModel` (the `ColumnView` sorter) →
  `MultiSelection` → `GtkColumnView`. Snapshot-authoritative: a signature
  guards store rebuilds; no second library state is kept.
- `ui/window.rs` — replaced the WR-009 toolbar/table placeholder with the
  `Library` widget in the content `GtkPaned`, and routed single-row selection
  to the player-hint placeholder (the WR-011 seam carries id/media_path/POVs).
- `ui/mod.rs`, `ui/style.css` — module wiring and one compact `.wr-chip` rule.

## Contract checked (acceptance criterion → evidence)

- Correct column set/data/default order per category → `columns_for`/`Family`
  maps every WR-000 category family to the exact left-to-right columns
  (Raid: Details, Encounter, Result, Pull, Difficulty, Duration, Date,
  Viewpoints, Creator; Dungeon: …Level, Affixes…; Pvp: …Map…; Clips: Type,
  Source activity; Manual: Type). Default newest-first via `build_rows` sort
  with no active column sorter; each data column has a `CustomSorter` (Result
  by outcome rank, Duration/Date/Viewpoints/Level numeric, Difficulty by rank,
  Creator by viewpoint count) and toggles asc/desc with native indication.
- Suggestion narrow/Tab, exact chips/AND across POVs, paired dates, individual
  removal, category-change clearing → `filters` unit tests + `Inner`
  search/chip/date handlers; `reset_for_category` clears chips/date/selection
  /sort and swaps columns.
- Plain/Ctrl/Shift selection, keyboard nav, multiselect count, protect/tag,
  reveal, single/bulk delete, cancellation, partial failure → native
  `MultiSelection` conventions; single selection loads the sole row (multi does
  not); bulk bar count + Protect/Unprotect rule (unless every selected viewpoint
  is protected → Protect); `AlertDialog` delete confirm names exact recording
  and viewpoint-file counts and only "delete" sends `Command::Delete`; storage
  reports per-item results without rollback (coordinator, unchanged).
- Preferred/default POV load and no stale selection → category change
  auto-selects the newest row; filtered-out/deleted rows leave the model, so no
  removed entry stays selected.
- Bounded row factories, no thumbnails → `SignalListItemFactory`
  setup/bind/unbind reuse row widgets; no image/thumbnail generation exists.
- No database/search framework/pagination/per-category screen/custom date
  picker/GTK-thread FS work → one `CustomFilter`, native `GtkCalendar` pair,
  `GtkFileLauncher::open_containing_folder` for reveal; all data comes from the
  coordinator snapshot.

## Commands and raw results
```text
cargo fmt --manifest-path native/Cargo.toml --check      # clean
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings   # clean
cargo test --manifest-path native/Cargo.toml --all-targets   # 24 lib/bin + 7 vertical-slice, all pass
cargo build --manifest-path native/Cargo.toml --release  # Finished, optimized
```

Filter/suggestion unit tests (in `ui/filters.rs`): raid suggestion coverage,
dungeon timed/abandoned + affixes, AND-matching across correlated POVs +
inclusive dates, narrowing excludes selected + substring, combined-suggestion
label dedup, combatant name suggestions.

## Decisions and deviations
- Game-specific spec/class/affix imagery is rejected for native redistribution
  by WR-000's assets report; the Details column and affix cell are therefore
  icon+text, not sprites. Spec/affix **names** (factual data) are reused as
  lookup tables. Approver: maintainer (WR-000 assets decision).
- Selection uses GTK `MultiSelection` (plain/Ctrl/Shift/Ctrl+A/arrows) rather
  than an explicit checkbox column — the platform feature the ticket step 5
  maps to. The bulk bar surfaces multiselect count and actions.
- `Selection.media_path`/`viewpoints` are populated now as the WR-011 player
  seam (marked `#[allow(dead_code)]` with a `ponytail:` note) so WR-011 loads
  the preferred/default POV without re-plumbing.

## Review (codex, BLOCKER/HIGH/CRITICAL pass)

A codex agent reviewed the diff; no BLOCKER/CRITICAL issues. Six HIGH items
raised, four fixed and two assessed as intended legacy parity:

- Fixed — tag save truncated by byte index (`String::truncate`) could panic on a
  multi-byte 1024th char; the entry already caps at 1024 characters, so the
  redundant truncate was removed.
- Fixed — a snapshot-driven store rebuild dropped the user's selection and
  jumped the player to the newest row; the rebuild now remembers selected ids
  and re-selects survivors, defaulting to newest only for a fresh category or a
  deleted/filtered-out selection.
- Fixed — the row star icon/tooltip used primary-only `protected` while the
  toggle used `all_protected`; both now use `all_protected`, consistent with
  the action value.
- Fixed — inclusive date end used a fixed 86 400 000 ms; it now uses local
  `add_days(1) - 1 ms`, keeping 23/25-hour DST days one calendar day wide.
- No change (legacy parity) — suggestion list dedup is by label, matching the
  legacy `VideoFilter` `Map(label→tag)`; row matching remains group-aware
  (`Chip` = `(group, label)`), so exact filtering is unaffected. Same-label
  cross-group collisions are the legacy behaviour and practically nonexistent
  in WoW data.
- No change (legacy parity) — tag/class/keystone-level are not suggestions in
  the legacy `VideoFilter` (tag emits only a generic "Tagged" marker; no class
  or level chip), and WR-000 row 48 lists exactly the implemented set
  (protection/tag/flavour/player/spec/combatants/zone + result/difficulty/
  encounter/dungeon/affix).

## Skipped (YAGNI)
- No observable-collection/repository/cache/pagination/background-filter engine;
  a signature-guarded one-pass store rebuild covers reactive updates.
- Per-id optimistic disabling reduced to one `mutation_pending` bulk-bar guard
  cleared by the authoritative next snapshot; the model never mirrors library
  state.

## Known limitations
- No populated real sidecar corpus or interactive display was available in this
  environment, so per-category in-Flatpak manual traversal
  (category/filter/date/selection/keyboard/action/error screenshots) is not
  claimed here. This is the same maintainer-approved WR-000 source-traced
  deferral; WR-015 owns the final populated visual/manual parity pass and
  timing budgets. This section does not hide a failed automated criterion —
  all named automated checks pass.

## Approval
- Reviewer/maintainer: pending sign-off.
- Date/result: code complete; automated gates green; Flatpak manual acceptance
  deferred to owner/WR-015 under the WR-000 deviation.
