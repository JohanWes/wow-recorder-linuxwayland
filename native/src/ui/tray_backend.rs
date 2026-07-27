// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc::SyncSender};

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;

#[cfg(not(feature = "development"))]
const ICON_NAME: &str = "io.github.JohanWes.WarcraftRecorder";
#[cfg(feature = "development")]
const ICON_NAME: &str = "io.github.JohanWes.WarcraftRecorder.Devel";

/// The only event carried over the bounded channel is Open; it is idempotent
/// (present the window) so dropping it under saturation is harmless. Quit is a
/// latched flag instead — see `RecorderTray::request_quit`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayEvent {
    Open,
}

pub struct TrayBackend {
    handle: Handle<RecorderTray>,
    available: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
}

struct RecorderTray {
    events: SyncSender<TrayEvent>,
    available: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
    /// Nudges the shell's main loop; Open and Quit are otherwise invisible
    /// until the slow safety tick.
    wake: Arc<dyn Fn() + Send + Sync>,
    title: String,
    status: ksni::Status,
}

impl TrayBackend {
    pub fn start(
        events: SyncSender<TrayEvent>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, ksni::Error> {
        let available = Arc::new(AtomicBool::new(true));
        let quit_requested = Arc::new(AtomicBool::new(false));
        let tray = RecorderTray {
            events,
            available: Arc::clone(&available),
            quit_requested: Arc::clone(&quit_requested),
            wake,
            title: "Warcraft Recorder".into(),
            status: ksni::Status::Active,
        };

        let mut service = tray.assume_sni_available(true);
        if std::env::var_os("FLATPAK_ID").is_some() {
            service = service.disable_dbus_name(true);
        }
        let handle = service.spawn()?;

        Ok(Self {
            handle,
            available,
            quit_requested,
        })
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    /// Set by the tray's Quit menu item. The GTK pump reads this each tick and
    /// dispatches one graceful `Shutdown`; using a latch instead of a channel
    /// send keeps the single-threaded tray executor from ever parking (a
    /// blocking send there would deadlock `shutdown`).
    pub fn quit_requested(&self) -> bool {
        self.quit_requested.load(Ordering::Acquire)
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

impl RecorderTray {
    /// Non-blocking: the window-present intent is idempotent, so a dropped Open
    /// under a saturated channel is fine and never stalls the tray executor.
    fn request_open(&self) {
        let _ = self.events.try_send(TrayEvent::Open);
        (self.wake)();
    }

    fn request_quit(&self) {
        self.quit_requested.store(true, Ordering::Release);
        (self.wake)();
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
        self.request_open();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open".into(),
                icon_name: "window-new-symbolic".into(),
                activate: Box::new(|tray: &mut RecorderTray| tray.request_open()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|tray: &mut RecorderTray| tray.request_quit()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_quit_never_block_a_saturated_channel() {
        // Capacity-one channel; a second Open would block a blocking send.
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let quit = Arc::new(AtomicBool::new(false));
        let tray = RecorderTray {
            events: sender,
            available: Arc::new(AtomicBool::new(true)),
            quit_requested: Arc::clone(&quit),
            wake: Arc::new(|| {}),
            title: "Warcraft Recorder".into(),
            status: ksni::Status::Active,
        };

        tray.request_open(); // fills the single slot
        tray.request_open(); // dropped by try_send rather than blocking
        tray.request_quit(); // latches without touching the channel

        assert!(quit.load(Ordering::Acquire));
    }
}
