# Changelog

Notable changes to the native Linux/Wayland application. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Release history before the native rewrite belongs to the upstream Electron
project, [aza547/wow-recorder](https://github.com/aza547/wow-recorder).

## 1.0.9 - 2026-09-06

### Fixed
- A seek issued while Clapper is still prerolling is deferred until the item
  is ready, instead of dropping it and wedging every later seek for that
  video.
- Scrubbing back past the first cast of a selected meter spell no longer
  panics the process.
- A second launcher invocation claims the single-instance lock before any
  storage work, so it no longer moves the first instance's active recording
  into Recovery.

### Changed
- Finishing a recording, deleting, and evicting now update the library index
  in place instead of rescanning the whole library, which blocked the
  coordinator and allocated heavily on large libraries.
- A hidden combat meter no longer rebuilds its widgets on every tick, and the
  occurrence and death histories are virtualized, keeping large meters cheap.

## 1.0.8 - 2026-09-03

### Fixed
- Captures that begin and end in the same moment no longer desynchronize the
  gpu-screen-recorder recording toggle, which had silently stopped every later
  capture from producing a file.
- A capture that produces no video file now replaces the recorder child, so
  the next recording still saves instead of going silent.

## 1.0.7 - 2026-08-21

### Added
- A local combat meter is available alongside each recording, with damage
  done, damage taken, healing, interrupts, dispels, casts and deaths. It
  supports current-fight and overall views, player spell and target
  breakdowns, and seeking from meter rows into the video.
- Previous and next controls jump between the visible combat-timeline markers.

### Changed
- Player controls now focus on video and combat review; the drawing overlay
  has been removed.

### Fixed
- Combat logs with the newer UTC-offset timestamp suffix are parsed instead of
  silently rejecting every event.

## 1.0.6 - 2026-08-13

### Added
- Midnight patch 12.1 raid encounters and Mythic+ dungeons are now recognized
  and recorded.

## 1.0.5 - 2026-08-10

### Fixed
- Resolved screen-capture crashes no longer leave stale problem indicators
  after automatic restart, rearming, or capture-target reselection.

## 1.0.4 - 2026-08-09

### Fixed
- Combat-log timestamps now use the system timezone and daylight-saving
  changes instead of UTC.
- A stale saved-recording event can no longer be attached to a newer
  recording.

## 1.0.3 - 2026-08-08

### Fixed
- Restarting the media worker no longer deadlocks when the worker is busy:
  the shutdown request now waits to be delivered instead of being dropped.
- Folder validation no longer overwrites an existing probe file: the write
  probe uses an exclusive unique name and reports write failures.

### Changed
- `install.sh` is the documented one-command install: on a machine without an
  AppImage it only adds the remote, installs the app, and starts it, and a
  re-run updates an existing install instead of failing. Missing Flatpak now
  prints the command that installs it, and the installer warns when the
  session cannot record: X11, no screen-capture portal, or no PipeWire, each
  with the fix spelled out.
- README rewritten for players: one install command, the three things the
  system needs, first-run steps, and measured footprint against the Electron
  build.

## 1.0.2 - 2026-07-28

### Added
- A "What's new" dialog on the first start after an update, listing the
  commits between the previous release and the installed version. Closing it
  records the version, so it appears once per update.

## 1.0.1 - 2026-07-28

### Fixed
- The post-migration notice stayed pending after being dismissed, so it
  reappeared on every start: a settings save carried the draft the notice
  itself had opened Settings on, writing the pending flag back.
- "Advanced combat logging is off" was reported for every sandboxed install.
  The check reads `Config.wtf` beside the Logs folder, which the folder portal
  does not export, so an unreadable file now reads as unknown rather than off.
- The AppImage migration left the old app running and its binary on disk.

### Added
- The recording folder and combat-log folder rows pulse until they are
  selected, including after a migration, where the imported paths need picking
  again before the sandbox can reach them.

## 1.0.0 - 2026-07-27

### Added
- Native Rust/GTK4 application: combat-log watching, activity detection,
  `gpu-screen-recorder` capture, JSON sidecar library, playback with a combat
  timeline, clipping, drawing overlay, and local POV switching.
- Live keystone timers from the API when computing a Mythic+ result, with
  hardcoded timers as a fallback.
- A one-time notice on the first launch after a legacy import: what changed,
  what carried over, and the two folders the sandbox needs selected again.

### Changed
- Flatpak is the only install and update path. Recordings and configuration
  stay local; legacy configuration is imported once and left untouched.
- Cloud, account, upload, and localization features are not part of this fork.
- `install.sh` migrates an AppImage install instead of replacing it: it
  installs the Flatpak, preserves the AppImage for rollback, retires the
  AppImage launchers, and starts the native app.
