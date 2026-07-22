// SPDX-License-Identifier: GPL-3.0-or-later

//! The combat seek track: one `GtkDrawingArea` drawing activity spans, death
//! markers, encounter/round boundaries, the playhead, and clip handles
//! directly from `TimelineItem`s. Offsets convert to pixels at draw/hit-test
//! time; visibility preferences filter drawn items only.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;

use warcraft_recorder::domain::{
    Category, DeathMarkerVisibility, LibraryEntry, MarkerVisibility, Outcome, TimelineItem,
    TimelineKind, TimelineShape,
};

/// Marker visibility preferences, mirrored from `InterfaceSettings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkerPrefs {
    pub deaths: DeathMarkerVisibility,
    pub encounters: MarkerVisibility,
    pub rounds: MarkerVisibility,
}

/// Clip handles in milliseconds, always `0 <= start < end <= duration`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipRangeMs {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// The legacy initial clip range: current position ±15 s, clamped to media.
pub fn initial_clip_range(position_ms: u64, duration_ms: u64) -> ClipRangeMs {
    let start_ms = position_ms.saturating_sub(15_000);
    let end_ms = (position_ms + 15_000).min(duration_ms);
    ClipRangeMs {
        start_ms,
        end_ms: end_ms.max(start_ms + 1).min(duration_ms.max(1)),
    }
}

/// Move one clip handle, preserving `start < end` within the media.
pub fn drag_clip_handle(range: ClipRangeMs, start_handle: bool, to_ms: u64) -> ClipRangeMs {
    if start_handle {
        ClipRangeMs {
            start_ms: to_ms.min(range.end_ms.saturating_sub(1)),
            end_ms: range.end_ms,
        }
    } else {
        ClipRangeMs {
            start_ms: range.start_ms,
            end_ms: to_ms.max(range.start_ms + 1),
        }
    }
}

fn plain_name(name: &str) -> &str {
    name.split('-').next().unwrap_or(name)
}

/// The timeline items the preferences allow for this entry. Clips draw no
/// markers (legacy: clip metadata is lifted from the parent and bogus here).
pub fn visible_items(entry: &LibraryEntry, prefs: MarkerPrefs) -> Vec<&TimelineItem> {
    if entry.category == Category::Clip {
        return Vec::new();
    }
    let own = entry
        .player
        .as_ref()
        .map(|player| plain_name(&player.name).to_owned());
    entry
        .timeline
        .iter()
        .filter(|item| match item.kind() {
            TimelineKind::Death => match prefs.deaths {
                DeathMarkerVisibility::None => false,
                DeathMarkerVisibility::All => true,
                DeathMarkerVisibility::Own => match (&own, item.label()) {
                    (Some(own), Some(label)) => plain_name(label) == own,
                    _ => false,
                },
            },
            TimelineKind::Encounter | TimelineKind::Trash => {
                prefs.encounters == MarkerVisibility::Visible
            }
            TimelineKind::Round => prefs.rounds == MarkerVisibility::Visible,
            TimelineKind::Activity | TimelineKind::Unknown(_) => true,
        })
        .collect()
}

pub fn ms_to_x(ms: u64, duration_ms: u64, width: f64) -> f64 {
    if duration_ms == 0 {
        return 0.0;
    }
    (ms as f64 / duration_ms as f64) * width
}

pub fn x_to_ms(x: f64, duration_ms: u64, width: f64) -> u64 {
    if width <= 0.0 {
        return 0;
    }
    ((x.clamp(0.0, width) / width) * duration_ms as f64).round() as u64
}

/// `(start_ms, end_ms, label)` of one visible item, for tooltip hit-testing.
pub type LabelledItem = (u64, Option<u64>, Option<String>);

/// The hover/focus label: the nearest visible item within `radius_px` of `x`
/// (a span counts as zero distance from anywhere inside it), with its
/// timestamp.
pub fn hover_label(
    items: &[LabelledItem],
    x: f64,
    duration_ms: u64,
    width: f64,
    radius_px: f64,
) -> Option<String> {
    let mut best: Option<(f64, &LabelledItem)> = None;
    for item in items {
        let start_x = ms_to_x(item.0, duration_ms, width);
        let distance = match item.1 {
            Some(end_ms) => {
                let end_x = ms_to_x(end_ms, duration_ms, width);
                if x >= start_x && x <= end_x {
                    0.0
                } else {
                    (x - start_x).abs().min((x - end_x).abs())
                }
            }
            None => (x - start_x).abs(),
        };
        if distance <= radius_px && best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, item));
        }
    }
    best.map(|(_, (start_ms, _, label))| {
        let time = format_mm_ss(*start_ms);
        match label {
            Some(label) => format!("{label} — {time}"),
            None => time,
        }
    })
}

pub fn format_mm_ss(ms: u64) -> String {
    let total = ms / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

const TRACK_HEIGHT: i32 = 26;
const HANDLE_RADIUS: f64 = 5.0;

/// Which handle a pointer press grabbed in clip mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grab {
    Start,
    End,
    Seek,
}

pub struct Timeline {
    pub widget: gtk4::DrawingArea,
    state: Rc<State>,
}

struct State {
    duration_ms: Cell<u64>,
    position_ms: Cell<u64>,
    items: RefCell<Vec<OwnedItem>>,
    clip: Cell<Option<ClipRangeMs>>,
    grab: Cell<Option<Grab>>,
    #[allow(clippy::type_complexity)]
    on_seek: RefCell<Option<Box<dyn Fn(u64)>>>,
    labels: RefCell<Vec<LabelledItem>>,
}

/// A draw-ready copy of one visible item, so redraws never re-filter.
struct OwnedItem {
    span: bool,
    death: bool,
    start_ms: u64,
    end_ms: u64,
    win: bool,
}

impl Timeline {
    pub fn new() -> Self {
        let widget = gtk4::DrawingArea::new();
        widget.set_content_height(TRACK_HEIGHT);
        widget.set_hexpand(true);
        widget.set_focusable(true);
        widget.add_css_class("wr-timeline");
        widget.set_has_tooltip(true);
        let state = Rc::new(State {
            duration_ms: Cell::new(0),
            position_ms: Cell::new(0),
            items: RefCell::new(Vec::new()),
            clip: Cell::new(None),
            grab: Cell::new(None),
            on_seek: RefCell::new(None),
            labels: RefCell::new(Vec::new()),
        });
        let timeline = Self { widget, state };
        timeline.connect_draw();
        timeline.connect_pointer();
        timeline.connect_tooltip_and_keys();
        timeline
    }

    pub fn connect_seek(&self, on_seek: impl Fn(u64) + 'static) {
        *self.state.on_seek.borrow_mut() = Some(Box::new(on_seek));
    }

    /// Replace the drawn markers from the entry and preferences.
    pub fn set_entry(&self, entry: Option<&LibraryEntry>, prefs: MarkerPrefs) {
        let mut items = self.state.items.borrow_mut();
        let mut labels = self.state.labels.borrow_mut();
        items.clear();
        labels.clear();
        if let Some(entry) = entry {
            self.state.duration_ms.set(entry.duration_ms.max(1));
            for item in visible_items(entry, prefs) {
                items.push(OwnedItem {
                    span: item.shape() == TimelineShape::Span,
                    death: item.kind() == &TimelineKind::Death,
                    start_ms: item.start_ms(),
                    end_ms: item.end_ms().unwrap_or(item.start_ms()),
                    win: matches!(item.outcome(), Some(Outcome::Win | Outcome::Complete)),
                });
                labels.push((
                    item.start_ms(),
                    item.end_ms(),
                    item.label().map(str::to_owned),
                ));
            }
        } else {
            self.state.duration_ms.set(0);
        }
        drop(items);
        drop(labels);
        self.widget.queue_draw();
    }

    pub fn set_position(&self, position_ms: u64) {
        if self.state.position_ms.replace(position_ms) != position_ms {
            self.widget.queue_draw();
        }
    }

    pub fn set_clip(&self, clip: Option<ClipRangeMs>) {
        self.state.clip.set(clip);
        self.widget.queue_draw();
    }

    pub fn clip(&self) -> Option<ClipRangeMs> {
        self.state.clip.get()
    }

    fn connect_draw(&self) {
        let state = Rc::clone(&self.state);
        self.widget.set_draw_func(move |_, cr, width, height| {
            let width = f64::from(width);
            let height = f64::from(height);
            let duration = state.duration_ms.get();
            let mid = height / 2.0;

            // Rail: the muted activity track (legacy #5A2F27 tone).
            cr.set_source_rgba(0.35, 0.19, 0.15, 1.0);
            rounded_bar(cr, 0.0, mid - 3.0, width, 6.0);
            let _ = cr.fill();
            if duration == 0 {
                return;
            }

            // Spans (encounter/trash/round/activity) in stable outcome colors.
            for item in state.items.borrow().iter().filter(|item| item.span) {
                let start = ms_to_x(item.start_ms, duration, width);
                let end = ms_to_x(item.end_ms, duration, width).max(start + 2.0);
                if item.win {
                    cr.set_source_rgba(0.12, 1.0, 0.0, 0.55);
                } else {
                    cr.set_source_rgba(1.0, 0.0, 0.0, 0.55);
                }
                rounded_bar(cr, start, mid - 3.0, end - start, 6.0);
                let _ = cr.fill();
            }

            // Elapsed track up to the playhead (accent #bb4220).
            let played = ms_to_x(state.position_ms.get(), duration, width);
            cr.set_source_rgba(0.73, 0.26, 0.13, 0.9);
            rounded_bar(cr, 0.0, mid - 1.5, played, 3.0);
            let _ = cr.fill();

            // Point markers: deaths as diamonds, other points as ticks.
            for item in state.items.borrow().iter().filter(|item| !item.span) {
                let x = ms_to_x(item.start_ms, duration, width);
                if item.win {
                    cr.set_source_rgba(0.12, 1.0, 0.0, 1.0);
                } else {
                    cr.set_source_rgba(1.0, 0.0, 0.0, 1.0);
                }
                if item.death {
                    cr.move_to(x, mid - 6.0);
                    cr.line_to(x + 4.0, mid);
                    cr.line_to(x, mid + 6.0);
                    cr.line_to(x - 4.0, mid);
                    cr.close_path();
                    let _ = cr.fill();
                } else {
                    cr.rectangle(x - 1.0, mid - 7.0, 2.0, 14.0);
                    let _ = cr.fill();
                }
            }

            // Clip range and handles.
            if let Some(clip) = state.clip.get() {
                let start = ms_to_x(clip.start_ms, duration, width);
                let end = ms_to_x(clip.end_ms, duration, width);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
                cr.rectangle(start, 1.0, end - start, height - 2.0);
                let _ = cr.fill();
                cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                for x in [start, end] {
                    cr.arc(x, mid, HANDLE_RADIUS, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                }
            }

            // Playhead.
            cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            cr.arc(played, mid, 5.0, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        });
    }

    fn connect_pointer(&self) {
        let drag = gtk4::GestureDrag::new();
        let state = Rc::clone(&self.state);
        let widget = self.widget.clone();
        drag.connect_drag_begin(move |_, x, _| {
            let duration = state.duration_ms.get();
            if duration == 0 {
                return;
            }
            let width = f64::from(widget.width());
            let grab = match state.clip.get() {
                Some(clip) => {
                    let start_x = ms_to_x(clip.start_ms, duration, width);
                    let end_x = ms_to_x(clip.end_ms, duration, width);
                    if (x - start_x).abs() <= HANDLE_RADIUS * 2.0 {
                        Grab::Start
                    } else if (x - end_x).abs() <= HANDLE_RADIUS * 2.0 {
                        Grab::End
                    } else {
                        Grab::Seek
                    }
                }
                None => Grab::Seek,
            };
            state.grab.set(Some(grab));
            apply_pointer(&state, &widget, x);
        });
        let state = Rc::clone(&self.state);
        let widget = self.widget.clone();
        drag.connect_drag_update(move |gesture, offset_x, _| {
            if let Some((start_x, _)) = gesture.start_point() {
                apply_pointer(&state, &widget, start_x + offset_x);
            }
        });
        let state = Rc::clone(&self.state);
        drag.connect_drag_end(move |_, _, _| state.grab.set(None));
        self.widget.add_controller(drag);
    }

    fn connect_tooltip_and_keys(&self) {
        // Hover/focus label: nearest marker within 8 px, else the timestamp.
        let state = Rc::clone(&self.state);
        self.widget
            .connect_query_tooltip(move |widget, x, _, keyboard, tooltip| {
                let duration_ms = state.duration_ms.get();
                if duration_ms == 0 {
                    return false;
                }
                let width = f64::from(widget.width());
                let x = if keyboard {
                    ms_to_x(state.position_ms.get(), duration_ms, width)
                } else {
                    f64::from(x)
                };
                let text = hover_label(&state.labels.borrow(), x, duration_ms, width, 8.0)
                    .unwrap_or_else(|| format_mm_ss(x_to_ms(x, duration_ms, width)));
                tooltip.set_text(Some(&text));
                true
            });

        // Keyboard seeking on the focused track: Left/Right nudge 5 s, matching
        // the player shortcut interval.
        let key = gtk4::EventControllerKey::new();
        let state = Rc::clone(&self.state);
        key.connect_key_pressed(move |_, keyval, _, _| {
            let duration = state.duration_ms.get();
            if duration == 0 {
                return gtk4::glib::Propagation::Proceed;
            }
            let position = state.position_ms.get();
            let target = match keyval {
                gtk4::gdk::Key::Left => position.saturating_sub(5_000),
                gtk4::gdk::Key::Right => (position + 5_000).min(duration),
                _ => return gtk4::glib::Propagation::Proceed,
            };
            if let Some(on_seek) = state.on_seek.borrow().as_ref() {
                on_seek(target);
            }
            gtk4::glib::Propagation::Stop
        });
        self.widget.add_controller(key);
    }
}

/// Route a pointer x according to the active grab: seek or move a handle.
fn apply_pointer(state: &Rc<State>, widget: &gtk4::DrawingArea, x: f64) {
    let duration = state.duration_ms.get();
    if duration == 0 {
        return;
    }
    let ms = x_to_ms(x, duration, f64::from(widget.width()));
    match state.grab.get() {
        Some(Grab::Start) | Some(Grab::End) => {
            if let Some(clip) = state.clip.get() {
                let start = state.grab.get() == Some(Grab::Start);
                state.clip.set(Some(drag_clip_handle(clip, start, ms)));
                widget.queue_draw();
            }
        }
        _ => {
            if let Some(on_seek) = state.on_seek.borrow().as_ref() {
                on_seek(ms);
            }
        }
    }
}

fn rounded_bar(cr: &gtk4::cairo::Context, x: f64, y: f64, width: f64, height: f64) {
    let radius = (height / 2.0).min(width / 2.0).max(0.0);
    if width <= 0.0 {
        return;
    }
    cr.new_sub_path();
    cr.arc(
        x + width - radius,
        y + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        x + radius,
        y + radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        3.0 * std::f64::consts::FRAC_PI_2,
    );
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use warcraft_recorder::domain::{
        ActivityDetails, Codec, GameFlavor, MediaFacts, PlayerSummary, RecordingId,
    };

    fn entry_with(category: Category, timeline: Vec<TimelineItem>) -> LibraryEntry {
        LibraryEntry {
            id: RecordingId::new(),
            media_path: PathBuf::from("/rec/v.mkv"),
            sidecar_path: PathBuf::from("/rec/v.json"),
            category,
            flavor: GameFlavor::Retail,
            title: "T".to_owned(),
            start_unix_ms: 0,
            duration_ms: 120_000,
            outcome: Outcome::Win,
            protected: false,
            tag: None,
            activity_hash: None,
            player: Some(PlayerSummary {
                name: "Alice-Realm".to_owned(),
                realm: None,
                guid: None,
                class_id: None,
                spec_id: None,
            }),
            combatants: Vec::new(),
            details: ActivityDetails::Manual,
            timeline,
            media: MediaFacts {
                fps: None,
                width: None,
                height: None,
                codec: Some(Codec::H264),
            },
        }
    }

    fn death(name: &str, at: u64) -> TimelineItem {
        TimelineItem::point(
            TimelineKind::Death,
            at,
            Some(name.to_owned()),
            Some(Outcome::Loss),
            None,
        )
    }

    #[test]
    fn visibility_filters_deaths_rounds_and_encounters_without_mutating_data() {
        let entry = entry_with(
            Category::MythicPlus,
            vec![
                death("Alice", 1_000),
                death("Bob", 2_000),
                TimelineItem::span(TimelineKind::Encounter, 0, 10_000, None, None, None).unwrap(),
                TimelineItem::span(TimelineKind::Round, 0, 5_000, None, None, None).unwrap(),
            ],
        );
        let all = MarkerPrefs {
            deaths: DeathMarkerVisibility::All,
            encounters: MarkerVisibility::Visible,
            rounds: MarkerVisibility::Visible,
        };
        assert_eq!(visible_items(&entry, all).len(), 4);

        let own_only = MarkerPrefs {
            deaths: DeathMarkerVisibility::Own,
            encounters: MarkerVisibility::Hidden,
            rounds: MarkerVisibility::Hidden,
        };
        let visible = visible_items(&entry, own_only);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].label(), Some("Alice"));
        // Filtering is presentation only.
        assert_eq!(entry.timeline.len(), 4);

        // Clips never draw markers.
        let clip = entry_with(Category::Clip, vec![death("Alice", 1_000)]);
        assert!(visible_items(&clip, all).is_empty());
    }

    #[test]
    fn pixel_mapping_round_trips_and_hover_prefers_the_nearest_item() {
        assert_eq!(
            x_to_ms(ms_to_x(30_000, 120_000, 600.0), 120_000, 600.0),
            30_000
        );
        assert_eq!(x_to_ms(-10.0, 120_000, 600.0), 0);
        assert_eq!(x_to_ms(999.0, 120_000, 600.0), 120_000);

        let items = vec![
            (30_000, None, Some("Alice".to_owned())),
            (60_000, None, Some("Bob".to_owned())),
        ];
        let x = ms_to_x(31_000, 120_000, 600.0);
        assert_eq!(
            hover_label(&items, x, 120_000, 600.0, 8.0).as_deref(),
            Some("Alice — 0:30")
        );
        // A span reports zero distance from anywhere inside it.
        let items = vec![(0, Some(120_000), Some("Round 1".to_owned()))];
        assert_eq!(
            hover_label(&items, 300.0, 120_000, 600.0, 8.0).as_deref(),
            Some("Round 1 — 0:00")
        );
    }

    #[test]
    fn clip_bounds_stay_ordered_inside_the_media() {
        let initial = initial_clip_range(5_000, 120_000);
        assert_eq!(initial.start_ms, 0);
        assert_eq!(initial.end_ms, 20_000);
        let tail = initial_clip_range(115_000, 120_000);
        assert_eq!(tail.end_ms, 120_000);

        let range = ClipRangeMs {
            start_ms: 10_000,
            end_ms: 20_000,
        };
        // The start handle cannot cross the end handle, and vice versa.
        assert_eq!(drag_clip_handle(range, true, 25_000).start_ms, 19_999);
        assert_eq!(drag_clip_handle(range, false, 5_000).end_ms, 10_001);
        assert_eq!(drag_clip_handle(range, true, 0).start_ms, 0);
    }
}
