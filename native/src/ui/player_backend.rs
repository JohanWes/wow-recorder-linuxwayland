// SPDX-License-Identifier: GPL-3.0-or-later

use clapper_gtk::prelude::AvExt;

/// The concrete Clapper objects used by Warcraft Recorder's player UI.
#[derive(Clone)]
pub struct PlayerBackend {
    video: clapper_gtk::Video,
    player: clapper::Player,
}

impl PlayerBackend {
    pub fn new() -> Result<Self, &'static str> {
        clapper::init()?;
        let video = clapper_gtk::Video::new();
        let player = video.player().ok_or("ClapperGtk did not create a player")?;
        Ok(Self { video, player })
    }

    pub fn widget(&self) -> &clapper_gtk::Video {
        &self.video
    }

    pub fn open_uri(&self, uri: &str) -> Result<(), &'static str> {
        let queue = self.player.queue().ok_or("Clapper player has no queue")?;
        let item = clapper::MediaItem::new(uri);
        queue.clear();
        queue.add_item(&item);
        if !queue.select_item(Some(&item)) {
            return Err("Clapper did not select the requested media item");
        }
        Ok(())
    }

    pub fn play(&self) {
        self.player.play();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn position(&self) -> f64 {
        self.player.position()
    }

    pub fn duration(&self) -> f64 {
        self.player
            .queue()
            .and_then(|queue| queue.current_item())
            .map_or(0.0, |item| item.duration())
    }

    pub fn seek(&self, position_seconds: f64) {
        self.player.seek(position_seconds);
    }

    pub fn set_volume(&self, volume: f64) {
        self.player.set_volume(volume);
    }

    pub fn set_muted(&self, muted: bool) {
        self.player.set_mute(muted);
    }

    pub fn set_speed(&self, speed: f64) {
        self.player.set_speed(speed);
    }

    pub fn advance_frame(&self) {
        self.player.advance_frame();
    }

    pub fn stop(&self) {
        self.player.stop();
    }
}
