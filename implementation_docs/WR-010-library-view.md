# WR-010: Virtualized library table, filters, and local actions

## Goal

Replace the current sortable category tables with one native virtualized `GtkColumnView` that preserves suggestion-chip search, paired-date filtering, row/multiselect behavior, protection/tagging, reveal, and deletion without thumbnails or duplicated per-category screens.

## Dependencies

WR-007 and WR-009 must be `DONE`.

## Owned files

- `native/src/ui/library.rs`
- `native/src/ui/filters.rs`
- `native/src/ui/library_actions.rs` only if actions make `library.rs` unwieldy
- `native/src/ui/mod.rs` module wiring
- UI/model tests for filtering/sorting/action dispatch
- `implementation_docs/reports/wr-010-library-evidence.md`

## Model pipeline

Use GTK's native chain over the snapshot's library entries:

`gio::ListStore` (or a small GLib object wrapper) → category filter → structured/date filter → sort model → `GtkMultiSelection` → `GtkColumnView`.

Rebuild/update the store from coordinator snapshots in the simplest measured-safe way. Start with replace/diff-by-ID code of at most one direct pass; do not create a generic observable collection, cache, repository, pagination layer, or background filter engine. GTK column factories must bind/unbind reused row widgets correctly.

## Columns and rows

1. Implement the exact shared and category-family columns in UI-BRIEF/WR-000. One function returns column specifications for the selected category; reuse bind/format functions for truly shared fields.
2. Shared leading cells provide selection, protection/star, and compact Details (approved spec/class image and player/title/tag). Category-specific facts remain text/icon cells, including affix images and result labels.
3. Default sort is newest first. Each current sortable header cycles ascending/descending and shows native indication; switching category restores that category's current/default sort behavior recorded by WR-000.
4. Single plain click clears other selections, selects the row, and asks the player area to load the preferred/default local POV. Ctrl-click toggles; Shift-click selects the visible range. Ctrl+A and Up/Down keyboard behavior match WR-000 using GTK selection conventions. Do not add row gestures unrelated to current behavior.
5. Virtualization must keep all results browsable by scrolling. Removing numbered pagination is an implementation change, not a feature loss; do not cap the model.

## Structured search and dates

Implement the encoded suggestion/chip examples recorded by WR-000, not a query language.

- Generate/deduplicate the same typed suggestions from the currently filtered entries and their correlated POVs, excluding already selected identities. Typing narrows labels; it does not filter until a suggestion is selected. Tab accepts the active item.
- Encode/decode chip identity with one small typed enum/value, not packed JSON or a general parser. A row passes only when all selected exact suggestion identities occur in that correlated activity.
- Native paired From/To date controls use local displayed dates and inclusive boundaries; filter only when both endpoints exist.
- Display selected chips and paired date control above the table with individual chip removal. Switching category clears selection/filter state exactly as the baseline records.

## Actions and feedback

- Protection star sends `SetProtected` for the intended entry/correlated viewpoints exactly as WR-000. Bulk bar determines Protect vs Unprotect from the selection using the recorded rule.
- Tag opens one small native dialog, trims/validates to the current maximum/empty semantics, and sends `SetTag`.
- Reveal resolves a local path through the coordinator/storage and uses `GtkFileLauncher::open_containing_folder` (or current GIO equivalent). Do not use deprecated `gtk_show_uri` or add `ashpd`.
- Delete for row(s) opens one confirmation with exact count and whether correlated POV/source files are included according to baseline. Only confirmation sends `Delete`; display partial per-item failures without rolling back successful deletes.
- During an outstanding mutation, disable only the affected action/IDs. Snapshot success is authoritative; do not optimistically maintain a second library state.
- Empty category and filtered-empty messages/actions match UI-BRIEF.

## Acceptance criteria

- All WR-000 categories render the correct column set/data, sort directions, and default order.
- Suggestion narrowing/Tab selection, exact chips/AND matching across POVs, paired dates, individual removal, and category-change clearing match current results against the shared dataset.
- Plain/Ctrl/Shift selection, keyboard navigation, multiselect count, protection/tag, reveal, single/bulk delete, cancellation, and partial failure behave as specified.
- Selecting a row loads the right preferred/default local POV and no filtered-out/deleted entry remains selected.
- Row factories stay bounded to visible rows; no thumbnails are generated or loaded.
- No database, search framework, pagination state, per-category screen duplication, custom date picker, or GTK-main-thread filesystem work exists.

## Tests and evidence

Use table-driven pure filter/sort cases from WR-000 and a thin GTK model test for selection/action payloads. Do not snapshot every cell or mock GTK internals. Manually record each category, filter/date examples, selection modes, actions/errors, and keyboard traversal inside Flatpak. WR-015 owns timing budgets.

## Not in scope

Player controls/timeline, multi-POV synchronization, new metadata extraction, thumbnails, cloud storage filters/actions, or redesigning the search syntax.
