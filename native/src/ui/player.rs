// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent player pane: one ClapperGtk video with Warcraft Recorder's
//! compact control row, the combat timeline, the drawing overlay, clip mode,
//! and a single-view viewpoint selector. Volume/mute are process-shared
//! session state; speed, position, drawings, and the clip range are
//! session-only. All playback state lives in Clapper; this pane only issues
//! commands and mirrors positions.
//!
//! Multi-POV grid playback (synchronized 2–4 player grid) was removed from
//! the product by maintainer decision (2026-07-22); the viewpoint selector
//! and the kill-video editor remain.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;

use warcraft_recorder::coordinator::{AppSnapshot, ClipRange, Command};
use warcraft_recorder::domain::{
    Category, DeathMarkerVisibility, LibraryEntry, MarkerVisibility, RecordingId,
};

use super::library::Selection;
use super::multipov;
use super::player_backend::PlayerBackend;
use super::timeline::{self, MarkerPrefs, Timeline};
use super::{ActionSink, ShellAction, drawing, kill_video};

const SPEEDS: [f64; 4] = [0.25, 0.5, 1.0, 2.0];
const SEEK_STEP_SECONDS: f64 = 5.0;

pub struct Player {
    pub widget: gtk4::Box,
    inner: Rc<Inner>,
}

struct Inner {
    sink: ActionSink,

    stack: gtk4::Stack,
    error_bar: gtk4::Box,
    timeline: Timeline,
    drawing: drawing::Overlay,
    time_label: gtk4::Label,
    play_button: gtk4::Button,
    speed_button: gtk4::Button,
    mute_button: gtk4::Button,
    volume_scale: gtk4::Scale,
    pov_dropdown: gtk4::DropDown,
    clip_button: gtk4::Button,
    clip_actions: gtk4::Box,
    drawing_toggle: gtk4::ToggleButton,
    marker_button: gtk4::MenuButton,
    reveal_button: gtk4::Button,

    /// The one Clapper backend; `None` only when Clapper failed to start.
    backend: Option<PlayerBackend>,

    entries: RefCell<Arc<[LibraryEntry]>>,
    prefs: Cell<MarkerPrefs>,
    /// POVs of the selected activity and the id currently loaded.
    povs: RefCell<Vec<multipov::Pov>>,
    active_id: RefCell<Option<RecordingId>>,
    preferred_player: RefCell<Option<String>>,

    playing: Cell<bool>,
    speed_index: Cell<usize>,
    muted: Cell<bool>,
    position_seconds: Cell<f64>,
    duration_ms: Cell<u64>,
    fps: Cell<Option<u32>>,
    clip_mode: Cell<bool>,

    /// Async seek: the newest requested target and whether one is in flight.
    pending_seek: Cell<Option<f64>>,
    seek_in_flight: Cell<bool>,
    /// Guard: snapshot-driven widget updates must not dispatch commands.
    updating: Cell<bool>,
}

impl Player {
    pub fn new(sink: ActionSink) -> Self {
        let placeholder = adw::StatusPage::new();
        placeholder.set_title("No recording selected");
        placeholder.set_description(Some("Select a recording below to review it."));

        let backend = PlayerBackend::new()
            .map_err(|error| tracing::warn!(error, "player backend unavailable"))
            .ok();

        let drawing = drawing::Overlay::new();
        let video_overlay = gtk4::Overlay::new();
        if let Some(backend) = &backend {
            video_overlay.set_child(Some(backend.widget()));
        }
        video_overlay.add_overlay(&drawing.area);
        video_overlay.set_vexpand(true);

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
        let marker_button = gtk4::MenuButton::new();
        marker_button.set_icon_name("view-list-symbolic");
        marker_button.set_tooltip_text(Some("Marker visibility"));
        let drawing_toggle = gtk4::ToggleButton::new();
        drawing_toggle.set_icon_name("document-edit-symbolic");
        drawing_toggle.set_tooltip_text(Some("Toggle drawing"));
        drawing_toggle.update_property(&[gtk4::accessible::Property::Label("Toggle drawing")]);
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
            marker_button.upcast_ref(),
            drawing_toggle.upcast_ref(),
            clip_button.upcast_ref(),
            clip_actions.upcast_ref(),
            pov_dropdown.upcast_ref(),
            reveal_button.upcast_ref(),
            fullscreen_button.upcast_ref(),
        ] {
            controls.append(widget);
        }

        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.add_css_class("player-area");
        widget.append(&stack);
        widget.append(&drawing.toolbar);
        widget.append(&controls);

        let inner = Rc::new(Inner {
            sink,
            stack,
            error_bar,
            timeline,
            drawing,
            time_label,
            play_button,
            speed_button,
            mute_button,
            volume_scale,
            pov_dropdown,
            clip_button,
            clip_actions,
            drawing_toggle,
            marker_button,
            reveal_button,
            backend,
            entries: RefCell::new(Arc::from(Vec::new())),
            prefs: Cell::new(MarkerPrefs {
                deaths: DeathMarkerVisibility::Own,
                encounters: MarkerVisibility::Visible,
                rounds: MarkerVisibility::Visible,
            }),
            povs: RefCell::new(Vec::new()),
            active_id: RefCell::new(None),
            preferred_player: RefCell::new(None),
            playing: Cell::new(false),
            speed_index: Cell::new(2),
            muted: Cell::new(false),
            position_seconds: Cell::new(0.0),
            duration_ms: Cell::new(0),
            fps: Cell::new(None),
            clip_mode: Cell::new(false),
            pending_seek: Cell::new(None),
            seek_in_flight: Cell::new(false),
            updating: Cell::new(false),
        });

        inner.connect_backend();
        inner.connect_controls(
            &clip_create,
            &clip_cancel,
            &fullscreen_button,
            &error_reveal,
        );

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
        }
        inner.rebuild_marker_menu();
        inner.updating.set(false);
    }

    /// Table selection changed. `None` shows the placeholder.
    pub fn set_selection(&self, selection: Option<&Selection>) {
        self.inner.set_selection(selection);
    }

    /// Raid Creator: open the kill-video editor for this activity.
    pub fn open_kill_video(&self, correlated_id: &RecordingId) {
        let inner = &self.inner;
        let entries = inner.entries.borrow();
        let povs = inner.povs.borrow();
        let sources = kill_video::sources_for(&povs, &entries);
        if sources.len() < 2 {
            return;
        }
        kill_video::present(
            inner.stack.upcast_ref(),
            Rc::clone(&inner.sink),
            correlated_id.clone(),
            sources,
        );
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
        self.timeline.connect_seek(move |ms| {
            this.request_seek(ms as f64 / 1_000.0);
        });
        let this = Rc::clone(self);
        self.drawing_toggle.connect_toggled(move |toggle| {
            this.drawing.set_enabled(toggle.is_active());
        });
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

        // New activity: stop, clear drawings and clip mode, seek to zero.
        self.exit_clip_mode();
        self.drawing.reset();
        self.drawing_toggle.set_active(false);
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
        let is_clip = entry.category == Category::Clip;
        drop(entries);
        *self.active_id.borrow_mut() = Some(id.clone());

        self.error_bar.set_visible(false);
        let Some(backend) = &self.backend else {
            self.error_bar.set_visible(true);
            return;
        };
        backend.stop();
        if let Err(error) = backend.open_uri(&uri) {
            tracing::warn!(error, "player failed to load media");
            self.error_bar.set_visible(true);
            return;
        }
        backend.set_speed(SPEEDS[self.speed_index.get()]);
        backend.set_volume(self.volume_scale.value());
        backend.set_muted(self.muted.get());
        backend.play();
        self.playing.set(true);
        self.play_button
            .set_icon_name("media-playback-pause-symbolic");
        if retain_position {
            let position = self.position_seconds.get();
            self.request_seek(position);
        } else {
            self.position_seconds.set(0.0);
        }
        // Clips cannot be re-clipped (legacy: the clip button is unavailable).
        self.clip_button.set_sensitive(!is_clip);
        self.stack.set_visible_child_name("video");
        self.refresh_timeline();
        self.watch_for_failure();
    }

    /// Playback failure has no supported error signal in the bindings; if the
    /// player still is not ready shortly after a load, surface the one
    /// recovery row. A later successful load hides it again.
    fn watch_for_failure(self: &Rc<Self>) {
        let this = Rc::clone(self);
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
            let ready = this.backend.as_ref().is_some_and(PlayerBackend::is_ready);
            if !ready && this.active_id.borrow().is_some() {
                this.error_bar.set_visible(true);
            }
        });
    }

    fn unload(self: &Rc<Self>) {
        self.exit_clip_mode();
        self.drawing.reset();
        self.drawing_toggle.set_active(false);
        if let Some(backend) = &self.backend {
            backend.stop();
        }
        *self.active_id.borrow_mut() = None;
        self.povs.borrow_mut().clear();
        self.playing.set(false);
        self.stack.set_visible_child_name("placeholder");
        self.timeline.set_entry(None, self.prefs.get());
        self.rebuild_pov_selector();
    }

    // -- transport -----------------------------------------------------------

    fn toggle_playing(&self) {
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
    fn request_seek(self: &Rc<Self>, seconds: f64) {
        let seconds = seconds.clamp(0.0, self.duration_ms.get() as f64 / 1_000.0);
        self.position_seconds.set(seconds);
        self.timeline.set_position((seconds * 1_000.0) as u64);
        if self.seek_in_flight.get() {
            self.pending_seek.set(Some(seconds));
            return;
        }
        self.seek_in_flight.set(true);
        if let Some(backend) = &self.backend {
            backend.seek(seconds);
        }
    }

    fn on_seek_done(self: &Rc<Self>) {
        match self.pending_seek.take() {
            Some(target) => {
                if let Some(backend) = &self.backend {
                    backend.seek(target);
                }
            }
            None => self.seek_in_flight.set(false),
        }
    }

    fn on_position(&self, seconds: f64) {
        // While a seek is pending, presentation keeps the requested target.
        if self.seek_in_flight.get() {
            return;
        }
        self.position_seconds.set(seconds);
        let position_ms = (seconds * 1_000.0) as u64;
        self.timeline.set_position(position_ms);
        self.time_label.set_text(&format!(
            "{} / {}",
            timeline::format_mm_ss(position_ms),
            timeline::format_mm_ss(self.duration_ms.get()),
        ));
    }

    fn handle_key(self: &Rc<Self>, keyval: gtk4::gdk::Key) -> gtk4::glib::Propagation {
        let position = self.position_seconds.get();
        match keyval {
            gtk4::gdk::Key::space | gtk4::gdk::Key::k | gtk4::gdk::Key::K => {
                self.toggle_playing();
            }
            gtk4::gdk::Key::j | gtk4::gdk::Key::J | gtk4::gdk::Key::Left => {
                self.request_seek(position - SEEK_STEP_SECONDS);
            }
            gtk4::gdk::Key::l | gtk4::gdk::Key::L | gtk4::gdk::Key::Right => {
                self.request_seek(position + SEEK_STEP_SECONDS);
            }
            gtk4::gdk::Key::comma => {
                // Approximate previous frame while paused: known FPS, else the
                // legacy 30 fps assumption.
                if !self.playing.get() {
                    let fps = self.fps.get().unwrap_or(30).max(1);
                    self.request_seek(position - 1.0 / f64::from(fps));
                }
            }
            gtk4::gdk::Key::period => {
                if !self.playing.get()
                    && let Some(backend) = &self.backend
                {
                    backend.advance_frame();
                }
            }
            _ => return gtk4::glib::Propagation::Proceed,
        }
        gtk4::glib::Propagation::Stop
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
        if self.duration_ms.get() == 0 {
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
