// SPDX-License-Identifier: GPL-3.0-or-later

//! Process entry point. `main` starts the coordinator and the tray service,
//! runs the GTK shell, then joins both top-level handles after the
//! application loop returns. No child or thread is detached.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use warcraft_recorder::config::Config;
use warcraft_recorder::coordinator;

mod ui;

use ui::tray_backend::TrayBackend;

#[cfg(not(feature = "development"))]
const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder";
#[cfg(feature = "development")]
const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder.Devel";

fn main() {
    tracing_subscriber::fmt::init();

    let setup = match coordinator::Setup::from_environment() {
        Ok(setup) => setup,
        Err(error) => {
            tracing::error!(%error, "cannot resolve the configuration directory");
            eprintln!("warcraft-recorder: {error}");
            std::process::exit(1);
        }
    };
    let options = shell_options(APP_ID, &setup);
    let coordinator = Rc::new(RefCell::new(coordinator::start(setup)));

    let (tray_events_tx, tray_events_rx) = mpsc::sync_channel(8);
    let tray = TrayBackend::start(tray_events_tx)
        .map(Rc::new)
        .map_err(|error| tracing::warn!(%error, "tray service unavailable"))
        .ok();

    tracing::info!(application_id = APP_ID, "starting application");
    let code = ui::run(
        &options,
        Rc::clone(&coordinator),
        tray.clone(),
        tray_events_rx,
    );

    // The GTK loop has exited: stop the coordinator and join it, then shut
    // down the tray service thread.
    coordinator.borrow_mut().shutdown();
    if let Some(tray) = tray {
        tray.shutdown();
    }
    std::process::exit(code);
}

/// The shell options that come from outside the GTK loop: paths the shell
/// launches but never reads, plus the initial interface flags read once
/// before the loop starts (live values arrive with the first snapshot).
fn shell_options(app_id: &'static str, setup: &coordinator::Setup) -> ui::ShellOptions {
    let config = Config::load(&setup.config_path).unwrap_or_default();
    ui::ShellOptions {
        app_id,
        data_dir: setup.data_dir.clone(),
        config_dir: config_dir(&setup.config_path),
        start_minimized: config.interface.start_minimized,
        close_to_tray: config.interface.close_to_tray,
        minimize_to_tray: config.interface.minimize_to_tray,
    }
}

fn config_dir(config_path: &std::path::Path) -> PathBuf {
    config_path
        .parent()
        .map_or_else(PathBuf::new, std::path::Path::to_owned)
}
