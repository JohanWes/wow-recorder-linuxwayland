// SPDX-License-Identifier: GPL-3.0-or-later

//! The damage-meter overlay: a compact Details/Skada-style ranking laid over
//! the player video and fed from `LibraryEntry.meter` interval aggregates. The
//! player owns one instance on its `video_overlay`; visibility, filters, and
//! drag position are session-only state. Current and Overall totals stop at
//! the latest completed interval.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gtk4::prelude::*;

use warcraft_recorder::domain::{
    LibraryEntry, MeterDeath, MeterDeathEventKind, MeterFight, MeterMetric,
};
use warcraft_recorder::meter::{
    MeterProjection, ProjectedActor, ProjectedEntry, SAMPLE_INTERVAL_MS, fight_index_at,
    has_untimed_totals, project_current, project_overall,
};

use super::filters::class_css_class;
use super::timeline::format_mm_ss;

/// Raid-marker values (`destRaidFlags & 0xff`) in display order.
const MARKERS: [(u8, &str); 8] = [
    (0x01, "Star"),
    (0x02, "Circle"),
    (0x04, "Diamond"),
    (0x08, "Triangle"),
    (0x10, "Moon"),
    (0x20, "Square"),
    (0x40, "Cross"),
    (0x80, "Skull"),
];

/// Max natural height of the ranking/breakdown list; the scroller takes over
/// beyond it.
const MAX_LIST_HEIGHT: i32 = 260;
/// Minimum panel width/height once the user has resized it; a smaller
/// viewport wins.
const MIN_WIDTH: i32 = 240;
const MIN_HEIGHT: i32 = 140;
/// Bounded target rows fold into "Other", which is not a selectable target.
const OTHER_KEY: &str = "Other";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Metric(MeterMetric),
    Deaths,
}

fn view_label(view: View) -> &'static str {
    match view {
        View::Metric(MeterMetric::Damage) => "Damage Done",
        View::Metric(MeterMetric::DamageTaken) => "Damage Taken",
        View::Metric(MeterMetric::Healing) => "Healing Done",
        View::Metric(MeterMetric::Interrupts) => "Interrupts",
        View::Metric(MeterMetric::Dispels) => "Dispels",
        View::Deaths => "Deaths",
    }
}

fn view_key(view: View) -> &'static str {
    match view {
        View::Metric(MeterMetric::Damage) => "damage",
        View::Metric(MeterMetric::DamageTaken) => "damage_taken",
        View::Metric(MeterMetric::Healing) => "healing",
        View::Metric(MeterMetric::Interrupts) => "interrupts",
        View::Metric(MeterMetric::Dispels) => "dispels",
        View::Deaths => "deaths",
    }
}

fn view_from_key(key: &str) -> Option<View> {
    Some(match key {
        "damage" => View::Metric(MeterMetric::Damage),
        "damage_taken" => View::Metric(MeterMetric::DamageTaken),
        "healing" => View::Metric(MeterMetric::Healing),
        "interrupts" => View::Metric(MeterMetric::Interrupts),
        "dispels" => View::Metric(MeterMetric::Dispels),
        "deaths" => View::Deaths,
        _ => return None,
    })
}

fn view_empty_message(view: View) -> String {
    let noun = match view {
        View::Metric(MeterMetric::Damage) => "damage",
        View::Metric(MeterMetric::DamageTaken) => "damage taken",
        View::Metric(MeterMetric::Healing) => "healing",
        View::Metric(MeterMetric::Interrupts) => "interrupts",
        View::Metric(MeterMetric::Dispels) => "dispels",
        View::Deaths => "deaths",
    };
    format!("No {noun} in this fight.")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SegmentKind {
    Overall,
    Current,
}

/// What a claimed drag sequence on the meter does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragMode {
    /// Header drags move the meter.
    Move,
    /// Grip drags resize the meter.
    Resize,
}

fn segment_key(segment: SegmentKind) -> &'static str {
    match segment {
        SegmentKind::Overall => "overall",
        SegmentKind::Current => "current",
    }
}

fn segment_label(segment: SegmentKind) -> &'static str {
    match segment {
        SegmentKind::Overall => "Overall",
        SegmentKind::Current => "Current fight",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetSel {
    All,
    Name(String),
    Marker(u8),
}

/// `meter.target` action key: "all", "name:<target>", or "marker:<value>".
fn target_key(target: &TargetSel) -> String {
    match target {
        TargetSel::All => "all".to_owned(),
        TargetSel::Name(name) => format!("name:{name}"),
        TargetSel::Marker(marker) => format!("marker:{marker}"),
    }
}

fn target_from_key(key: &str) -> Option<TargetSel> {
    match key {
        "all" => Some(TargetSel::All),
        _ => match key.split_once(':')? {
            ("name", name) => Some(TargetSel::Name(name.to_owned())),
            ("marker", value) => Some(TargetSel::Marker(value.parse().ok()?)),
            _ => None,
        },
    }
}

/// The title suffix a filter adds, if any: `Damage Done to Skull`.
fn target_label(target: &TargetSel) -> Option<String> {
    match target {
        TargetSel::All => None,
        TargetSel::Name(name) => Some(name.clone()),
        TargetSel::Marker(marker) => marker_name(*marker).map(str::to_owned),
    }
}

fn marker_name(value: u8) -> Option<&'static str> {
    MARKERS
        .iter()
        .find(|(marker, _)| *marker == value)
        .map(|(_, name)| *name)
}

/// Details-style compact amounts: 1234 → "1.23K", 5_000_000 → "5.00M".
fn format_compact(amount: u64) -> String {
    let mut value = amount as f64;
    let mut suffix = "";
    for candidate in ["K", "M", "B"] {
        if value < 1_000.0 {
            break;
        }
        value /= 1_000.0;
        suffix = candidate;
    }
    if suffix.is_empty() {
        return amount.to_string();
    }
    let decimals = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    match decimals {
        0 => format!("{value:.0}{suffix}"),
        1 => format!("{value:.1}{suffix}"),
        _ => format!("{value:.2}{suffix}"),
    }
}

/// The selected entry's meter facts plus the combatant GUID → spec id join
/// for class colors. Cloned from the snapshot entry; no file I/O.
struct EntryMeter {
    fights: Vec<MeterFight>,
    spec_by_guid: HashMap<String, u16>,
}

impl EntryMeter {
    fn from_entry(entry: &LibraryEntry) -> Self {
        let spec_by_guid = entry
            .combatants
            .iter()
            .filter_map(|combatant| Some((combatant.guid.clone()?, combatant.spec_id?)))
            .collect();
        Self {
            fights: entry.meter.fights.clone(),
            spec_by_guid,
        }
    }
}

/// Actor total for a view: spell amounts unfiltered, or the matching target
/// rows when a target or marker is selected. Utility totals count events.
fn actor_total(actor: &ProjectedActor, view: MeterMetric, target: &TargetSel) -> u64 {
    match target {
        TargetSel::All => actor
            .spells
            .iter()
            .filter(|entry| entry.metric == view)
            .map(|entry| entry.amount)
            .sum(),
        _ => actor
            .targets
            .iter()
            .filter(|entry| entry.metric == view && matches_target(entry, target))
            .map(|entry| entry.amount)
            .sum(),
    }
}

/// The active target filter, applied to target rows only: by name across all
/// markers, or by marker across all names.
fn matches_target(entry: &ProjectedEntry, target: &TargetSel) -> bool {
    match target {
        TargetSel::All => true,
        TargetSel::Name(name) => &entry.key == name,
        TargetSel::Marker(marker) => entry.marker == *marker,
    }
}

struct Inner {
    root: gtk4::Box,
    header: gtk4::Box,
    title: gtk4::Label,
    context_menu: gtk4::PopoverMenu,
    content: gtk4::Box,
    scroller: gtk4::ScrolledWindow,
    grip: gtk4::Label,
    empty_label: gtk4::Label,
    actions: gtk4::gio::SimpleActionGroup,

    entry: RefCell<Option<EntryMeter>>,
    view: Cell<View>,
    segment: Cell<SegmentKind>,
    /// The open actor breakdown: the actor's GUID.
    target: RefCell<TargetSel>,
    breakdown: RefCell<Option<String>>,
    /// Fight index the Current segment last rendered from.
    current_fight: Cell<Option<usize>>,
    /// Drag mode chosen at drag begin, if the sequence was claimed.
    drag_mode: Cell<Option<DragMode>>,
    /// Geometry captured at drag begin: `(margin_end, margin_bottom, width,
    /// height)`.
    drag_geometry: Cell<(i32, i32, i32, i32)>,
    /// The user's chosen panel size once resized: an explicit size request
    /// instead of the natural size, so it survives a temporary viewport
    /// shrink.
    desired_size: Cell<Option<(i32, i32)>>,
    /// Last playhead position, kept so a segment switch can pick the Current
    /// fight even while another segment was rendered.
    position_ms: Cell<u64>,
}

pub struct DamageMeter {
    pub widget: gtk4::Box,
    inner: Rc<Inner>,
}

impl DamageMeter {
    pub fn new() -> Self {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.add_css_class("wr-meter");
        root.set_size_request(300, -1);
        root.set_halign(gtk4::Align::End);
        root.set_valign(gtk4::Align::End);
        root.set_margin_end(16);
        root.set_margin_bottom(16);
        root.set_visible(false);

        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        header.add_css_class("wr-meter-header");
        let title = gtk4::Label::new(Some("Damage Done"));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        // Cap the title's natural width: its long text is the header's main
        // width contributor, so the explicit root widths from resizing can
        // govern.
        title.set_max_width_chars(1);
        let context_menu = gtk4::PopoverMenu::from_model(None::<&gtk4::gio::Menu>);
        context_menu.set_parent(&title);
        context_menu.set_position(gtk4::PositionType::Bottom);
        context_menu.set_has_arrow(false);
        context_menu.update_property(&[gtk4::accessible::Property::Label("Meter options")]);
        let close = gtk4::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Hide meter"));
        close.update_property(&[gtk4::accessible::Property::Label("Hide meter")]);
        header.append(&title);
        header.append(&close);

        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk4::PolicyType::Never);
        scroller.set_propagate_natural_height(true);
        scroller.set_vexpand(true);
        scroller.set_max_content_height(MAX_LIST_HEIGHT);
        scroller.set_child(Some(&content));
        // The resize grip: a dim, bottom-right corner handle the drag
        // gesture picks on.
        let grip = gtk4::Label::new(Some("◢"));
        grip.add_css_class("dim-label");
        grip.set_halign(gtk4::Align::End);
        grip.set_size_request(16, 16);
        grip.set_cursor_from_name(Some("se-resize"));
        grip.set_tooltip_text(Some("Resize meter"));
        grip.update_property(&[gtk4::accessible::Property::Label("Resize meter")]);
        let empty_label = gtk4::Label::new(None);
        empty_label.add_css_class("dim-label");
        empty_label.set_halign(gtk4::Align::Center);
        // Wrapped, so a long empty-state text never imposes a width floor
        // on the resizable panel.
        empty_label.set_wrap(true);
        empty_label.set_margin_top(16);
        empty_label.set_margin_bottom(16);

        let actions = gtk4::gio::SimpleActionGroup::new();
        root.insert_action_group("meter", Some(&actions));

        root.append(&header);
        root.append(&scroller);
        root.append(&grip);

        let inner = Rc::new(Inner {
            root,
            header,
            title,
            context_menu,
            content,
            scroller,
            grip,
            empty_label,
            actions,
            entry: RefCell::new(None),
            view: Cell::new(View::Metric(MeterMetric::Damage)),
            segment: Cell::new(SegmentKind::Current),
            target: RefCell::new(TargetSel::All),
            position_ms: Cell::new(0),
            breakdown: RefCell::new(None),
            current_fight: Cell::new(None),
            drag_mode: Cell::new(None),
            drag_geometry: Cell::new((16, 16, 0, 0)),
            desired_size: Cell::new(None),
        });

        inner.connect_actions();
        inner.connect_clicks();
        {
            let inner = Rc::clone(&inner);
            close.connect_clicked(move |_| inner.set_visible(false));
        }
        inner.refresh();

        Self {
            widget: inner.root.clone(),
            inner,
        }
    }

    /// Feed the selected entry's meter facts; `None` clears. No file I/O:
    /// everything arrives on the snapshot. A target filter is reset: names and
    /// markers of a previous recording need not occur here.
    pub fn set_entry(&self, entry: Option<&LibraryEntry>) {
        let inner = &self.inner;
        inner.entry.replace(entry.map(EntryMeter::from_entry));
        inner.position_ms.set(0);
        inner.current_fight.set(None);
        inner.breakdown.replace(None);
        inner.target.replace(TargetSel::All);
        inner.refresh();
    }
    /// The playhead moved. Both segment modes are cumulative through the
    /// latest completed sample, so refresh on each interval or fight boundary.
    pub fn set_position(&self, position_ms: u64) {
        let inner = &self.inner;
        let previous_interval = inner.position_ms.replace(position_ms) / SAMPLE_INTERVAL_MS;
        let previous_fight = inner.current_fight.get();
        inner.sync_current_fight();
        if position_ms / SAMPLE_INTERVAL_MS != previous_interval
            || inner.current_fight.get() != previous_fight
        {
            inner.refresh();
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.widget.set_visible(visible);
    }

    pub fn toggle(&self) {
        self.widget.set_visible(!self.widget.is_visible());
    }

    /// Re-clamp the drag margins to the current overlay allocation; once
    /// resized, the desired size is reapplied, capped to the viewport so a
    /// temporary shrink does not lose it.
    pub fn clamp_position(&self) {
        let inner = &self.inner;
        if let Some((width, height)) = inner.desired_size.get()
            && let Some((viewport_width, viewport_height)) = inner.viewport_size()
            && viewport_width > 0
            && viewport_height > 0
        {
            inner
                .root
                .set_size_request(width.min(viewport_width), height.min(viewport_height));
        }
        inner.move_to(
            f64::from(inner.root.margin_end()),
            f64::from(inner.root.margin_bottom()),
        );
    }

    /// Install the meter's drag gesture on the overlay it was added to.
    pub fn attach_drag(&self, overlay: &gtk4::Overlay) {
        self.inner.connect_drag(overlay);
    }
}

impl Inner {
    fn set_visible(&self, visible: bool) {
        self.root.set_visible(visible);
    }

    fn connect_actions(self: &Rc<Self>) {
        let view = stateful_action("view", view_key(self.view.get()));
        self.actions.add_action(&view);
        let this = Rc::clone(self);
        view.connect_change_state(move |action, state| {
            // With a change-state handler connected, GLib leaves the state
            // update to it; the default handler is suppressed.
            let Some(state) = state else {
                return;
            };
            action.set_state(state);
            if let Some(key) = state.str()
                && let Some(view) = view_from_key(key)
            {
                this.set_view(view);
            }
        });

        let segment = stateful_action("segment", segment_key(self.segment.get()));
        self.actions.add_action(&segment);
        let this = Rc::clone(self);
        segment.connect_change_state(move |action, state| {
            let Some(state) = state else {
                return;
            };
            action.set_state(state);
            if let Some(key) = state.str() {
                let segment = match key {
                    "overall" => SegmentKind::Overall,
                    "current" => SegmentKind::Current,
                    _ => return,
                };
                this.set_segment(segment);
            }
        });

        let target = stateful_action("target", "all");
        self.actions.add_action(&target);
        let this = Rc::clone(self);
        target.connect_change_state(move |action, state| {
            let Some(state) = state else {
                return;
            };
            action.set_state(state);
            if let Some(key) = state.str()
                && let Some(target) = target_from_key(key)
            {
                this.set_target(target);
            }
        });
    }

    /// Title-and-grip drag: the title moves the meter by pixel margins, the
    /// bottom-right grip resizes it, both clamped so the panel stays inside
    /// the overlay allocation. Other header controls are denied so their
    /// clicks survive. The controller lives on the stationary overlay because
    /// overlay-relative drag coordinates stay valid while the meter moves.
    fn connect_drag(self: &Rc<Self>, overlay: &gtk4::Overlay) {
        let drag = gtk4::GestureDrag::new();
        {
            let overlay = (*overlay).clone();
            let this = Rc::clone(self);
            drag.connect_drag_begin(move |gesture, start_x, start_y| {
                // The mode comes from the overlay-relative pick: the grip
                // resizes, the header moves, anything else is denied.
                this.drag_mode.set(None);
                let Some(picked) = overlay.pick(start_x, start_y, gtk4::PickFlags::DEFAULT) else {
                    gesture.set_state(gtk4::EventSequenceState::Denied);
                    return;
                };
                let mode = if picked == this.grip || picked.is_ancestor(&this.grip) {
                    DragMode::Resize
                } else if picked == this.header
                    || picked == this.title
                    || picked.is_ancestor(&this.title)
                {
                    DragMode::Move
                } else {
                    gesture.set_state(gtk4::EventSequenceState::Denied);
                    return;
                };
                let width = this.root.width();
                let height = this.root.height();
                this.drag_geometry.set((
                    this.root.margin_end(),
                    this.root.margin_bottom(),
                    width,
                    height,
                ));
                if mode == DragMode::Resize {
                    // Freeze the current allocation as the explicit size
                    // request; the natural height would otherwise override
                    // it, so the scroller must stop propagating it. Both
                    // changes only queue layout, so nothing jumps.
                    this.root.set_size_request(width, height);
                    this.scroller.set_propagate_natural_height(false);
                    this.desired_size.set(Some((width, height)));
                }
                this.drag_mode.set(Some(mode));
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            });
        }
        {
            let this = Rc::clone(self);
            drag.connect_drag_update(move |_, offset_x, offset_y| {
                let (end, bottom, width, height) = this.drag_geometry.get();
                match this.drag_mode.get() {
                    Some(DragMode::Move) => {
                        this.move_to(f64::from(end) - offset_x, f64::from(bottom) - offset_y);
                    }
                    Some(DragMode::Resize) => {
                        this.resize_to(end, bottom, width, height, offset_x, offset_y);
                    }
                    None => {}
                }
            });
        }
        overlay.add_controller(drag);
    }

    /// Secondary click on the title opens meter options; elsewhere on the
    /// panel it returns an open actor breakdown to the ranking.
    fn connect_clicks(self: &Rc<Self>) {
        let menu_click = gtk4::GestureClick::new();
        menu_click.set_button(gtk4::gdk::BUTTON_SECONDARY);
        let this = Rc::clone(self);
        menu_click.connect_pressed(move |gesture, _, x, y| {
            this.context_menu.set_menu_model(Some(&this.menu_model()));
            this.context_menu
                .set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            this.context_menu.popup();
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        });
        self.title.add_controller(menu_click);

        let back_click = gtk4::GestureClick::new();
        back_click.set_button(gtk4::gdk::BUTTON_SECONDARY);
        let this = Rc::clone(self);
        back_click.connect_pressed(move |gesture, _, _, _| {
            if this.breakdown.borrow().is_none() {
                return;
            }
            this.breakdown.replace(None);
            this.refresh();
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        });
        self.root.add_controller(back_click);
    }

    fn set_view(self: &Rc<Self>, view: View) {
        if self.view.replace(view) != view {
            self.target.replace(TargetSel::All);
            self.breakdown.replace(None);
            self.refresh();
        }
    }

    fn set_segment(self: &Rc<Self>, segment: SegmentKind) {
        if self.segment.replace(segment) != segment {
            // Pick the Current fight from the last known playhead before the
            // re-render; positions arrived while Overall was shown too.
            self.sync_current_fight();
            self.refresh();
        }
    }

    /// Track the Current fight from the last known playhead position, even
    /// while another segment is rendered, so a segment switch never shows a
    /// stale fight.
    fn sync_current_fight(&self) {
        let index = self
            .entry
            .borrow()
            .as_ref()
            .and_then(|entry| fight_index_at(&entry.fights, self.position_ms.get()));
        self.current_fight.replace(index);
    }

    fn set_target(self: &Rc<Self>, target: TargetSel) {
        if *self.target.borrow() != target {
            self.target.replace(target);
            self.refresh();
        }
    }

    /// One full re-render: header and content derive from the same session
    /// state and selected entry. Menu models are built only when opened, so a
    /// playback tick never replaces an open popover.
    fn refresh(self: &Rc<Self>) {
        self.sync_action_states();
        let fight = self.selected_fight();
        self.rebuild_title(fight.as_ref());
        self.rebuild_content(fight);
    }

    /// Mirror the cells into the stateful actions so rebuilt menus mark the
    /// active items. `set_state` updates the property directly.
    fn sync_action_states(&self) {
        let target = target_key(&self.target.borrow());
        for (name, state) in [
            ("view", view_key(self.view.get())),
            ("segment", segment_key(self.segment.get())),
            ("target", target.as_str()),
        ] {
            if let Some(action) = self
                .actions
                .lookup_action(name)
                .and_downcast::<gtk4::gio::SimpleAction>()
            {
                action.set_state(&state.to_variant());
            }
        }
    }

    /// The selected Current or Overall projection at the playhead.
    fn selected_fight(&self) -> Option<MeterProjection> {
        let entry = self.entry.borrow();
        let entry = entry.as_ref()?;
        if entry.fights.is_empty() {
            return None;
        }
        match self.segment.get() {
            SegmentKind::Overall => Some(project_overall(&entry.fights, self.position_ms.get())),
            SegmentKind::Current => project_current(&entry.fights, self.position_ms.get()),
        }
    }

    /// Header text: `{mm:ss} {view}`, with the target appended when filtered.
    fn rebuild_title(&self, fight: Option<&MeterProjection>) {
        let view = self.view.get();
        let label = view_label(view);
        let title = match (view, target_label(&self.target.borrow())) {
            (View::Metric(MeterMetric::DamageTaken), Some(target)) => {
                format!("{label} from {target}")
            }
            (View::Metric(_), Some(target)) => format!("{label} to {target}"),
            (_, _) => label.to_owned(),
        };
        match fight {
            Some(fight) => self
                .title
                .set_text(&format!("{} {title}", format_mm_ss(fight.elapsed_ms))),
            None => self.title.set_text(&title),
        }
    }

    fn menu_model(self: &Rc<Self>) -> gtk4::gio::Menu {
        let menu = gtk4::gio::Menu::new();

        let view_section = gtk4::gio::Menu::new();
        for view in [
            View::Metric(MeterMetric::Damage),
            View::Metric(MeterMetric::DamageTaken),
            View::Metric(MeterMetric::Healing),
            View::Metric(MeterMetric::Interrupts),
            View::Metric(MeterMetric::Dispels),
            View::Deaths,
        ] {
            let item = gtk4::gio::MenuItem::new(Some(view_label(view)), None);
            item.set_action_and_target_value(
                Some("meter.view"),
                Some(&view_key(view).to_variant()),
            );
            view_section.append_item(&item);
        }
        menu.append_section(None, &view_section);

        let segment_section = gtk4::gio::Menu::new();
        for segment in [SegmentKind::Overall, SegmentKind::Current] {
            let item = gtk4::gio::MenuItem::new(Some(segment_label(segment)), None);
            item.set_action_and_target_value(
                Some("meter.segment"),
                Some(&segment_key(segment).to_variant()),
            );
            segment_section.append_item(&item);
        }
        menu.append_section(None, &segment_section);

        if matches!(self.view.get(), View::Metric(_)) {
            // Only names and markers present in the selected segment; a dead
            // entry would be a filter with no rows.
            let target_section = gtk4::gio::Menu::new();
            let targets = gtk4::gio::Menu::new();
            let all = gtk4::gio::MenuItem::new(Some("All targets"), None);
            all.set_action_and_target_value(Some("meter.target"), Some(&"all".to_variant()));
            targets.append_item(&all);
            let (names, markers) = self.target_choices();
            for name in names {
                let item = gtk4::gio::MenuItem::new(Some(&name), None);
                item.set_action_and_target_value(
                    Some("meter.target"),
                    Some(&format!("name:{name}").to_variant()),
                );
                targets.append_item(&item);
            }
            for marker in markers {
                let label = marker_name(marker).unwrap_or("Other");
                let item = gtk4::gio::MenuItem::new(Some(label), None);
                item.set_action_and_target_value(
                    Some("meter.target"),
                    Some(&format!("marker:{marker}").to_variant()),
                );
                targets.append_item(&item);
            }
            target_section.append_submenu(Some("Target"), &targets);
            menu.append_section(None, &target_section);
        }

        menu
    }

    /// Target names (sorted) and marker values (canonical order) present in
    /// the selected segment for the active view. `Other` is never a
    /// selectable target.
    fn target_choices(&self) -> (Vec<String>, Vec<u8>) {
        let View::Metric(metric) = self.view.get() else {
            return (Vec::new(), Vec::new());
        };
        let Some(fight) = self.selected_fight() else {
            return (Vec::new(), Vec::new());
        };
        let mut names = BTreeSet::new();
        let mut markers = Vec::new();
        for actor in &fight.actors {
            for entry in actor.targets.iter().filter(|entry| entry.metric == metric) {
                if entry.key != OTHER_KEY {
                    names.insert(entry.key.clone());
                }
                if entry.marker != 0 && !markers.contains(&entry.marker) {
                    markers.push(entry.marker);
                }
            }
        }
        markers.sort_by_key(|marker| {
            MARKERS
                .iter()
                .position(|(value, _)| value == marker)
                .unwrap_or(MARKERS.len())
        });
        (names.into_iter().collect(), markers)
    }

    fn rebuild_content(self: &Rc<Self>, fight: Option<MeterProjection>) {
        let Some(fight) = fight else {
            let has_fights = self
                .entry
                .borrow()
                .as_ref()
                .is_some_and(|entry| !entry.fights.is_empty());
            self.show_empty(if has_fights {
                "No fight yet at this point."
            } else {
                "No combat data for this recording."
            });
            return;
        };
        let view = self.view.get();
        let breakdown = self.breakdown.borrow().clone();
        if view == View::Deaths {
            self.rebuild_deaths(&fight, breakdown.as_deref());
            return;
        }
        let View::Metric(metric) = view else {
            unreachable!();
        };
        let target = self.target.borrow().clone();
        if let Some(guid) = &breakdown
            && let Some(actor) = fight.actors.iter().find(|actor| &actor.guid == guid)
        {
            self.rebuild_breakdown(actor, metric, &target);
            return;
        }
        // A segment switch may have left the breakdown without its actor.
        if breakdown.is_some() {
            self.breakdown.replace(None);
        }
        let mut ranked: Vec<(&ProjectedActor, u64)> = fight
            .actors
            .iter()
            .filter_map(|actor| {
                let total = actor_total(actor, metric, &target);
                (total > 0).then_some((actor, total))
            })
            .collect();
        if ranked.is_empty() {
            let untimed = self
                .entry
                .borrow()
                .as_ref()
                .is_some_and(|entry| has_untimed_totals(&entry.fights));
            if untimed {
                self.show_empty("This recording has no time-resolved meter data.");
            } else {
                self.show_empty(&view_empty_message(view));
            }
            return;
        }
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        self.rebuild_ranking(&fight, &ranked, metric);
    }

    /// Dense ranked buttons: class-colored fill behind white labels, compact
    /// total and rate on the right. Activating a row opens its breakdown.
    fn rebuild_ranking(
        self: &Rc<Self>,
        fight: &MeterProjection,
        ranked: &[(&ProjectedActor, u64)],
        view: MeterMetric,
    ) {
        let top = ranked.first().map_or(1, |(_, total)| *total);
        let utility = matches!(view, MeterMetric::Interrupts | MeterMetric::Dispels);
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        for (rank, (actor, total)) in ranked.iter().enumerate() {
            let right = if utility || fight.elapsed_ms == 0 {
                format_compact(*total)
            } else {
                let rate = u128::from(*total) * 1_000 / u128::from(fight.elapsed_ms);
                format!(
                    "{} ({})",
                    format_compact(*total),
                    format_compact(rate as u64)
                )
            };
            let overlay = fill_line(
                self.class_for(&actor.guid),
                &format!("{}. {}", rank + 1, actor.name),
                &right,
                *total as f64 / top as f64,
            );
            let button = gtk4::Button::new();
            button.add_css_class("flat");
            button.add_css_class("wr-meter-row");
            button.set_child(Some(&overlay));
            let this = Rc::clone(self);
            let guid = actor.guid.clone();
            button.connect_clicked(move |_| this.open_breakdown(guid.clone()));
            content.append(&button);
        }
        self.set_content(&content);
    }

    fn rebuild_deaths(self: &Rc<Self>, fight: &MeterProjection, selected_guid: Option<&str>) {
        if let Some(guid) = selected_guid {
            let deaths: Vec<&MeterDeath> = fight
                .deaths
                .iter()
                .filter(|death| death.guid == guid)
                .collect();
            if !deaths.is_empty() {
                self.rebuild_death_breakdown(&deaths);
                return;
            }
            self.breakdown.replace(None);
        }
        let mut ranked: Vec<(String, String, u64)> = Vec::new();
        for death in &fight.deaths {
            if let Some((_, _, count)) = ranked.iter_mut().find(|(guid, _, _)| guid == &death.guid)
            {
                *count += 1;
            } else {
                ranked.push((death.guid.clone(), death.name.clone(), 1));
            }
        }
        if ranked.is_empty() {
            self.show_empty(&view_empty_message(View::Deaths));
            return;
        }
        ranked.sort_by(|a, b| b.2.cmp(&a.2));
        let top = ranked[0].2;
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        for (rank, (guid, name, count)) in ranked.into_iter().enumerate() {
            let overlay = fill_line(
                self.class_for(&guid),
                &format!("{}. {name}", rank + 1),
                &count.to_string(),
                count as f64 / top as f64,
            );
            let button = gtk4::Button::new();
            button.add_css_class("flat");
            button.add_css_class("wr-meter-row");
            button.set_child(Some(&overlay));
            let this = Rc::clone(self);
            button.connect_clicked(move |_| this.open_breakdown(guid.clone()));
            content.append(&button);
        }
        self.set_content(&content);
    }

    fn rebuild_death_breakdown(&self, deaths: &[&MeterDeath]) {
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        let name = gtk4::Label::new(Some(&deaths[0].name));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        content.append(&name);
        for (index, death) in deaths.iter().enumerate() {
            content.append(&heading(&format!(
                "Death {} — {}",
                index + 1,
                format_mm_ss(death.at_ms)
            )));
            for event in &death.events {
                let before_ms = death.at_ms.saturating_sub(event.at_ms);
                let sign = match event.kind {
                    MeterDeathEventKind::Damage => "-",
                    MeterDeathEventKind::Healing => "+",
                };
                content.append(&death_event_row(
                    event.kind,
                    &format!(
                        "-{:.1}s {} ({})",
                        before_ms as f64 / 1_000.0,
                        event.spell_name,
                        event.source_name
                    ),
                    &format!("{sign}{}", format_compact(event.amount)),
                ));
            }
            content.append(&death_event_row(
                MeterDeathEventKind::Damage,
                "0.0s Death",
                "",
            ));
        }
        self.set_content(&content);
    }

    /// The actor drilldown in the same scroller: actor name, then the Spells
    /// and Targets lists with the active filters intact. Secondary click on
    /// the panel returns to the ranking.
    fn rebuild_breakdown(
        self: &Rc<Self>,
        actor: &ProjectedActor,
        view: MeterMetric,
        target: &TargetSel,
    ) {
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        let name = gtk4::Label::new(Some(&actor.name));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        content.append(&name);

        content.append(&heading("Spells"));
        let spell_total: u64 = actor
            .spells
            .iter()
            .filter(|entry| entry.metric == view)
            .map(|entry| entry.amount)
            .sum();
        for entry in actor.spells.iter().filter(|entry| entry.metric == view) {
            content.append(&self.breakdown_row(actor, entry, spell_total));
        }

        content.append(&heading(if view == MeterMetric::DamageTaken {
            "Sources"
        } else {
            "Targets"
        }));
        let target_total: u64 = actor
            .targets
            .iter()
            .filter(|entry| entry.metric == view && matches_target(entry, target))
            .map(|entry| entry.amount)
            .sum();
        for entry in actor
            .targets
            .iter()
            .filter(|entry| entry.metric == view && matches_target(entry, target))
        {
            content.append(&self.breakdown_row(actor, entry, target_total));
        }
        self.set_content(&content);
    }

    /// One breakdown line: `name … amount share% hits`, sharing the ranking
    /// row visual with the fill proportional to the share.
    fn breakdown_row(
        &self,
        actor: &ProjectedActor,
        entry: &ProjectedEntry,
        total: u64,
    ) -> gtk4::Overlay {
        let share = if total == 0 {
            0.0
        } else {
            entry.amount as f64 / total as f64 * 100.0
        };
        let right = format!(
            "{} {:.1}% {}",
            format_compact(entry.amount),
            share,
            entry.hits
        );
        let row = fill_line(
            self.class_for(&actor.guid),
            &entry.key,
            &right,
            share / 100.0,
        );
        row.add_css_class("wr-meter-row");
        row
    }

    fn open_breakdown(self: &Rc<Self>, guid: String) {
        self.breakdown.replace(Some(guid));
        self.refresh();
    }
    fn class_for(&self, guid: &str) -> Option<&'static str> {
        let entry = self.entry.borrow();
        entry
            .as_ref()?
            .spec_by_guid
            .get(guid)
            .and_then(|spec| class_css_class(*spec))
    }

    /// Replace the scroller content; the empty state label is shown only by
    /// `show_empty`.
    fn set_content(&self, content: &impl IsA<gtk4::Widget>) {
        self.clear_content();
        self.content.append(content);
    }

    fn show_empty(&self, message: &str) {
        self.clear_content();
        self.empty_label.set_text(message);
        self.content.append(&self.empty_label);
    }

    fn clear_content(&self) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
    }

    /// Grip drag: resize the root from the captured geometry, clamped to the
    /// current viewport. The margins preserve the top-left until the grip
    /// reaches the viewport edge, after which growth continues left/up.
    /// Sets the request and margins directly: `move_to` clamps against the
    /// stale allocation, which only the relayout this request queues
    /// updates.
    fn resize_to(
        &self,
        margin_end: i32,
        margin_bottom: i32,
        width: i32,
        height: i32,
        offset_x: f64,
        offset_y: f64,
    ) {
        let Some((viewport_width, viewport_height)) = self.viewport_size() else {
            return;
        };
        if viewport_width <= 0 || viewport_height <= 0 {
            return;
        }
        let target_width = (width as f64 + offset_x)
            .clamp(
                f64::from(MIN_WIDTH.min(viewport_width)),
                f64::from(viewport_width),
            )
            .round() as i32;
        let target_height = (height as f64 + offset_y)
            .clamp(
                f64::from(MIN_HEIGHT.min(viewport_height)),
                f64::from(viewport_height),
            )
            .round() as i32;
        let end =
            (margin_end - (target_width - width)).clamp(0, (viewport_width - target_width).max(0));
        let bottom = (margin_bottom - (target_height - height))
            .clamp(0, (viewport_height - target_height).max(0));
        self.root.set_size_request(target_width, target_height);
        self.root.set_margin_end(end);
        self.root.set_margin_bottom(bottom);
        self.desired_size.set(Some((target_width, target_height)));
    }

    /// Pixel margins from a drag, clamped so the meter stays inside the
    /// overlay allocation.
    fn move_to(&self, margin_end: f64, margin_bottom: f64) {
        let Some((width, height)) = self.viewport_size() else {
            return;
        };
        let max_end = (width - self.root.width()).max(0) as f64;
        let max_bottom = (height - self.root.height()).max(0) as f64;
        self.root
            .set_margin_end(margin_end.clamp(0.0, max_end) as i32);
        self.root
            .set_margin_bottom(margin_bottom.clamp(0.0, max_bottom) as i32);
    }

    /// The overlay allocation the meter is positioned in: the video overlay
    /// it was added to.
    fn viewport_size(&self) -> Option<(i32, i32)> {
        let overlay = self.root.parent().and_downcast::<gtk4::Overlay>()?;
        Some((overlay.width(), overlay.height()))
    }
}

/// One dense meter row visual: class-colored fill behind always-white labels,
/// left label expanding, right label aligned end.
fn fill_line(class: Option<&str>, left: &str, right: &str, fraction: f64) -> gtk4::Overlay {
    let fill = gtk4::ProgressBar::new();
    fill.set_show_text(false);
    fill.set_fraction(fraction.clamp(0.0, 1.0));
    fill.add_css_class("wr-meter-fill");
    if let Some(class) = class {
        fill.add_css_class(class);
    }
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    line.set_margin_start(6);
    line.set_margin_end(6);
    let left_label = gtk4::Label::new(Some(left));
    left_label.set_xalign(0.0);
    left_label.set_hexpand(true);
    left_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let right_label = gtk4::Label::new(Some(right));
    right_label.set_xalign(1.0);
    right_label.add_css_class("numeric");
    line.append(&left_label);
    line.append(&right_label);
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&fill));
    overlay.add_overlay(&line);
    overlay
}

fn death_event_row(kind: MeterDeathEventKind, left: &str, right: &str) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    row.add_css_class("wr-meter-death-event");
    row.add_css_class(match kind {
        MeterDeathEventKind::Damage => "damage",
        MeterDeathEventKind::Healing => "healing",
    });
    let left_label = gtk4::Label::new(Some(left));
    left_label.set_xalign(0.0);
    left_label.set_hexpand(true);
    left_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let right_label = gtk4::Label::new(Some(right));
    right_label.set_xalign(1.0);
    right_label.add_css_class("numeric");
    row.append(&left_label);
    row.append(&right_label);
    row
}

fn heading(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class("caption-heading");
    label.set_xalign(0.0);
    label
}

/// A stateful string action for the meter action group.
fn stateful_action(name: &str, state: &str) -> gtk4::gio::SimpleAction {
    gtk4::gio::SimpleAction::new_stateful(
        name,
        Some(gtk4::glib::VariantTy::STRING),
        &state.to_variant(),
    )
}
