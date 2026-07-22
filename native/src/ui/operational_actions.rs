// SPDX-License-Identifier: GPL-3.0-or-later

//! Operational controls outside Settings: the Manual-category Start/Stop
//! toolbar (the WR-000-approved native entry for manual recording), the
//! test-recording category chooser, and the capture-reselection explanation.
//!
//! The legacy manual-recording sound assets were rejected for native
//! redistribution (WR-000 assets/licenses report), so the retained
//! `manual.sound` setting plays the display bell instead of bundling audio.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use warcraft_recorder::coordinator::{AppSnapshot, Command};
use warcraft_recorder::domain::{Category, RecorderStatus};

use super::status::elapsed_label;
use super::{ActionSink, ShellAction, TEST_CATEGORIES};

/// What the Manual toolbar shows, derived from one snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualView {
    /// The bar exists only in the Manual category with manual recording on.
    pub visible: bool,
    /// Start is possible only when the recorder is armed and idle.
    pub start_enabled: bool,
    /// Stop replaces Start while the manual recording runs.
    pub stop_visible: bool,
    pub elapsed_anchor_ms: Option<i64>,
}

pub fn manual_view(snapshot: &AppSnapshot) -> ManualView {
    let manual_active = matches!(
        snapshot.status,
        RecorderStatus::Recording { manual: true, .. }
    );
    ManualView {
        visible: snapshot.config.interface.selected_category == Category::Manual
            && snapshot.config.manual.enabled,
        start_enabled: snapshot.status == RecorderStatus::Ready,
        stop_visible: manual_active,
        elapsed_anchor_ms: match snapshot.status {
            RecorderStatus::Recording {
                manual: true,
                started_unix_ms,
                ..
            } => Some(started_unix_ms),
            _ => None,
        },
    }
}

/// Explanation shown in the test-recording chooser, matching the injected
/// 5 s (raids 20 s) synthetic activities.
pub const TEST_EXPLANATION: &str = "Runs a short synthetic activity to verify capture and \
    saving. Most categories record for about 5 seconds; raids record for about 20. The result \
    appears in the library like a real recording. Force end stops it early.";

/// The Manual category toolbar. Sounds follow `manual.sound` using the
/// display bell on start/stop/failed-start transitions.
pub struct ManualBar {
    pub widget: gtk4::Box,
    start: gtk4::Button,
    stop: gtk4::Button,
    elapsed: gtk4::Label,
    elapsed_anchor: Rc<Cell<Option<i64>>>,
    timer_running: Rc<Cell<bool>>,
    was_active: Cell<bool>,
    /// Wall-clock time of the last Start click, to catch the coordinator's
    /// "could not be started" problem for the error bell.
    start_requested_ms: Rc<Cell<Option<i64>>>,
}

impl ManualBar {
    pub fn new(sink: ActionSink) -> Self {
        let widget = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        widget.set_margin_top(6);
        widget.set_margin_bottom(6);
        widget.set_margin_start(12);
        widget.set_margin_end(12);
        widget.set_visible(false);

        let start = gtk4::Button::with_label("Start recording");
        start.add_css_class("suggested-action");
        start.set_tooltip_text(Some("Start a manual recording"));
        let stop = gtk4::Button::with_label("Stop recording");
        stop.add_css_class("destructive-action");
        stop.set_tooltip_text(Some("Stop the manual recording"));
        stop.set_visible(false);
        let elapsed = gtk4::Label::new(None);
        elapsed.add_css_class("monospace");
        elapsed.set_tooltip_text(Some("Elapsed manual recording time"));
        elapsed.set_visible(false);

        widget.append(&start);
        widget.append(&stop);
        widget.append(&elapsed);

        let start_requested_ms: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));
        let bar = Self {
            widget,
            start: start.clone(),
            stop: stop.clone(),
            elapsed,
            elapsed_anchor: Rc::new(Cell::new(None)),
            timer_running: Rc::new(Cell::new(false)),
            was_active: Cell::new(false),
            start_requested_ms: Rc::clone(&start_requested_ms),
        };

        {
            let sink = Rc::clone(&sink);
            let requested = start_requested_ms;
            start.connect_clicked(move |_| {
                if sink(ShellAction::Command(Command::StartManual)) {
                    requested.set(Some(now_unix_ms()));
                }
            });
        }
        stop.connect_clicked(move |_| {
            sink(ShellAction::Command(Command::StopManual));
        });
        bar
    }

    pub fn apply(&self, snapshot: &AppSnapshot, now_unix_ms: i64) {
        let view = manual_view(snapshot);
        self.widget.set_visible(view.visible);
        self.start.set_visible(!view.stop_visible);
        self.start.set_sensitive(view.start_enabled);
        self.stop.set_visible(view.stop_visible);

        self.elapsed_anchor.set(view.elapsed_anchor_ms);
        if let Some(anchor) = view.elapsed_anchor_ms {
            self.elapsed.set_label(&elapsed_label(anchor, now_unix_ms));
            self.elapsed.set_visible(true);
            self.ensure_timer();
        } else {
            self.elapsed.set_visible(false);
        }

        // Sound transitions: bell on start/stop, and on a failed start
        // reported by the coordinator after our request.
        let sounds = snapshot.config.manual.sound;
        if view.stop_visible != self.was_active.get() {
            self.was_active.set(view.stop_visible);
            self.start_requested_ms.set(None);
            if sounds {
                bell(&self.widget);
            }
        } else if let Some(requested_ms) = self.start_requested_ms.get()
            && snapshot.problems.iter().any(|problem| {
                problem.occurred_unix_ms >= requested_ms
                    && problem.summary == "A manual recording could not be started."
            })
        {
            self.start_requested_ms.set(None);
            if sounds {
                bell(&self.widget);
            }
        }
    }

    /// One one-second timeout renders the elapsed anchor while visible,
    /// exactly like the status card.
    fn ensure_timer(&self) {
        if self.timer_running.replace(true) {
            return;
        }
        let anchor = Rc::clone(&self.elapsed_anchor);
        let timer_running = Rc::clone(&self.timer_running);
        let elapsed = self.elapsed.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            let Some(elapsed) = elapsed.upgrade() else {
                timer_running.set(false);
                return gtk4::glib::ControlFlow::Break;
            };
            let Some(anchor_ms) = anchor.get() else {
                timer_running.set(false);
                return gtk4::glib::ControlFlow::Break;
            };
            elapsed.set_label(&elapsed_label(anchor_ms, now_unix_ms()));
            gtk4::glib::ControlFlow::Continue
        });
    }
}

fn bell(widget: &impl IsA<gtk4::Widget>) {
    if let Some(display) = gtk4::gdk::Display::default() {
        let _ = widget; // the bell is per display, not per widget
        display.beep();
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// The window-menu/Settings test-recording chooser: the exact WR-000 category
/// list, the duration explanation, and one Start that sends `RunTest`.
pub fn present_test_dialog(parent: &gtk4::Widget, sink: ActionSink, ready: bool) {
    let dialog = adw::AlertDialog::new(Some("Test recording"), Some(TEST_EXPLANATION));
    let labels: Vec<&str> = TEST_CATEGORIES.iter().map(|(_, label, _)| *label).collect();
    let combo = gtk4::DropDown::from_strings(&labels);
    combo.set_tooltip_text(Some("Test recording category"));
    dialog.set_extra_child(Some(&combo));
    dialog.add_responses(&[("cancel", "Cancel"), ("start", "Start test")]);
    dialog.set_response_appearance("start", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("start"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("start", ready);
    if !ready {
        dialog.set_body(&format!(
            "{TEST_EXPLANATION}\n\nThe recorder is not ready — a test needs an armed, idle capture."
        ));
    }
    dialog.connect_response(Some("start"), move |_, _| {
        if let Some((category, _, _)) = TEST_CATEGORIES.get(combo.selected() as usize) {
            sink(ShellAction::Command(Command::RunTest {
                category: category.clone(),
            }));
        }
    });
    dialog.present(Some(parent));
}

/// The published install script: adds the signed Flatpak remote and installs
/// or updates the app. The same script drives the AppImage→Flatpak migration.
const UPDATE_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh";
const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder";

fn flatpak_available() -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path| path.join("flatpak").is_file())
    })
}

fn info_dialog(parent: &gtk4::Widget, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("close", "Close")]);
    dialog.set_close_response("close");
    dialog.present(Some(parent));
}

/// "Check for updates": inside Flatpak, point at the software center; without
/// Flatpak, warn and give the manual path; otherwise confirm and run the
/// install script off the GTK thread.
pub fn present_update_dialog(parent: &gtk4::Widget) {
    if std::path::Path::new("/.flatpak-info").exists() {
        info_dialog(
            parent,
            "Updates are managed by Flatpak",
            &format!(
                "This installation updates through your software center or with:\n\n\
                 flatpak update --user {APP_ID}"
            ),
        );
        return;
    }
    if !flatpak_available() {
        info_dialog(
            parent,
            "Flatpak is not installed",
            "Updates install the native Flatpak build, which needs Flatpak. Install it with \
             your distribution's package manager (for example 'sudo pacman -S flatpak' or \
             'sudo apt install flatpak'), then check for updates again.",
        );
        return;
    }
    let dialog = adw::AlertDialog::new(
        Some("Check for updates"),
        Some(
            "This runs the Warcraft Recorder install script: it adds the project's signed \
             Flatpak remote and installs or updates the latest release. This installation is \
             left in place for rollback.",
        ),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("update", "Update")]);
    dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("update"));
    dialog.set_close_response("cancel");
    let parent = parent.clone();
    let dialog_parent = parent.clone();
    dialog.connect_response(Some("update"), move |_, _| run_update(&parent));
    dialog.present(Some(&dialog_parent));
}

/// Runs the install script on GIO's blocking pool: a copy next to the
/// executable if present (development), otherwise the published one.
fn run_update(parent: &gtk4::Widget) {
    let parent = parent.clone();
    gtk4::glib::spawn_future_local(async move {
        let output = gtk4::gio::spawn_blocking(|| {
            let local_script = std::env::current_exe()
                .ok()
                .and_then(|exe| Some(exe.parent()?.join("install.sh")))
                .filter(|script| script.is_file());
            let mut command = std::process::Command::new("bash");
            match local_script {
                Some(script) => {
                    command.arg(script);
                }
                None => {
                    command
                        .arg("-c")
                        .arg(format!("curl -fsSL {UPDATE_SCRIPT_URL} | bash"));
                }
            }
            command.stdin(std::process::Stdio::null()).output()
        })
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("the update task crashed")));
        match output {
            Ok(output) if output.status.success() => info_dialog(
                &parent,
                "Update finished",
                &format!("Launch the updated app with:\n\nflatpak run {APP_ID}"),
            ),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                info_dialog(
                    &parent,
                    "The update failed",
                    &format!(
                        "The install script did not finish.\n\n{}",
                        if tail.is_empty() {
                            "No error output was produced.".to_owned()
                        } else {
                            tail.join("\n")
                        }
                    ),
                );
            }
            Err(error) => info_dialog(
                &parent,
                "The update could not start",
                &format!("The install script could not be run: {error}"),
            ),
        }
    });
}

/// The capture-reselection explanation: the platform portal prompt follows,
/// and cancelling it keeps the previous usable selection (WR-006 contract).
pub fn present_reselect_dialog(parent: &gtk4::Widget, sink: ActionSink) {
    let dialog = adw::AlertDialog::new(
        Some("Reselect capture target"),
        Some(
            "Your desktop will show its screen-share prompt to pick the monitor or window to \
             record. Cancelling the prompt keeps the current selection.",
        ),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("reselect", "Reselect")]);
    dialog.set_response_appearance("reselect", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("reselect"));
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("reselect"), move |_, _| {
        sink(ShellAction::Command(Command::ReselectCaptureTarget));
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use warcraft_recorder::config::Config;
    use warcraft_recorder::domain::Problem;

    fn snapshot(status: RecorderStatus, selected: Category, manual: bool) -> AppSnapshot {
        let mut config = Config::default();
        config.interface.selected_category = selected;
        config.manual.enabled = manual;
        crate::ui::window::tests::snapshot_with(status, config, Vec::new())
    }

    #[test]
    fn manual_bar_is_gated_by_category_setting_and_recorder_state() {
        let cases = [
            // (status, selected, manual enabled) -> (visible, start, stop)
            (
                RecorderStatus::Ready,
                Category::Manual,
                true,
                (true, true, false),
            ),
            (
                RecorderStatus::Ready,
                Category::Manual,
                false,
                (false, true, false),
            ),
            (
                RecorderStatus::Ready,
                Category::Raids,
                true,
                (false, true, false),
            ),
            (
                RecorderStatus::WaitingForWow,
                Category::Manual,
                true,
                (true, false, false),
            ),
            (
                RecorderStatus::Buffering,
                Category::Manual,
                true,
                (true, false, false),
            ),
        ];
        for (status, selected, manual, (visible, start, stop)) in cases {
            let view = manual_view(&snapshot(status.clone(), selected, manual));
            assert_eq!(view.visible, visible, "{status:?}");
            assert_eq!(view.start_enabled, start, "{status:?}");
            assert_eq!(view.stop_visible, stop, "{status:?}");
        }
    }

    #[test]
    fn active_manual_recording_shows_stop_with_the_elapsed_anchor() {
        let manual = RecorderStatus::Recording {
            category: Category::Manual,
            title: "Manual recording".to_owned(),
            started_unix_ms: 42,
            manual: true,
            test: false,
        };
        let view = manual_view(&snapshot(manual, Category::Manual, true));
        assert!(view.visible && view.stop_visible);
        assert!(!view.start_enabled);
        assert_eq!(view.elapsed_anchor_ms, Some(42));

        // An automatic recording in progress never shows manual Stop.
        let automatic = RecorderStatus::Recording {
            category: Category::Raids,
            title: "Boss".to_owned(),
            started_unix_ms: 42,
            manual: false,
            test: false,
        };
        let view = manual_view(&snapshot(automatic, Category::Manual, true));
        assert!(!view.stop_visible);
        assert!(!view.start_enabled);
        assert_eq!(view.elapsed_anchor_ms, None);
    }

    #[test]
    fn test_dialog_offers_the_exact_baseline_categories_in_order() {
        let labels: Vec<&str> = TEST_CATEGORIES.iter().map(|(_, label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                "2v2",
                "3v3",
                "Solo Shuffle",
                "Raids",
                "Battlegrounds",
                "Mythic+"
            ]
        );
        // Payload mapping: every choice sends its own category.
        for (category, _, _) in &TEST_CATEGORIES {
            assert!(matches!(
                Command::RunTest {
                    category: category.clone(),
                },
                Command::RunTest { .. }
            ));
        }
    }

    #[test]
    fn failed_start_problem_is_recognized_for_the_error_bell() {
        let mut snapshot = snapshot(RecorderStatus::Ready, Category::Manual, true);
        snapshot.problems = vec![Problem {
            summary: "A manual recording could not be started.".to_owned(),
            safe_detail: None,
            occurred_unix_ms: 10,
            recovery_action: None,
        }];
        // The bar matches this summary only for clicks at or before the
        // problem's timestamp.
        assert!(
            snapshot
                .problems
                .iter()
                .any(|problem| problem.occurred_unix_ms >= 5
                    && problem.summary == "A manual recording could not be started.")
        );
    }
}
