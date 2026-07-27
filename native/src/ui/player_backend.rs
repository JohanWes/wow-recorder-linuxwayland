// SPDX-License-Identifier: GPL-3.0-or-later

use clapper_gtk::prelude::AvExt;
use gtk4::glib::prelude::Cast;

/// How precisely a seek has to land, which decides how much decoding GStreamer
/// does before it can present a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    /// Nearest keyframe. No decode from the previous keyframe, which is what
    /// lets the picture keep up with a dragged playhead.
    Preview,
    /// Clapper's default approximation: where the playhead comes to rest.
    Settle,
    /// Exact frame. Only worth its decode cost when the frame itself is the
    /// point, as in stepping backwards.
    Exact,
}

/// The concrete Clapper objects used by Warcraft Recorder's player UI.
#[derive(Clone)]
pub struct PlayerBackend {
    video: clapper_gtk::Video,
    player: clapper::Player,
}

#[derive(Clone)]
pub struct VideoStreamToken(clapper::VideoStream);

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

    pub fn seek(&self, position_seconds: f64, mode: SeekMode) {
        self.player.seek_custom(
            position_seconds,
            match mode {
                SeekMode::Preview => clapper::PlayerSeekMethod::Fast,
                SeekMode::Settle => clapper::PlayerSeekMethod::Normal,
                SeekMode::Exact => clapper::PlayerSeekMethod::Accurate,
            },
        );
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

    pub fn video_stream_token(&self) -> Option<VideoStreamToken> {
        self.current_video_stream().map(VideoStreamToken)
    }

    /// Dimensions reported by a newly decoded active video stream.
    pub fn video_dimensions(
        &self,
        expected_uri: &str,
        previous_stream: Option<&VideoStreamToken>,
    ) -> Option<(u32, u32)> {
        if !self.is_ready() {
            return None;
        }
        let current_uri = self.player.queue()?.current_item()?.uri()?;
        if current_uri.as_str() != expected_uri {
            return None;
        }
        let stream = self.current_video_stream()?;
        if previous_stream.is_some_and(|previous| previous.0 == stream) {
            return None;
        }
        let width = u32::try_from(stream.width()).ok()?;
        let height = u32::try_from(stream.height()).ok()?;
        (width > 0 && height > 0).then_some((width, height))
    }

    fn current_video_stream(&self) -> Option<clapper::VideoStream> {
        self.player
            .video_streams()?
            .current_stream()?
            .downcast::<clapper::VideoStream>()
            .ok()
    }

    /// Playing or paused with media: seeks/steps are meaningful.
    pub fn is_ready(&self) -> bool {
        matches!(
            self.player.state(),
            clapper::PlayerState::Playing | clapper::PlayerState::Paused
        )
    }

    pub fn connect_position_updated(&self, callback: impl Fn(f64) + 'static) {
        self.player
            .connect_position_notify(move |player| callback(player.position()));
    }

    pub fn connect_seek_done(&self, callback: impl Fn() + 'static) {
        self.player.connect_seek_done(move |_| callback());
    }
}
