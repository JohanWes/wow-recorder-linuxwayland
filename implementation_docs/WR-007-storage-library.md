# WR-007: Storage, legacy library, clips, and media jobs

## Goal

Implement the filesystem-backed local library and the serial media operations needed to finalize recordings, create clips, and create retained local multi-POV kill videos. Keep JSON sidecars as source of truth and do not add a database, thumbnail cache, or media framework.

## Dependencies

WR-003 and WR-006 must be `DONE`. WR-002 must have proven/bundled the required minimal FFmpeg executable and codecs/filters for retained media jobs.

## Owned files

- `native/src/storage.rs`
- `native/src/media_jobs.rs`
- `native/src/lib.rs` module export
- storage/media-job tests in those modules
- legacy sidecar fixtures/goldens supplied by WR-000

## Storage API

Implement one concrete `Storage` rooted at the configured directory:

- `scan() -> LibraryIndex`;
- `finalize(draft: RecordingDraft, artifacts: CaptureArtifacts, media: CombinedMedia) -> LibraryEntry`;
- `update(entry, protected/tag change)`;
- `delete(entries) -> DeleteResult`;
- `reveal_path(entry) -> &Path` (UI launches it later);
- `enforce_limit(StorageLimit, entries) -> EvictionResult`;
- `sweep_orphans() -> RecoveryReport` for the startup cleanup pass.

Implementation reconciliation (2026-07-20): `finalize` receives the media the
worker already combined so storage spawns no process, and the mutating calls
take the scanned `LibraryEntry` (which carries its own media/sidecar paths)
instead of a bare identifier, so no second in-memory index is needed.

`LibraryIndex` contains entries plus derived local correlations. It does not retain open file handles or UI objects.

## Sidecars and scan

1. New sidecars serialize the WR-003 model with a schema version and paths relative to the storage root where practical. Write a sibling `.tmp`, flush, and rename. GSR intermediates retain GSR-chosen names in configured replay/regular directories; final media uses a unique temp name until media work completes.
2. Scan only the configured directory level/layout proven by WR-000. Ignore unrelated and unsupported files with one bounded warning summary; do not recursively crawl home or attempt media repair.
3. Deserialize new sidecars first, then a private `LegacySidecar` matching WR-000's real JSON keys including category, timing/result/flavour, encounter/dungeon/arena/solo details, player/combatants, timelines, protected/tag, encoder/FPS facts, clip parent/source, unique hash, and optional old fields. Convert to the clean model; never expose cloud-only fields. Record internally whether each entry came from a new or legacy sidecar.
4. Missing optional legacy values display as unknown/empty. Invalid required path/timing/category data yields a skipped-entry diagnostic, not a panic or fabricated value.
5. Correlate local POVs with WR-000's exact unique-hash and start-time tolerance. Exclude clips, manual, and solo cases exactly as the baseline does. Pick the primary deterministically: reverse-chronological order with the media path as tie-break, because the baseline's directory `mtime` ordering does not survive a copy or restore.

## Finalization and startup sweep

The coordinator holds the active `RecordingDraft` in memory; there is no on-disk pending descriptor or capture journal. The current application has no interrupted-recording recovery, so parity does not require one: a crash mid-capture loses that recording, but must never silently delete playable media. Finalization order keeps the crash states cheap to clean up:

1. GSR closes the optional replay and required regular artifacts returned by Recorder.
2. If replay exists, FFmpeg stream-copies its final requested duration to a trim temp, then concatenates trim+regular to a final-media temp. If replay is missing or trim/concat fails in the baseline-approved way, copy regular to final-media temp and report actual replay as zero. Verify playable/nonzero output and derive actual media start from usable trimmed replay duration plus regular start. Reconciliation (2026-07-20): the trim's own `-sseof` progress under-reports the keyframe-aligned file by roughly a second and the bundled FFmpeg ships no `ffprobe`, so the usable duration comes from one extra stream-copy remux of the trim to a null Matroska sink, with the trim progress as fallback.
3. Write/flush the sidecar temp.
4. Rename media temp to final media, then sidecar temp to final sidecar.
5. Remove replay, regular, trim/list/temp intermediates only after both final names exist.

`sweep_orphans` runs once at startup, before scan and before any capture is armed: any media, GSR artifact, or `.tmp` intermediate in the configured storage/replay/regular directories that no sidecar references moves to one `Recovery/` directory with a reason file; a final media file lacking its sidecar between renames is also swept there. Never claim files by name/time proximity for completion, silently delete playable media, repair arbitrary corruption, undelete, or add a transaction journal.

## Mutations and storage limit

- Tag/protection updates rewrite only the sidecar atomically. For a new sidecar, serialize the typed model. For a legacy sidecar, reread the original JSON into a storage-private `serde_json::Value`, patch only the exact legacy `protected`/`tag` keys, and atomically write it back so unknown/old fields and schema remain readable by the final AppImage. This is the sole permitted untyped JSON escape hatch; it never enters the domain model.
- Delete confirms scope in UI; storage removes media plus sidecar for each requested entry and returns per-entry failures. It never follows symlinks outside the root.
- `StorageLimit::Unlimited` returns without eviction at startup/finalization. `Gib(nonzero)` converts with checked arithmetic and evicts oldest unprotected finalized recordings until under limit. Never evict partial/finalizing work, protected entries, or unrecognized files. If protected content alone exceeds a positive limit, report without deletion.

## Clip and kill-video jobs

Use one concrete `MediaJob` enum consumed serially by WR-008's single media worker:

- `FinalizeRecording { pending, artifacts }`;
- `CreateClip { source, start_ms, end_ms }`;
- `CreateKillVideo { ordered_segments, width, height, fps, audio_mode }`.

The worker also receives `MediaControl::Shutdown` on a separate capacity-one channel. Every FFmpeg invocation uses an exclusively created per-job progress file with `-progress <path> -nostats`; stdout is null and stderr is redirected to an exclusively created per-job log file — no pipes, matching WR-006's recorder-log approach. The worker loop uses a 50 ms `recv_timeout`, then `try_wait`, and reads newly appended complete `key=value` lines from the progress file using a stored byte offset/partial-line buffer. After the child exits, read at most the final 8 KiB of the stderr file for diagnostics, then remove both per-job files.

On Shutdown, this loop observes control within one polling interval. Allow automatic finalization the WR-000/WR-015 bounded grace period (WR-000 named no number; `MediaConfig::finalize_grace` sets it to 30 s and WR-008 may lower it from configuration), then send SIGINT using WR-006's checked process helper, wait two seconds while continuing the same poll loop, and `Child::kill` if needed. User clip/kill-video jobs are cancelled immediately through the same SIGINT/kill sequence. Remove the per-job files and disposable temps, and preserve viable recording artifacts for the next-start orphan sweep. Join the one worker. Do not detach a child, add a reader/monitor thread, or introduce an async/process crate.

Build FFmpeg argv directly and spawn with `std::process::Command`; never invoke a shell or add a Rust FFmpeg wrapper. Clip uses the baseline's stream-copy/re-encode rule and clips timeline spans/points to the selected range. Its sidecar retains source ID/category/title and gets a new automatically generated ID/date/name.

Kill-video behavior matches WR-000: validate at least two distinct local correlated sources; validate all segment bounds; validate current resolution and FPS choices; order/reorder/trim sources; use the current smooth video/audio transitions and single-source-or-switched audio modes; normalize to the selected output as the baseline does; emit progress from parsed FFmpeg progress output; encode with the recorded H.264/AAC settings and automatically generated output name; write a Clips sidecar with source/segment provenance. Reject cloud/nonlocal/missing sources. No presets, background queue persistence, arbitrary filters, or export formats are added.

All output paths use exclusive collision-safe creation and sanitized display names; identifiers, not titles, provide uniqueness.

## Acceptance criteria

- Every WR-000 legacy sidecar maps to the exact golden and remains unmodified on disk.
- New recording finalize, scan/restart, tag/protect, bulk delete, zero/unlimited, and positive-limit eviction preserve model/path invariants.
- Replay+regular trim/concat and regular-only fallback produce correct media start, duration, and clipped timeline offsets.
- The startup sweep quarantines unreferenced artifacts to `Recovery/` with a reason and never touches sidecar-referenced media; no speculative recovery branches exist.
- Clip output is playable and its adjusted metadata/timeline matches the selected interval.
- A two/three-POV kill-video fixture preserves segment order, transitions, selected audio behavior, progress, and produces a correlated-source Clips entry.
- FFmpeg/GSR paths with spaces are safe arguments; no shell, database, thumbnail, recursive scan, media-editing crate, or per-entry worker is introduced.
- Shutdown during finalization/clip/kill-video leaves no FFmpeg child and either completes a valid output or leaves only artifacts the startup sweep quarantines.
- A deliberately silent long-running fake FFmpeg plus a fake that continuously writes stderr/progress are both terminated by the SIGINT/kill escalation; the worker joins, no child remains, and diagnostics read at most the final 8 KiB. No exact-latency assertion.

## Tests

Use temporary directories and the minimal WR-000 fixtures. Cover: new/legacy scan; finalize plus one small table of interruption leftovers proving the startup sweep quarantines them; new-sidecar update plus legacy patch preserving unknown fields; zero/unlimited plus protected positive-limit eviction; partial bulk-delete failure; clip argument/metadata golden; kill-video argument/metadata golden; progress/stderr/shutdown with one small fake executable. Run one real clip and one short real two-POV montage inside Flatpak as manual evidence. Do not generate exhaustive corrupt JSON/media cases or timing benchmarks here.

## Not in scope

GTK launchers/dialogs, cloud import/export, media repair, library history, thumbnails, database indexing, concurrent transcodes, or codecs/options absent from WR-000.
