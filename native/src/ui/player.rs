// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent player pane: one ClapperGtk video with Warcraft Recorder's
//! compact control row, combat timeline, clip mode, and a single-view
//! viewpoint selector. Volume/mute are process-shared session state; speed,
//! position, and the clip range are session-only. All playback state lives in
//! Clapper; this pane only issues commands and mirrors positions.
//!
//! Multi-POV grid playback (synchronized 2–4 player grid) was removed from
//! the product by maintainer decision (2026-07-22); the viewpoint selector
//! and individual local recordings remain.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use libadwaita as adw;

use warcraft_recorder::coordinator::{AppSnapshot, ClipRange, Command};
use warcraft_recorder::domain::{
    Category, DeathMarkerVisibility, LibraryEntry, MarkerVisibility, RecordingId,
};

use super::library::Selection;
use super::multipov;
use super::player_backend::{PlayerBackend, SeekMode, VideoStreamToken};
use super::timeline::{self, MarkerDirection, MarkerPrefs, Timeline};
use super::{ActionSink, ShellAction};

const SPEEDS: [f64; 4] = [0.25, 0.5, 1.0, 2.0];
const SEEK_STEP_SECONDS: f64 = 5.0;
/// Pointer idle before fullscreen collapses the bottom bar, and how often that
/// idle is checked. One repeating source beats re-arming a timer per motion.
const IDLE_HIDE: Duration = Duration::from_secs(2);
const IDLE_TICK: Duration = Duration::from_millis(250);
type VideoDimensionsHandler = Rc<dyn Fn(u32, u32)>;

pub struct Player {
    pub widget: gtk4::Box,
    inner: Rc<Inner>,
}

struct Inner {
    sink: ActionSink,

    stack: gtk4::Stack,
    video_overlay: gtk4::Overlay,
    size_probe: gtk4::DrawingArea,
    placeholder: adw::StatusPage,
    empty_reveal: gtk4::Button,
    error_bar: gtk4::Box,
    timeline: Timeline,
    time_label: gtk4::Label,
    play_button: gtk4::Button,
    speed_button: gtk4::Button,
    mute_button: gtk4::Button,
    volume_scale: gtk4::Scale,
    pov_dropdown: gtk4::DropDown,
    clip_button: gtk4::Button,
    clip_actions: gtk4::Box,
    marker_button: gtk4::MenuButton,
    previous_marker_button: gtk4::Button,
    next_marker_button: gtk4::Button,
    reveal_button: gtk4::Button,

    /// Collapsing the control row in fullscreen gives the video the whole
    /// surface, which is what closes the letterbox bars.
    bottom_bar: gtk4::Revealer,
    fullscreen: Cell<bool>,
    last_motion: Cell<Instant>,
    last_pointer: Cell<(f64, f64)>,

    /// Lets the shell size the paned player to the selected video's aspect
    /// ratio without making the player own window layout.
    video_dimensions_handler: RefCell<Option<VideoDimensionsHandler>>,

    /// The one Clapper backend; `None` only when Clapper failed to start.
    backend: Option<PlayerBackend>,

    entries: RefCell<Arc<Vec<LibraryEntry>>>,
    prefs: Cell<MarkerPrefs>,
    /// POVs of the selected activity and the id currently loaded.
    povs: RefCell<Vec<multipov::Pov>>,
    active_id: RefCell<Option<RecordingId>>,
    preferred_player: RefCell<Option<String>>,

    media_usable: Cell<bool>,
    playing: Cell<bool>,
    speed_index: Cell<usize>,
    muted: Cell<bool>,
    position_seconds: Cell<f64>,
    duration_ms: Cell<u64>,
    fps: Cell<Option<u32>>,
    clip_mode: Cell<bool>,

    /// Async seek: the newest requested target with the precision it needs,
    /// and whether one is in flight.
    pending_seek: Cell<Option<(f64, SeekMode)>>,
    seek_in_flight: Cell<bool>,
    /// Bumped by every load and unload so timers armed for an earlier media
    /// item cannot report on the current one.
    load_generation: Cell<u64>,
    /// Whole seconds and duration the time label was last rendered from.
    time_label_state: Cell<Option<(u64, u64)>>,
    /// Guard: snapshot-driven widget updates must not dispatch commands.
    updating: Cell<bool>,
}

impl Player {
    pub fn new(sink: ActionSink) -> Self {
        let placeholder = adw::StatusPage::new();
        placeholder.set_title("No recording selected");
        placeholder.set_description(Some("Select a recording below to review it."));
        let empty_reveal = gtk4::Button::with_label("Reveal in folder");
        empty_reveal.set_halign(gtk4::Align::Center);
        empty_reveal.set_visible(false);
        placeholder.set_child(Some(&empty_reveal));

        let backend = PlayerBackend::new()
            .map_err(|error| tracing::warn!(error, "player backend unavailable"))
            .ok();

        let video_overlay = gtk4::Overlay::new();
        if let Some(backend) = &backend {
            video_overlay.set_child(Some(backend.widget()));
        }
        video_overlay.set_vexpand(true);
        // GTK4 gives plain widgets no allocation signal, so this
        // always-allocated probe tells the shell when the video viewport was
        // re-laid out. It paints nothing and takes no input.
        let size_probe = gtk4::DrawingArea::new();
        size_probe.set_can_target(false);
        video_overlay.add_overlay(&size_probe);

        // One recovery row for playback failure; the library stays usable.
        let error_label = gtk4::Label::new(Some("This recording could not be played."));
        error_label.set_hexpand(true);
        error_label.set_xalign(0.0);
        let error_reveal = gtk4::Button::with_label("Reveal in folder");
        let error_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        error_bar.add_css_class("toolbar");
        error_bar.append(&error_label);
        error_bar.append(&error_reveal);
        error_bar.set_visible(false);

        let video_page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        video_page.append(&error_bar);
        video_page.append(&video_overlay);

        let stack = gtk4::Stack::new();
        stack.add_named(&placeholder, Some("placeholder"));
        stack.add_named(&video_page, Some("video"));
        stack.set_visible_child_name("placeholder");
        stack.set_vexpand(true);

        let timeline = Timeline::new();
        let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        controls.add_css_class("toolbar");

        let play_button = icon_button("media-playback-start-symbolic", "Play/pause");
        let mute_button = icon_button("audio-volume-high-symbolic", "Mute");
        let volume_scale = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, 0.0, 1.0, 0.05);
        volume_scale.set_size_request(90, -1);
        volume_scale.set_value(1.0);
        volume_scale.set_tooltip_text(Some("Volume"));
        volume_scale.update_property(&[gtk4::accessible::Property::Label("Volume")]);
        let time_label = gtk4::Label::new(Some("0:00 / 0:00"));
        time_label.add_css_class("numeric");
        let speed_button = gtk4::Button::with_label("1x");
        speed_button.set_tooltip_text(Some("Playback speed"));
        let previous_marker_button = icon_button("media-skip-backward-symbolic", "Previous marker");
        let next_marker_button = icon_button("media-skip-forward-symbolic", "Next marker");
        previous_marker_button.set_sensitive(false);
        next_marker_button.set_sensitive(false);
        let marker_button = gtk4::MenuButton::new();
        marker_button.set_icon_name("view-list-symbolic");
        marker_button.set_tooltip_text(Some("Marker visibility"));
        let clip_button = icon_button("edit-cut-symbolic", "Clip");
        let clip_create = gtk4::Button::with_label("Create clip");
        clip_create.add_css_class("suggested-action");
        let clip_cancel = gtk4::Button::with_label("Cancel");
        let clip_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        clip_actions.append(&clip_create);
        clip_actions.append(&clip_cancel);
        clip_actions.set_visible(false);
        let reveal_button = icon_button("folder-open-symbolic", "Reveal in folder");
        let fullscreen_button = icon_button("view-fullscreen-symbolic", "Fullscreen");
        let pov_dropdown = gtk4::DropDown::from_strings(&[]);
        pov_dropdown.set_tooltip_text(Some("Viewpoint"));
        pov_dropdown.update_property(&[gtk4::accessible::Property::Label("Viewpoint")]);
        pov_dropdown.set_visible(false);

        for widget in [
            play_button.upcast_ref::<gtk4::Widget>(),
            mute_button.upcast_ref(),
            volume_scale.upcast_ref(),
            time_label.upcast_ref(),
        ] {
            controls.append(widget);
        }
        controls.append(&timeline.widget);
        for widget in [
            speed_button.upcast_ref::<gtk4::Widget>(),
            previous_marker_button.upcast_ref(),
            marker_button.upcast_ref(),
            next_marker_button.upcast_ref(),
            clip_button.upcast_ref(),
            clip_actions.upcast_ref(),
            pov_dropdown.upcast_ref(),
            reveal_button.upcast_ref(),
            fullscreen_button.upcast_ref(),
        ] {
            controls.append(widget);
        }

        let bottom_bar = gtk4::Revealer::new();
        bottom_bar.set_child(Some(&controls));
        bottom_bar.set_transition_type(gtk4::RevealerTransitionType::SlideUp);
        bottom_bar.set_transition_duration(250);
        bottom_bar.set_reveal_child(true);

        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.add_css_class("player-area");
        widget.append(&stack);
        widget.append(&bottom_bar);

        let inner = Rc::new(Inner {
            sink,
            stack,
            video_overlay: video_overlay.clone(),
            size_probe,
            placeholder,
            empty_reveal,
            error_bar,
            timeline,
            time_label,
            play_button,
            speed_button,
            mute_button,
            volume_scale,
            pov_dropdown,
            clip_button,
            clip_actions,
            marker_button,
            previous_marker_button,
            next_marker_button,
            reveal_button,
            bottom_bar,
            fullscreen: Cell::new(false),
            last_motion: Cell::new(Instant::now()),
            last_pointer: Cell::new((f64::NAN, f64::NAN)),
            video_dimensions_handler: RefCell::new(None),
            backend,
            entries: RefCell::new(Arc::new(Vec::new())),
            prefs: Cell::new(MarkerPrefs {
                deaths: DeathMarkerVisibility::Own,
                encounters: MarkerVisibility::Visible,
                rounds: MarkerVisibility::Visible,
            }),
            povs: RefCell::new(Vec::new()),
            active_id: RefCell::new(None),
            preferred_player: RefCell::new(None),
            media_usable: Cell::new(false),
            playing: Cell::new(false),
            speed_index: Cell::new(2),
            muted: Cell::new(false),
            position_seconds: Cell::new(0.0),
            duration_ms: Cell::new(0),
            fps: Cell::new(None),
            clip_mode: Cell::new(false),
            pending_seek: Cell::new(None),
            seek_in_flight: Cell::new(false),
            load_generation: Cell::new(0),
            time_label_state: Cell::new(None),
            updating: Cell::new(false),
        });

        // Handle primary presses before Clapper's click recognizer. Letting
        // that recognizer see a first press makes it wait for the double-click
        // interval before toggling playback. Every press therefore follows the
        // same direct transport path as Space; the second press also toggles
        // fullscreen.
        let video_click = gtk4::GestureClick::new();
        video_click.set_button(gtk4::gdk::BUTTON_PRIMARY);
        video_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        {
            let this = Rc::clone(&inner);
            video_click.connect_pressed(move |gesture, n_press, _, _| {
                gesture.set_state(gtk4::EventSequenceState::Claimed);
                this.toggle_playing();
                if n_press == 2 {
                    this.toggle_fullscreen();
                }
            });
        }
        video_overlay.add_controller(video_click);

        inner.connect_backend();
        inner.connect_controls(
            &clip_create,
            &clip_cancel,
            &fullscreen_button,
            &error_reveal,
            &inner.empty_reveal,
        );
        // The common default preferences do not differ on the first snapshot,
        // so install the initial menu here rather than relying on an update.
        inner.rebuild_marker_menu();

        Self { widget, inner }
    }

    /// Route key presses that are not for an editable widget. Installed once on
    /// the window by the shell.
    pub fn install_shortcuts(&self, window: &gtk4::Window) {
        let inner = Rc::clone(&self.inner);
        let key = gtk4::EventControllerKey::new();
        key.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let key_window = window.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if focus_is_editable(&key_window) || inner.active_id.borrow().is_none() {
                return gtk4::glib::Propagation::Proceed;
            }
            inner.handle_key(keyval)
        });
        window.add_controller(key);

        // Capture phase so pointer movement anywhere, including over the video
        // widget that handles its own motion, counts as activity.
        let inner = Rc::clone(&self.inner);
        let motion = gtk4::EventControllerMotion::new();
        motion.set_propagation_phase(gtk4::PropagationPhase::Capture);
        motion.connect_motion(move |_, x, y| inner.wake_controls(x, y));
        window.add_controller(motion);
    }

    /// Fullscreen collapses the bottom bar and the pointer after a short idle,
    /// and restores both on the next movement.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        self.inner.set_fullscreen(fullscreen);
    }

    pub fn apply_snapshot(&self, snapshot: &AppSnapshot) {
        let inner = &self.inner;
        inner.updating.set(true);
        *inner.entries.borrow_mut() = Arc::clone(&snapshot.entries);
        let interface = &snapshot.config.interface;
        let prefs = MarkerPrefs {
            deaths: interface.death_markers,
            encounters: interface.encounter_markers,
            rounds: interface.round_markers,
        };
        if inner.prefs.replace(prefs) != prefs {
            inner.refresh_timeline();
            inner.rebuild_marker_menu();
        }
        inner.updating.set(false);
    }

    /// Pause when the window goes away to the tray; playback must not keep
    /// running behind a hidden window.
    pub fn pause(&self) {
        if self.inner.playing.get() {
            self.inner.toggle_playing();
        }
    }

    /// Table selection changed. `None` shows the placeholder.
    pub fn set_selection(&self, selection: Option<&Selection>) {
        self.inner.set_selection(selection);
    }

    /// Ask the shell to fit its player pane whenever the active POV changes.
    pub fn connect_video_dimensions(&self, handler: impl Fn(u32, u32) + 'static) {
        *self.inner.video_dimensions_handler.borrow_mut() = Some(Rc::new(handler));
    }

    /// Observe real player viewport allocations. Unlike `width-request`, this
    /// fires for every relayout: window resize, chrome shown/hidden, and the
    /// fullscreen transitions.
    pub fn connect_viewport_resize(&self, handler: impl Fn() + 'static) {
        self.inner
            .size_probe
            .connect_resize(move |_, _, _| handler());
    }

    /// Allocated size of the region the video is drawn into.
    pub fn viewport_size(&self) -> (i32, i32) {
        (
            self.inner.video_overlay.width(),
            self.inner.video_overlay.height(),
        )
    }
}

impl Inner {
    // -- construction helpers -----------------------------------------------

    fn connect_backend(self: &Rc<Self>) {
        let Some(backend) = &self.backend else {
            return;
        };
        let this = Rc::clone(self);
        backend.connect_seek_done(move || this.on_seek_done());
        let this = Rc::clone(self);
        backend.connect_position_updated(move |seconds| this.on_position(seconds));
        let this = Rc::clone(self);
        backend.widget().connect_toggle_fullscreen(move |_| {
            this.toggle_fullscreen();
        });
    }

    fn connect_controls(
        self: &Rc<Self>,
        clip_create: &gtk4::Button,
        clip_cancel: &gtk4::Button,
        fullscreen_button: &gtk4::Button,
        error_reveal: &gtk4::Button,
        empty_reveal: &gtk4::Button,
    ) {
        let this = Rc::clone(self);
        self.play_button
            .connect_clicked(move |_| this.toggle_playing());
        let this = Rc::clone(self);
        self.mute_button.connect_clicked(move |_| {
            this.set_muted(!this.muted.get());
        });
        let this = Rc::clone(self);
        self.volume_scale.connect_value_changed(move |scale| {
            this.set_muted(false);
            if let Some(backend) = &this.backend {
                backend.set_volume(scale.value());
            }
        });
        let this = Rc::clone(self);
        self.speed_button.connect_clicked(move |_| {
            let next = (this.speed_index.get() + 1) % SPEEDS.len();
            this.set_speed(next);
        });
        let this = Rc::clone(self);
        self.timeline.connect_seek(move |ms, scrubbing| {
            // Dragging asks for a picture now, not an exact frame; the
            // keyframe snap is what keeps the video with the pointer.
            let mode = if scrubbing {
                SeekMode::Preview
            } else {
                SeekMode::Settle
            };
            this.request_seek(ms as f64 / 1_000.0, mode);
        });
        let this = Rc::clone(self);
        self.previous_marker_button
            .connect_clicked(move |_| this.jump_marker(MarkerDirection::Previous));
        let this = Rc::clone(self);
        self.next_marker_button
            .connect_clicked(move |_| this.jump_marker(MarkerDirection::Next));
        let this = Rc::clone(self);
        self.clip_button.connect_clicked(move |_| {
            this.enter_clip_mode();
        });
        let this = Rc::clone(self);
        clip_create.connect_clicked(move |_| this.create_clip());
        let this = Rc::clone(self);
        clip_cancel.connect_clicked(move |_| this.exit_clip_mode());
        let this = Rc::clone(self);
        self.reveal_button.connect_clicked(move |_| this.reveal());
        let this = Rc::clone(self);
        error_reveal.connect_clicked(move |_| this.reveal());
        let this = Rc::clone(self);
        empty_reveal.connect_clicked(move |_| this.reveal());
        let this = Rc::clone(self);
        fullscreen_button.connect_clicked(move |_| this.toggle_fullscreen());
        let this = Rc::clone(self);
        self.pov_dropdown.connect_selected_notify(move |dropdown| {
            if this.updating.get() {
                return;
            }
            let index = dropdown.selected() as usize;
            let pov = this.povs.borrow().get(index).cloned();
            if let Some(pov) = pov {
                *this.preferred_player.borrow_mut() = pov.player.clone();
                this.load_pov(&pov.id, true);
            }
        });
    }

    // -- selection and loading ----------------------------------------------

    fn set_selection(self: &Rc<Self>, selection: Option<&Selection>) {
        let Some(selection) = selection else {
            self.unload();
            return;
        };
        // Same activity (e.g. snapshot-driven reselect): keep everything.
        let same_activity = self
            .active_id
            .borrow()
            .as_ref()
            .is_some_and(|active| selection.viewpoints.contains(active));
        if same_activity {
            self.refresh_timeline();
            return;
        }
        let entries = self.entries.borrow();
        let resolved: Vec<&LibraryEntry> = selection
            .viewpoints
            .iter()
            .filter_map(|id| entries.iter().find(|entry| &entry.id == id))
            .collect();
        if resolved.is_empty() {
            drop(entries);
            self.unload();
            return;
        }
        let povs = multipov::povs(&resolved);
        let preferred = self.preferred_player.borrow().clone();
        let chosen = multipov::choose(&povs, preferred.as_deref())
            .map(|pov| pov.id.clone())
            .unwrap_or_else(|| selection.id.clone());
        drop(entries);

        // New activity: stop, leave clip mode, and seek to zero.
        *self.povs.borrow_mut() = povs;
        self.position_seconds.set(0.0);
        self.load_pov(&chosen, false);
        self.rebuild_pov_selector();
    }

    /// Load one POV. `retain_position` keeps the current progress
    /// (same-activity POV switch); otherwise playback starts at zero.
    fn load_pov(self: &Rc<Self>, id: &RecordingId, retain_position: bool) {
        let entries = self.entries.borrow();
        let Some(entry) = entries.iter().find(|entry| &entry.id == id) else {
            return;
        };
        let uri = gtk4::gio::File::for_path(&entry.media_path)
            .uri()
            .to_string();
        self.duration_ms.set(entry.duration_ms);
        self.fps.set(entry.media.fps);
        let dimensions = entry.media.width.zip(entry.media.height);
        let has_content = entry.media.has_content;
        let is_clip = entry.category == Category::Clip;
        drop(entries);
        let replacing_media = self.active_id.replace(Some(id.clone())).is_some();
        // A seek belongs to the media it was issued against: a completion that
        // arrives after the swap must not drive the new item, and a seek that
        // never completes must not wedge the new one.
        let generation = self.load_generation.get().wrapping_add(1);
        self.load_generation.set(generation);
        self.seek_in_flight.set(false);
        self.pending_seek.set(None);

        self.error_bar.set_visible(false);
        self.empty_reveal.set_visible(false);
        self.set_media_usable(false, is_clip);
        if !has_content {
            if replacing_media && let Some(backend) = &self.backend {
                backend.stop();
            }
            self.playing.set(false);
            self.play_button
                .set_icon_name("media-playback-start-symbolic");
            self.placeholder.set_title("Recording unavailable");
            self.placeholder
                .set_description(Some("The media file is empty."));
            self.empty_reveal.set_visible(true);
            self.stack.set_visible_child_name("placeholder");
            self.position_seconds.set(0.0);
            self.show_position(0.0);
            self.refresh_timeline();
            return;
        }
        let Some(backend) = &self.backend else {
            self.error_bar.set_visible(true);
            return;
        };
        let previous_video_stream = backend.video_stream_token();
        backend.stop();
        if let Err(error) = backend.open_uri(&uri) {
            tracing::warn!(error, "player failed to load media");
            self.error_bar.set_visible(true);
            return;
        }
        backend.set_speed(SPEEDS[self.speed_index.get()]);
        backend.set_volume(self.volume_scale.value());
        backend.set_muted(self.muted.get());
        self.set_media_usable(true, is_clip);
        backend.play();
        self.playing.set(true);
        self.play_button
            .set_icon_name("media-playback-pause-symbolic");
        if retain_position {
            let position = self.position_seconds.get();
            self.request_seek(position, SeekMode::Settle);
        } else {
            self.position_seconds.set(0.0);
        }
        self.show_position(self.position_seconds.get());
        self.stack.set_visible_child_name("video");
        self.report_video_dimensions(dimensions);
        self.watch_for_video_dimensions(id.clone(), uri, previous_video_stream);
        self.refresh_timeline();
        self.watch_for_failure(generation);
    }

    fn report_video_dimensions(&self, dimensions: Option<(u32, u32)>) -> bool {
        let Some((width, height)) = dimensions.filter(|(width, height)| *width > 0 && *height > 0)
        else {
            return false;
        };
        if let Some(handler) = self.video_dimensions_handler.borrow().as_ref() {
            handler(width, height);
        }
        true
    }

    /// Legacy sidecars carry no media dimensions. Wait briefly for Clapper's
    /// decoder to expose the authoritative active-stream size.
    fn watch_for_video_dimensions(
        self: &Rc<Self>,
        id: RecordingId,
        expected_uri: String,
        previous_stream: Option<VideoStreamToken>,
    ) {
        let this = Rc::clone(self);
        let attempts = Rc::new(Cell::new(0_u8));
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if this.active_id.borrow().as_ref() != Some(&id) {
                return gtk4::glib::ControlFlow::Break;
            }
            if this.report_video_dimensions(this.backend.as_ref().and_then(|backend| {
                backend.video_dimensions(&expected_uri, previous_stream.as_ref())
            })) {
                return gtk4::glib::ControlFlow::Break;
            }
            attempts.set(attempts.get().saturating_add(1));
            if attempts.get() >= 100 {
                gtk4::glib::ControlFlow::Break
            } else {
                gtk4::glib::ControlFlow::Continue
            }
        });
    }

    /// Playback failure has no supported error signal in the bindings; if the
    /// player still is not ready shortly after a load, surface the one
    /// recovery row. A later successful load hides it again.
    ///
    /// The check belongs to the load that armed it: without the generation a
    /// slow first load would be blamed on whatever the user selected next.
    fn watch_for_failure(self: &Rc<Self>, generation: u64) {
        let this = Rc::clone(self);
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
            if this.load_generation.get() != generation {
                return;
            }
            let ready = this.backend.as_ref().is_some_and(PlayerBackend::is_ready);
            if !ready && this.active_id.borrow().is_some() {
                let is_clip = this.active_entry_is_clip();
                this.set_media_usable(false, is_clip);
                this.error_bar.set_visible(true);
            }
        });
    }

    fn unload(self: &Rc<Self>) {
        self.exit_clip_mode();
        if let Some(backend) = &self.backend {
            backend.stop();
        }
        *self.active_id.borrow_mut() = None;
        self.povs.borrow_mut().clear();
        self.playing.set(false);
        self.load_generation
            .set(self.load_generation.get().wrapping_add(1));
        self.seek_in_flight.set(false);
        self.pending_seek.set(None);
        self.position_seconds.set(0.0);
        self.duration_ms.set(0);
        self.show_position(0.0);
        self.set_media_usable(false, false);
        self.placeholder.set_title("No recording selected");
        self.placeholder
            .set_description(Some("Select a recording below to review it."));
        self.empty_reveal.set_visible(false);
        self.stack.set_visible_child_name("placeholder");
        self.timeline.set_entry(None, self.prefs.get());
        self.rebuild_pov_selector();
    }

    // -- transport -----------------------------------------------------------

    fn set_media_usable(&self, usable: bool, is_clip: bool) {
        self.media_usable.set(usable);
        self.play_button.set_sensitive(usable);
        self.speed_button.set_sensitive(usable);
        self.mute_button.set_sensitive(usable);
        self.volume_scale.set_sensitive(usable);
        self.clip_button.set_sensitive(usable && !is_clip);
        self.update_marker_navigation();
    }

    fn active_entry_is_clip(&self) -> bool {
        let active = self.active_id.borrow();
        let entries = self.entries.borrow();
        active
            .as_ref()
            .and_then(|id| entries.iter().find(|entry| &entry.id == id))
            .is_some_and(|entry| entry.category == Category::Clip)
    }

    fn toggle_playing(&self) {
        if !self.media_usable.get() {
            return;
        }
        let playing = !self.playing.get();
        self.playing.set(playing);
        if let Some(backend) = &self.backend {
            if playing {
                backend.play();
            } else {
                backend.pause();
            }
        }
        self.play_button.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
    }

    fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        if let Some(backend) = &self.backend {
            backend.set_muted(muted);
        }
        self.mute_button.set_icon_name(if muted {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        });
    }

    fn set_speed(&self, index: usize) {
        self.speed_index.set(index);
        let speed = SPEEDS[index];
        if let Some(backend) = &self.backend {
            backend.set_speed(speed);
        }
        self.speed_button.set_label(&format!("{speed}x"));
    }

    /// Asynchronous seeking: keep only the newest target while one is pending.
    /// A preview target may be superseded by a settling one, so the mode
    /// travels with the position.
    fn request_seek(self: &Rc<Self>, seconds: f64, mode: SeekMode) {
        if !self.media_usable.get() {
            return;
        }
        let seconds = seconds.clamp(0.0, self.duration_ms.get() as f64 / 1_000.0);
        self.show_position(seconds);
        if self.seek_in_flight.get() {
            self.pending_seek.set(Some((seconds, mode)));
            return;
        }
        self.seek_in_flight.set(true);
        if let Some(backend) = &self.backend {
            backend.seek(seconds, mode);
        }
    }

    fn on_seek_done(self: &Rc<Self>) {
        // A completion for media that has since been replaced: loading already
        // reset the seek state, so acting on it would drive the new item.
        if !self.seek_in_flight.get() {
            return;
        }
        match self.pending_seek.take() {
            Some((target, mode)) => {
                if let Some(backend) = &self.backend {
                    backend.seek(target, mode);
                }
            }
            None => {
                // The requested target stays on screen until Clapper's own
                // position notification arrives. Re-reading the position here
                // would report the keyframe a preview landed on and yank the
                // playhead backwards out from under the pointer mid-drag.
                self.seek_in_flight.set(false);
            }
        }
    }

    fn on_position(&self, seconds: f64) {
        // While a seek is pending, presentation keeps the requested target.
        if self.seek_in_flight.get() {
            return;
        }
        self.show_position(seconds);
    }

    /// The one place playhead and clock are presented, so every path that
    /// changes position (load, unload, scrub, seek completion, playback)
    /// agrees. The label only shows whole seconds, so only reformat when one
    /// actually ticks over.
    fn show_position(&self, seconds: f64) {
        self.position_seconds.set(seconds);
        let position_ms = (seconds * 1_000.0) as u64;
        let duration_ms = self.duration_ms.get();
        self.timeline.set_position(position_ms);
        let rendered = (position_ms / 1_000, duration_ms);
        if self.time_label_state.replace(Some(rendered)) != Some(rendered) {
            self.time_label.set_text(&format!(
                "{} / {}",
                timeline::format_mm_ss(position_ms),
                timeline::format_mm_ss(duration_ms),
            ));
        }
    }

    fn handle_key(self: &Rc<Self>, keyval: gtk4::gdk::Key) -> gtk4::glib::Propagation {
        if !self.media_usable.get() {
            return gtk4::glib::Propagation::Proceed;
        }
        let position = self.position_seconds.get();
        match keyval {
            gtk4::gdk::Key::space | gtk4::gdk::Key::k | gtk4::gdk::Key::K => {
                self.toggle_playing();
            }
            gtk4::gdk::Key::j | gtk4::gdk::Key::J | gtk4::gdk::Key::Left => {
                self.request_seek(position - SEEK_STEP_SECONDS, SeekMode::Settle);
            }
            gtk4::gdk::Key::l | gtk4::gdk::Key::L | gtk4::gdk::Key::Right => {
                self.request_seek(position + SEEK_STEP_SECONDS, SeekMode::Settle);
            }
            gtk4::gdk::Key::bracketleft => self.jump_marker(MarkerDirection::Previous),
            gtk4::gdk::Key::bracketright => self.jump_marker(MarkerDirection::Next),
            gtk4::gdk::Key::comma => {
                // Previous frame while paused: known FPS, else assume 30. The
                // frame is the point, so this is the one seek worth decoding
                // exactly.
                if !self.playing.get() {
                    let fps = self.fps.get().unwrap_or(30).max(1);
                    self.request_seek(position - 1.0 / f64::from(fps), SeekMode::Exact);
                }
            }
            gtk4::gdk::Key::period => {
                if !self.playing.get()
                    && let Some(backend) = &self.backend
                {
                    backend.advance_frame();
                }
            }
            gtk4::gdk::Key::Escape => {
                // Escape only leaves fullscreen; it is not claimed otherwise.
                match self.stack.root().and_downcast::<gtk4::Window>() {
                    Some(window) if window.is_fullscreen() => window.set_fullscreened(false),
                    _ => return gtk4::glib::Propagation::Proceed,
                }
            }
            _ => return gtk4::glib::Propagation::Proceed,
        }
        gtk4::glib::Propagation::Stop
    }

    // -- fullscreen idle -----------------------------------------------------

    fn set_fullscreen(self: &Rc<Self>, fullscreen: bool) {
        self.fullscreen.set(fullscreen);
        self.last_motion.set(Instant::now());
        self.reveal_bottom_bar();
        if !fullscreen {
            return;
        }
        let this = Rc::clone(self);
        gtk4::glib::timeout_add_local(IDLE_TICK, move || {
            if !this.fullscreen.get() {
                return gtk4::glib::ControlFlow::Break;
            }
            if this.last_motion.get().elapsed() >= IDLE_HIDE {
                this.bottom_bar.set_reveal_child(false);
                this.set_pointer_hidden(true);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    fn wake_controls(&self, x: f64, y: f64) {
        // Collapsing the bar relayouts everything under a pointer that never
        // moved, and GTK re-sends motion at that same position to re-resolve
        // hover. Treating that as activity makes the collapse undo itself.
        if self.last_pointer.replace((x, y)) == (x, y) {
            return;
        }
        self.last_motion.set(Instant::now());
        self.reveal_bottom_bar();
    }

    fn reveal_bottom_bar(&self) {
        if !self.bottom_bar.reveals_child() {
            self.bottom_bar.set_reveal_child(true);
            self.set_pointer_hidden(false);
        }
    }

    fn set_pointer_hidden(&self, hidden: bool) {
        let name = hidden.then_some("none");
        if let Some(window) = self.stack.root().and_downcast::<gtk4::Window>() {
            window.set_cursor_from_name(name);
        }
        // Clapper sets a cursor on its own video widget, and the innermost
        // widget with one wins, so the window alone leaves the pointer visible
        // over the video, which is the whole screen in fullscreen.
        if let Some(backend) = &self.backend {
            backend.widget().set_cursor_from_name(name);
        }
    }

    fn toggle_fullscreen(&self) {
        if let Some(window) = self.stack.root().and_downcast::<gtk4::Window>() {
            window.set_fullscreened(!window.is_fullscreen());
        }
    }

    fn reveal(&self) {
        let active = self.active_id.borrow();
        let entries = self.entries.borrow();
        let Some(entry) = active
            .as_ref()
            .and_then(|id| entries.iter().find(|entry| &entry.id == id))
        else {
            return;
        };
        let launcher = gtk4::FileLauncher::new(Some(&gtk4::gio::File::for_path(&entry.media_path)));
        let parent = self.stack.root().and_downcast::<gtk4::Window>();
        launcher.open_containing_folder(
            parent.as_ref(),
            None::<&gtk4::gio::Cancellable>,
            |result| {
                if let Err(error) = result {
                    tracing::warn!(%error, "could not reveal the recording");
                }
            },
        );
    }

    // -- clip mode -----------------------------------------------------------

    fn enter_clip_mode(&self) {
        if !self.media_usable.get() || self.duration_ms.get() == 0 {
            return;
        }
        self.clip_mode.set(true);
        let position_ms = (self.position_seconds.get() * 1_000.0) as u64;
        self.timeline.set_clip(Some(timeline::initial_clip_range(
            position_ms,
            self.duration_ms.get(),
        )));
        self.clip_button.set_visible(false);
        self.clip_actions.set_visible(true);
    }

    fn exit_clip_mode(&self) {
        self.clip_mode.set(false);
        self.timeline.set_clip(None);
        self.clip_button.set_visible(true);
        self.clip_actions.set_visible(false);
    }

    fn create_clip(&self) {
        if !self.media_usable.get() {
            return;
        }
        let (Some(range), Some(source)) = (self.timeline.clip(), self.active_id.borrow().clone())
        else {
            return;
        };
        let accepted = (self.sink)(ShellAction::Command(Command::CreateClip(ClipRange {
            source,
            start_ms: range.start_ms,
            end_ms: range.end_ms.min(self.duration_ms.get()),
        })));
        // Exit only after the command is accepted; progress and errors arrive
        // through the snapshot's work/problem fields.
        if accepted {
            self.exit_clip_mode();
        }
    }

    // -- markers -------------------------------------------------------------

    fn refresh_timeline(&self) {
        let active = self.active_id.borrow();
        let entries = self.entries.borrow();
        let entry = active
            .as_ref()
            .and_then(|id| entries.iter().find(|entry| &entry.id == id));
        self.timeline.set_entry(entry, self.prefs.get());
        drop(entries);
        drop(active);
        self.update_marker_navigation();
    }

    /// Seek to the nearest visible marker strictly before/after the playhead.
    fn jump_marker(self: &Rc<Self>, direction: MarkerDirection) {
        let position_ms = (self.position_seconds.get() * 1_000.0) as u64;
        let target_ms = {
            let active = self.active_id.borrow();
            let entries = self.entries.borrow();
            active
                .as_ref()
                .and_then(|id| entries.iter().find(|entry| &entry.id == id))
                .and_then(|entry| {
                    timeline::marker_target(entry, self.prefs.get(), position_ms, direction)
                })
        };
        if let Some(target_ms) = target_ms {
            self.request_seek(target_ms as f64 / 1_000.0, SeekMode::Settle);
        }
    }

    fn update_marker_navigation(&self) {
        let usable = self.media_usable.get() && {
            let active = self.active_id.borrow();
            let entries = self.entries.borrow();
            active
                .as_ref()
                .and_then(|id| entries.iter().find(|entry| &entry.id == id))
                .is_some_and(|entry| !timeline::visible_items(entry, self.prefs.get()).is_empty())
        };
        self.previous_marker_button.set_sensitive(usable);
        self.next_marker_button.set_sensitive(usable);
    }

    /// The marker-visibility popover: death radio plus two check rows. Rebuilt
    /// from config so it always reflects the authoritative snapshot.
    fn rebuild_marker_menu(self: &Rc<Self>) {
        let prefs = self.prefs.get();
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        content.set_margin_end(8);

        let deaths_label = gtk4::Label::new(Some("Deaths"));
        deaths_label.add_css_class("caption-heading");
        deaths_label.set_xalign(0.0);
        content.append(&deaths_label);
        let mut group: Option<gtk4::CheckButton> = None;
        for (value, label) in [
            (DeathMarkerVisibility::None, "Hidden"),
            (DeathMarkerVisibility::Own, "Own deaths"),
            (DeathMarkerVisibility::All, "All deaths"),
        ] {
            let radio = gtk4::CheckButton::with_label(label);
            if let Some(group) = &group {
                radio.set_group(Some(group));
            } else {
                group = Some(radio.clone());
            }
            radio.set_active(prefs.deaths == value);
            let this = Rc::clone(self);
            radio.connect_toggled(move |radio| {
                if radio.is_active() && !this.updating.get() {
                    let mut prefs = this.prefs.get();
                    prefs.deaths = value;
                    this.dispatch_markers(prefs);
                }
            });
            content.append(&radio);
        }
        for (encounters, label) in [(true, "Encounter segments"), (false, "Round boundaries")] {
            let check = gtk4::CheckButton::with_label(label);
            let current = if encounters {
                prefs.encounters
            } else {
                prefs.rounds
            };
            check.set_active(current == MarkerVisibility::Visible);
            let this = Rc::clone(self);
            check.connect_toggled(move |check| {
                if this.updating.get() {
                    return;
                }
                let mut prefs = this.prefs.get();
                let value = if check.is_active() {
                    MarkerVisibility::Visible
                } else {
                    MarkerVisibility::Hidden
                };
                if encounters {
                    prefs.encounters = value;
                } else {
                    prefs.rounds = value;
                }
                this.dispatch_markers(prefs);
            });
            content.append(&check);
        }
        let popover = gtk4::Popover::new();
        popover.set_child(Some(&content));
        self.marker_button.set_popover(Some(&popover));
    }

    fn dispatch_markers(self: &Rc<Self>, prefs: MarkerPrefs) {
        // Optimistic local filter; the config write comes back via snapshot.
        self.prefs.set(prefs);
        self.refresh_timeline();
        (self.sink)(ShellAction::Command(Command::SetMarkerVisibility {
            deaths: prefs.deaths,
            encounters: prefs.encounters,
            rounds: prefs.rounds,
        }));
    }

    // -- viewpoint selector ---------------------------------------------------

    fn rebuild_pov_selector(self: &Rc<Self>) {
        let povs = self.povs.borrow();
        self.updating.set(true);
        let labels: Vec<&str> = povs.iter().map(|pov| pov.label.as_str()).collect();
        self.pov_dropdown
            .set_model(Some(&gtk4::StringList::new(&labels)));
        if let Some(index) = self
            .active_id
            .borrow()
            .as_ref()
            .and_then(|id| povs.iter().position(|pov| &pov.id == id))
        {
            self.pov_dropdown.set_selected(index as u32);
        }
        self.updating.set(false);
        self.pov_dropdown.set_visible(povs.len() > 1);
    }
}

fn icon_button(icon: &str, label: &str) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk4::accessible::Property::Label(label)]);
    button
}

/// Player shortcuts are ignored while an editable widget has focus.
fn focus_is_editable(window: &gtk4::Window) -> bool {
    let Some(focus): Option<gtk4::Widget> = gtk4::prelude::GtkWindowExt::focus(window) else {
        return false;
    };
    focus.is::<gtk4::Text>()
        || focus.is::<gtk4::TextView>()
        || focus.ancestor(gtk4::Editable::static_type()).is_some()
}
