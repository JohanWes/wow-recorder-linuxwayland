# WR-007: storage, legacy library, and media-job evidence

WR-015 addendum (2026-07-22): the deterministic performance corpus uses
zero-byte media placeholders. Scanning retains them with runtime-only
`media.has_content = false`; finalization still requires non-empty real media,
and the player refuses unusable media. Mutation/deletion now accepts only
direct children of the intentionally flat recording directory and rejects
leaf or intermediate-directory symlink escapes. Regression tests cover both.

## Environment

- Commit under test: b5879b76be5812e9377048fa97a236ff6c979b80 plus the WR-007 work in this tree.
- Linux 7.1.2-3-cachyos, KDE Plasma, native Wayland, x86_64; Flatpak 1.18.0, GNOME Platform/SDK 50.
- Rust checks ran in the WR-002 SDK sandbox (`flatpak-builder --run`, build dir `wr007-build`, stopped after the Clapper module) because the host installs no Clapper development files.
- Real media transforms ran inside the installed `io.github.JohanWes.WarcraftRecorder.Devel` sandbox with its bundled minimal FFmpeg (no `ffprobe` is shipped).

## Contract checked

| Acceptance criterion | Evidence |
|---|---|
| Every WR-000 legacy sidecar maps to the exact golden and stays unmodified on disk | `storage::tests::legacy_sidecars_map_to_the_golden_and_stay_unmodified_on_disk` against `tests/native/golden/legacy-scan.json`; the test re-reads every fixture byte-for-byte after the scan. |
| Finalize, scan/restart, tag/protect, bulk delete, zero/unlimited and positive-limit eviction preserve model/path invariants | `finalize_writes_media_relative_markers_and_survives_a_rescan`, `updates_rewrite_native_sidecars_and_patch_legacy_ones_in_place`, `deletion_reports_per_entry_failures_and_refuses_paths_outside_the_root`, `storage_limits_evict_only_unprotected_recordings_oldest_first`. |
| Trim/concat and regular-only fallback produce correct media start, duration, and clipped offsets | `finalization_trims_and_concatenates_the_replay` (73 s media, marker moved 20 s → 18 s), `a_failing_trim_falls_back_to_the_regular_recording_alone` (70 s media, marker 20 s → 15 s), `regular_only_finalization_clips_markers_before_the_media_start`. Real numbers below. |
| The startup sweep quarantines unreferenced artifacts with a reason and never touches referenced media | `startup_sweep_quarantines_interruption_leftovers_only`: five leftovers (orphan media, `.json.tmp`, replay, regular, staging trim) moved to `Recovery/` with `*.recovery.txt`; the eleven fixture recordings still scan. |
| Clip output is playable and its metadata/timeline matches the selected interval | `clip_job_writes_a_clips_entry_for_the_selected_interval` plus the real sandbox clip below (6.033 s for a requested 4 s stream copy; the input-side seek lands on the preceding keyframe, as in the baseline). |
| A two/three-POV kill video preserves order, transitions, audio behavior, progress, and produces a correlated-source Clips entry | `kill_video_job_preserves_order_audio_progress_and_provenance`, `clip_and_kill_video_arguments_match_their_goldens`, plus the real two-POV montage below. |
| Paths with spaces are safe arguments; no shell, database, thumbnail, recursive scan, media crate, or per-entry worker | Every test tree uses `recordings with space/`; argv is built as `Vec<String>` for `std::process::Command`. `native/Cargo.toml` gains no dependency. |
| Shutdown during a job leaves no FFmpeg child and either completes or leaves only sweepable artifacts | `shutdown_terminates_a_silent_child_and_leaves_no_process`, `shutdown_terminates_a_chatty_child_and_reads_a_bounded_log_tail`, `a_dropped_control_channel_still_interrupts_finalization`: each asserts `pgrep -f <staging dir>` finds nothing after the worker joins. |
| Silent and chatty fakes are both terminated by the SIGINT/kill escalation; diagnostics read at most 8 KiB | Same two tests (`tests/native/bin/fake-ffmpeg.sh` modes `silent`, `chatty`) plus `a_failed_job_reports_the_ffmpeg_log_tail`. No latency is asserted. |

## Commands and raw results

Rust verification inside the SDK sandbox:

~~~text
cargo fmt --manifest-path native/Cargo.toml --check                                          PASS
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings   PASS
cargo test --manifest-path native/Cargo.toml --all-targets                                   76 passed, 1 passed
cargo build --manifest-path native/Cargo.toml --release                                      PASS
~~~

Real trim + concat + clip + two-POV montage, run through the app sandbox's bundled FFmpeg over the WR-006 captures (3440x1440 H.264/AAC, replay 9.866 s, regular 6.015 s, second POV 8.633 s). The argv is the exact shape `trim_args`, `measure_args`, `concat_args`, `clip_args`, and `kill_video_args` build:

~~~text
flatpak run --command=sh io.github.JohanWes.WarcraftRecorder.Devel /var/data/wr007-proof/run.sh

== trim      ffmpeg -progress <p> -nostats -nostdin -hide_banner -sseof -5 -i <replay>
             -c:v copy -c:a copy -avoid_negative_ts make_zero -y <trim.mkv>       out_time_us=4968000
== measure   ffmpeg -progress <p> -nostats -nostdin -hide_banner -i <trim.mkv>
             -c copy -y -f matroska /dev/null                                     out_time_us=5845000
== concat    ffmpeg ... -f concat -safe 0 -i <list> -c:v copy -c:a copy
             -avoid_negative_ts make_zero -y <final media.mp4>                    out_time_us=11799000
== clip      ffmpeg ... -ss 2 -i <final media.mp4> -t 4 -c:v copy -c:a copy
             -avoid_negative_ts make_zero -movflags +faststart -y <clip out 2.mp4>  out_time_us=4036667
== kill      ffmpeg ... -i <replay> -i <pov2> -filter_complex <graph> -map [v] -map [a]
             -shortest -c:v libx264 -crf 22 -c:a aac -preset fast -pix_fmt yuv420p
             -movflags +faststart -xerror -y <multiview out.mp4>                  out_time_us=7433333
ALL-OK
~~~

Host `ffprobe` on the sandbox outputs:

| Output | Duration | Streams | SHA-256 |
|---|---|---|---|
| `replay-trim.mkv` | 5.866 s | H.264 3440x1440 + AAC | (intermediate, removed by the worker) |
| `final media.mp4` | 11.860 s (= 5.866 + 6.015) | H.264 3440x1440 + AAC | `4cc546efc2cf02c344e2cfe4c0a94fca3ebb6ac651634229514695dba47ad5c1` |
| `clip out 2.mp4` | 6.033 s for a requested 4 s | H.264 3440x1440 + AAC | `d9a15b8d4f08b3334f702e970303ca4b66f5f37178ffcd79d62fe52ff1f87039` |
| `multiview out.mp4` | 7.500 s (= 4.0 + 3.5 segments) | H.264 1280x720 + AAC | `2db38fead4c3ba7a1c64c506bb640d981e5be7d54cf6807cdc868517871ae2b3` |

## Manual scenarios

| Scenario | Preconditions | Steps | Expected | Actual | Pass |
|---|---|---|---|---|---|
| Real replay trim and concat | WR-006 replay + regular captures | Run the trim/measure/concat argv in the sandbox | One playable file of trim + regular length | 11.860 s MP4, H.264 + AAC | Yes |
| Real clip | Concatenated 11.86 s recording | Input-side seek 2 s (`-ss` before `-i`, the baseline's `setStartTime` rule), duration 4 s, stream copy | Playable ~4 s MP4 | 6.033 s MP4: the seek lands on the preceding keyframe, matching the baseline's keyframe-aligned lead-in | Yes |
| Real two-POV montage | Two independent GSR replays | Legacy filter graph at 1280x720/30 with switched audio | 7.5 s normalized H.264/AAC montage | 7.500 s, 1280x720, both inputs decoded | Yes |
| Bundled-FFmpeg capability check | Minimal WR-002 FFmpeg | `-f null -` versus `-f matroska /dev/null` | A working duration probe | The null muxer is absent; the Matroska remux to `/dev/null` works and is what `measure_args` uses | Yes |

## Measurements

| Metric | Method | Samples | Result |
|---|---|---|---|
| Usable replay lead-in accuracy | Compare the derived lead-in against `ffprobe` on the real trim | `-sseof` progress 4.968 s; Matroska remux 5.845 s; true duration 5.866 s | The remux pass is within one frame (21 ms); the raw `-sseof` progress was 898 ms short, so it is only the fallback |

## Files and artifacts

- `tests/native/fixtures/legacy/sidecars/*.json` — eleven anonymized legacy sidecars covering protected/tagged raid POV A, correlated POV B with missing optional fields, abandoned Mythic+, arena, battleground, solo shuffle, Classic raid, Era raid, Classic challenge mode, manual, and a clip parented to POV A by its legacy filename.
- `tests/native/golden/legacy-scan.json` — the mapped library index (entries, correlation groups, legacy flags, skip/ignore counts).
- `tests/native/golden/clip-args.txt`, `kill-video-args.txt`, `kill-video-single-audio-args.txt` — FFmpeg argv goldens.
- `tests/native/bin/fake-ffmpeg.sh` — deterministic FFmpeg stand-in with `ok`/`fail`/`silent`/`chatty` modes.
- Sandbox evidence: `~/.var/app/io.github.JohanWes.WarcraftRecorder.Devel/data/wr007-proof/`.

## Decisions and deviations

- `Storage::finalize` takes the already-combined media (`CombinedMedia`) instead of spawning FFmpeg itself, and `update`/`delete`/`reveal_path`/`enforce_limit` take the scanned `LibraryEntry` instead of a bare identifier. Storage stays process-free and no second in-memory index exists. Recorded in the ticket and the module header.
- Finalization derives the usable lead-in from an extra Matroska-to-`/dev/null` remux of the trim, because `-sseof` progress under-reports the keyframe-aligned file by ~0.9 s and the bundled FFmpeg has no `ffprobe`. The trim's own progress value remains the fallback.
- The shutdown grace for an in-flight automatic finalization is 30 s (`MediaConfig::finalize_grace`); WR-000/WR-015 named no number. WR-008 may lower it from configuration without changing the worker.
- Correlation primary selection is the first entry in reverse-chronological order with the media path as tie-break. The baseline used directory `mtime` order, which is not reproducible across a copy or restore.
- Legacy clips carry no parent identifier, so the parent is rebuilt by stripping the ` - Clipped at <date>` suffix the baseline appends to the source video name.
- The Korean legacy category translation (`convertKoreanVideoCategory`) is not ported; unknown categories map to `Category::Unknown` and still display. This fork is English-only and Linux-only.
- The clip seek is input-side (`-ss` before `-i`), matching fluent-ffmpeg's `setStartTime`; an earlier draft of this change sought on the output side, which starts the clip up to one GOP later than the baseline.
- The kill-video tag carries an extra `. Viewpoints: <titles>` suffix so the Clips sidecar records multi-source provenance; the baseline tag ends after the creation date. The kill-video name appends the encounter only when a difficulty is also known, matching the baseline condition.
- The worker treats a disconnected control channel as shutdown, and the interruption deadline is fixed at first observation: previously every disconnected poll pushed the finalize grace forward, so a dead coordinator would have let an in-flight FFmpeg run past its grace while busy-spinning. `a_dropped_control_channel_still_interrupts_finalization` covers it.

## Skipped (YAGNI)

- No corrupt-JSON or malformed-media matrix beyond the three skip diagnostics the scan actually produces.
- No interrupted-recording recovery, capture journal, or undelete: the architecture cannot create a resumable state and the sweep only quarantines.
- No thumbnail, database, per-entry worker, background queue persistence, export presets, or arbitrary filter support.

## Known limitations

- Stream-copy clips and trims are keyframe-aligned, so real durations differ from the request by up to one GOP (6.033 s for a requested 4 s above, with the lead-in landing on the preceding keyframe exactly as the baseline's input-side seek does). This matches the baseline and is inherent to copying without re-encoding.
- The real-media evidence uses the WR-006 desktop captures, not a live WoW session; end-to-end automatic capture through finalization is WR-008's headless flow evidence.

## Approval

- Reviewer: maintainer (via user authorization in this session).
- Date/result: 2026-07-20 — automated checks and real sandbox clip/montage evidence pass; WR-007 `DONE`.
