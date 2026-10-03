# Release notes

Commit subjects per release, written by `scripts/generate-release-notes.sh` and
compiled into the binary: the "What's new" dialog reads the section matching the
running version. Only `## <version>` headings and `- ` lines are parsed.

## 1.0.13
- Update intro video with smoother meter transitions
- Run replay buffer only while World of Warcraft runs
- Cap player demuxer queue so playback stops loading whole files

## 1.0.12
- Show the damage meter's Target menu entry in full
- Give app icon an orange legendary border
- Draw the tray icon from the app's own icon so it matches the running build

## 1.0.11
- Write native sidecars as compact JSON
- Move spell data to a separate mmapped gresource and align embedded bundle
- Enable fat LTO and one codegen unit in Flatpak release builds
- Shift per-spell target samples when finalizing the meter
- End an overrunning capture early so a superseding activity is kept
- Reset the capture restart backoff after a stable child exits
- Run test recordings in Test mode so the status card labels them
- Drop x264, swscale, avdevice and unused filters from the FFmpeg build
- Remove the no-op ActivityAction::Update timeline action
- Drop the always-equal Begin.detected_at_ms from activity actions
- Load damage meters on demand instead of holding them in the index
- Drop unread recorder API: StartRequest.mode, CaptureStarted, RestartScheduled.at_ms
- Scan the library once at startup and sweep against that scan
- Remove never-produced Reconfiguring, Buffering and Fatal recorder statuses
- Drop test-only LogTailer replay mode and path accessor
- Limit runtime sweeps to failed captures and wait for idle media work
- Log skipped sidecars on rescan and drop unused storage API
- Trim Complete and Abandon actions to the id the coordinator reads
- Stop parsing combat event fields the activity engine never reads
- Resolve the config path once in Setup::from_environment
- Move the recorder's log tail helper into process.rs
- Use storage::now_unix_ms instead of the recorder's own copy
- Define EMPTY_GUID once in the parser and share it
- Share the raid-marker read and the utility-event arm in the parser
- End classic arenas through the battleground death-count path
- Move activity data tables and MD5 into activity/tables.rs
- Unload the player when a rebuild leaves the library table empty
- Select the solo-shuffle round roster through one helper pair
- Switch sidebar category on selection so arrow keys navigate
- Keep player shortcuts out of dialogs and popovers
- Drain the shell only on wakes and drop the 250 ms poll
- Use one shared row context menu and connect cell handlers once
- Reuse unchanged library row objects so rebuilds keep focus and selection
- Break reference cycles that leaked the Settings dialog on close
- Show a loading page until the first library snapshot arrives
- Repaint the timeline only when the playhead moves half a pixel
- Refresh the viewpoint selector when the same activity is reselected
- Animate library transitions and flip the protect star immediately
- Keep a single fullscreen idle tick across quick fullscreen toggles
- Crossfade between the player placeholder and the video
- Replace the busy banner with toasts and toast clip and delete results
- Fade the status light and drop dead CSS and sidebar row fields
- Update damage meter rows in place instead of rebuilding every tick
- Animate meter bar fills with libadwaita timed animations
- Throttle playhead-driven meter refreshes to ten per second
- Create the Clapper backend on the first load instead of at startup
- Drop meter hover and click workarounds that rebuilt rows needed
- Parse the spell database off the GTK thread with boxed strings
- Defer loading selections until the player is shown, not in the tray
- Reuse shared log tail helper in media jobs
- Drop unused wr-timeline CSS class from timeline widget
- Connect audio device combo handlers once instead of per refresh
- Add Unreleased changelog section for the improvement sprint
- Rewrite README with verified features, setup and measured footprint
- Bump version to 1.0.11 with release notes

## 1.0.10
- Retire the AppImage migration release
- Add buffs tab with per-player buff uptime to meter
- Cut duplicated test boilerplate and dead derived state
- Remove AppImage and Electron config migration code
- Drop legacy Bloodlust backfill and legacy_ids tracking
- Speed up sidecar parsing and keep coordinator responsive during bulk edits
- Drop the Electron sidecar reader and code it kept alive
- Restore a leaner Electron sidecar reader
- Drop the folded Other buff row and show buff icons

## 1.0.9
- Claim the single instance before touching storage
- Release the spell borrow before clearing it
- Skip hidden meter refreshes and virtualize the histories
- Defer seeks until Clapper reports the item ready
- Fold completions into the library index instead of rescanning

## 1.0.8
- Correct the documented application sizes
- Replace the GSR child when a capture produces no video file
- Stop the GSR recording toggle from desyncing
- Prove the next recording saves after a desync
- Use as_chunks for the MD5 block loop

## 1.0.7
- Add focused combat review tools
- Add resizable combat damage meter
- local dmg meter improvements
- Improve local damage meter accuracy and menus
- Sync local meter fights to capturing player combat
- Keep group meter fight active after host death
- Simplify combat meter navigation controls
- Add damage taken and death meter views
- Scope death logs to their fight
- Draw death log rows as draining health bars
- Fix meter row clicks and solidify bar fills
- Add per-spell statistics and target split to meter
- Add casts meter view, overheal detail, and seekable meter rows
- Remove meter row hover highlight and stray images
- Animate meter bar fills between samples
- Strip UTC-offset suffix from combat log timestamps before parsing
- Add spell icons and stable tooltips
- order by spell %
- remove casts in non detailed view
- Document the local combat meter
