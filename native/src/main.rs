// SPDX-License-Identifier: GPL-3.0-or-later

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::AdwApplicationWindowExt;
use std::rc::Rc;
use std::sync::mpsc;

#[cfg(feature = "development")]
use std::cell::RefCell;

#[cfg(feature = "development")]
#[path = "ui/player_backend.rs"]
mod player_backend;
#[path = "ui/tray_backend.rs"]
mod tray_backend;

#[cfg(not(feature = "development"))]
const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder";
#[cfg(feature = "development")]
const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder.Devel";

fn main() {
    tracing_subscriber::fmt::init();

    #[cfg(feature = "development")]
    if folder_access_probe_requested() {
        std::process::exit(run_folder_access_probe());
    }

    let application = adw::Application::builder().application_id(APP_ID).build();
    let probe_uri = player_probe_uri();
    let folder_probe = folder_probe_requested();
    #[cfg(feature = "development")]
    let capture_probe = capture_probe_requested();
    #[cfg(feature = "development")]
    let capture_child = Rc::new(RefCell::new(None));
    let (tray_events_tx, tray_events_rx) = mpsc::sync_channel(8);
    let tray = tray_backend::TrayBackend::start(tray_events_tx)
        .map(Rc::new)
        .map_err(|error| tracing::warn!(%error, "tray service unavailable"))
        .ok();

    if let Some(tray) = &tray {
        tray.update("Warcraft Recorder — Ready", ksni::Status::Active);
    }

    let activate_tray = tray.clone();
    #[cfg(feature = "development")]
    let activate_capture_child = Rc::clone(&capture_child);
    application.connect_activate(move |application| {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Warcraft Recorder")
            .default_width(960)
            .default_height(540)
            .build();

        if let Some(content) = probe_content(probe_uri.as_deref(), folder_probe) {
            window.set_content(Some(&content));
        }

        let close_tray = activate_tray.clone();
        window.connect_close_request(move |window| {
            if close_tray.as_ref().is_some_and(|tray| tray.is_available()) {
                window.set_visible(false);
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        window.present();

        #[cfg(feature = "development")]
        if capture_probe && activate_capture_child.borrow().is_none() {
            match start_capture_probe() {
                Ok(child) => *activate_capture_child.borrow_mut() = Some(child),
                Err(error) => tracing::error!(%error, "capture probe failed to start"),
            }
        }
    });

    let event_application = application.clone();
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        while let Ok(event) = tray_events_rx.try_recv() {
            match event {
                tray_backend::TrayEvent::Open => {
                    if let Some(window) = event_application.active_window() {
                        window.present();
                    } else {
                        event_application.activate();
                    }
                }
                tray_backend::TrayEvent::Quit => event_application.quit(),
            }
        }
        gtk4::glib::ControlFlow::Continue
    });

    tracing::info!(application_id = APP_ID, "starting application");
    application.run_with_args(&[env!("CARGO_PKG_NAME")]);

    #[cfg(feature = "development")]
    if let Some(mut child) = capture_child.take() {
        let process_id = child.id().to_string();
        let _ = std::process::Command::new("kill")
            .args(["-INT", &process_id])
            .status();
        let _ = child.wait();
    }

    if let Some(tray) = tray {
        tray.shutdown();
    }
}

#[cfg(feature = "development")]
fn player_probe_uri() -> Option<String> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--player-probe" {
            let value = args.next()?;
            return Some(
                gtk4::gio::File::for_commandline_arg(value)
                    .uri()
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(feature = "development")]
fn folder_probe_requested() -> bool {
    std::env::args_os().any(|arg| arg == "--folder-probe")
}

#[cfg(not(feature = "development"))]
fn folder_probe_requested() -> bool {
    false
}

#[cfg(feature = "development")]
fn folder_access_probe_requested() -> bool {
    std::env::args_os().any(|arg| arg == "--folder-access-probe")
}

#[cfg(feature = "development")]
fn capture_probe_requested() -> bool {
    std::env::args_os().any(|arg| arg == "--capture-probe")
}

#[cfg(feature = "development")]
fn start_capture_probe() -> std::io::Result<std::process::Child> {
    use gtk4::gio::prelude::SettingsExt;

    let settings = gtk4::gio::Settings::new(APP_ID);
    let replay = settings.string("replay-folder");
    let recording = settings.string("recording-folder");
    std::process::Command::new("gpu-screen-recorder")
        .args([
            "-w",
            "portal",
            "-restore-portal-session",
            "yes",
            "-portal-session-token-filepath",
            "/var/data/wr002/portal-token",
            "-r",
            "8",
            "-replay-storage",
            "ram",
            "-restart-replay-on-save",
            "no",
            "-c",
            "mkv",
            "-f",
            "30",
            "-keyint",
            "2",
            "-bm",
            "cbr",
            "-q",
            "5000",
            "-k",
            "h264",
            "-ac",
            "aac",
            "-cursor",
            "no",
            "-a",
            "wr002_null.monitor",
            "-sc",
            "/var/data/wr002/gsr-hook.sh",
            "-o",
            replay.as_str(),
            "-ro",
            recording.as_str(),
        ])
        .spawn()
}

#[cfg(feature = "development")]
fn run_folder_access_probe() -> i32 {
    use gtk4::gio::prelude::SettingsExt;

    let settings = gtk4::gio::Settings::new(APP_ID);
    let mut failed = false;
    for key in ["wow-log-folder", "recording-folder", "replay-folder"] {
        let path = settings.string(key);
        let path = std::path::Path::new(path.as_str());
        let rust_access = path.is_dir();
        let gsr_access = child_can_start_in("gpu-screen-recorder", "--version", path);
        let ffmpeg_access = child_can_start_in("ffmpeg", "-version", path);
        println!(
            "{key}\trust={rust_access}\tgsr={gsr_access}\tffmpeg={ffmpeg_access}\t{}",
            path.display()
        );
        failed |= !(rust_access && gsr_access && ffmpeg_access);
    }
    i32::from(failed)
}

#[cfg(feature = "development")]
fn child_can_start_in(program: &str, version_arg: &str, directory: &std::path::Path) -> bool {
    std::process::Command::new(program)
        .arg(version_arg)
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(feature = "development"))]
fn player_probe_uri() -> Option<String> {
    None
}

#[cfg(feature = "development")]
fn probe_content(uri: Option<&str>, folder_probe: bool) -> Option<gtk4::Widget> {
    if folder_probe {
        return Some(folder_probe_content());
    }
    let uri = uri?;
    let player = Rc::new(
        player_backend::PlayerBackend::new()
            .unwrap_or_else(|error| panic!("player probe initialization failed: {error}")),
    );
    player
        .open_uri(uri)
        .unwrap_or_else(|error| panic!("player probe could not open media: {error}"));

    let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    for (label, action) in [
        ("Play", ProbeAction::Play),
        ("Pause", ProbeAction::Pause),
        ("0.25×", ProbeAction::Speed(0.25)),
        ("1×", ProbeAction::Speed(1.0)),
        ("2×", ProbeAction::Speed(2.0)),
        ("Mute", ProbeAction::Mute),
        ("Volume 50%", ProbeAction::Volume(0.5)),
        ("Seek start", ProbeAction::SeekFraction(0.02)),
        ("Seek middle", ProbeAction::SeekFraction(0.5)),
        ("Seek end", ProbeAction::SeekFraction(0.98)),
        ("Ten rapid seeks", ProbeAction::RapidSeek),
        ("Previous frame", ProbeAction::SeekBackFrame),
        ("Next frame", ProbeAction::NextFrame),
        ("Stop", ProbeAction::Stop),
    ] {
        let button = gtk4::Button::with_label(label);
        let action_player = Rc::clone(&player);
        button.connect_clicked(move |_| run_probe_action(&action_player, action));
        controls.append(&button);
    }

    let position = gtk4::Label::new(Some("0.000 / 0.000 s"));
    let position_player = Rc::clone(&player);
    let position_label = position.clone();
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        position_label.set_label(&format!(
            "{:.3} / {:.3} s",
            position_player.position(),
            position_player.duration()
        ));
        gtk4::glib::ControlFlow::Continue
    });

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    content.append(player.widget());
    content.append(&controls);
    content.append(&position);
    Some(content.upcast())
}

#[cfg(not(feature = "development"))]
fn probe_content(_uri: Option<&str>, _folder_probe: bool) -> Option<gtk4::Widget> {
    None
}

#[cfg(feature = "development")]
fn folder_probe_content() -> gtk4::Widget {
    use gtk4::gio::prelude::SettingsExt;

    let settings = gtk4::gio::Settings::new(APP_ID);
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    for (key, title) in [
        ("wow-log-folder", "Select WoW log folder"),
        ("recording-folder", "Select recording folder"),
        ("replay-folder", "Select replay folder"),
    ] {
        let button = gtk4::Button::with_label(title);
        let path_label = gtk4::Label::new(Some(settings.string(key).as_str()));
        path_label.set_selectable(true);
        path_label.set_xalign(0.0);

        let selection_settings = settings.clone();
        let selection_label = path_label.clone();
        button.connect_clicked(move |button| {
            let dialog = gtk4::FileDialog::builder().title(title).modal(true).build();
            let current = selection_settings.string(key);
            if !current.is_empty() {
                dialog.set_initial_folder(Some(&gtk4::gio::File::for_path(current.as_str())));
            }
            let parent = button.root().and_downcast::<gtk4::Window>();
            let result_settings = selection_settings.clone();
            let result_label = selection_label.clone();
            dialog.select_folder(
                parent.as_ref(),
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    if let Ok(folder) = result
                        && let Some(path) = folder.path()
                    {
                        let value = path.to_string_lossy();
                        if result_settings.set_string(key, &value).is_ok() {
                            result_label.set_label(&value);
                        }
                    }
                },
            );
        });

        content.append(&button);
        content.append(&path_label);
    }

    content.upcast()
}

#[cfg(feature = "development")]
#[derive(Clone, Copy)]
enum ProbeAction {
    Play,
    Pause,
    Speed(f64),
    Mute,
    Volume(f64),
    SeekFraction(f64),
    RapidSeek,
    SeekBackFrame,
    NextFrame,
    Stop,
}

#[cfg(feature = "development")]
fn run_probe_action(player: &player_backend::PlayerBackend, action: ProbeAction) {
    match action {
        ProbeAction::Play => player.play(),
        ProbeAction::Pause => player.pause(),
        ProbeAction::Speed(speed) => player.set_speed(speed),
        ProbeAction::Mute => player.set_muted(true),
        ProbeAction::Volume(volume) => {
            player.set_muted(false);
            player.set_volume(volume);
        }
        ProbeAction::SeekFraction(fraction) => player.seek(player.duration() * fraction),
        ProbeAction::RapidSeek => {
            let duration = player.duration();
            for step in 0..10 {
                player.seek(duration * f64::from(step) / 10.0);
            }
        }
        ProbeAction::SeekBackFrame => {
            player.pause();
            player.seek((player.position() - (1.0 / 30.0)).max(0.0));
        }
        ProbeAction::NextFrame => {
            player.pause();
            player.advance_frame();
        }
        ProbeAction::Stop => player.stop(),
    }
}
