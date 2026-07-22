// SPDX-License-Identifier: GPL-3.0-or-later

//! Native GTK4/libadwaita shell.
//!
//! The GTK thread owns widgets only. It receives immutable `AppSnapshot`s
//! through the capacity-one coordinator channel and dispatches typed
//! `Command`s back; every send is nonblocking. One 33 ms timeout drains the
//! snapshot, tray-event, and coordinator-stopped receivers. No stores, no
//! per-widget view models, no string events, no blocking work.

pub mod drawing;
pub mod filters;
pub mod library;
pub mod multipov;
pub mod operational_actions;
pub mod player;
pub mod player_backend;
pub mod settings;
pub mod sidebar;
pub mod status;
pub mod timeline;
pub mod tray;
pub mod tray_backend;
pub mod window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gtk4::prelude::*;
use libadwaita as adw;

use warcraft_recorder::coordinator::{Command, CoordinatorHandle};
use warcraft_recorder::domain::Category;

use tray_backend::{TrayBackend, TrayEvent};

/// Everything `run` needs that is not the coordinator or the tray.
pub struct ShellOptions {
    pub app_id: &'static str,
    /// Recorder diagnostics directory (`gsr.log`, events, hook, token).
    pub data_dir: PathBuf,
    /// Fallback directory for "Open logs" when `data_dir` does not exist yet.
    pub config_dir: PathBuf,
    /// Initial interface flags read before the GTK loop starts; live values
    /// arrive with the first snapshot.
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub minimize_to_tray: bool,
}

/// The one sink every widget uses to reach the coordinator or shell actions.
/// Returns `false` when the command channel was full (caller shows one Busy
/// problem and may disable the initiating widget briefly).
pub type ActionSink = Rc<dyn Fn(ShellAction) -> bool>;

#[derive(Clone, Debug, PartialEq)]
pub enum ShellAction {
    Command(Command),
    /// Maps `RecoveryAction::Retry`; the shell retries arming capture.
    Retry,
    OpenSettings,
    /// Opens the test-recording category chooser (WR-012).
    TestRecording,
    OpenLogs,
    About,
    Quit,
}

/// Category rail metadata in the exact WR-000 order: label and symbolic icon.
pub const CATEGORIES: [(Category, &str, &str); 10] = [
    (Category::TwoVTwo, "2v2", "wr-category-2v2-symbolic"),
    (Category::ThreeVThree, "3v3", "wr-category-3v3-symbolic"),
    (Category::FiveVFive, "5v5", "wr-category-5v5-symbolic"),
    (
        Category::Skirmish,
        "Skirmish",
        "wr-category-skirmish-symbolic",
    ),
    (
        Category::SoloShuffle,
        "Solo Shuffle",
        "wr-category-solo-shuffle-symbolic",
    ),
    (
        Category::MythicPlus,
        "Mythic+",
        "wr-category-mythic-plus-symbolic",
    ),
    (Category::Raids, "Raids", "wr-category-raids-symbolic"),
    (
        Category::Battlegrounds,
        "Battlegrounds",
        "wr-category-battlegrounds-symbolic",
    ),
    (Category::Manual, "Manual", "wr-category-manual-symbolic"),
    (Category::Clip, "Clips", "wr-category-clips-symbolic"),
];

/// The exact WR-000 test-recording choices, in menu order.
pub const TEST_CATEGORIES: [(Category, &str, &str); 6] = [
    (Category::TwoVTwo, "2v2", "2v2"),
    (Category::ThreeVThree, "3v3", "3v3"),
    (Category::SoloShuffle, "Solo Shuffle", "solo-shuffle"),
    (Category::Raids, "Raids", "raids"),
    (Category::Battlegrounds, "Battlegrounds", "battlegrounds"),
    (Category::MythicPlus, "Mythic+", "mythic-plus"),
];

pub fn category_label(category: &Category) -> &str {
    CATEGORIES
        .iter()
        .find(|(candidate, _, _)| candidate == category)
        .map(|(_, label, _)| *label)
        .unwrap_or("Recordings")
}

/// Build and run the application; returns the process exit code. The caller
/// joins the coordinator and tray handles after this returns.
pub fn run(
    options: &ShellOptions,
    coordinator: Rc<RefCell<CoordinatorHandle>>,
    tray: Option<Rc<TrayBackend>>,
    tray_events: Receiver<TrayEvent>,
) -> i32 {
    gtk4::gio::resources_register_include!("warcraft-recorder.gresource")
        .expect("register compiled GResource bundle");

    let application = adw::Application::builder()
        .application_id(options.app_id)
        .build();

    application.connect_startup(|_| {
        let Some(display) = gtk4::gdk::Display::default() else {
            return;
        };
        let provider = gtk4::CssProvider::new();
        provider.load_from_resource("/io/github/JohanWes/WarcraftRecorder/style.css");
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        gtk4::IconTheme::for_display(&display)
            .add_resource_path("/io/github/JohanWes/WarcraftRecorder/icons");
    });

    let shell: Rc<RefCell<Option<window::Shell>>> = Rc::new(RefCell::new(None));
    {
        let shell_cell = Rc::clone(&shell);
        let activate_coordinator = Rc::clone(&coordinator);
        let activate_tray = tray.clone();
        let data_dir = options.data_dir.clone();
        let config_dir = options.config_dir.clone();
        let start_minimized = options.start_minimized;
        let close_to_tray = options.close_to_tray;
        let minimize_to_tray = options.minimize_to_tray;
        application.connect_activate(move |application| {
            if shell_cell.borrow().is_none() {
                let built = window::Shell::build(
                    application,
                    Rc::clone(&activate_coordinator),
                    activate_tray.clone(),
                    &data_dir,
                    &config_dir,
                    close_to_tray,
                    minimize_to_tray,
                );
                *shell_cell.borrow_mut() = Some(built);
            }
            let shell_ref = shell_cell.borrow();
            let built = shell_ref.as_ref().expect("shell built above");
            let watcher_available = activate_tray
                .as_ref()
                .is_some_and(|tray| tray.is_available());
            if tray::start_hidden(watcher_available, start_minimized) {
                built.hide_to_tray();
            } else {
                built.present();
            }
        });
    }

    // The single pump: drains tray events, the newest snapshot, and the
    // coordinator-stopped signal. Installed once before the loop starts and
    // removed with the application.
    {
        let app = application.clone();
        let shutdown_sent = Cell::new(false);
        let coordinator_stopped = Cell::new(false);
        // Last tooltip pushed to the tray, so we only pay the cross-thread
        // `update` round-trip on a real change instead of every snapshot.
        let last_tray_title = RefCell::new(String::new());
        gtk4::glib::timeout_add_local(Duration::from_millis(33), move || {
            while let Ok(TrayEvent::Open) = tray_events.try_recv() {
                if let Some(shell) = shell.borrow().as_ref() {
                    shell.present();
                }
            }
            let quit_requested = tray.as_ref().is_some_and(|tray| tray.quit_requested());
            if quit_requested && !shutdown_sent.get() {
                shutdown_sent.set(coordinator.borrow().send(Command::Shutdown));
            }
            if let Ok(snapshot) = coordinator.borrow().snapshots.try_recv()
                && let Some(shell) = shell.borrow().as_ref()
            {
                shell.apply_snapshot(&snapshot);
                if let Some(tray) = &tray {
                    let title = format!("Warcraft Recorder — {}", status::view(&snapshot).title);
                    if *last_tray_title.borrow() != title {
                        tray.update(title.clone(), ksni::Status::Active);
                        *last_tray_title.borrow_mut() = title;
                    }
                }
            }
            if let Some(shell) = shell.borrow().as_ref()
                && let Some(tray) = &tray
            {
                shell.set_tray_available(tray.is_available());
            }
            if !coordinator_stopped.get() && coordinator.borrow().stopped.try_recv().is_ok() {
                coordinator_stopped.set(true);
                app.quit();
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    application.run_with_args(&[env!("CARGO_PKG_NAME")]).into()
}

fn simple_action(application: &adw::Application, name: &str, activate: impl Fn() + 'static) {
    let action = gtk4::gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| activate());
    application.add_action(&action);
}

/// Register the primary-menu actions shared by the shell.
pub fn install_actions(application: &adw::Application, sink: ActionSink) {
    for (name, action) in [
        ("settings", ShellAction::OpenSettings),
        ("about", ShellAction::About),
        ("open-logs", ShellAction::OpenLogs),
        ("test-recording", ShellAction::TestRecording),
    ] {
        let sink = Rc::clone(&sink);
        simple_action(application, name, move || {
            sink(action.clone());
        });
    }
}

/// The window primary menu: Test recording, Open logs, About. There is no
/// update UI; the Flatpak remote and software center own updates.
pub fn primary_menu() -> gtk4::gio::Menu {
    let menu = gtk4::gio::Menu::new();
    menu.append(Some("Test recording…"), Some("app.test-recording"));
    menu.append(Some("Open logs"), Some("app.open-logs"));
    menu.append(Some("About Warcraft Recorder"), Some("app.about"));
    menu
}
