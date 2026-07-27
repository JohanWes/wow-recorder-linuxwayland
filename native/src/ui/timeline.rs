// SPDX-License-Identifier: GPL-3.0-or-later

//! A compact three-lane combat ruler: neutral activity spans, independently
//! typed Bloodlust spans, and point events. Prepared items retain their domain
//! kind and lane so drawing and x/y hit-testing never infer semantics from
//! labels. The warm ruler is reserved for elapsed playback and the playhead.

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

/// The initial clip range: current position ±15 s, clamped to the media.
///
/// Empty media has no valid ordered range, so it is represented as `0..0`;
/// callers only expose clip mode when a non-empty recording is loaded.
pub fn initial_clip_range(position_ms: u64, duration_ms: u64) -> ClipRangeMs {
    if duration_ms == 0 {
        return ClipRangeMs {
            start_ms: 0,
            end_ms: 0,
        };
    }

    let position_ms = position_ms.min(duration_ms);
    let mut start_ms = position_ms.saturating_sub(15_000);
    let mut end_ms = position_ms.saturating_add(15_000).min(duration_ms);
    if start_ms == end_ms {
        if end_ms < duration_ms {
            end_ms += 1;
        } else {
            start_ms = start_ms.saturating_sub(1);
        }
    }
    ClipRangeMs { start_ms, end_ms }
}

/// Move one clip handle, preserving `start < end`.
///
/// Pointer input is clamped to media duration before reaching this helper.
pub fn drag_clip_handle(range: ClipRangeMs, start_handle: bool, to_ms: u64) -> ClipRangeMs {
    if start_handle {
        ClipRangeMs {
            start_ms: to_ms.min(range.end_ms.saturating_sub(1)),
            end_ms: range.end_ms,
        }
    } else {
        ClipRangeMs {
            start_ms: range.start_ms,
            end_ms: to_ms.max(range.start_ms.saturating_add(1)),
        }
    }
}

fn clamp_clip_range(range: ClipRangeMs, duration_ms: u64) -> Option<ClipRangeMs> {
    if duration_ms == 0 {
        return None;
    }
    let start_ms = range.start_ms.min(duration_ms - 1);
    Some(ClipRangeMs {
        start_ms,
        end_ms: range.end_ms.clamp(start_ms + 1, duration_ms),
    })
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
            TimelineKind::Bloodlust => true,
            TimelineKind::Encounter | TimelineKind::Trash => {
                prefs.encounters == MarkerVisibility::Visible
            }
            TimelineKind::Round => prefs.rounds == MarkerVisibility::Visible,
            TimelineKind::Activity | TimelineKind::Unknown(_) => true,
        })
        .collect()
}

pub fn ms_to_x(ms: u64, duration_ms: u64, width: f64) -> f64 {
    if duration_ms == 0 || width <= 0.0 {
        return 0.0;
    }
    (ms.min(duration_ms) as f64 / duration_ms as f64) * width
}

pub fn x_to_ms(x: f64, duration_ms: u64, width: f64) -> u64 {
    if duration_ms == 0 || width <= 0.0 {
        return 0;
    }
    ((x.clamp(0.0, width) / width) * duration_ms as f64).round() as u64
}

fn lane_at_y(y: f64) -> Option<Lane> {
    if !(READOUT_HEIGHT..=f64::from(TRACK_HEIGHT)).contains(&y) {
        None
    } else if y < ACTIVITY_BAND_BOTTOM {
        Some(Lane::Activity)
    } else if y < BLOODLUST_BAND_BOTTOM {
        Some(Lane::Bloodlust)
    } else {
        Some(Lane::Events)
    }
}

fn point_priority(kind: &TimelineKind) -> u8 {
    if kind == &TimelineKind::Death { 0 } else { 1 }
}

/// Select an item on the pointer's lane. Point events win before containing
/// spans; ties choose the nearest point or the most specific (shortest) span.
fn hit_test(items: &[OwnedItem], x: f64, y: f64, duration_ms: u64, width: f64) -> Option<usize> {
    let lane = lane_at_y(y)?;

    let mut best_point: Option<(usize, f64, u8)> = None;
    for (index, item) in items.iter().enumerate() {
        if item.lane != lane || item.shape != TimelineShape::Point {
            continue;
        }
        let distance = (x - ms_to_x(item.start_ms, duration_ms, width)).abs();
        if distance > POINT_HIT_RADIUS {
            continue;
        }
        let priority = point_priority(&item.kind);
        if best_point.is_none_or(|(_, best_distance, best_priority)| {
            distance < best_distance || (distance == best_distance && priority < best_priority)
        }) {
            best_point = Some((index, distance, priority));
        }
    }
    if let Some((index, _, _)) = best_point {
        return Some(index);
    }

    let mut best_span: Option<(usize, f64, u64)> = None;
    for (index, item) in items.iter().enumerate() {
        if item.lane != lane || item.shape != TimelineShape::Span {
            continue;
        }
        let start_x = ms_to_x(item.start_ms, duration_ms, width);
        let end_x = ms_to_x(item.end_ms, duration_ms, width);
        let distance = if x < start_x {
            start_x - x
        } else if x > end_x {
            x - end_x
        } else {
            0.0
        };
        if distance > SPAN_ENDPOINT_HIT_RADIUS {
            continue;
        }
        let duration = item.end_ms.saturating_sub(item.start_ms);
        if best_span.is_none_or(|(_, best_distance, best_duration)| {
            distance < best_distance || (distance == best_distance && duration < best_duration)
        }) {
            best_span = Some((index, distance, duration));
        }
    }
    best_span.map(|(index, _, _)| index)
}

fn kind_title(kind: &TimelineKind, shape: TimelineShape) -> &str {
    match kind {
        TimelineKind::Death => "Death",
        TimelineKind::Bloodlust => "Bloodlust",
        TimelineKind::Encounter if shape == TimelineShape::Point => "Encounter boundary",
        TimelineKind::Encounter => "Encounter",
        TimelineKind::Trash if shape == TimelineShape::Point => "Trash boundary",
        TimelineKind::Trash => "Trash",
        TimelineKind::Round if shape == TimelineShape::Point => "Round boundary",
        TimelineKind::Round => "Round",
        TimelineKind::Activity if shape == TimelineShape::Point => "Activity boundary",
        TimelineKind::Activity => "Activity",
        TimelineKind::Unknown(name) => name,
    }
}

fn item_readout(item: &TimelineItem) -> String {
    let kind = kind_title(item.kind(), item.shape());
    let title = match item.label().filter(|label| !label.trim().is_empty()) {
        Some(label) if label.eq_ignore_ascii_case(kind) => label.to_owned(),
        Some(label) => format!("{kind}: {label}"),
        None => kind.to_owned(),
    };
    match item.end_ms().filter(|end_ms| *end_ms > item.start_ms()) {
        Some(end_ms) => format!(
            "{title} · {}–{}",
            format_mm_ss(item.start_ms()),
            format_mm_ss(end_ms)
        ),
        None => format!("{title} · {}", format_mm_ss(item.start_ms())),
    }
}

fn pointer_readout(items: &[OwnedItem], x: f64, y: f64, duration_ms: u64, width: f64) -> String {
    hit_test(items, x, y, duration_ms, width)
        .and_then(|index| items.get(index))
        .map(|item| item.readout.clone())
        .unwrap_or_else(|| format_mm_ss(x_to_ms(x, duration_ms, width)))
}

pub fn format_mm_ss(ms: u64) -> String {
    let total = ms / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

fn playhead_readout(position_ms: u64, duration_ms: u64) -> String {
    if duration_ms == 0 {
        "No recording loaded".to_owned()
    } else {
        format!(
            "{} / {}",
            format_mm_ss(position_ms.min(duration_ms)),
            format_mm_ss(duration_ms)
        )
    }
}

// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

const TRACK_HEIGHT: i32 = 58;
const READOUT_HEIGHT: f64 = 16.0;
const READOUT_PADDING_X: f64 = 4.0;
const READOUT_BASELINE_Y: f64 = 11.0;
const READOUT_FONT_SIZE: f64 = 10.0;

const ACTIVITY_BAND_BOTTOM: f64 = 28.0;
const ACTIVITY_BAR_TOP: f64 = 19.0;
const ACTIVITY_BAR_HEIGHT: f64 = 6.0;
const BLOODLUST_BAND_BOTTOM: f64 = 38.0;
const BLOODLUST_BAR_TOP: f64 = 30.0;
const BLOODLUST_BAR_HEIGHT: f64 = 5.0;
const EVENT_TICK_TOP: f64 = 40.0;
const EVENT_TICK_BOTTOM: f64 = 48.0;
const EVENT_STONE_MID: f64 = 50.0;
const EVENT_RAIL_Y: f64 = 44.0;

const PROGRESS_Y: f64 = 56.0;
const PROGRESS_HEIGHT: f64 = 2.0;
const PLAYHEAD_RADIUS: f64 = 4.0;
const PLAYHEAD_CENTER_Y: f64 = PROGRESS_Y - PLAYHEAD_RADIUS / 2.0;
const MIN_SPAN_WIDTH: f64 = 2.0;
const POINT_HIT_RADIUS: f64 = 8.0;
const SPAN_ENDPOINT_HIT_RADIUS: f64 = 6.0;
const HANDLE_HIT_SIZE: f64 = 24.0;
const BRACKET_ARM: f64 = 5.0;
const BRACKET_LINE_WIDTH: f64 = 2.0;

const ACCESSIBLE_LABEL: &str = "Combat timeline";
const ACCESSIBLE_DESCRIPTION: &str = "Activity, Bloodlust, and event lanes. Left and Right seek five seconds; drag clip brackets to adjust a clip.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Activity,
    Bloodlust,
    Events,
}

impl Lane {
    fn for_item(kind: &TimelineKind, shape: TimelineShape) -> Self {
        match (kind, shape) {
            (TimelineKind::Death, _) | (_, TimelineShape::Point) => Self::Events,
            (TimelineKind::Bloodlust, TimelineShape::Span) => Self::Bloodlust,
            (_, TimelineShape::Span) => Self::Activity,
        }
    }
}

/// Which handle a pointer press grabbed in clip mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pointer_active: Cell<bool>,
    readout: RefCell<String>,
    #[allow(clippy::type_complexity)]
    on_seek: RefCell<Option<Box<dyn Fn(u64)>>>,
}

/// Draw-ready semantic data. Filtering, ownership, and human-readable
/// formatting happen when the entry changes, never inside the draw callback.
#[derive(Clone, Debug)]
struct OwnedItem {
    shape: TimelineShape,
    lane: Lane,
    kind: TimelineKind,
    start_ms: u64,
    end_ms: u64,
    outcome: Option<Outcome>,
    readout: String,
}

impl OwnedItem {
    fn from_item(item: &TimelineItem) -> Self {
        Self {
            shape: item.shape(),
            lane: Lane::for_item(item.kind(), item.shape()),
            kind: item.kind().clone(),
            start_ms: item.start_ms(),
            end_ms: item.end_ms().unwrap_or(item.start_ms()),
            outcome: item.outcome(),
            readout: item_readout(item),
        }
    }
}

impl Timeline {
    pub fn new() -> Self {
        let widget = gtk4::DrawingArea::new();
        widget.set_content_height(TRACK_HEIGHT);
        widget.set_hexpand(true);
        widget.set_focusable(true);
        widget.add_css_class("wr-timeline");
        widget.set_has_tooltip(true);
        widget.set_accessible_role(gtk4::AccessibleRole::Slider);
        widget.update_property(&[
            gtk4::accessible::Property::Label(ACCESSIBLE_LABEL),
            gtk4::accessible::Property::Description(ACCESSIBLE_DESCRIPTION),
            gtk4::accessible::Property::ValueMin(0.0),
            gtk4::accessible::Property::ValueMax(0.0),
            gtk4::accessible::Property::ValueNow(0.0),
            gtk4::accessible::Property::ValueText("No recording loaded"),
        ]);

        let state = Rc::new(State {
            duration_ms: Cell::new(0),
            position_ms: Cell::new(0),
            items: RefCell::new(Vec::new()),
            clip: Cell::new(None),
            grab: Cell::new(None),
            pointer_active: Cell::new(false),
            readout: RefCell::new("No recording loaded".to_owned()),
            on_seek: RefCell::new(None),
        });
        let timeline = Self { widget, state };
        timeline
            .widget
            .connect_has_focus_notify(|widget| widget.queue_draw());
        timeline.connect_draw();
        timeline.connect_pointer();
        timeline.connect_readout_tooltip_and_keys();
        timeline
    }

    pub fn connect_seek(&self, on_seek: impl Fn(u64) + 'static) {
        *self.state.on_seek.borrow_mut() = Some(Box::new(on_seek));
    }

    /// Replace the prepared marker data from the entry and preferences.
    pub fn set_entry(&self, entry: Option<&LibraryEntry>, prefs: MarkerPrefs) {
        let duration_ms = entry.map_or(0, |entry| entry.duration_ms);
        let prepared = entry
            .map(|entry| {
                visible_items(entry, prefs)
                    .into_iter()
                    .map(OwnedItem::from_item)
                    .collect()
            })
            .unwrap_or_default();

        self.state.duration_ms.set(duration_ms);
        self.state
            .position_ms
            .set(self.state.position_ms.get().min(duration_ms));
        self.state.items.replace(prepared);
        self.state.clip.set(
            self.state
                .clip
                .get()
                .and_then(|range| clamp_clip_range(range, duration_ms)),
        );
        self.state.grab.set(None);
        self.state.pointer_active.set(false);
        replace_readout_with_playhead(&self.state);
        update_accessibility(&self.widget, &self.state);
        self.widget.queue_draw();
    }

    pub fn set_position(&self, position_ms: u64) {
        let duration_ms = self.state.duration_ms.get();
        let position_ms = position_ms.min(duration_ms);
        let position_changed = self.state.position_ms.replace(position_ms) != position_ms;
        let readout_changed =
            !self.state.pointer_active.get() && replace_readout_with_playhead(&self.state);
        update_accessibility(&self.widget, &self.state);
        if position_changed || readout_changed {
            self.widget.queue_draw();
        }
    }

    pub fn set_clip(&self, clip: Option<ClipRangeMs>) {
        let clip = clip.and_then(|range| clamp_clip_range(range, self.state.duration_ms.get()));
        if self.state.clip.replace(clip) != clip {
            self.widget.queue_draw();
        }
    }

    pub fn clip(&self) -> Option<ClipRangeMs> {
        self.state.clip.get()
    }

    fn connect_draw(&self) {
        let state = Rc::clone(&self.state);
        self.widget
            .set_draw_func(move |widget, cr, width, _height| {
                let width = f64::from(width);
                let colors = TimelineColors::fixed();

                // Three quiet rails establish the activity, Bloodlust, and event
                // lanes without adding a legend or a fourth visual layer.
                set_source_color(cr, &colors.rail);
                rounded_bar(cr, 0.0, ACTIVITY_BAR_TOP, width, ACTIVITY_BAR_HEIGHT);
                let _ = cr.fill();
                rounded_bar(cr, 0.0, BLOODLUST_BAR_TOP, width, BLOODLUST_BAR_HEIGHT);
                let _ = cr.fill();
                rounded_bar(cr, 0.0, EVENT_RAIL_Y, width, EVENT_RAIL_HEIGHT);
                let _ = cr.fill();

                let duration_ms = state.duration_ms.get();
                if duration_ms > 0 {
                    if let Some(clip) = state.clip.get() {
                        let start_x = ms_to_x(clip.start_ms, duration_ms, width);
                        let end_x = ms_to_x(clip.end_ms, duration_ms, width);
                        set_source_color(cr, &colors.clip_fill);
                        cr.rectangle(
                            start_x,
                            READOUT_HEIGHT,
                            (end_x - start_x).max(0.0),
                            PROGRESS_Y - READOUT_HEIGHT,
                        );
                        let _ = cr.fill();
                    }

                    let items = state.items.borrow();
                    for item in items.iter().filter(|item| {
                        item.shape == TimelineShape::Span && item.lane == Lane::Activity
                    }) {
                        let (start_x, end_x) =
                            span_x_bounds(item.start_ms, item.end_ms, duration_ms, width);
                        set_source_color(cr, &colors.activity);
                        rounded_bar(
                            cr,
                            start_x,
                            ACTIVITY_BAR_TOP,
                            end_x - start_x,
                            ACTIVITY_BAR_HEIGHT,
                        );
                        let _ = cr.fill();
                        draw_outcome_cap(
                            cr,
                            start_x,
                            end_x,
                            ACTIVITY_BAR_TOP,
                            ACTIVITY_BAR_HEIGHT,
                            item.outcome,
                            &colors,
                        );
                    }

                    for item in items.iter().filter(|item| {
                        item.shape == TimelineShape::Span && item.lane == Lane::Bloodlust
                    }) {
                        let (start_x, end_x) =
                            span_x_bounds(item.start_ms, item.end_ms, duration_ms, width);
                        set_source_color(cr, &colors.bloodlust);
                        rounded_bar(
                            cr,
                            start_x,
                            BLOODLUST_BAR_TOP,
                            end_x - start_x,
                            BLOODLUST_BAR_HEIGHT,
                        );
                        let _ = cr.fill();
                    }

                    for item in items
                        .iter()
                        .filter(|item| item.shape == TimelineShape::Point)
                    {
                        let x = edge_safe_x(
                            ms_to_x(item.start_ms, duration_ms, width),
                            width,
                            EVENT_MARKER_HALF_WIDTH,
                        );
                        if item.kind == TimelineKind::Death {
                            draw_death_marker(cr, x, &colors);
                        } else {
                            let color =
                                outcome_color(item.outcome, &colors).unwrap_or(&colors.event);
                            set_source_color(cr, color);
                            cr.rectangle(
                                x - EVENT_TICK_WIDTH / 2.0,
                                EVENT_TICK_TOP,
                                EVENT_TICK_WIDTH,
                                EVENT_TICK_BOTTOM - EVENT_TICK_TOP,
                            );
                            let _ = cr.fill();
                        }
                    }
                    drop(items);

                    let played_x =
                        ms_to_x(state.position_ms.get(), duration_ms, width).clamp(0.0, width);
                    set_source_color(cr, &colors.progress);
                    rounded_bar(cr, 0.0, PROGRESS_Y, played_x, PROGRESS_HEIGHT);
                    let _ = cr.fill();

                    if let Some(clip) = state.clip.get() {
                        let start_x = ms_to_x(clip.start_ms, duration_ms, width);
                        let end_x = ms_to_x(clip.end_ms, duration_ms, width);
                        set_source_color(cr, &colors.clip_bracket);
                        draw_clip_bracket(cr, start_x, true);
                        draw_clip_bracket(cr, end_x, false);
                    }

                    // The playhead is warm like elapsed progress, not another
                    // event color. Its center stays visibly inside both edges.
                    let playhead_x = edge_safe_x(played_x, width, PLAYHEAD_RADIUS);
                    set_source_color(cr, &colors.progress);
                    cr.set_line_width(PLAYHEAD_LINE_WIDTH);
                    cr.move_to(playhead_x, READOUT_HEIGHT);
                    cr.line_to(playhead_x, PROGRESS_Y);
                    let _ = cr.stroke();
                    cr.arc(
                        playhead_x,
                        PLAYHEAD_CENTER_Y,
                        PLAYHEAD_RADIUS,
                        0.0,
                        std::f64::consts::TAU,
                    );
                    let _ = cr.fill();
                }

                if state.pointer_active.get() || widget.has_focus() {
                    let readout = state.readout.borrow();
                    draw_readout(cr, width, readout.as_str(), &colors.readout);
                }
            });
    }

    fn connect_pointer(&self) {
        let drag = gtk4::GestureDrag::new();
        let state = Rc::clone(&self.state);
        let widget = self.widget.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let duration_ms = state.duration_ms.get();
            if duration_ms == 0 {
                return;
            }
            let grab = grab_at(
                state.clip.get(),
                x,
                y,
                duration_ms,
                f64::from(widget.width()),
                f64::from(widget.height()),
            );
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

    fn connect_readout_tooltip_and_keys(&self) {
        let motion = gtk4::EventControllerMotion::new();
        let state = Rc::clone(&self.state);
        let widget = self.widget.clone();
        motion.connect_motion(move |_, x, y| {
            let duration_ms = state.duration_ms.get();
            if duration_ms == 0 {
                return;
            }
            state.pointer_active.set(true);
            let text = {
                let items = state.items.borrow();
                pointer_readout(&items, x, y, duration_ms, f64::from(widget.width()))
            };
            if replace_readout(&state, text) {
                widget.queue_draw();
            }
            update_accessibility(&widget, &state);
        });

        let state = Rc::clone(&self.state);
        let widget = self.widget.clone();
        motion.connect_leave(move |_| {
            state.pointer_active.set(false);
            if replace_readout_with_playhead(&state) {
                widget.queue_draw();
            }
            update_accessibility(&widget, &state);
        });
        self.widget.add_controller(motion);

        let state = Rc::clone(&self.state);
        self.widget
            .connect_query_tooltip(move |widget, x, y, keyboard, tooltip| {
                let duration_ms = state.duration_ms.get();
                if duration_ms == 0 {
                    return false;
                }
                let text = if keyboard {
                    playhead_readout(state.position_ms.get(), duration_ms)
                } else {
                    pointer_readout(
                        &state.items.borrow(),
                        f64::from(x),
                        f64::from(y),
                        duration_ms,
                        f64::from(widget.width()),
                    )
                };
                tooltip.set_text(Some(&text));
                true
            });

        // Focused Left/Right seeking stays at the player-wide five-second
        // interval; empty item lists never disable seeking.
        let key = gtk4::EventControllerKey::new();
        let state = Rc::clone(&self.state);
        key.connect_key_pressed(move |_, keyval, _, _| {
            let duration_ms = state.duration_ms.get();
            if duration_ms == 0 {
                return gtk4::glib::Propagation::Proceed;
            }
            let target = match keyval {
                gtk4::gdk::Key::Left => {
                    keyboard_seek_target(state.position_ms.get(), duration_ms, false)
                }
                gtk4::gdk::Key::Right => {
                    keyboard_seek_target(state.position_ms.get(), duration_ms, true)
                }
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

const EVENT_RAIL_HEIGHT: f64 = 2.0;
const EVENT_TICK_WIDTH: f64 = 2.0;
const EVENT_MARKER_HALF_WIDTH: f64 = 5.0;
const OUTCOME_CAP_WIDTH: f64 = 2.0;
const OUTCOME_CAP_OVERHANG: f64 = 1.0;
const PLAYHEAD_LINE_WIDTH: f64 = 1.5;
const DEATH_STONE_HALF_WIDTH: f64 = 4.0;
const DEATH_STONE_TOP_OFFSET: f64 = 6.0;
const DEATH_STONE_BASE_OFFSET: f64 = 6.0;
const DEATH_STONE_SHOULDER_OFFSET: f64 = 1.0;
const DEATH_DETAIL_LINE_WIDTH: f64 = 1.0;
const DEATH_CROSS_HALF_WIDTH: f64 = 2.0;

struct TimelineColors {
    rail: gtk4::gdk::RGBA,
    activity: gtk4::gdk::RGBA,
    bloodlust: gtk4::gdk::RGBA,
    event: gtk4::gdk::RGBA,
    death_tick: gtk4::gdk::RGBA,
    death_stone: gtk4::gdk::RGBA,
    death_detail: gtk4::gdk::RGBA,
    readout: gtk4::gdk::RGBA,
    clip_fill: gtk4::gdk::RGBA,
    clip_bracket: gtk4::gdk::RGBA,
    progress: gtk4::gdk::RGBA,
    success: gtk4::gdk::RGBA,
    error: gtk4::gdk::RGBA,
}

impl TimelineColors {
    /// Fixed player colors. The player is black in both system themes, so
    /// resolving CSS tokens on every draw only adds deprecated lookups and
    /// needless work. `activity` matches the library's CSS activity token.
    fn fixed() -> Self {
        Self {
            rail: rgba(0x30, 0x38, 0x41, 1.0),
            activity: rgba(0x63, 0x78, 0x8a, 1.0),
            bloodlust: rgba(0x8a, 0x5a, 0xa8, 1.0),
            event: rgba(0xa8, 0xb2, 0xbc, 1.0),
            death_tick: rgba(0xff, 0xff, 0xff, 1.0),
            death_stone: rgba(0x74, 0x6a, 0x80, 1.0),
            death_detail: rgba(0x30, 0x2a, 0x35, 1.0),
            readout: rgba(0xc2, 0xca, 0xd2, 1.0),
            clip_fill: rgba(0xe8, 0xec, 0xf0, 0.16),
            clip_bracket: rgba(0xe6, 0xe9, 0xec, 1.0),
            progress: rgba(0xd9, 0x6a, 0x4a, 1.0),
            success: rgba(0x2e, 0xc2, 0x7e, 1.0),
            error: rgba(0xe0, 0x1b, 0x24, 1.0),
        }
    }
}

fn rgba(red: u8, green: u8, blue: u8, alpha: f32) -> gtk4::gdk::RGBA {
    gtk4::gdk::RGBA::new(
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        alpha,
    )
}

fn set_source_color(cr: &gtk4::cairo::Context, color: &gtk4::gdk::RGBA) {
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
}

fn outcome_color(outcome: Option<Outcome>, colors: &TimelineColors) -> Option<&gtk4::gdk::RGBA> {
    match outcome {
        Some(Outcome::Win | Outcome::Complete) => Some(&colors.success),
        Some(Outcome::Loss | Outcome::Abandoned) => Some(&colors.error),
        Some(Outcome::Unknown) | None => None,
    }
}

fn span_x_bounds(start_ms: u64, end_ms: u64, duration_ms: u64, width: f64) -> (f64, f64) {
    if width <= 0.0 {
        return (0.0, 0.0);
    }
    let mut start_x = ms_to_x(start_ms, duration_ms, width);
    let mut end_x = ms_to_x(end_ms, duration_ms, width).max(start_x);
    if end_x - start_x < MIN_SPAN_WIDTH {
        if start_x + MIN_SPAN_WIDTH <= width {
            end_x = start_x + MIN_SPAN_WIDTH;
        } else {
            start_x = (width - MIN_SPAN_WIDTH).max(0.0);
            end_x = width;
        }
    }
    (start_x, end_x)
}

fn draw_outcome_cap(
    cr: &gtk4::cairo::Context,
    start_x: f64,
    end_x: f64,
    y: f64,
    height: f64,
    outcome: Option<Outcome>,
    colors: &TimelineColors,
) {
    let Some(color) = outcome_color(outcome, colors) else {
        return;
    };
    set_source_color(cr, color);
    cr.rectangle(
        (end_x - OUTCOME_CAP_WIDTH).max(start_x),
        y - OUTCOME_CAP_OVERHANG,
        OUTCOME_CAP_WIDTH.min(end_x - start_x),
        height + OUTCOME_CAP_OVERHANG * 2.0,
    );
    let _ = cr.fill();
}

fn edge_safe_x(x: f64, width: f64, inset: f64) -> f64 {
    if width <= 0.0 {
        return 0.0;
    }
    let inset = inset.min(width / 2.0);
    x.clamp(inset, width - inset)
}

fn draw_death_marker(cr: &gtk4::cairo::Context, x: f64, colors: &TimelineColors) {
    // A true-white event tick remains distinct from the muted grey-purple
    // gravestone below it.
    set_source_color(cr, &colors.death_tick);
    cr.rectangle(
        x - EVENT_TICK_WIDTH / 2.0,
        EVENT_TICK_TOP,
        EVENT_TICK_WIDTH,
        EVENT_TICK_BOTTOM - EVENT_TICK_TOP,
    );
    let _ = cr.fill();

    set_source_color(cr, &colors.death_stone);
    cr.move_to(
        x - DEATH_STONE_HALF_WIDTH,
        EVENT_STONE_MID + DEATH_STONE_BASE_OFFSET,
    );
    cr.line_to(
        x - DEATH_STONE_HALF_WIDTH,
        EVENT_STONE_MID - DEATH_STONE_SHOULDER_OFFSET,
    );
    cr.curve_to(
        x - DEATH_STONE_HALF_WIDTH,
        EVENT_STONE_MID - DEATH_STONE_TOP_OFFSET,
        x + DEATH_STONE_HALF_WIDTH,
        EVENT_STONE_MID - DEATH_STONE_TOP_OFFSET,
        x + DEATH_STONE_HALF_WIDTH,
        EVENT_STONE_MID - DEATH_STONE_SHOULDER_OFFSET,
    );
    cr.line_to(
        x + DEATH_STONE_HALF_WIDTH,
        EVENT_STONE_MID + DEATH_STONE_BASE_OFFSET,
    );
    cr.close_path();
    let _ = cr.fill_preserve();

    set_source_color(cr, &colors.death_detail);
    cr.set_line_width(DEATH_DETAIL_LINE_WIDTH);
    let _ = cr.stroke();
    cr.move_to(
        x - DEATH_CROSS_HALF_WIDTH,
        EVENT_STONE_MID - DEATH_STONE_SHOULDER_OFFSET,
    );
    cr.line_to(
        x + DEATH_CROSS_HALF_WIDTH,
        EVENT_STONE_MID - DEATH_STONE_SHOULDER_OFFSET,
    );
    cr.move_to(x, EVENT_STONE_MID - DEATH_STONE_TOP_OFFSET / 2.0);
    cr.line_to(x, EVENT_STONE_MID + DEATH_CROSS_HALF_WIDTH);
    let _ = cr.stroke();
}

fn draw_clip_bracket(cr: &gtk4::cairo::Context, x: f64, start: bool) {
    let arm = if start { BRACKET_ARM } else { -BRACKET_ARM };
    cr.set_line_width(BRACKET_LINE_WIDTH);
    cr.move_to(x, READOUT_HEIGHT);
    cr.line_to(x, PROGRESS_Y);
    cr.move_to(x, READOUT_HEIGHT);
    cr.line_to(x + arm, READOUT_HEIGHT);
    cr.move_to(x, PROGRESS_Y);
    cr.line_to(x + arm, PROGRESS_Y);
    let _ = cr.stroke();
}

fn handle_hit_interval(handle_x: f64, width: f64) -> (f64, f64) {
    let target_width = HANDLE_HIT_SIZE.min(width.max(0.0));
    let max_start = (width - target_width).max(0.0);
    let start = (handle_x - target_width / 2.0).clamp(0.0, max_start);
    (start, start + target_width)
}

fn grab_at(
    clip: Option<ClipRangeMs>,
    x: f64,
    y: f64,
    duration_ms: u64,
    width: f64,
    height: f64,
) -> Grab {
    let Some(clip) = clip else {
        return Grab::Seek;
    };
    if duration_ms == 0 || y < READOUT_HEIGHT || y > PROGRESS_Y.min(height) || width <= 0.0 {
        return Grab::Seek;
    }

    let start_x = ms_to_x(clip.start_ms, duration_ms, width);
    let end_x = ms_to_x(clip.end_ms, duration_ms, width);
    let (start_left, start_right) = handle_hit_interval(start_x, width);
    let (end_left, end_right) = handle_hit_interval(end_x, width);
    let start_hit = (start_left..=start_right).contains(&x);
    let end_hit = (end_left..=end_right).contains(&x);
    match (start_hit, end_hit) {
        (true, true) if (x - end_x).abs() < (x - start_x).abs() => Grab::End,
        (true, _) => Grab::Start,
        (_, true) => Grab::End,
        _ => Grab::Seek,
    }
}

fn keyboard_seek_target(position_ms: u64, duration_ms: u64, forward: bool) -> u64 {
    if forward {
        position_ms.saturating_add(5_000).min(duration_ms)
    } else {
        position_ms.min(duration_ms).saturating_sub(5_000)
    }
}

fn replace_readout(state: &State, text: String) -> bool {
    let mut readout = state.readout.borrow_mut();
    if *readout == text {
        false
    } else {
        *readout = text;
        true
    }
}

fn replace_readout_with_playhead(state: &State) -> bool {
    replace_readout(
        state,
        playhead_readout(state.position_ms.get(), state.duration_ms.get()),
    )
}

fn update_accessibility(widget: &gtk4::DrawingArea, state: &State) {
    let duration_ms = state.duration_ms.get();
    let position_ms = state.position_ms.get().min(duration_ms);
    let readout = state.readout.borrow();
    widget.update_property(&[
        gtk4::accessible::Property::ValueMin(0.0),
        gtk4::accessible::Property::ValueMax(duration_ms as f64),
        gtk4::accessible::Property::ValueNow(position_ms as f64),
        gtk4::accessible::Property::ValueText(readout.as_str()),
    ]);
}

/// Route a pointer x according to the active grab: seek or move a handle.
fn apply_pointer(state: &Rc<State>, widget: &gtk4::DrawingArea, x: f64) {
    let duration_ms = state.duration_ms.get();
    if duration_ms == 0 {
        return;
    }
    let to_ms = x_to_ms(x, duration_ms, f64::from(widget.width()));
    match state.grab.get() {
        Some(Grab::Start) | Some(Grab::End) => {
            if let Some(clip) = state.clip.get() {
                let start = state.grab.get() == Some(Grab::Start);
                let moved = drag_clip_handle(clip, start, to_ms);
                if let Some(moved) = clamp_clip_range(moved, duration_ms) {
                    state.clip.set(Some(moved));
                    let endpoint = if start { moved.start_ms } else { moved.end_ms };
                    let label = if start { "Clip start" } else { "Clip end" };
                    replace_readout(state, format!("{label} · {}", format_mm_ss(endpoint)));
                    update_accessibility(widget, state);
                    widget.queue_draw();
                }
            }
        }
        _ => {
            if let Some(on_seek) = state.on_seek.borrow().as_ref() {
                on_seek(to_ms);
            }
        }
    }
}

fn draw_readout(cr: &gtk4::cairo::Context, width: f64, text: &str, color: &gtk4::gdk::RGBA) {
    let _ = cr.save();
    cr.rectangle(
        READOUT_PADDING_X,
        0.0,
        (width - READOUT_PADDING_X * 2.0).max(0.0),
        READOUT_HEIGHT,
    );
    cr.clip();
    set_source_color(cr, color);
    cr.set_font_size(READOUT_FONT_SIZE);
    cr.move_to(READOUT_PADDING_X, READOUT_BASELINE_Y);
    let _ = cr.show_text(text);
    let _ = cr.restore();
}

fn rounded_bar(cr: &gtk4::cairo::Context, x: f64, y: f64, width: f64, height: f64) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let radius = (height / 2.0).min(width / 2.0);
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
                has_content: true,
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

    fn prepared(items: &[TimelineItem]) -> Vec<OwnedItem> {
        items.iter().map(OwnedItem::from_item).collect()
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
                TimelineItem::span(
                    TimelineKind::Bloodlust,
                    3_000,
                    43_000,
                    Some("Fury of the Aspects".to_owned()),
                    None,
                    None,
                )
                .unwrap(),
            ],
        );
        let all = MarkerPrefs {
            deaths: DeathMarkerVisibility::All,
            encounters: MarkerVisibility::Visible,
            rounds: MarkerVisibility::Visible,
        };
        assert_eq!(visible_items(&entry, all).len(), 5);

        let own_only = MarkerPrefs {
            deaths: DeathMarkerVisibility::Own,
            encounters: MarkerVisibility::Hidden,
            rounds: MarkerVisibility::Hidden,
        };
        let visible = visible_items(&entry, own_only);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].label(), Some("Alice"));
        assert_eq!(visible[1].kind(), &TimelineKind::Bloodlust);
        assert_eq!(entry.timeline.len(), 5);

        let clip = entry_with(Category::Clip, vec![death("Alice", 1_000)]);
        assert!(visible_items(&clip, all).is_empty());
    }

    #[test]
    fn lane_hit_testing_prefers_points_and_never_infers_bloodlust() {
        let source = vec![
            TimelineItem::span(
                TimelineKind::Activity,
                10_000,
                100_000,
                Some("Pull".to_owned()),
                Some(Outcome::Win),
                None,
            )
            .unwrap(),
            TimelineItem::span(
                TimelineKind::Bloodlust,
                40_000,
                60_000,
                Some("Fury of the Aspects".to_owned()),
                None,
                None,
            )
            .unwrap(),
            death("Alice", 50_000),
            TimelineItem::span(
                TimelineKind::Activity,
                45_000,
                55_000,
                Some("Bloodlust".to_owned()),
                None,
                None,
            )
            .unwrap(),
        ];
        let items = prepared(&source);
        let x = ms_to_x(50_000, 120_000, 600.0);

        let event_hit = hit_test(&items, x, EVENT_RAIL_Y, 120_000, 600.0).unwrap();
        assert_eq!(items[event_hit].kind, TimelineKind::Death);
        assert_eq!(items[event_hit].lane, Lane::Events);
        assert_eq!(
            pointer_readout(&items, x, EVENT_RAIL_Y, 120_000, 600.0),
            "Death: Alice · 0:50"
        );

        let activity_hit = hit_test(&items, x, ACTIVITY_BAR_TOP, 120_000, 600.0).unwrap();
        assert_eq!(items[activity_hit].lane, Lane::Activity);
        assert_eq!(items[3].lane, Lane::Activity);

        let bloodlust_hit = hit_test(&items, x, BLOODLUST_BAR_TOP, 120_000, 600.0).unwrap();
        assert_eq!(bloodlust_hit, 1);
        assert_eq!(items[bloodlust_hit].kind, TimelineKind::Bloodlust);

        // Point priority is explicit even if malformed data places a
        // containing span on the event lane.
        let same_lane = prepared(&[
            TimelineItem::span(TimelineKind::Death, 40_000, 60_000, None, None, None).unwrap(),
            death("Alice", 50_000),
        ]);
        assert_eq!(
            hit_test(&same_lane, x, EVENT_RAIL_Y, 120_000, 600.0),
            Some(1)
        );
    }

    #[test]
    fn mapping_and_seeking_clamp_without_requiring_timeline_items() {
        assert_eq!(
            x_to_ms(ms_to_x(30_000, 120_000, 600.0), 120_000, 600.0),
            30_000
        );
        assert_eq!(ms_to_x(200_000, 120_000, 600.0), 600.0);
        assert_eq!(x_to_ms(-10.0, 120_000, 600.0), 0);
        assert_eq!(x_to_ms(999.0, 120_000, 600.0), 120_000);
        assert_eq!(hit_test(&[], 300.0, EVENT_RAIL_Y, 120_000, 600.0), None);
        assert_eq!(
            grab_at(
                None,
                300.0,
                EVENT_RAIL_Y,
                120_000,
                600.0,
                f64::from(TRACK_HEIGHT),
            ),
            Grab::Seek
        );

        assert_eq!(keyboard_seek_target(7_000, 120_000, false), 2_000);
        assert_eq!(keyboard_seek_target(3_000, 120_000, false), 0);
        assert_eq!(keyboard_seek_target(117_000, 120_000, true), 120_000);
        assert_eq!(keyboard_seek_target(u64::MAX, 120_000, true), 120_000);
    }

    #[test]
    fn clip_bounds_targets_and_crossing_invariants_hold() {
        let initial = initial_clip_range(5_000, 120_000);
        assert_eq!(
            initial,
            ClipRangeMs {
                start_ms: 0,
                end_ms: 20_000,
            }
        );
        assert_eq!(initial_clip_range(115_000, 120_000).end_ms, 120_000);
        assert_eq!(
            initial_clip_range(u64::MAX, 120_000),
            ClipRangeMs {
                start_ms: 105_000,
                end_ms: 120_000,
            }
        );
        assert_eq!(
            initial_clip_range(0, 0),
            ClipRangeMs {
                start_ms: 0,
                end_ms: 0,
            }
        );

        let range = ClipRangeMs {
            start_ms: 10_000,
            end_ms: 20_000,
        };
        assert_eq!(drag_clip_handle(range, true, 25_000).start_ms, 19_999);
        assert_eq!(drag_clip_handle(range, false, 5_000).end_ms, 10_001);
        assert_eq!(drag_clip_handle(range, true, 0).start_ms, 0);

        let clamped = clamp_clip_range(
            ClipRangeMs {
                start_ms: 130_000,
                end_ms: 5_000,
            },
            120_000,
        )
        .unwrap();
        assert_eq!(
            clamped,
            ClipRangeMs {
                start_ms: 119_999,
                end_ms: 120_000,
            }
        );
        assert!(clamp_clip_range(range, 0).is_none());

        let (start_left, start_right) = handle_hit_interval(0.0, 600.0);
        let (end_left, end_right) = handle_hit_interval(600.0, 600.0);
        assert_eq!(start_right - start_left, HANDLE_HIT_SIZE);
        assert_eq!(end_right - end_left, HANDLE_HIT_SIZE);
        let full = Some(ClipRangeMs {
            start_ms: 0,
            end_ms: 120_000,
        });
        assert_eq!(
            grab_at(
                full,
                HANDLE_HIT_SIZE - 1.0,
                EVENT_RAIL_Y,
                120_000,
                600.0,
                f64::from(TRACK_HEIGHT),
            ),
            Grab::Start
        );
        assert_eq!(
            grab_at(
                full,
                600.0 - HANDLE_HIT_SIZE + 1.0,
                EVENT_RAIL_Y,
                120_000,
                600.0,
                f64::from(TRACK_HEIGHT),
            ),
            Grab::End
        );
        assert_eq!(
            grab_at(full, 0.0, 4.0, 120_000, 600.0, f64::from(TRACK_HEIGHT)),
            Grab::Seek
        );
    }
}
