// SPDX-License-Identifier: GPL-2.0-only

use gtk4::prelude::*;
use libadwaita as adw;

const APP_ID: &str = "io.github.JohanWes.WarcraftRecorder";

fn main() {
    tracing_subscriber::fmt::init();

    let application = adw::Application::builder().application_id(APP_ID).build();

    application.connect_activate(|application| {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Warcraft Recorder")
            .build();
        window.present();
    });

    tracing::info!(application_id = APP_ID, "starting application");
    application.run();
}
