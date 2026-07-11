# Tauri/Rust Port Contract

Single source of truth for the Electron → Tauri v2 port. All implementation
agents follow this. Branch: `tauri-port`. Linux/Wayland only.

## Goal

Minimal E2E port: watch WoW retail combat logs → drive gpu-screen-recorder →
cut/queue videos with metadata JSON → list/play/delete/protect/tag/clip in the
React UI → settings persisted to config JSON. Not a 1:1 port.

## Scope cuts (delete, do not port)

- Windows paths, OBS/noobs, uiohook/global hotkeys, Poller (rust-ps.exe), powerMonitor.
- Classic / Era / PTR flavours: retail only. Remove their settings UI and config usage from ported UI (keep unknown config keys untouched on disk).
- Kill video compositing (KillVideoDialog/KillVideoSourceTimeline/KillVideoProgress, queueCreateKillVideo).
- Auto-updater (updateService, UpdateDialog, CheckForUpdatesButton, update IPC).
- Cloud remnants (`videoButtonCloud`, `deleteVideosCloud`, `cloud` flags stay in types but always false).
- System tray, minimizeToTray behaviour (plain minimize/close), volmeter/audio-settings preview (Linux stubs already), drawing overlay preview reconfigure channels (`configurePreview`, `volmeter`, `audioSettingsOpen`, source-position channels, `getAllDisplays`, `getEncoders`, `getNextKeyPress`, `getSensibleEncoderDefault`).

## Kept behaviour (spec = existing TS files)

- Config schema/defaults: `src/config/configSchema.ts`, validation: `src/utils/configUtils.ts` (`getBaseConfig`, `validateBaseConfig`). File stays `config-v3.json`, flat JSON `{key: value}`, in the Tauri app config dir.
- GSR recorder: `src/main/recording/LinuxRecorder.ts` (replay buffer + regular recording, SIGUSR1/SIGRTMIN, `-sc` hook script → events tsv, portal token file, ffmpeg trim/concat combine, crash restart with backoff, cleanup).
- Combat log parsing: `src/parsing/*` (CombatLogWatcher tail semantics, LogLine, LogHandler + RetailLogHandler) and `src/activitys/*` (retail: RaidEncounter, ChallengeModeDungeon, ArenaMatch, SoloShuffle, Battleground, Manual).
- Static data: extract the retail subsets needed for parsing from `src/main/constants.ts` into Rust.
- Video pipeline: `src/main/VideoProcessQueue.ts` (ffmpeg cut with offset/duration, output name, `.json` metadata sidecar — same format as today, see `Metadata` in `src/main/types.ts`), `src/storage/DiskClient.ts`, `src/storage/DiskSizeMonitor.ts` (size-cap pruning of unprotected videos).
- Manager state machine: `src/main/Manager.ts` — on valid config: start GSR buffer; on activity start: start recording; on activity end: stop → combine → queue → refresh videos + status events. Reconfigure = stop, revalidate, restart.
- Playback: replace `vod://wcr/<path>` with Tauri asset protocol (`convertFileSrc`), asset scope limited to `storagePath`.
- Frontend: existing React renderer reused. `window.electron` preload API is emulated by `src/renderer/electronShim.ts` over `@tauri-apps/api` invoke/listen. `sendSync('config', ['get', key])` is served from a config cache preloaded before React renders and updated on every set.

## Layout & ownership

| Path | Owner |
|---|---|
| `src-tauri/**` (Cargo.toml, tauri.conf.json, src/) | Rust agents |
| `src-tauri/src/{main.rs,state.rs,config.rs,types.rs,commands.rs,manager.rs}` | Agent B (skeleton) / Agent F (integration) |
| `src-tauri/src/recorder/**` | Agent C |
| `src-tauri/src/parser/**` (watcher, logline, handler, activities, constants) | Agent D |
| `src-tauri/src/storage/**` (video queue, disk client, size monitor) | Agent E |
| `index.html`, `vite.config.ts`, `package.json`, `tsconfig.json`, `tailwind.*`, `src/renderer/**`, `src/localisation/**`, `src/types/**` | Agent A (frontend) |
| Deletion of Electron leftovers, CI, docs | Agent G (final) |

Rust: tokio async, `serde`/`serde_json`, `notify` crate for fs watching, spawn
`ffmpeg`/`gpu-screen-recorder` from PATH. Keep camelCase JSON via
`#[serde(rename_all = "camelCase")]` so existing TS types work unchanged.

## Tauri commands (renderer → backend)

All payloads/returns JSON-camelCase matching existing TS types in `src/main/types.ts`.

| Command | Args | Returns | Replaces |
|---|---|---|---|
| `config_get_all` | – | full config object | preload for sendSync cache |
| `config_set` | `key, value` | – | `config` set |
| `config_set_values` | `{key: value}` map | – | `config` set_values |
| `reconfigure_base` | – | – | `reconfigureBase` |
| `select_path` | – | `String` (dir, '' if cancelled) | `selectPath` |
| `select_file` | – | `String` | `selectFile` |
| `get_videos` | – | `RendererVideo[]` | initial `setDiskVideos` |
| `delete_videos` | `videoPaths: String[]` | – | `deleteVideosDisk` |
| `protect_videos` | `videoPaths, protect: bool` | – | `videoButtonDisk` protect |
| `tag_videos` | `videoPaths, tag: String` | – | `videoButtonDisk` tag |
| `open_in_explorer` | `path` | – | `videoButtonDisk` open / `logPath` |
| `clip_video` | `source, offset: f64, duration: f64, metadata` | – | `clip` |
| `recorder_start` / `recorder_restart` / `recorder_stop` / `recorder_save_replay` | – | – | `recorder` linux* |
| `get_gsr_audio_devices` | – | `{inputs, outputs}` | `getLinuxGsrAudioDevices` |
| `toggle_manual_recording` | – | – | `toggleManualRecording` |
| `force_stop_recording` | – | – | `forceStopRecording` |
| `test_run` | `category: String, endTest: bool` | – | `test` (injects canned log lines into parser) |
| `write_clipboard` | `text` | – | `writeClipboard` (or tauri clipboard plugin) |
| `open_url` | `url` | – | `openURL` (tauri opener plugin) |
| `get_app_version` | – | `String` | `updateVersionDisplay` |

Window min/max/close: use Tauri window API directly in the shim (`window` channel).
`videoPlayerSettings` get/set: keep renderer-local in the shim (in-memory), no backend.

## Tauri events (backend → renderer)

Same payload shapes the renderer already handles (see `src/main/types.ts`):

`updateRecStatus {status, msg?}`, `updateActivityStatus`, `setDiskVideos`,
`updateDiskStatus {usage, limit}`, `updateMicStatus`, `playAudio` (SoundAlerts),
`pausePlayer`, `updateAdvancedLoggingStatus`, `updateErrorReport {date, reason}`.
Window focus: shim maps Tauri window focus events to `window-focus-status`.

## Acceptance for every agent

- `cargo check` (and `cargo test` where tests exist) passes in `src-tauri`.
- `npx tsc --noEmit` and `npm run build` pass for frontend changes.
- No new heavyweight dependencies without need. Follow existing code style.
