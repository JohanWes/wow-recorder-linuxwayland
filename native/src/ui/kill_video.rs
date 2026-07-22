// SPDX-License-Identifier: GPL-3.0-or-later

//! The kill-video editor: one native dialog with a source list, a single
//! ordered segment track, one reused Clapper preview player, output options,
//! and Render/Cancel. It only builds `CreateKillVideo` payloads; all FFmpeg
//! work stays in the media worker.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use warcraft_recorder::coordinator::{ClipRange, Command};
use warcraft_recorder::domain::{LibraryEntry, RecordingId};
use warcraft_recorder::media_jobs::KillAudio;

use super::multipov::Pov;
use super::player_backend::PlayerBackend;
use super::timeline::format_mm_ss;
use super::{ActionSink, ShellAction};

pub const FPS_CHOICES: [u32; 4] = [10, 20, 30, 60];

/// The legacy `obsResolutions` output choices (factual table).
#[rustfmt::skip]
pub const RESOLUTIONS: [(u32, u32); 20] = [
    (1024, 768), (1280, 720), (1280, 800), (1280, 1024), (1360, 768), (1366, 768),
    (1440, 900), (1600, 900), (1680, 1050), (1920, 1080), (1920, 1200), (2560, 1080),
    (2560, 1440), (2560, 1600), (3360, 1440), (3440, 1440), (3440, 1200), (3520, 990),
    (3840, 1080), (3840, 2160),
];
const DEFAULT_RESOLUTION: usize = 9; // 1920x1080
const DEFAULT_FPS: usize = 3; // 60
/// A boundary drag keeps both neighbours at least this long.
const MIN_SEGMENT_MS: u64 = 1_000;

/// One montage source. POV sources share one time axis, so a segment's range
/// on the montage track is the same range in its source media.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub id: RecordingId,
    pub label: String,
    pub player: Option<String>,
    pub duration_ms: u64,
    pub media_uri: String,
}

/// The single ordered segment track: an ordered subset of sources plus the
/// boundaries between them. `boundaries.len() == order.len() + 1`, boundaries
/// are strictly increasing, start at 0, and end at the shared total — no gaps,
/// overlaps, or out-of-range segments by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    /// Indices into the full source list, in montage order.
    pub order: Vec<usize>,
    pub boundaries: Vec<u64>,
    pub total_ms: u64,
}

/// The initial equal allocation over every source, using the shortest source
/// duration as the shared total (legacy behavior).
pub fn initial_track(sources: &[Source]) -> Track {
    let total_ms = sources
        .iter()
        .map(|source| source.duration_ms)
        .min()
        .unwrap_or(0);
    let count = sources.len().max(1) as u64;
    let boundaries = (0..=sources.len() as u64)
        .map(|index| total_ms * index / count)
        .collect();
    Track {
        order: (0..sources.len()).collect(),
        boundaries,
        total_ms,
    }
}

impl Track {
    /// Move the segment at `from` to position `to`, keeping boundaries.
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from < self.order.len() && to < self.order.len() {
            let source = self.order.remove(from);
            self.order.insert(to, source);
        }
    }

    /// Drag the boundary between segment `index` and `index + 1`, clamped so
    /// both neighbours keep at least `MIN_SEGMENT_MS`.
    pub fn resize_boundary(&mut self, index: usize, to_ms: u64) {
        if index == 0 || index >= self.boundaries.len() - 1 {
            return; // The outer edges are fixed at 0 and total.
        }
        let low = self.boundaries[index - 1] + MIN_SEGMENT_MS;
        let high = self.boundaries[index + 1].saturating_sub(MIN_SEGMENT_MS);
        if low <= high {
            self.boundaries[index] = to_ms.clamp(low, high);
        }
    }

    /// Remove one segment while more than two remain; its duration is
    /// redistributed by rescaling the remaining boundaries over the total.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.order.len() <= 2 || index >= self.order.len() {
            return false;
        }
        let removed_ms = self.boundaries[index + 1] - self.boundaries[index];
        self.order.remove(index);
        self.boundaries.remove(index + 1);
        let old_total = self.total_ms - removed_ms;
        // Rescale interior boundaries back over the full total.
        for (position, boundary) in self.boundaries.iter_mut().enumerate() {
            if position == 0 {
                continue;
            }
            let mut value = *boundary;
            if position > index {
                value -= removed_ms;
            }
            *boundary = if old_total == 0 {
                self.total_ms * position as u64 / (self.order.len() as u64)
            } else {
                (u128::from(value) * u128::from(self.total_ms) / u128::from(old_total)) as u64
            };
        }
        *self.boundaries.last_mut().expect("nonempty") = self.total_ms;
        true
    }

    /// The segment index whose montage range contains `ms`.
    pub fn segment_at(&self, ms: u64) -> usize {
        self.boundaries
            .windows(2)
            .position(|window| ms < window[1])
            .unwrap_or(self.order.len().saturating_sub(1))
    }

    /// The `CreateKillVideo` segment payload.
    pub fn ranges(&self, sources: &[Source]) -> Vec<ClipRange> {
        self.order
            .iter()
            .enumerate()
            .filter_map(|(position, source_index)| {
                sources.get(*source_index).map(|source| ClipRange {
                    source: source.id.clone(),
                    start_ms: self.boundaries[position],
                    end_ms: self.boundaries[position + 1],
                })
            })
            .collect()
    }
}

/// The audio payload for the current single-audio toggle/selection, matching
/// the legacy fallback: an unknown source falls back to switched audio.
pub fn audio_payload(
    single_audio: bool,
    source_player: Option<&str>,
    track: &Track,
    sources: &[Source],
) -> KillAudio {
    if !single_audio {
        return KillAudio::Switched;
    }
    track
        .order
        .iter()
        .position(|source_index| {
            sources
                .get(*source_index)
                .is_some_and(|source| source.player.as_deref() == source_player)
        })
        .map_or(KillAudio::Switched, KillAudio::Source)
}

// ---------------------------------------------------------------------------
// Dialog
// ---------------------------------------------------------------------------

pub fn sources_for(povs: &[Pov], entries: &[LibraryEntry]) -> Vec<Source> {
    povs.iter()
        .filter_map(|pov| {
            entries
                .iter()
                .find(|entry| entry.id == pov.id)
                .map(|entry| Source {
                    id: entry.id.clone(),
                    label: pov.label.clone(),
                    player: pov.player.clone(),
                    duration_ms: entry.duration_ms,
                    media_uri: gtk4::gio::File::for_path(&entry.media_path)
                        .uri()
                        .to_string(),
                })
        })
        .collect()
}

struct Editor {
    sources: Vec<Source>,
    track: RefCell<Track>,
    fps: Cell<usize>,
    resolution: Cell<usize>,
    single_audio: Cell<bool>,
    audio_player: RefCell<Option<String>>,
    preview: PlayerBackend,
    preview_source: Cell<usize>,
    playhead_ms: Cell<u64>,
    ruler: gtk4::DrawingArea,
    segment_list: gtk4::Box,
    correlated_id: RecordingId,
    sink: ActionSink,
}

/// Open the kill-video editor for a correlated activity with ≥2 local POVs.
pub fn present(
    parent: &gtk4::Widget,
    sink: ActionSink,
    correlated_id: RecordingId,
    sources: Vec<Source>,
) {
    let Ok(preview) = PlayerBackend::new() else {
        tracing::warn!("kill-video preview player unavailable");
        return;
    };
    let editor = Rc::new(Editor {
        track: RefCell::new(initial_track(&sources)),
        fps: Cell::new(DEFAULT_FPS),
        resolution: Cell::new(DEFAULT_RESOLUTION),
        single_audio: Cell::new(false),
        audio_player: RefCell::new(sources.first().and_then(|source| source.player.clone())),
        preview,
        preview_source: Cell::new(usize::MAX),
        playhead_ms: Cell::new(0),
        ruler: gtk4::DrawingArea::new(),
        segment_list: gtk4::Box::new(gtk4::Orientation::Vertical, 4),
        correlated_id,
        sources,
        sink,
    });

    let dialog = adw::Dialog::new();
    dialog.set_title("Kill video creator");
    dialog.set_content_width(860);

    // Preview player with its own compact transport.
    let video = editor.preview.widget().clone();
    video.set_size_request(-1, 300);
    let play = gtk4::Button::from_icon_name("media-playback-start-symbolic");
    play.set_tooltip_text(Some("Play/pause preview"));
    play.update_property(&[gtk4::accessible::Property::Label("Play/pause preview")]);
    {
        let editor = Rc::clone(&editor);
        let playing = Cell::new(false);
        play.connect_clicked(move |button| {
            if playing.get() {
                editor.preview.pause();
                button.set_icon_name("media-playback-start-symbolic");
            } else {
                editor.preview.play();
                button.set_icon_name("media-playback-pause-symbolic");
            }
            playing.set(!playing.get());
        });
    }
    let mute = gtk4::ToggleButton::new();
    mute.set_icon_name("audio-volume-muted-symbolic");
    mute.set_tooltip_text(Some("Mute preview"));
    mute.update_property(&[gtk4::accessible::Property::Label("Mute preview")]);
    {
        let editor = Rc::clone(&editor);
        mute.connect_toggled(move |button| editor.preview.set_muted(button.is_active()));
    }

    // Ruler: the montage track with segment blocks, boundaries, and playhead.
    editor.ruler.set_content_height(48);
    editor.ruler.set_hexpand(true);
    connect_ruler(&editor);

    let transport = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    transport.append(&play);
    transport.append(&mute);
    transport.append(&editor.ruler);

    // Output options.
    let fps_labels: Vec<String> = FPS_CHOICES.iter().map(u32::to_string).collect();
    let fps = drop_down(&fps_labels, DEFAULT_FPS, "Output FPS");
    {
        let editor = Rc::clone(&editor);
        fps.connect_selected_notify(move |dropdown| {
            editor.fps.set(dropdown.selected() as usize);
        });
    }
    let resolution_labels: Vec<String> = RESOLUTIONS
        .iter()
        .map(|(width, height)| format!("{width}x{height}"))
        .collect();
    let resolution = drop_down(&resolution_labels, DEFAULT_RESOLUTION, "Output resolution");
    {
        let editor = Rc::clone(&editor);
        resolution.connect_selected_notify(move |dropdown| {
            editor.resolution.set(dropdown.selected() as usize);
        });
    }

    let audio_switch = gtk4::Switch::new();
    audio_switch.set_valign(gtk4::Align::Center);
    audio_switch.set_tooltip_text(Some("Use one audio track for the whole video"));
    let audio_labels: Vec<String> = editor
        .sources
        .iter()
        .map(|source| source.label.clone())
        .collect();
    let audio_source = drop_down(&audio_labels, 0, "Audio track source");
    audio_source.set_sensitive(false);
    {
        let editor = Rc::clone(&editor);
        let audio_source = audio_source.clone();
        audio_switch.connect_state_set(move |_, enabled| {
            editor.single_audio.set(enabled);
            audio_source.set_sensitive(enabled);
            gtk4::glib::Propagation::Proceed
        });
    }
    {
        let editor = Rc::clone(&editor);
        audio_source.connect_selected_notify(move |dropdown| {
            *editor.audio_player.borrow_mut() = editor
                .sources
                .get(dropdown.selected() as usize)
                .and_then(|source| source.player.clone());
        });
    }

    let options = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    for (label, widget) in [
        ("FPS", fps.upcast_ref::<gtk4::Widget>()),
        ("Resolution", resolution.upcast_ref()),
        ("Single audio track", audio_switch.upcast_ref()),
        ("Audio source", audio_source.upcast_ref()),
    ] {
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        let caption = gtk4::Label::new(Some(label));
        caption.add_css_class("caption-heading");
        caption.set_xalign(0.0);
        column.append(&caption);
        column.append(widget);
        options.append(&column);
    }

    let warning = gtk4::Label::new(Some(
        "Rendering is CPU-intensive and can take a while. The finished video \
         appears in Clips automatically.",
    ));
    warning.add_css_class("dim-label");
    warning.set_wrap(true);
    warning.set_xalign(0.0);

    // Footer: Cancel / Reset / Render.
    let cancel = gtk4::Button::with_label("Cancel");
    let reset = gtk4::Button::with_label("Reset");
    let render = gtk4::Button::with_label("Render");
    render.add_css_class("suggested-action");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let editor = Rc::clone(&editor);
        reset.connect_clicked(move |_| {
            *editor.track.borrow_mut() = initial_track(&editor.sources);
            editor.refresh();
        });
    }
    {
        let editor = Rc::clone(&editor);
        let dialog = dialog.clone();
        render.connect_clicked(move |_| {
            let track = editor.track.borrow();
            let (width, height) = RESOLUTIONS[editor.resolution.get()];
            let command = Command::CreateKillVideo {
                correlated_id: editor.correlated_id.clone(),
                segments: track.ranges(&editor.sources),
                width,
                height,
                fps: FPS_CHOICES[editor.fps.get()],
                audio: audio_payload(
                    editor.single_audio.get(),
                    editor.audio_player.borrow().as_deref(),
                    &track,
                    &editor.sources,
                ),
            };
            drop(track);
            if (editor.sink)(ShellAction::Command(command)) {
                dialog.close();
            }
        });
    }
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    footer.set_halign(gtk4::Align::End);
    footer.append(&cancel);
    footer.append(&reset);
    footer.append(&render);

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&video);
    content.append(&transport);
    content.append(&editor.segment_list);
    content.append(&options);
    content.append(&warning);
    content.append(&footer);

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));
    dialog.set_child(Some(&toolbar_view));

    {
        let editor = Rc::clone(&editor);
        dialog.connect_closed(move |_| editor.preview.stop());
    }

    editor.refresh();
    editor.sync_preview(0);
    dialog.present(Some(parent));
}

impl Editor {
    /// Redraw the ruler and rebuild the segment rows after any track change.
    fn refresh(self: &Rc<Self>) {
        self.ruler.queue_draw();
        while let Some(child) = self.segment_list.first_child() {
            self.segment_list.remove(&child);
        }
        let track = self.track.borrow().clone();
        for (position, source_index) in track.order.iter().enumerate() {
            let Some(source) = self.sources.get(*source_index) else {
                continue;
            };
            let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
            let label = gtk4::Label::new(Some(&format!(
                "{}. {}  ({} – {})",
                position + 1,
                source.label,
                format_mm_ss(track.boundaries[position]),
                format_mm_ss(track.boundaries[position + 1]),
            )));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            row.append(&label);
            for (icon, tooltip, delta) in [
                ("go-up-symbolic", "Move earlier", -1_i32),
                ("go-down-symbolic", "Move later", 1),
            ] {
                let button = gtk4::Button::from_icon_name(icon);
                button.add_css_class("flat");
                button.set_tooltip_text(Some(tooltip));
                button.update_property(&[gtk4::accessible::Property::Label(tooltip)]);
                let target = position as i32 + delta;
                button.set_sensitive(target >= 0 && (target as usize) < track.order.len());
                let editor = Rc::clone(self);
                button.connect_clicked(move |_| {
                    editor
                        .track
                        .borrow_mut()
                        .reorder(position, target.max(0) as usize);
                    editor.refresh();
                });
                row.append(&button);
            }
            let remove = gtk4::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Remove source"));
            remove.update_property(&[gtk4::accessible::Property::Label("Remove source")]);
            remove.set_sensitive(track.order.len() > 2);
            {
                let editor = Rc::clone(self);
                remove.connect_clicked(move |_| {
                    if editor.track.borrow_mut().remove(position) {
                        editor.refresh();
                    }
                });
            }
            row.append(&remove);
            self.segment_list.append(&row);
        }
    }

    /// Load/seek the preview to the montage playhead, switching the media to
    /// the active segment's source when the playhead crosses a boundary.
    fn sync_preview(self: &Rc<Self>, playhead_ms: u64) {
        self.playhead_ms.set(playhead_ms);
        let track = self.track.borrow();
        let segment = track.segment_at(playhead_ms);
        let Some(source_index) = track.order.get(segment).copied() else {
            return;
        };
        drop(track);
        if self.preview_source.replace(source_index) != source_index
            && let Some(source) = self.sources.get(source_index)
            && let Err(error) = self.preview.open_uri(&source.media_uri)
        {
            tracing::warn!(error, "kill-video preview failed to load");
        }
        self.preview.seek(playhead_ms as f64 / 1_000.0);
        self.ruler.queue_draw();
    }
}

fn connect_ruler(editor: &Rc<Editor>) {
    let state = Rc::clone(editor);
    editor.ruler.set_draw_func(move |_, cr, width, height| {
        let track = state.track.borrow();
        if track.total_ms == 0 {
            return;
        }
        let width = f64::from(width);
        let height = f64::from(height);
        let to_x = |ms: u64| ms as f64 / track.total_ms as f64 * width;
        for (position, _) in track.order.iter().enumerate() {
            let start = to_x(track.boundaries[position]);
            let end = to_x(track.boundaries[position + 1]);
            // Alternate block tones so adjacent segments read as distinct.
            if position % 2 == 0 {
                cr.set_source_rgba(0.73, 0.26, 0.13, 0.55);
            } else {
                cr.set_source_rgba(0.73, 0.26, 0.13, 0.30);
            }
            cr.rectangle(
                start + 1.0,
                6.0,
                (end - start - 2.0).max(1.0),
                height - 12.0,
            );
            let _ = cr.fill();
        }
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        for boundary in &track.boundaries[1..track.boundaries.len() - 1] {
            let x = to_x(*boundary);
            cr.rectangle(x - 1.5, 2.0, 3.0, height - 4.0);
            let _ = cr.fill();
        }
        let x = to_x(state.playhead_ms.get().min(track.total_ms));
        cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
        cr.rectangle(x - 1.0, 0.0, 2.0, height);
        let _ = cr.fill();
    });

    // Drag near a boundary resizes it; elsewhere scrubs the playhead.
    let drag = gtk4::GestureDrag::new();
    let state = Rc::clone(editor);
    let grabbed: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    {
        let state = Rc::clone(&state);
        let grabbed = Rc::clone(&grabbed);
        drag.connect_drag_begin(move |_, x, _| {
            let track = state.track.borrow();
            if track.total_ms == 0 {
                return;
            }
            let width = f64::from(state.ruler.width()).max(1.0);
            let boundary = (1..track.boundaries.len() - 1).find(|index| {
                let boundary_x = track.boundaries[*index] as f64 / track.total_ms as f64 * width;
                (x - boundary_x).abs() <= 6.0
            });
            drop(track);
            grabbed.set(boundary);
            if boundary.is_none() {
                scrub(&state, x);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let grabbed = Rc::clone(&grabbed);
        drag.connect_drag_update(move |gesture, dx, _| {
            let Some((start_x, _)) = gesture.start_point() else {
                return;
            };
            let x = start_x + dx;
            match grabbed.get() {
                Some(index) => {
                    let width = f64::from(state.ruler.width()).max(1.0);
                    let total = state.track.borrow().total_ms;
                    let ms = ((x / width).clamp(0.0, 1.0) * total as f64) as u64;
                    state.track.borrow_mut().resize_boundary(index, ms);
                    state.refresh();
                }
                None => scrub(&state, x),
            }
        });
    }
    {
        let grabbed = Rc::clone(&grabbed);
        drag.connect_drag_end(move |_, _, _| grabbed.set(None));
    }
    editor.ruler.add_controller(drag);
}

fn scrub(editor: &Rc<Editor>, x: f64) {
    let total = editor.track.borrow().total_ms;
    let width = f64::from(editor.ruler.width()).max(1.0);
    let ms = ((x / width).clamp(0.0, 1.0) * total as f64) as u64;
    editor.sync_preview(ms);
}

fn drop_down(labels: &[String], selected: usize, accessible: &str) -> gtk4::DropDown {
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let dropdown = gtk4::DropDown::from_strings(&refs);
    dropdown.set_selected(selected as u32);
    dropdown.set_tooltip_text(Some(accessible));
    dropdown.update_property(&[gtk4::accessible::Property::Label(accessible)]);
    dropdown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(durations: &[u64]) -> Vec<Source> {
        durations
            .iter()
            .enumerate()
            .map(|(index, duration_ms)| Source {
                id: RecordingId::new(),
                label: format!("Player{index}"),
                player: Some(format!("Player{index}")),
                duration_ms: *duration_ms,
                media_uri: format!("file:///v{index}.mkv"),
            })
            .collect()
    }

    #[test]
    fn initial_allocation_is_equal_over_the_shortest_source() {
        let sources = sources(&[90_000, 60_000, 120_000]);
        let track = initial_track(&sources);
        assert_eq!(track.total_ms, 60_000);
        assert_eq!(track.boundaries, vec![0, 20_000, 40_000, 60_000]);
        assert_eq!(track.order, vec![0, 1, 2]);
        let ranges = track.ranges(&sources);
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[1].start_ms, 20_000);
        assert_eq!(ranges[1].end_ms, 40_000);
    }

    #[test]
    fn boundary_resize_is_clamped_and_removal_redistributes() {
        let sources = sources(&[60_000, 60_000, 60_000]);
        let mut track = initial_track(&sources);

        // Clamped so both neighbours keep at least the minimum length.
        track.resize_boundary(1, 100);
        assert_eq!(track.boundaries[1], MIN_SEGMENT_MS);
        track.resize_boundary(1, 39_500);
        assert_eq!(track.boundaries[1], 39_000);
        // Outer edges never move.
        track.resize_boundary(0, 5_000);
        assert_eq!(track.boundaries[0], 0);

        // Removal only above two sources; total stays covered with no gaps.
        assert!(track.remove(0));
        assert_eq!(track.order, vec![1, 2]);
        assert_eq!(track.boundaries.len(), 3);
        assert_eq!(*track.boundaries.first().unwrap(), 0);
        assert_eq!(*track.boundaries.last().unwrap(), 60_000);
        assert!(track.boundaries.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!track.remove(0));
    }

    /// The one thin action-routing check: the editor model assembles exactly
    /// the `CreateKillVideo` command the Render button dispatches.
    #[test]
    fn render_routes_one_create_kill_video_command() {
        let sources = sources(&[60_000, 60_000]);
        let track = initial_track(&sources);
        let correlated = sources[0].id.clone();
        let command = Command::CreateKillVideo {
            correlated_id: correlated.clone(),
            segments: track.ranges(&sources),
            width: RESOLUTIONS[9].0,
            height: RESOLUTIONS[9].1,
            fps: FPS_CHOICES[3],
            audio: audio_payload(true, Some("Player1"), &track, &sources),
        };
        match command {
            Command::CreateKillVideo {
                correlated_id,
                segments,
                width,
                height,
                fps,
                audio,
            } => {
                assert_eq!(correlated_id, correlated);
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].source, sources[0].id);
                assert_eq!(segments[1].end_ms, 60_000);
                assert_eq!((width, height, fps), (1920, 1080, 60));
                assert_eq!(audio, KillAudio::Source(1));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn reorder_playhead_mapping_and_audio_payload() {
        let sources = sources(&[60_000, 60_000, 60_000]);
        let mut track = initial_track(&sources);
        track.reorder(0, 2);
        assert_eq!(track.order, vec![1, 2, 0]);
        assert_eq!(track.segment_at(0), 0);
        assert_eq!(track.segment_at(20_000), 1);
        assert_eq!(track.segment_at(59_999), 2);
        assert_eq!(track.segment_at(60_000), 2);

        assert_eq!(
            audio_payload(false, Some("Player0"), &track, &sources),
            KillAudio::Switched
        );
        // Player0's segment is now third in montage order.
        assert_eq!(
            audio_payload(true, Some("Player0"), &track, &sources),
            KillAudio::Source(2)
        );
        // Unknown source falls back to switched audio, like the legacy -1.
        assert_eq!(
            audio_payload(true, Some("Nobody"), &track, &sources),
            KillAudio::Switched
        );
    }
}
