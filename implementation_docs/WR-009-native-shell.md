# WR-009: Native shell and UI system

## Goal

Build the recognizable native window composition, styling, navigation, status presentation, and coordinator bridge. Leave full table/player/settings behavior to WR-010–012.

## Dependencies

WR-008 must be `DONE`. Follow `UI-BRIEF.md`; do not invent a second design direction.

## Owned files

- `native/src/main.rs`
- `native/src/ui/mod.rs`
- `native/src/ui/window.rs`
- `native/src/ui/sidebar.rs`
- `native/src/ui/status.rs`
- `native/src/ui/tray.rs`
- `native/src/ui/style.css`
- `data/resources.gresource.xml` and approved reused UI assets
- UI smoke tests limited to state-to-visible-shell behavior
- `implementation_docs/reports/wr-009-ui-evidence.md`

## Implementation

1. Create one `AdwApplicationWindow`. Its root is `AdwNavigationSplitView`: compact category/status sidebar and content pane.
2. Content uses one vertical `GtkPaned`. Top is a black player placeholder with one helpful empty label; bottom contains a toolbar/filter placeholder and empty `GtkColumnView` placeholder. Keep the divider position for the current process only, matching the current app; WR-011 owns the real player.
3. Sidebar shows the product mark/name, `StatusCard`, category rows in WR-000 order, and Settings at the bottom. No Home/Recent/dashboard destinations. Category rows show approved current assets and derived counts; obey `hide_empty_categories` without removing Manual/Clips contrary to WR-000. Selection dispatches `SetSelectedCategory` so the current category restores on restart.
4. `StatusCard` renders the WR-000 distinctions from the snapshot: Setup/invalid, Waiting for WoW, Reconfiguring/arming, Ready/watching logs, Recording with activity title and elapsed anchor, Overrunning, Finalizing/saving progress, Manual, Test, and Fatal error. Add visible Force end only where the baseline permits it. Elapsed display uses one GTK timeout while visible; the coordinator does not publish a snapshot every second.
5. Show microphone status only if WR-000 proves the Linux recorder emits it, plus per-flavour advanced-combat-logging warnings and bounded recovered recorder failures with occurrence/reason plus Open logs/report link. These are status details, not a notification history.
6. Add one window primary menu with placeholders/actions wired where possible: Test recording, Open logs, About. WR-012 supplies actual behavior. Do not create nav pages for them and do not add update UI: updates are Flatpak/software-center-owned.
7. On the GTK thread, install one `glib::timeout_add_local` source at 33 ms. Each tick drains the capacity-one standard snapshot receiver, bounded tray-event receiver, and coordinator-stopped receiver with `try_recv`, applies only the newest snapshot, handles `TrayEvent::Open` by presenting the existing window, and returns immediately when empty. `TrayEvent::Quit` sends typed `Shutdown` once and keeps the GTK loop alive until `CoordinatorStopped`; only then call `Application::quit`. Stop/remove the source with the application. The shape is:

   ```rust
   let source = glib::timeout_add_local(Duration::from_millis(33), move || {
       while let Ok(ev) = tray_rx.try_recv() {
           match ev {
               TrayEvent::Open => window.present(),
               TrayEvent::Quit => request_shutdown_once(&handle, &mut shutdown_sent),
           }
       }
       if let Ok(snapshot) = snapshot_rx.try_recv() {
           apply_snapshot(&widgets, &snapshot); // capacity-one channel: always newest
       }
       if stopped_rx.try_recv().is_ok() {
           app.quit();
       }
       glib::ControlFlow::Continue
   });
   ```

   Do not use the removed gtk-rs `MainContext::channel`, block on `recv`, call GTK from the tray thread, add an async runtime, or create Redux-like stores, component traits, view models per widget, string event names, or JSON IPC.
8. Dispatch typed commands nonblocking. A full channel disables the initiating action briefly and shows one Busy problem; never block the GTK thread.
9. Show setup/error banners with English summary and one recovery action from `Problem`. Technical detail goes in an expandable area and logs; do not add toast history or notifications.
10. Put CSS and assets in one GResource. Use libadwaita defaults except the small palette/density/timeline hooks in UI-BRIEF. Reuse licensed current category/class assets instead of generating a new icon language. Generic actions use GTK symbolic icons.
11. Integrate WR-002's concrete tray backend using the event path above. Hide-to-tray pauses playback presentation but coordinator/GSR continue. Hold the application while hidden. `main`, not a GTK callback or coordinator, owns and joins both top-level handles after `Application::run` returns; coordinator owns/joins its media worker. When the watcher is offline, override hide settings so minimize/close cannot strand the process.
12. Window minimum/default size and narrow collapse follow UI-BRIEF. Do not add persistent layout/window geometry unless WR-000 proves a current persisted preference.

## Acceptance criteria

- At 1440×900 the sidebar, resizable player-above-table structure, toolbar region, and status card are recognizable as an updated current app.
- Category selection changes the content title/placeholder, newest entry intent, and active sidebar state without opening a separate page.
- A generated snapshot containing 2,000 entries can be delivered without parsing/scanning/blocking work on the GTK thread (table population/performance is finalized in WR-010/015).
- All proven status, advanced-logging, recovered-error details, and recovery actions render from typed snapshot data; Force end is sent only when valid.
- Tray Open/Quit, hide/reopen with continued capture, explicit Quit, and no-watcher fallback match UI-BRIEF.
- Sidebar collapse works at the documented narrow width, focus order is visible/logical, icon buttons have names/tooltips, and 200% scaling does not overlap shell controls.
- No generic widget library, thumbnail/card layout, extra navigation, animation system, notification framework, or UI thread doing core work exists.

## Verification

Run standard checks and the app inside Flatpak. Record dark and light 1440×900 screenshots plus one narrow-window screenshot, widget inspector hierarchy, startup/setup/recording/finalizing/error states, and keyboard traversal. A maintainer compares workflow/identity with current UI and signs the report.

## Not in scope

Real table factories/filtering, media playback/timeline, settings form fields, chooser/update/log launching, kill-video editor, or pixel-perfect reproduction of Electron.
