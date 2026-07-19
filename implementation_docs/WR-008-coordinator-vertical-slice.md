# WR-008: Coordinator and complete headless vertical slice

## Goal

Connect config, log polling, activity detection, recorder control, storage, and media jobs behind one typed coordinator. Prove complete recording flows headlessly before GTK feature work.

## Dependencies

WR-004, WR-005, WR-006, and WR-007 must be `DONE`.

## Owned files

- `native/src/coordinator.rs`
- `native/src/lib.rs` exports
- coordinator/integration tests in the owned module or `native/tests/vertical_slice.rs`

## Ownership and channels

- `main` owns the coordinator and tray join handles. The GTK thread holds `CoordinatorHandle` with one bounded `SyncSender<Command>`, one capacity-one standard-library snapshot receiver, and one capacity-one coordinator-stopped receiver.
- One coordinator thread owns `Config`, `LogTailer`s, parser/activity engine, `Recorder`, current library/index, active recording draft, and current status/problems.
- One media/storage worker serially executes `MediaJob`s. It may keep two small FIFO queues so automatic finalization is chosen before user clip/kill-video work; do not add a generic priority queue/framework.
- The WR-002 tray backend has one joinable StatusNotifierItem service thread, also owned by `main`. Its callbacks send only `TrayEvent::Open` or `TrayEvent::Quit` through a bounded standard-library channel polled by GTK; the tray thread never calls GTK or coordinator code. No other long-lived threads, async runtime, global singleton, trait-object service registry, or generic event bus.

Use bounded standard-library channels so accidental UI repetition cannot grow memory. UI calls use `try_send`; a full command queue becomes one Busy problem/action-disabled state. For snapshots, the coordinator keeps at most one unsent newest snapshot locally when the capacity-one channel is full and retries it on the next loop, replacing that pending value when newer state exists. This coalesces state without losing the final state or creating a diff store:

```rust
// pending: Option<Arc<AppSnapshot>>; newest state always wins
let snap = pending.take().unwrap_or_else(|| build_snapshot(&state));
if let Err(TrySendError::Full(unsent)) = snapshot_tx.try_send(snap) {
    pending = Some(unsent); // replaced next loop if newer state arrives
}
```

## Commands

Define concrete variants with only their needed payloads:

- `Arm`, `Disarm`, `ForceEnd`;
- `StartManual`, `StopManual`, `RunTest { category }`;
- `ReselectCaptureTarget`;
- `SaveConfig { draft }`;
- `SetProtected { ids, value }`, `SetTag { id, tag }`, `Delete { ids }`;
- `CreateClip { source, start_ms, end_ms }`;
- `CreateKillVideo { correlated_id, segments, width, height, fps, audio_mode }`;
- `SetSelectedCategory { category }`, `SetMarkerVisibility { deaths, encounters, rounds }` for the current persisted UI fields; media selection/player state stays in UI;
- `Shutdown`.

Do not send closure callbacks, string command names, JSON payloads, or GTK objects.

## Snapshot

Publish one immutable/cloned `AppSnapshot` after observable changes, not on every poll tick. It contains:

- library entries/correlations and per-category counts needed by the UI;
- recorder state, active ID/title/mode/start time, requested replay/regular start, elapsed-time anchor, and microphone state only if WR-000 proves it reachable;
- current config or a sanitized settings view;
- media-job progress/queue count;
- storage used/limit and protected-over-limit warning;
- setup validation, advanced-combat-logging status per retained flavour, and bounded current/recovered recorder `Problem`s matching WR-000's visible error-report behavior;

Prefer simple `Arc<[LibraryEntry]>`/`Arc<AppSnapshot>` sharing if cloning the 2,000-entry model is measured as costly; do not design a diff/observable store pre-emptively. The UI may derive row formatting but not reparse sidecars.

## Coordinator loop

1. On startup, load/migrate config, run Storage's `sweep_orphans`, scan the library, validate setup, and arm when valid/enabled.
2. Repeatedly drain a small bounded number of commands, poll recorder events and log tailers, feed parsed events to the activity engine, and process emitted actions. Use `recv_timeout` for idle pacing; no busy loop or test sleeps.
3. On `Begin`, calculate requested replay as `detected_at - activity_start + extra_lead_in`, clamp to capacity, keep the `RecordingDraft` in memory, and call `Recorder::begin`. If `begin` fails, surface one Problem and drop the draft; there is no on-disk pending state. Keep activity/timeline times absolute because actual media start is unknown.
4. `ForceEnd` calls `ActivityEngine::force_end` with the active flavour/current occurrence time, processes its domain action once, then calls Recorder end. Normal complete/abandon follows the same end/finalization path with the pending draft plus replay/regular artifacts. The media worker trims/concats, calculates actual media start, then converts/clamps ordered point/span times to media offsets. Missing replay yields regular-only output and clips early markers; missing regular is actionable and preserves any replay artifact.
5. On discard/cancel, stop regular capture and remove/quarantine returned artifacts through Storage per WR-000. Successful finalization inserts the entry and enforces only a positive storage limit.
6. Manual/test paths reuse the same recorder/finalize/storage flow with their category/title/timeout behavior from WR-000; do not create separate recorder implementations.
7. `SaveConfig` uses direct field-group comparisons, not a reconfiguration framework: UI-only; activity/log paths; storage root/limit; replay/capture/audio/target. Reject the command while an active recording, overrun, or finalization makes reconfiguration unsafe. Validate and authorization-probe the whole draft first; on any problem, reject without touching disk or runtime. On success, atomically save the draft, then rebuild only the changed groups: reopen tailers, rescan a changed storage root without eviction, rearm the recorder with the new settings, then enforce a positive new storage limit. A subsystem failure after a successful save disarms, keeps the saved config, and surfaces one Problem with a Rearm/Reselect recovery action — do not build draft-arm/rollback staging. Publish success only when runtime and disk both match the draft. `SetSelectedCategory` and `SetMarkerVisibility` patch only their named fields through the same atomic save path and do not reconfigure subsystems.
8. User mutations call `Storage`; update the in-memory index only after success and return per-item problems for partial bulk failure.
9. Capture permission/child/config/path/media failures become stable `Problem` values with one recovery action. Log technical cause once. Do not build a notification/history center.
10. Shutdown stops accepting UI commands, resolves active capture per baseline, shuts down Recorder, sends media-worker Shutdown, and joins that owned worker. Automatic finalization gets WR-007's bounded grace/recovery path; clip/kill jobs cancel. The coordinator sends one `CoordinatorStopped` signal and exits; it never joins itself or the tray. After the GTK loop exits, `main` requests Shutdown if it was not already requested, joins the coordinator, tells the tray backend to stop, and joins the tray. No child/thread may be detached.

## Test seams

Use generic type parameters or one narrow recorder/process test trait only where real OS work must be replaced. The real domain/storage types operate on temp directories. Provide a manual `tick()`/step harness around the same coordinator core so tests advance without wall-clock sleeps; production wraps it in the thread loop.

## Acceptance criteria

- Headless replay of representative raid/PvE, PvP/round, manual, and test flows creates the exact final media-sidecar/index outcomes; WR-005 already covers every category's pure transitions, so do not repeat every fixture here.
- Automatic replay lead-in, hook-correlated two-artifact capture, regular-only fallback, and post-trim media marker offsets are correct.
- Complete, `Abandoned`, force-end, discard, capture failure, restart/rescan with the startup sweep, and graceful shutdown are proven.
- Tag/protect/bulk delete, clip, and kill-video commands serialize through the real storage/media boundary and publish correct snapshots/progress.
- Finalization starts before queued user transcodes without parallel media workers.
- No GTK thread blocking, unbounded channel, polling busy loop, async runtime, generic event system, or duplicated category logic exists.

## Tests

Add four vertical scenarios: one automatic PvE complete, one PvP/round abandon or loss, one manual/test lifecycle, and one queued finalize-before-clip/kill-video case. Add focused failure/shutdown scenarios only for boundaries not covered in WR-006/WR-007. Avoid duplicate parser split tests, every-category integration permutations, and timing assertions.

## Not in scope

GTK widgets, player state, update-network requests, desktop notifications, daemon/service mode, multi-process IPC, or performance optimization without WR-015 evidence.
