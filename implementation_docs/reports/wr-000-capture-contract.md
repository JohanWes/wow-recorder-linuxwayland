# WR-000: recorder/capture contract

## Environment

- Commit `15d7728774a6390db62c7e52f715929b98f8d799`; Wayland host; `gpu-screen-recorder --version` returned `5.13.9` at `/usr/bin/gpu-screen-recorder`.
- No capture was started: portal selection changes desktop capture state and no consent to record a real display/audio stream was inferred. All behavior below is source-derived; exact runtime environment/portal trace remains manual evidence.

## Reproducible argv

`LinuxRecorder.spawnGsrReplay` (`src/main/recording/LinuxRecorder.ts:423-505`) spawns with inherited environment (`env: process.env`) and this exact order:

```text
gpu-screen-recorder
  -w portal
  -restore-portal-session yes
  -portal-session-token-filepath <USER_DATA>/gsr-portal.token
  -r <linuxGsrBufferSeconds>
  -replay-storage <linuxGsrReplayStorage>
  -restart-replay-on-save no
  -c mkv
  -f <obsFPS>
  -bm cbr
  -q <linuxGsrBitrateKbps>
  -k <linuxGsrCodec>
  -ac aac
  -cursor <yes|no>
  -o <OBS_PATH>/replay
  -ro <OBS_PATH>/regular
  -sc <USER_DATA>/gsr-hook.sh
  -v no
  [-a <output>|<input>]
```

Audio uses unique non-empty output then input values joined with literal `|`; output falls back from new `linuxGsrAudioOutput` to legacy `linuxGsrAudio`, input defaults empty (`LinuxRecorder.ts:485-498`). Discovery runs `gpu-screen-recorder --list-audio-devices` with 2 s timeout, recognizes `default_output`, `default_input`, and `device:<nonspace>`, de-duplicates by value, and always inserts missing defaults (`src/main/main.ts:377-483`).

## Portal, event, signal and child lifecycle

- Token: `<USER_DATA>/gsr-portal.token`; `-restore-portal-session yes` reuses it. Reselect calls `restartCapture(true)`, sends SIGINT, waits up to 2 s for exit, deletes the token (ENOENT accepted), then arms again (`LinuxRecorder.ts:300-320,580-598`). The selected monitor/window is portal-owned; no project-side ID exists.
- Hook: generated executable `<USER_DATA>/gsr-hook.sh` appends `epoch_ms<TAB>kind<TAB>filepath` to `<USER_DATA>/gsr-events.tsv`. Kinds accepted: `regular`, `replay`, `screenshot` (`LinuxRecorder.ts:35-116,600-611`). Paths may contain tabs; remaining fields are rejoined.
- Replay save: SIGUSR1, with a 20 s matching-event timeout (`LinuxRecorder.ts:202-228,378-403`). Regular start/stop: numeric SIGRTMIN resolved once by `bash -lc 'kill -l SIGRTMIN'` (`LinuxRecorder.ts:230-256,613-644`). Regular save wait is 30 s.
- Shutdown: SIGINT, cancel restart timer/watch, clear capture state (`LinuxRecorder.ts:322-342`). Before Electron quit invokes recorder shutdown (`src/main/main.ts:663-678`).
- Exit: records code/signal, sets state None, and if capture is desired schedules unbounded retries after 2, 4, 8, 16, then 30 seconds maximum; successful respawn does not reset the attempt counter until a deliberate `startBuffer` (`LinuxRecorder.ts:513-520,541-578`).

## Scenario contracts

| Scenario | Exact contract |
|---|---|
| Arm/buffer | Validate `gpu-screen-recorder --version`; create `replay`, `regular`, `staging` and managed marker; spawn argv; wait 500 ms and fail if exited/no PID; state becomes Recording (`LinuxRecorder.ts:153-200,405-539`). |
| Automatic start | Parser computes `delay=(Date.now-activity.logStart)/1000`, asserts nonnegative, calls `startRecording(delay)` (`LogHandler.ts:216-239`). Recorder waits for replay event, SIGUSR1 saves replay, SIGRTMIN starts regular. |
| Lead-in | At stop, replay trim duration is `max(1, round(delay + linuxGsrLeadInSeconds))`; FFmpeg uses `-sseof -N`, stream-copies audio/video, then concat-demuxer stream-copies replay+regular. Missing replay or transform error falls back to regular only (`LinuxRecorder.ts:646-725`). |
| Automatic stop | Activity overrun elapses first; SIGRTMIN stops regular; await regular/replay events; create `activity-<epoch>-<randomhex>.mkv`; intermediate files are deleted; downstream queue chooses final output naming (`LogHandler.ts:241-350`; `LinuxRecorder.ts:240-292`). |
| Force end | Parser sets overrun zero, end timestamp now plus optional timeout delta, result false, and uses normal stop/finalization (`LogHandler.ts:361-387`). Status-card Force end routes through `Manager.ts:774-781`. |
| Manual | Same recorder start/stop, with activity start timestamp now and Manual metadata; optional start/stop/error MP3 (`LogHandler.ts:514-540`). |
| Test | Feeds canned log lines through active handler; capture semantics are identical (`Manager.ts:241-262`). |
| Reselect | Restart with token deletion as described above. |
| Graceful shutdown | SIGINT child; no wait/join is performed by current Electron `before-quit`. Native adapter must supervise/join according to its ticket without changing user outcome. |

## Intermediate, final, and sidecar naming

- GSR chooses filenames emitted by the hook for the `replay/` and `regular/` directories. The application does not prescribe their basename, so a concrete GSR runtime name remains unverified and is not part of the native contract.
- Automatic concatenation chooses `activity-<Date.now epoch milliseconds>-<Math.random hexadecimal fraction without 0.>.mkv` in the capture root. For example shape only: `activity-1735689600000-a1b2c3.mkv`. This is an intermediate source, not the library name (`LinuxRecorder.ts:646-656`).
- Normal finalization passes that intermediate stem as `name` and `activity.getFileName()` as `suffix`. The queue joins them with literal ` - `, replaces every `< > : " / | ? *` character with a space, collapses consecutive ASCII spaces, and appends `.mp4` in `storagePath`. Thus Linux automatic output has exact shape `activity-<epoch-ms>-<random-hex> - <activity display title>.mp4` after sanitization (`LogHandler.ts:337-349`, `VideoProcessQueue.ts:587-613`).
- User-created clip output uses the selected source's `video.videoName` plus ` - Clipped at YYYY-MM-DD HH-MM-SS.mp4`, with the same sanitization. The timestamp is local time at the clip request (`Manager.ts:601-629`, `util.ts:658-670`).
- Save Replay uses GSR's replay stem plus ` - Replay YYYY-MM-DD HH-MM-SS.mp4`, also sanitized. It cannot be made more concrete without a runtime GSR hook event (`Manager.ts:719-748`).
- Kill video output is `YYYY-MM-DD HH-MM-SS - Multiview - <encounterName> [<difficulty>] - Rendered at YYYY-MM-DD HH-MM-SS.mp4` in `storagePath`. The first timestamp uses activity `start` or filesystem `mtime`; the rendered timestamp is local wall time. The kill-video path does not call the normal filename sanitizer (`VideoProcessQueue.ts:735-755`).
- Every successful final `.mp4` above receives an adjacent `.json` with the identical basename/stem. `getMetadataFileNameForVideo()` strips only `.mp4`; `writeMetadataFile()` writes `JSON.stringify(metadata, null, 2)` as UTF-8 and does not add a trailing newline (`util.ts:140-143,285-293`). Kill videos use the same helper after composing their multi-POV metadata (`VideoProcessQueue.ts:322-364`).

## Runtime evidence still required

- Exact GSR stdout/stderr, PipeWire/portal implementation and token contents/lifecycle, portal-selected target behavior, actual audio devices, signal outcomes, GSR-generated replay/regular filenames, real detection delay and retry after a killed child were not exercised.
- `linuxGsrReplayStorage=disk` destination semantics are delegated to GSR; current argv does not use `bufferStoragePath` to relocate it.

## Skipped (YAGNI)

- No invented capture-token abstraction, monitor ID, retry cap, or portal environment variable is specified because current code supplies none.
