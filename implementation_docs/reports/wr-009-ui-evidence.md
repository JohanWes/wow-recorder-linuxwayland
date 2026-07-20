# WR-009 evidence: native shell and UI system

Status: **code complete and verified on host.** Formal dark/light/narrow
Flatpak screenshots remain for maintainer sign-off (see "Remaining" below).

## What was built

The native GTK4/libadwaita shell described in WR-009 and `UI-BRIEF.md`:

- `main.rs` — process entry: starts the coordinator and tray service, runs the
  shell, joins both handles after the loop returns. No detached threads.
- `ui/mod.rs` — the one 33 ms pump (drains tray events, applies the newest
  capacity-one snapshot, sends one `Shutdown` on tray Quit, quits on
  `CoordinatorStopped`), the primary menu, and the typed `ShellAction` sink.
- `ui/window.rs` — one `AdwApplicationWindow` → `AdwNavigationSplitView` →
  vertical `GtkPaned` (black player placeholder above, toolbar + `GtkColumnView`
  / empty-state `Stack` below), setup/problem/busy banners, About/Settings/Open
  logs actions, close/minimize-to-tray with application hold while hidden.
- `ui/sidebar.rs` — product mark, status card, ten category rows in WR-000
  order with derived counts and `hide_empty_categories` behaviour (Manual
  stays visible when manual recording is on), Settings at the bottom.
- `ui/status.rs` — the `StatusCard`: every `RecorderStatus` variant mapped to a
  visible state, elapsed anchor rendered by one 1 s timeout, Force end only for
  automatic recordings, per-flavour advanced-combat-logging warnings, and the
  bounded recovered-problem list with recovery actions.
- `ui/tray.rs` — pure hide/close/minimize decisions (never hide the only window
  without a watcher), integrating WR-002's `tray_backend`.
- `ui/style.css` + `data/resources.gresource.xml` — one GResource with the
  shell CSS, category symbolic icons, product mark, and icon-license notices.

## Defects found and fixed while completing the ticket

The half-finished branch had never compiled, so several defects were latent:

1. **`data/resources.gresource.xml` was invalid** — it used `sourcedir`
   attributes on `<gresource>`/`<file>`, which are not valid GResource XML, so
   `glib-compile-resources` failed and the bundle never built. Rewritten with
   valid syntax, two build-script `--sourcedir` roots, and the hicolor
   `icons/scalable/{actions,apps}` layout that `GtkIconTheme::add_resource_path`
   actually resolves (flat files under the path do not resolve).
2. **`libadwaita` had no version feature** — `Banner`, `NavigationSplitView`,
   `NavigationPage`, `ToolbarView`, `PreferencesDialog`, `AboutDialog`, and
   `Spinner` need `v1_6`. Enabled `features = ["v1_6"]` (host and GNOME 50
   runtime both ship libadwaita ≥ 1.9).
3. **`CoordinatorHandle::shutdown` took `self`** but `main` holds the handle in
   the shared `Rc<RefCell<…>>` the GTK closures also capture. Changed to
   `&mut self`; `join.take()` still guards the `Drop` path from joining twice.
4. **`elapsed_label` rendered negative time** — `i64::saturating_sub` clamps at
   `i64::MIN`, not zero, so a now-before-anchor snapshot produced `"0:-4"`. The
   author's own test asserted `"0:00"`; added `.max(0)`. (This is exactly the
   kind of bug the "never compiled" state hid.)
5. Assorted integration fixes: `ColumnView::new(None::<SelectionModel>)`,
   minimize handling via `GdkToplevel::connect_state_notify` (state is a
   toplevel property, not a base-surface one), an `Rc::clone` for the sink used
   after `Sidebar::new`, a cloned parent for the Open-logs fallback launcher,
   and removed dead imports/fields.

## Verification (repo root, host toolchain)

All standard checks pass with `--manifest-path native/Cargo.toml`:

| Check | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets --all-features -- -D warnings` | clean |
| `cargo test --all-targets` | 101 passed, 0 failed |
| `cargo build --release` | ok |

Host libraries used: gtk4 4.22, libadwaita 1.9, libclapper/libclapper-gtk 0.10
(the last two installed to unblock the WR-011 dependency already declared in
`Cargo.toml`; WR-009 itself adds no player code).

## Smoke launch

`./native/target/release/warcraft-recorder` launches cleanly on KDE Wayland:
GTK/libadwaita initialise, the GResource and icon theme register with no
warnings, and the shell renders as designed. Observed with no configured
recording directory:

- sidebar with product mark + "Warcraft Recorder", a "Setup required" status
  card, all ten category rows with `0` counts and their symbolic icons, and
  Settings (with a warning glyph) at the bottom;
- selecting **Mythic+** set the content title to "Mythic+";
- the setup banner "Choose the recording directory again to authorize access."
  with **Open Settings**;
- the black player placeholder ("No recording selected") above the search /
  date-range toolbar and the "No recordings in this category" empty state.

(The arena 2v2/3v3/5v5 icons are the Lucide rounded-square glyphs — verified as
the intended asset, not a missing-icon fallback.)

## Remaining for maintainer sign-off

Per the ticket's Verification section, a maintainer records the formal evidence
**inside the Flatpak sandbox** and compares workflow/identity with the current
app: dark and light 1440×900 screenshots, one narrow-window screenshot, the
widget-inspector hierarchy, the startup/setup/recording/finalizing/error
states, and keyboard traversal. The shell is ready for that pass.
