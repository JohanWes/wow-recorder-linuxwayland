# WR-008: coordinator and headless vertical-slice evidence

## Environment

- Commit under test: d917624b plus the WR-008 work in this tree.
- Linux 7.1.2-3-cachyos, KDE Plasma, native Wayland, x86_64; Flatpak 1.18.0, GNOME Platform/SDK 50.
- Rust checks ran in the GNOME SDK sandbox (`flatpak run --devel --command=sh org.gnome.Sdk//50`) with `PKG_CONFIG_PATH`/`LD_LIBRARY_PATH` pointed at the WR-002 Clapper build under `wr007-build/files`; the host installs no Clapper development files.
- The vertical slice is headless: no GTK, no display, no network.

## Contract checked

| Acceptance criterion | Evidence |
|---|---|
| Headless replay of raid/PvE, PvP/round, manual, and test flows creates the exact final media/sidecar/index outcomes | `vertical_slice::automatic_raid_completes_and_survives_a_restart`, `force_ended_solo_shuffle_is_abandoned_and_saved`, `manual_and_test_recordings_reuse_the_capture_pipeline`. WR-005 already covers per-category transitions, so no category matrix is repeated here. |
| Automatic replay lead-in, hook-correlated two-artifact capture, regular-only fallback, post-trim marker offsets | The raid scenario asserts `requested_replay_ms == 10_000` (detection delay + extra lead-in, clamped to the buffer), that both GSR artifacts are consumed by finalization, and that the death marker moved past its 500 ms activity offset. `missing_replay_falls_back_to_the_regular_recording` asserts the regular-only path saves and clips the pre-media death. |
| Complete, `Abandoned`, force-end, discard, capture failure, restart/rescan with the startup sweep, graceful shutdown | Complete: raid scenario. Force-end/abandon with zero overrun and a loss outcome: shuffle scenario. Capture failure: `missing_regular_artifact_reports_a_problem`. Restart/sweep/rescan: the raid scenario builds a second `Coordinator` over the same tree, asserts the stray `regular/Video_stray.mkv` is gone, `Recovery/` is populated, and the protected entry rescans. Shutdown: `production_handle_starts_and_shuts_down` runs the real `start()`/`CoordinatorHandle::shutdown()` wiring, which joins the coordinator and its media worker. |
| Tag/protect/bulk delete, clip, and kill-video commands serialize through the real storage/media boundary | Raid scenario patches tag and protection through `Storage::update` and then deletes; `finalization_precedes_queued_user_jobs` clips one entry and renders a two-POV kill video through the real `MediaWorker`. |
| Finalization starts before queued user transcodes without parallel media workers | `finalization_precedes_queued_user_jobs` sends `ForceEnd` and `CreateClip` in one command batch, so both jobs are queued before the tick's single `dispatch_media`. The observed entry order is `[Raids, Clip]`. |
| No GTK-thread blocking, unbounded channel, busy loop, async runtime, generic event system, or duplicated category logic | `native/src/lib.rs` has no GTK import; the command/snapshot/stopped/media-control channels are `sync_channel`; idle pacing is one `Receiver::recv_timeout` per tick with no sleep; no runtime or event bus is added; category rules stay in `activity.rs`. |

## Commands and raw results

~~~text
cargo fmt --manifest-path native/Cargo.toml --check                                          PASS
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings   PASS
cargo test --manifest-path native/Cargo.toml --all-targets            76 + 1 + 7 passed, 0 failed
cargo build --manifest-path native/Cargo.toml --release                                      PASS
~~~

~~~text
running 7 tests
test automatic_raid_completes_and_survives_a_restart ... ok
test finalization_precedes_queued_user_jobs ... ok
test force_ended_solo_shuffle_is_abandoned_and_saved ... ok
test manual_and_test_recordings_reuse_the_capture_pipeline ... ok
test missing_regular_artifact_reports_a_problem ... ok
test missing_replay_falls_back_to_the_regular_recording ... ok
test production_handle_starts_and_shuts_down ... ok
~~~

## Test seams

No recorder or process trait was added. The vertical slice drives the production `Recorder`, `Storage`, and `MediaWorker` against the existing WR-006/WR-007 fakes (`tests/native/bin/fake-gsr.sh`, `fake-ffmpeg.sh`) over a temp tree whose library directory contains a space. The only seam is `Coordinator::tick()`, the same core the production thread loop wraps, so tests advance deterministically without wall-clock sleeps.

## Deliberate deviations from the ticket text

- `Command::CreateKillVideo` carries only its ordered `ClipRange` segments; the ticket's separate `correlated_id` is redundant because the segments already name their sources.
- Test recordings synthesize the minimum parsed events per category instead of embedding the legacy `testButtonData` log dumps, whose bulk existed only to populate the legacy UI. Categories, the 5 s / 20 s (raid) duration ratio, and the reuse of the normal recorder/finalize path are preserved.
- The legacy Ctrl+Alt "test that never ends on its own" variant is not rebuilt; `ForceEnd` already stops a running test.
- Advanced-combat-logging status is read from `<log dir>/../WTF/Config.wtf` when tailers open (startup and `SaveConfig`). The legacy per-file watcher is not rebuilt.
- Discard and failed-end cleanup route through `Storage::sweep_orphans`, which quarantines the returned artifacts under `Recovery/` with a reason file; no separate quarantine API was added.

## Not covered here

GTK widgets, player state, notifications, and performance measurement remain with WR-009 through WR-012 and WR-015. `main.rs` still runs the WR-002 probe shell; WR-009 owns replacing it with the `CoordinatorHandle` wiring described in this ticket's step 10.
