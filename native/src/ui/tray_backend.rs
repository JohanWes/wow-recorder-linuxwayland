// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc::SyncSender};

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;

#[cfg(not(feature = "development"))]
const ICON_NAME: &str = "io.github.JohanWes.WarcraftRecorder";
#[cfg(feature = "development")]
const ICON_NAME: &str = "io.github.JohanWes.WarcraftRecorder.Devel";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayEvent {
    Open,
    Quit,
}

pub struct TrayBackend {
    handle: Handle<RecorderTray>,
    available: Arc<AtomicBool>,
}

struct RecorderTray {
    events: SyncSender<TrayEvent>,
    available: Arc<AtomicBool>,
    title: String,
    status: ksni::Status,
}

impl TrayBackend {
    pub fn start(events: SyncSender<TrayEvent>) -> Result<Self, ksni::Error> {
        let available = Arc::new(AtomicBool::new(true));
        let tray = RecorderTray {
            events,
            available: Arc::clone(&available),
            title: "Warcraft Recorder".into(),
            status: ksni::Status::Active,
        };

        let mut service = tray.assume_sni_available(true);
        if std::env::var_os("FLATPAK_ID").is_some() {
            service = service.disable_dbus_name(true);
        }
        let handle = service.spawn()?;

        Ok(Self { handle, available })
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    pub fn update(&self, title: impl Into<String>, status: ksni::Status) {
        let title = title.into();
        self.handle.update(move |tray| {
            tray.title = title;
            tray.status = status;
        });
    }

    pub fn shutdown(&self) {
        self.handle.shutdown().wait();
    }
}

impl ksni::Tray for RecorderTray {
    fn id(&self) -> String {
        "warcraft-recorder".into()
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn status(&self) -> ksni::Status {
        self.status
    }

    fn icon_name(&self) -> String {
        ICON_NAME.into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        send_event(&self.events, TrayEvent::Open);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open".into(),
                icon_name: "window-new-symbolic".into(),
                activate: Box::new(|tray: &mut RecorderTray| {
                    send_event(&tray.events, TrayEvent::Open);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|tray: &mut RecorderTray| {
                    send_event(&tray.events, TrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_online(&self) {
        self.available.store(true, Ordering::Release);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.available.store(false, Ordering::Release);
        true
    }
}

fn send_event(events: &SyncSender<TrayEvent>, event: TrayEvent) {
    match event {
        TrayEvent::Open => {
            let _ = events.try_send(event);
        }
        TrayEvent::Quit => {
            let _ = events.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_waits_for_space_in_a_saturated_channel() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(TrayEvent::Open).unwrap();

        let quit_sender = sender.clone();
        let quit_thread = std::thread::spawn(move || send_event(&quit_sender, TrayEvent::Quit));

        assert_eq!(receiver.recv().unwrap(), TrayEvent::Open);
        assert_eq!(receiver.recv().unwrap(), TrayEvent::Quit);
        quit_thread.join().unwrap();
    }
}
