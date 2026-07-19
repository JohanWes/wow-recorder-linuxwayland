# WR-015: Final parity/lean gates and stable publication

## Goal

Provide one final evidence package proving the staged native candidate is the same functional local product (except localization), measurably leaner/faster, safe to distribute, and visually recognizable; only then promote that exact candidate to stable and verify public installation.

## Dependencies

WR-013 and WR-014 must be `DONE`: the final AppImage notice is published, Electron is deleted in the candidate commit, and that commit's release candidate is built but unpublished.

## Owned files

- `implementation_docs/reports/wr-015-final.md`
- minimal benchmark/test harness files already anticipated by WR-000
- production files only for narrowly diagnosed gate failures, with the owning ticket/report cross-referenced
- stable release metadata/installation page/final AppImage release-note update used by the last publication step

## Test environment and method

Use one named physical machine and WR-000's deterministic library seed and documented measurement commands. Record OS/kernel, Wayland compositor, CPU/GPU/RAM, storage, Flatpak runtime, GSR/Clapper/GStreamer/FFmpeg versions, app commit, power state, and background-work controls.

For timing/RSS, perform one warm-up and five measured runs, report all values and median, and use median for gates. Do not encode wall-clock budgets as flaky unit tests.

## Gate 1: signed feature parity

Copy WR-000's full matrix and add native evidence/result for every `KEEP` row. At minimum cover:

- setup/path validation and every retained flavour/category/toggle/threshold;
- tray Open/Quit, close/minimize/background capture, start-minimized behavior if retained, and safe no-watcher fallback;
- automatic recording timing/lead-in, each retained activity-family fixture, complete/loss/abandon/discard, force-end, finalization/restart;
- manual/test recording, sounds/timeouts/shortcuts where retained, capture target reselection, devices and capture settings;
- legacy/new scan and every table family/column, sort, structured suggestions/chips, date filtering, selection, protection/tag, bulk/reveal/delete;
- H.264/AV1 player controls, speed, all shortcuts, approximate frames, fullscreen, marker visibility/navigation, timeline, drawing, clipping;
- two-to-four local POV playback/synchronization and kill-video segment/audio/progress/output;
- storage limits/protected behavior, settings Apply/Cancel, logs, About, shutdown/recovery, and the platform update path: fresh install and `flatpak update` from a local copy of the permanent remote.

For each row cite automated test, fixture/golden, or precise manual step/result. A manual-only UI/OS behavior is acceptable; do not create a test solely to make the matrix look automated. `REMOVE_*` rows must retain WR-000 evidence/approval. Any unexplained missing `KEEP` behavior fails release.

## Gate 2: size and dependency/code audit

Measure:

- final Flatpak application payload excluding shared runtime;
- production Rust/UI line count using the same exclusions documented in README;
- direct Cargo dependencies and bundled native executables/libraries;
- duplicate assets/libraries, debug symbols, unused GStreamer plugins/codecs, and licenses.

Pass: payload below 100 MiB; production LOC at/below 18,000 (target 12,000); direct dependencies at/below 16 (target 14); all bundled components needed by a parity row or platform requirement. If above a hard review threshold, release fails until a maintainer-approved deletion/exception is recorded.

Run unused-dependency/module/assets checks plus the WR-013 forbidden-term audit. Do not delete a required codec/media feature merely to meet size.

## Gate 3: performance/responsiveness

Using WR-000's deterministic 2,000-sidecar corpus:

- cold start to interactive library: below 1.0 s;
- idle RSS after load: below 150 MiB;
- suggestion narrowing, selected-chip filter, paired-date filter, and sort update: below 50 ms median;
- scroll through the table and selection/player load without unbounded row widgets;
- inspect GTK-main-thread frames while scanning, filtering, finalizing, clipping, and kill-video encoding; no stall above 100 ms caused by project work.

Also run a 60-minute armed/log-tail idle and a 30-minute playback session with repeated selection/seek/speed/fullscreen/POV changes. Record RSS/CPU at start/end and investigate sustained growth/runaway CPU; do not impose an arbitrary zero-growth assertion on caches owned by GTK/GStreamer.

Profile a failed metric first, attach the trace, change only the hot path, and rerun all gates. No cache/framework is accepted without before/after evidence and invalidation/LOC cost.

## Gate 4: sandbox/release safety

- Re-audit each Flatpak permission against exercised behavior and test denied log/storage/capture/audio/network access.
- Verify the app cannot traverse symlinks outside storage for delete/evict and does not log personal combat lines/tokens/device details unnecessarily.
- Verify signed install/update/rollback and migration/rollback from the final AppImage with data unchanged.
- Confirm no child GSR/FFmpeg process or tray service thread remains after explicit Quit, cancellation, worker failure, or forced shutdown; hiding/closing-to-tray intentionally keeps capture alive.
- Confirm license inventory/SPDX/AppStream/signatures/checksums match shipped payload.

## Gate 5: UI identity, usability, and accessibility

Compare WR-000's reference screenshots with native views at the same 1440×900 data/category, plus native light, dark, narrow, and 200% scale views. Review against UI-BRIEF:

- recognizable category rail/status and player-above-dense-table workflow;
- player/table resizing and newest/category-selection flow;
- readable current category columns, filters/chips/date/bulk bar, timeline, multi-POV, montage dialog, settings/status/errors;
- complete keyboard traversal/shortcuts, focus visibility, accessible names, screen-reader role/name/state spot checks;
- contrast and non-color outcome/marker cues; long labels do not overlap.

This is a workflow/identity comparison, not screenshot pixel matching. Record specific defects and fixes, not aesthetic adjectives alone.

## Failure/fix/restage loop

A failed gate blocks publication. A documentation-only correction to the evidence report may be reviewed in place, but any fix that changes production code, Cargo lock/sources, Flatpak manifest or permissions, bundled payload, signing inputs, or release metadata invalidates the candidate. Rebuild the release candidate on the new commit through WR-014's pipeline, record the new artifact/commit hashes, and rerun every gate in this ticket against that artifact—not only the failed check. Repeat until one unchanged signed artifact passes all gates. Superseded candidates must never be published.

## Acceptance criteria

- Every `KEEP` feature passes with cited evidence; only localization and approved `REMOVE_*` items are absent.
- All README hard size/memory/start/filter/stall and code/dependency gates pass or have explicit maintainer exception where the contract permits one.
- Long-running capture/playback has no diagnosed leak/runaway process, and media jobs never freeze the GTK thread.
- Sandbox denial, process cleanup, signing/update/rollback, migration, and license checks pass.
- UI brief and accessibility review are signed by a maintainer.
- Final report lists exact commands, raw samples/medians, artifact hashes, screenshots/traces, failures/fixes, skipped speculative work, and remaining known limitations.

## Stable publication (last action only)

After every gate above is green and maintainers sign the report:

1. Publish the exact tested commit/artifact: push WR-014's signed repo for that build to the permanent remote; do not rebuild from a different commit.
2. Verify signature/checksum, remote/AppStream metadata, a fresh public install, and `flatpak update` from the previous rehearsal install.
3. Flip the migration live: merge WR-013's Flatpak-installing `install.sh` to `main` and publish the release tag that makes existing AppImage installations offer the one-click migration update; update the permanent installation page and final AppImage release notes to the verified stable command/link.
4. Verify one real AppImage installation completes the migration update end to end. Record stable commit/artifact hashes, publication time, approver, and verification output. If publication/verification fails, stop, keep the signed evidence, and do not change gate results or publish a different rebuild.

The ticket is not `DONE` and the native release is not announced until these steps pass.

## Not in scope

Adding new features during validation, supporting other Linux package formats/platforms, synthetic benchmark optimizations users cannot observe, exhaustive fuzzing, pixel-perfect Electron replication, or hiding failed metrics through changed methodology.
