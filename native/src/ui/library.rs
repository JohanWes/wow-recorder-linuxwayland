// SPDX-License-Identifier: GPL-3.0-or-later

//! The library view: one virtualized `GtkColumnView` with the category column
//! set, suggestion-chip + paired-date filtering, native multiselect, and the
//! local protect/tag/reveal/delete actions (WR-010).
//!
//! Model pipeline (GTK-native, no bespoke collection):
//!
//! `gio::ListStore` (one boxed row per correlated activity of the selected
//! category, newest first) → `FilterListModel` (chips + date) → `SortListModel`
//! (the column-view sorter) → `MultiSelection` → `ColumnView`.
//!
//! Row data is immutable `Rc<RowModel>` wrapped in `glib::BoxedAnyObject`; the
//! coordinator snapshot is authoritative, so a changed snapshot rebuilds the
//! store rather than mutating widgets in place. The GTK thread does no file or
//! parsing work here.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gtk4::glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use warcraft_recorder::coordinator::{AppSnapshot, Command};
use warcraft_recorder::domain::{ActivityDetails, Category, LibraryEntry, Outcome, RecordingId};

use super::filters::{self, Chip};
use super::{ActionSink, ShellAction};

/// What the player area needs when a single row is chosen. WR-011 turns this
/// into an actual playback load; today the shell only shows the title.
// ponytail: `media_path`/`viewpoints` are the WR-011 player seam the ticket
// mandates ("load the preferred/default local POV"); unread until that lands.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Selection {
    pub id: RecordingId,
    pub title: String,
    pub media_path: PathBuf,
    /// Correlated local POV ids (primary first) for the viewpoint selector.
    pub viewpoints: Vec<RecordingId>,
}

/// Column families as WR-000 records them. The selected category maps to one
/// family which decides the column set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Raid,
    Dungeon,
    Pvp,
    Clip,
    Manual,
}

fn family_of(category: &Category) -> Family {
    match category {
        Category::Raids => Family::Raid,
        Category::MythicPlus => Family::Dungeon,
        Category::TwoVTwo
        | Category::ThreeVThree
        | Category::FiveVFive
        | Category::Skirmish
        | Category::SoloShuffle
        | Category::Battlegrounds => Family::Pvp,
        Category::Clip => Family::Clip,
        Category::Manual | Category::Unknown(_) => Family::Manual,
    }
}

/// Everything one table row displays, precomputed off the GTK thread's hot
/// path so factory bind callbacks are pure field reads.
struct RowModel {
    id: RecordingId,
    media_path: PathBuf,
    /// Primary + correlated POV ids, the target of protect/delete.
    correlated_ids: Vec<RecordingId>,
    protected: bool,
    all_protected: bool,
    tag: Option<String>,
    details: String,
    result: String,
    date_ms: i64,
    duration_ms: u64,
    // Family-specific display fields; only the family's columns read them.
    encounter: String,
    place: String,
    pull: String,
    difficulty: String,
    difficulty_order: u8,
    level: i64,
    affixes: String,
    kind: String,
    source: String,
    outcome_order: u8,
    /// The recording player's class CSS class (from spec id), for the
    /// class-colored Details name.
    class_css: Option<&'static str>,
    /// Union of suggestion chips across primary + POVs, for AND filtering.
    combined: BTreeSet<Chip>,
}

fn row_of(item: &glib::Object) -> Rc<RowModel> {
    item.downcast_ref::<BoxedAnyObject>()
        .expect("library rows are BoxedAnyObject")
        .borrow::<Rc<RowModel>>()
        .clone()
}

fn outcome_rank(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Win | Outcome::Complete => 0,
        Outcome::Loss | Outcome::Abandoned => 1,
        Outcome::Unknown => 2,
    }
}

fn difficulty_rank(id: Option<u32>) -> u8 {
    match id {
        Some(17) => 0, // LFR
        Some(14) => 1, // Normal
        Some(15) => 2, // Heroic
        Some(16) => 3, // Mythic
        _ => 4,
    }
}

fn raid_difficulty_label(id: Option<u32>, stored: Option<&str>) -> String {
    if let Some(stored) = stored.filter(|value| !value.is_empty()) {
        return stored.to_owned();
    }
    match id {
        Some(17) => "LFR",
        Some(14) => "Normal",
        Some(15) => "Heroic",
        Some(16) => "Mythic",
        _ => "",
    }
    .to_owned()
}

fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn format_date(unix_ms: i64) -> String {
    glib::DateTime::from_unix_local(unix_ms / 1000)
        .and_then(|dt| dt.format("%Y-%m-%d %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn details_line(entry: &LibraryEntry) -> String {
    match &entry.player {
        Some(player) if !player.name.is_empty() => player.name.clone(),
        _ => entry.title.clone(),
    }
}

fn result_label(entry: &LibraryEntry, family: Family) -> String {
    match (&entry.details, family) {
        (ActivityDetails::Raid { .. }, _) => match entry.outcome {
            Outcome::Win => "Kill",
            _ => "Wipe",
        }
        .to_owned(),
        (ActivityDetails::Dungeon { upgrade_level, .. }, _) => {
            if entry.outcome != Outcome::Complete {
                "Abandoned".to_owned()
            } else if upgrade_level.is_some_and(|level| level > 0) {
                format!("Timed +{}", upgrade_level.unwrap_or(0))
            } else {
                "Depleted".to_owned()
            }
        }
        (
            ActivityDetails::SoloRounds {
                rounds_won,
                rounds_played,
                ..
            },
            _,
        ) => match (rounds_won, rounds_played) {
            (Some(won), Some(played)) => format!("{won}/{played}"),
            _ => win_loss(entry.outcome),
        },
        _ => win_loss(entry.outcome),
    }
}

fn win_loss(outcome: Outcome) -> String {
    match outcome {
        Outcome::Win => "Win",
        Outcome::Loss => "Loss",
        _ => "",
    }
    .to_owned()
}

fn affixes_label(affixes: &[u32]) -> String {
    affixes
        .iter()
        .map(|id| filters::affix_name(*id).unwrap_or_else(|| id.to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build one row per correlated activity of `category`, newest first.
fn build_rows(snapshot: &AppSnapshot, category: &Category) -> Vec<Rc<RowModel>> {
    let by_id: HashMap<&RecordingId, &LibraryEntry> = snapshot
        .entries
        .iter()
        .map(|entry| (&entry.id, entry))
        .collect();
    let family = family_of(category);

    let mut rows: Vec<Rc<RowModel>> = snapshot
        .correlations
        .iter()
        .filter(|correlation| {
            by_id
                .get(&correlation.primary_id)
                .is_some_and(|entry| &entry.category == category)
        })
        .map(|correlation| {
            let primary = by_id
                .get(&correlation.primary_id)
                .copied()
                .expect("correlation primary is in the snapshot");
            let povs: Vec<&LibraryEntry> = correlation
                .local_pov_ids
                .iter()
                .filter_map(|id| by_id.get(id).copied())
                .collect();

            let mut correlated_ids = vec![primary.id.clone()];
            correlated_ids.extend(correlation.local_pov_ids.iter().cloned());

            let all_protected = primary.protected && povs.iter().all(|entry| entry.protected);
            let combined =
                filters::combined_suggestions(std::iter::once(primary).chain(povs.iter().copied()));

            let (
                encounter,
                place,
                pull,
                difficulty,
                difficulty_order,
                level,
                affixes,
                kind,
                source,
            ) = category_fields(primary, family);

            Rc::new(RowModel {
                id: primary.id.clone(),
                media_path: primary.media_path.clone(),
                correlated_ids,
                protected: primary.protected,
                all_protected,
                tag: primary.tag.clone(),
                details: details_line(primary),
                result: result_label(primary, family),
                date_ms: primary.start_unix_ms,
                duration_ms: primary.duration_ms,
                encounter,
                place,
                pull,
                difficulty,
                difficulty_order,
                level,
                affixes,
                kind,
                source,
                outcome_order: outcome_rank(primary.outcome),
                class_css: primary
                    .player
                    .as_ref()
                    .and_then(|player| player.spec_id)
                    .and_then(filters::class_css_class),
                combined,
            })
        })
        .collect();

    // Default order: newest first, matching the legacy reverse-chronological
    // scan/correlator. An active column sort overrides this in the sort model.
    rows.sort_by_key(|row| std::cmp::Reverse(row.date_ms));
    rows
}

type CategoryFields = (
    String,
    String,
    String,
    String,
    u8,
    i64,
    String,
    String,
    String,
);

fn category_fields(entry: &LibraryEntry, family: Family) -> CategoryFields {
    let mut encounter = String::new();
    let mut place = String::new();
    let mut pull = String::new();
    let mut difficulty = String::new();
    let mut difficulty_order = 4;
    let mut level = 0;
    let mut affixes = String::new();
    let mut kind = String::new();
    let mut source = String::new();

    match (&entry.details, family) {
        (
            ActivityDetails::Raid {
                zone_name,
                encounter_name,
                difficulty_id,
                difficulty: stored,
                pull: pull_number,
                ..
            },
            Family::Raid,
        ) => {
            encounter = encounter_name.clone().unwrap_or_default();
            place = zone_name.clone().unwrap_or_default();
            pull = pull_number
                .map(|value| value.to_string())
                .unwrap_or_default();
            difficulty = raid_difficulty_label(*difficulty_id, stored.as_deref());
            difficulty_order = difficulty_rank(*difficulty_id);
        }
        (
            ActivityDetails::Dungeon {
                dungeon_name,
                keystone_level,
                affixes: affix_ids,
                ..
            },
            Family::Dungeon,
        ) => {
            place = dungeon_name.clone().unwrap_or_default();
            level = keystone_level.map(i64::from).unwrap_or(0);
            affixes = affixes_label(affix_ids);
        }
        (ActivityDetails::ArenaOrBattleground { map_name, .. }, Family::Pvp) => {
            place = map_name.clone().unwrap_or_default();
        }
        (ActivityDetails::SoloRounds { map_name, .. }, Family::Pvp) => {
            place = map_name.clone().unwrap_or_default();
        }
        (
            ActivityDetails::Clip {
                source_category,
                source_title,
                ..
            },
            Family::Clip,
        ) => {
            kind = super::category_label(source_category).to_owned();
            source = source_title.clone().unwrap_or_default();
        }
        (_, Family::Manual) => {
            kind = "Manual".to_owned();
        }
        _ => {}
    }

    (
        encounter,
        place,
        pull,
        difficulty,
        difficulty_order,
        level,
        affixes,
        kind,
        source,
    )
}

/// Shared mutable view state referenced by every widget callback.
struct State {
    selected_chips: RefCell<Vec<Chip>>,
    date_range: Cell<Option<(i64, i64)>>,
    available: RefCell<Vec<Chip>>,
    suggestions_dirty: Cell<bool>,
    suggestions_active: Cell<bool>,
    rebuilding_store: Cell<bool>,
    category: RefCell<Option<Category>>,
    signature: Cell<u64>,
    /// A protect/tag/delete is in flight; the bulk bar stays disabled until the
    /// authoritative snapshot arrives.
    mutation_pending: Cell<bool>,
    /// The authoritative index objects used to build the current rows.  Status
    /// and progress snapshots reuse these Arcs, so retaining them lets the GTK
    /// thread avoid rebuilding row metadata for unrelated updates.
    entries: RefCell<Option<Arc<Vec<LibraryEntry>>>>,
    correlations: RefCell<Option<Arc<Vec<warcraft_recorder::domain::CorrelatedActivity>>>>,
}

pub struct Library {
    pub widget: gtk4::Box,
    inner: Rc<Inner>,
}

struct Inner {
    sink: ActionSink,
    on_select: Rc<dyn Fn(Option<Selection>)>,
    store: gio::ListStore,
    filter: gtk4::CustomFilter,
    filter_model: gtk4::FilterListModel,
    selection: gtk4::MultiSelection,
    column_view: gtk4::ColumnView,
    stack: gtk4::Stack,
    chips_box: gtk4::Box,
    chips_row: gtk4::Box,
    search: gtk4::SearchEntry,
    suggestion_popover: gtk4::Popover,
    suggestion_model: gio::ListStore,
    suggestion_list: gtk4::ListView,
    date_label: gtk4::Label,
    from_calendar: gtk4::Calendar,
    to_calendar: gtk4::Calendar,
    bulk_bar: gtk4::Revealer,
    bulk_count: gtk4::Label,
    protect_button: gtk4::Button,
    delete_button: gtk4::Button,
    state: State,
}

impl Library {
    pub fn new(sink: ActionSink, on_select: Rc<dyn Fn(Option<Selection>)>) -> Self {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        let filter = gtk4::CustomFilter::new(|_| true);
        let filter_model = gtk4::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sort_model = gtk4::SortListModel::new(Some(filter_model.clone()), None::<gtk4::Sorter>);
        let selection = gtk4::MultiSelection::new(Some(sort_model.clone()));
        let column_view = gtk4::ColumnView::new(Some(selection.clone()));
        column_view.set_hexpand(true);
        column_view.set_vexpand(true);
        column_view.add_css_class("data-table");
        sort_model.set_sorter(column_view.sorter().as_ref());

        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_child(Some(&column_view));
        scroll.set_vexpand(true);
        // Keep the compact overlay scrollbar used elsewhere in the shell.
        scroll.set_overlay_scrolling(true);

        let empty = adw::StatusPage::new();
        empty.set_title("No recordings in this category");
        empty.set_description(Some("Recordings in the selected category appear here."));
        empty.set_vexpand(true);
        let filtered_empty = adw::StatusPage::new();
        filtered_empty.set_title("No matches");
        filtered_empty.set_description(Some(
            "The selected chips and date range removed every recording.",
        ));
        filtered_empty.set_vexpand(true);

        let stack = gtk4::Stack::new();
        stack.add_named(&scroll, Some("table"));
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&filtered_empty, Some("filtered-empty"));
        stack.set_visible_child_name("empty");

        // Toolbar: search entry, date-range popover, clear.
        let search = gtk4::SearchEntry::new();
        search.set_placeholder_text(Some("Search recordings"));
        search.set_hexpand(true);
        let suggestion_model = gio::ListStore::new::<BoxedAnyObject>();
        let suggestion_selection = gtk4::NoSelection::new(Some(suggestion_model.clone()));
        let suggestion_list =
            gtk4::ListView::new(Some(suggestion_selection), Some(suggestion_factory()));
        suggestion_list.set_single_click_activate(true);
        // Search owns keyboard focus (Tab/Enter acceptance); pointer activation
        // must not steal it and collapse the popover before the click arrives.
        suggestion_list.set_focusable(false);
        suggestion_list.add_css_class("boxed-list");
        let suggestion_scroll = gtk4::ScrolledWindow::new();
        suggestion_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        suggestion_scroll.set_max_content_height(280);
        suggestion_scroll.set_propagate_natural_height(true);
        suggestion_scroll.set_child(Some(&suggestion_list));
        let suggestion_popover = gtk4::Popover::new();
        suggestion_popover.set_parent(&search);
        suggestion_popover.set_autohide(false);
        suggestion_popover.set_has_arrow(false);
        suggestion_popover.set_position(gtk4::PositionType::Bottom);
        suggestion_popover.set_child(Some(&suggestion_scroll));

        let date = build_date_control();
        let date_label = date.label.clone();
        let from_calendar = date.from_calendar.clone();
        let to_calendar = date.to_calendar.clone();

        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        toolbar.set_margin_top(6);
        toolbar.set_margin_bottom(6);
        toolbar.set_margin_start(12);
        toolbar.set_margin_end(12);
        toolbar.append(&search);
        toolbar.append(&date.button);

        // Active chips row (hidden until a chip or date range exists).
        let chips_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        chips_box.set_hexpand(true);
        let clear_button = gtk4::Button::with_label("Clear");
        clear_button.add_css_class("flat");
        let chips_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        chips_row.set_margin_start(12);
        chips_row.set_margin_end(12);
        chips_row.set_margin_bottom(6);
        chips_row.append(&chips_box);
        chips_row.append(&clear_button);
        chips_row.set_visible(false);

        // Bulk action bar (revealed while rows are selected).
        let bulk_count = gtk4::Label::new(Some("Selection"));
        bulk_count.set_hexpand(true);
        bulk_count.set_xalign(0.0);
        let protect_button = gtk4::Button::with_label("Protect");
        let delete_button = gtk4::Button::with_label("Delete");
        delete_button.add_css_class("destructive-action");
        let bulk_inner = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        bulk_inner.set_margin_top(6);
        bulk_inner.set_margin_bottom(6);
        bulk_inner.set_margin_start(12);
        bulk_inner.set_margin_end(12);
        bulk_inner.append(&bulk_count);
        bulk_inner.append(&protect_button);
        bulk_inner.append(&delete_button);
        let bulk_bar = gtk4::Revealer::new();
        bulk_bar.set_child(Some(&bulk_inner));
        bulk_bar.set_reveal_child(false);

        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.append(&toolbar);
        widget.append(&chips_row);
        widget.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        widget.append(&stack);
        widget.append(&bulk_bar);

        let inner = Rc::new(Inner {
            sink,
            on_select,
            store,
            filter,
            filter_model,
            selection,
            column_view,
            stack,
            chips_box,
            chips_row,
            search,
            suggestion_popover,
            suggestion_model,
            suggestion_list,
            date_label,
            from_calendar,
            to_calendar,
            bulk_bar,
            bulk_count,
            protect_button,
            delete_button,
            state: State {
                selected_chips: RefCell::new(Vec::new()),
                date_range: Cell::new(None),
                available: RefCell::new(Vec::new()),
                suggestions_dirty: Cell::new(true),
                suggestions_active: Cell::new(false),
                rebuilding_store: Cell::new(false),
                category: RefCell::new(None),
                signature: Cell::new(0),
                mutation_pending: Cell::new(false),
                entries: RefCell::new(None),
                correlations: RefCell::new(None),
            },
        });

        inner.install_filter();
        inner.connect_search();
        inner.connect_selection();
        inner.connect_bulk_actions(&clear_button);
        inner.connect_date_apply(&date);

        Self { widget, inner }
    }

    /// Rebuild from the authoritative snapshot. Cheap when nothing relevant
    /// changed: a signature guards the store rebuild.
    pub fn apply(&self, snapshot: &AppSnapshot) {
        self.inner.apply(snapshot);
    }
}

impl Inner {
    fn install_filter(self: &Rc<Self>) {
        let this = Rc::clone(self);
        self.filter.set_filter_func(move |item| {
            let row = row_of(item);
            let chips = this.state.selected_chips.borrow();
            filters::row_matches(
                &row.combined,
                row.date_ms,
                &chips,
                this.state.date_range.get(),
            )
        });
        // React to filter results: empty state, suggestions, and the bulk bar.
        let this = Rc::clone(self);
        self.filter_model.connect_items_changed(move |_, _, _, _| {
            if !this.state.rebuilding_store.get() {
                this.after_filter_change();
            }
        });
    }

    fn connect_search(self: &Rc<Self>) {
        let this = Rc::clone(self);
        self.search.connect_search_changed(move |_| {
            this.refresh_suggestion_popover();
        });
        let this = Rc::clone(self);
        self.search.connect_has_focus_notify(move |search| {
            this.state.suggestions_active.set(search.has_focus());
            this.refresh_suggestion_popover();
        });
        // Enter accepts the top narrowed suggestion, matching legacy Tab/Enter.
        let this = Rc::clone(self);
        self.search.connect_activate(move |_| {
            this.accept_first_suggestion();
        });
        // Tab also accepts the active suggestion instead of moving focus.
        let key = gtk4::EventControllerKey::new();
        let this = Rc::clone(self);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk4::gdk::Key::Tab && !this.state.available_narrowed().is_empty() {
                this.accept_first_suggestion();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.search.add_controller(key);

        let this = Rc::clone(self);
        self.suggestion_list.connect_activate(move |_, position| {
            if let Some(item) = this.suggestion_model.item(position) {
                this.add_chip(chip_of(&item));
            }
        });
    }

    fn connect_selection(self: &Rc<Self>) {
        let this = Rc::clone(self);
        self.selection.connect_selection_changed(move |_, _, _| {
            this.after_selection_change();
        });
    }

    fn connect_bulk_actions(self: &Rc<Self>, clear_button: &gtk4::Button) {
        let this = Rc::clone(self);
        self.protect_button.connect_clicked(move |_| {
            this.bulk_set_protected();
        });
        let this = Rc::clone(self);
        self.delete_button.connect_clicked(move |_| {
            this.confirm_delete(this.selected_rows());
        });
        let this = Rc::clone(self);
        clear_button.connect_clicked(move |_| {
            this.clear_filters();
        });
    }

    fn connect_date_apply(self: &Rc<Self>, date: &DateControl) {
        let popover = date.button.popover().expect("date control has a popover");
        let this = Rc::clone(self);
        let pop = popover.clone();
        date.apply.connect_clicked(move |_| {
            this.apply_date_range();
            pop.popdown();
        });
        let this = Rc::clone(self);
        date.clear.connect_clicked(move |_| {
            this.clear_date_range();
            popover.popdown();
        });
    }

    // --- filtering feedback -------------------------------------------------

    fn after_filter_change(self: &Rc<Self>) {
        self.state.suggestions_dirty.set(true);
        if self.state.suggestions_active.get() {
            self.refresh_available_suggestions();
        }
        self.update_stack();
        self.refresh_suggestion_popover();
    }

    fn update_stack(&self) {
        let name = if self.store.n_items() == 0 {
            "empty"
        } else if self.filter_model.n_items() == 0 {
            "filtered-empty"
        } else {
            "table"
        };
        self.stack.set_visible_child_name(name);
    }

    /// Suggestions come from the currently *filtered* rows (WR-000), deduped by
    /// label and minus the already-selected chips.
    fn refresh_available_suggestions(&self) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut chips = Vec::new();
        for index in 0..self.filter_model.n_items() {
            if let Some(item) = self.filter_model.item(index) {
                let row = row_of(&item);
                for chip in &row.combined {
                    if seen.insert(chip.label.clone()) {
                        chips.push(chip.clone());
                    }
                }
            }
        }
        chips.sort_unstable();
        *self.state.available.borrow_mut() = chips;
        self.state.suggestions_dirty.set(false);
    }

    fn refresh_suggestion_popover(self: &Rc<Self>) {
        if !self.state.suggestions_active.get() {
            self.suggestion_popover.popdown();
            return;
        }
        if self.state.suggestions_dirty.get() {
            self.refresh_available_suggestions();
        }
        self.rebuild_suggestion_model(&self.search.text());
        if self.search.has_focus() && self.suggestion_model.n_items() > 0 {
            self.suggestion_popover.popup();
        } else if self.suggestion_model.n_items() == 0 {
            self.suggestion_popover.popdown();
        }
    }

    fn rebuild_suggestion_model(&self, query: &str) {
        let suggestions: Vec<BoxedAnyObject> = self
            .state
            .available_narrowed_with(query)
            .into_iter()
            .map(BoxedAnyObject::new)
            .collect();
        self.suggestion_model
            .splice(0, self.suggestion_model.n_items(), &suggestions);
    }

    fn accept_first_suggestion(self: &Rc<Self>) {
        if let Some(chip) = self
            .state
            .available_narrowed_with(&self.search.text())
            .first()
        {
            self.add_chip(chip.clone());
        }
    }

    // --- chips and dates ----------------------------------------------------

    fn add_chip(self: &Rc<Self>, chip: Chip) {
        {
            let mut selected = self.state.selected_chips.borrow_mut();
            if selected.iter().any(|existing| existing.label == chip.label) {
                return;
            }
            selected.push(chip);
        }
        self.search.set_text("");
        self.refilter();
    }

    fn remove_chip(self: &Rc<Self>, label: &str) {
        self.state
            .selected_chips
            .borrow_mut()
            .retain(|chip| chip.label != label);
        self.refilter();
    }

    fn clear_filters(self: &Rc<Self>) {
        self.state.selected_chips.borrow_mut().clear();
        self.state.date_range.set(None);
        self.search.set_text("");
        self.from_calendar
            .select_day(&glib::DateTime::now_local().unwrap());
        self.to_calendar
            .select_day(&glib::DateTime::now_local().unwrap());
        self.date_label.set_text("Date range");
        self.refilter();
    }

    fn clear_date_range(self: &Rc<Self>) {
        self.state.date_range.set(None);
        self.date_label.set_text("Date range");
        self.refilter();
    }

    fn apply_date_range(self: &Rc<Self>) {
        let from = self.from_calendar.date();
        let to = self.to_calendar.date();
        // Inclusive local-day boundaries: 00:00:00.000 to 23:59:59.999. The end
        // is the start of the following local day minus a millisecond, so DST
        // days (23 or 25 hours) stay exactly one calendar day wide.
        let start = day_start_ms(&from);
        let end = to
            .add_days(1)
            .map(|next| day_start_ms(&next) - 1)
            .unwrap_or_else(|_| day_start_ms(&to) + 86_400_000 - 1);
        if start <= end {
            self.state.date_range.set(Some((start, end)));
            self.date_label.set_text(&format!(
                "{} – {}",
                from.format("%Y-%m-%d").unwrap_or_default(),
                to.format("%Y-%m-%d").unwrap_or_default()
            ));
        }
        self.refilter();
    }

    fn refilter(self: &Rc<Self>) {
        self.filter.changed(gtk4::FilterChange::Different);
        self.rebuild_chip_row();
        self.after_filter_change();
    }

    fn rebuild_chip_row(self: &Rc<Self>) {
        while let Some(child) = self.chips_box.first_child() {
            self.chips_box.remove(&child);
        }
        let chips = self.state.selected_chips.borrow().clone();
        for chip in &chips {
            let button = chip_pill(chip);
            let this = Rc::clone(self);
            let label = chip.label.clone();
            button.connect_clicked(move |_| this.remove_chip(&label));
            self.chips_box.append(&button);
        }
        let has_filter = !chips.is_empty() || self.state.date_range.get().is_some();
        self.chips_row.set_visible(has_filter);
    }

    // --- selection and player load -----------------------------------------

    fn after_selection_change(self: &Rc<Self>) {
        let selected = self.selected_rows();
        let count = selected.len();
        // Load the sole selection into the player; multiselect does not load.
        if count == 1 {
            let row = &selected[0];
            (self.on_select)(Some(Selection {
                id: row.id.clone(),
                title: row.details.clone(),
                media_path: row.media_path.clone(),
                viewpoints: row.correlated_ids.clone(),
            }));
        }
        self.update_bulk_bar(&selected);
    }

    fn selected_rows(&self) -> Vec<Rc<RowModel>> {
        let bitset = self.selection.selection();
        let mut rows = Vec::new();
        for index in 0..self.selection.n_items() {
            if bitset.contains(index)
                && let Some(item) = self.selection.item(index)
            {
                rows.push(row_of(&item));
            }
        }
        rows
    }

    fn update_bulk_bar(&self, selected: &[Rc<RowModel>]) {
        if selected.is_empty() {
            self.bulk_bar.set_reveal_child(false);
            return;
        }
        self.bulk_bar.set_reveal_child(true);
        let rows = selected.len();
        self.bulk_count
            .set_text(&format!("{rows} recording{} selected", plural(rows)));
        // Legacy rule: unless every selected viewpoint is protected, the action
        // is Protect; only an all-protected selection unprotects.
        let all_protected = selected.iter().all(|row| row.all_protected);
        self.protect_button.set_label(if all_protected {
            "Unprotect"
        } else {
            "Protect"
        });
        let pending = self.state.mutation_pending.get();
        self.protect_button.set_sensitive(!pending);
        self.delete_button.set_sensitive(!pending);
    }

    fn bulk_set_protected(self: &Rc<Self>) {
        let selected = self.selected_rows();
        if selected.is_empty() {
            return;
        }
        let value = !selected.iter().all(|row| row.all_protected);
        let ids = viewpoint_ids(&selected);
        self.send_mutation(Command::SetProtected { ids, value });
    }

    fn confirm_delete(self: &Rc<Self>, selected: Vec<Rc<RowModel>>) {
        if selected.is_empty() {
            return;
        }
        let ids = viewpoint_ids(&selected);
        let rows = selected.len();
        let body = format!(
            "Delete {rows} recording{} and their {} viewpoint file{}? This permanently \
             removes the media files and their metadata sidecars.",
            plural(rows),
            ids.len(),
            plural(ids.len()),
        );
        let dialog = adw::AlertDialog::new(Some("Delete recordings"), Some(&body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        dialog.connect_response(None, move |_, response| {
            if response == "delete" {
                this.send_mutation(Command::Delete { ids: ids.clone() });
            }
        });
        dialog.present(Some(&self.widget_root()));
    }

    fn send_mutation(self: &Rc<Self>, command: Command) {
        if (self.sink)(ShellAction::Command(command)) {
            self.state.mutation_pending.set(true);
            self.protect_button.set_sensitive(false);
            self.delete_button.set_sensitive(false);
        }
    }

    // --- per-row actions ----------------------------------------------------

    fn toggle_protect(self: &Rc<Self>, row: &RowModel) {
        // A single-row star applies to that activity's viewpoints, using the
        // same all-protected toggle rule as the bulk bar.
        self.send_mutation(Command::SetProtected {
            ids: row.correlated_ids.clone(),
            value: !row.all_protected,
        });
    }

    fn edit_tag(self: &Rc<Self>, row: &RowModel) {
        let entry = gtk4::Entry::new();
        entry.set_max_length(1024);
        entry.set_activates_default(true);
        if let Some(tag) = &row.tag {
            entry.set_text(tag);
        }
        let dialog = adw::AlertDialog::new(Some("Edit tag"), None);
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("save", "Save");
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        let id = row.id.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "save" {
                // The entry already caps input at 1024 characters (the legacy
                // limit); trim and let storage clear the tag when it is empty.
                this.send_mutation(Command::SetTag {
                    id: id.clone(),
                    tag: entry.text().trim().to_owned(),
                });
            }
        });
        dialog.present(Some(&self.widget_root()));
    }

    fn reveal(&self, row: &RowModel) {
        let launcher = gtk4::FileLauncher::new(Some(&gio::File::for_path(&row.media_path)));
        let parent = self.widget_root().root().and_downcast::<gtk4::Window>();
        launcher.open_containing_folder(parent.as_ref(), None::<&gio::Cancellable>, |result| {
            if let Err(error) = result {
                tracing::warn!(%error, "could not reveal the recording");
            }
        });
    }

    fn widget_root(&self) -> gtk4::Widget {
        self.stack.clone().upcast()
    }

    // --- snapshot application ----------------------------------------------

    fn apply(self: &Rc<Self>, snapshot: &AppSnapshot) {
        self.state.mutation_pending.set(false);
        let category = snapshot.config.interface.selected_category.clone();
        let category_changed = self.state.category.borrow().as_ref() != Some(&category);
        if category_changed {
            *self.state.category.borrow_mut() = Some(category.clone());
            self.reset_for_category(&category);
        }

        let index_changed = self
            .state
            .entries
            .borrow()
            .as_ref()
            .is_none_or(|entries| !Arc::ptr_eq(entries, &snapshot.entries))
            || self
                .state
                .correlations
                .borrow()
                .as_ref()
                .is_none_or(|correlations| !Arc::ptr_eq(correlations, &snapshot.correlations));
        if !category_changed && !index_changed {
            // Progress, recorder-state, and active-timeline snapshots do not
            // change the library.  In particular, do not rebuild suggestion
            // sets or touch the virtualized store while FFmpeg reports work.
            self.state.mutation_pending.set(false);
            self.update_bulk_bar(&self.selected_rows());
            return;
        }
        *self.state.entries.borrow_mut() = Some(Arc::clone(&snapshot.entries));
        *self.state.correlations.borrow_mut() = Some(Arc::clone(&snapshot.correlations));

        let rows = build_rows(snapshot, &category);
        let signature = rows_signature(&rows);
        if !category_changed && signature == self.state.signature.get() {
            // Nothing the table shows changed; re-enable the bulk bar only.
            self.update_bulk_bar(&self.selected_rows());
            return;
        }
        self.state.signature.set(signature);

        // Remember the selection so a snapshot-driven rebuild (tag/protect/new
        // finalize) does not silently jump the player to a different recording.
        let previously: HashSet<RecordingId> = self
            .selected_rows()
            .iter()
            .map(|row| row.id.clone())
            .collect();

        let rows: Vec<BoxedAnyObject> = rows.into_iter().map(BoxedAnyObject::new).collect();
        self.state.rebuilding_store.set(true);
        self.store.splice(0, self.store.n_items(), &rows);
        self.state.rebuilding_store.set(false);
        self.after_filter_change();

        // Re-select the surviving rows. If none survive — a fresh category, or
        // the selection was deleted/filtered out — open the newest by default,
        // matching the current app.
        let mut reselected = false;
        if !previously.is_empty() {
            for index in 0..self.selection.n_items() {
                if let Some(item) = self.selection.item(index)
                    && previously.contains(&row_of(&item).id)
                {
                    self.selection.select_item(index, !reselected);
                    reselected = true;
                }
            }
        }
        if !reselected && self.selection.n_items() > 0 {
            self.selection.select_item(0, true);
        }
        self.update_bulk_bar(&self.selected_rows());
    }

    fn reset_for_category(self: &Rc<Self>, category: &Category) {
        // Category change clears chips, dates, selection, and any active sort,
        // and swaps in the family's columns (WR-000 baseline).
        self.state.selected_chips.borrow_mut().clear();
        self.state.date_range.set(None);
        self.search.set_text("");
        self.date_label.set_text("Date range");
        self.rebuild_chip_row();
        self.selection.unselect_all();
        self.column_view
            .sort_by_column(None::<&gtk4::ColumnViewColumn>, gtk4::SortType::Ascending);
        self.rebuild_columns(family_of(category));
        (self.on_select)(None);
    }

    fn rebuild_columns(self: &Rc<Self>, family: Family) {
        while let Some(column) = self.column_view.columns().item(0) {
            let column = column
                .downcast::<gtk4::ColumnViewColumn>()
                .expect("column view holds columns");
            self.column_view.remove_column(&column);
        }
        for column in self.columns_for(family) {
            self.column_view.append_column(&column);
        }
    }

    fn columns_for(self: &Rc<Self>, family: Family) -> Vec<gtk4::ColumnViewColumn> {
        let mut columns = vec![self.star_column(), self.details_column()];
        match family {
            Family::Raid => {
                columns.push(text_column(
                    "Encounter",
                    true,
                    |r| r.encounter.clone(),
                    sort_by(|r| r.encounter.clone()),
                ));
                columns.push(result_column());
                columns.push(text_column(
                    "Pull",
                    false,
                    |r| r.pull.clone(),
                    sort_by(|r| r.pull.parse::<i64>().unwrap_or(0)),
                ));
                columns.push(text_column(
                    "Difficulty",
                    false,
                    |r| r.difficulty.clone(),
                    sort_by(|r| r.difficulty_order),
                ));
                columns.push(self.duration_column());
                columns.push(self.date_column());
            }
            Family::Dungeon => {
                columns.push(text_column(
                    "Dungeon",
                    true,
                    |r| r.place.clone(),
                    sort_by(|r| r.place.clone()),
                ));
                columns.push(result_column());
                columns.push(text_column(
                    "Level",
                    false,
                    level_label,
                    sort_by(|r| r.level),
                ));
                columns.push(text_column(
                    "Affixes",
                    false,
                    |r| r.affixes.clone(),
                    sort_by(|r| r.affixes.clone()),
                ));
                columns.push(self.duration_column());
                columns.push(self.date_column());
            }
            Family::Pvp => {
                columns.push(text_column(
                    "Map",
                    true,
                    |r| r.place.clone(),
                    sort_by(|r| r.place.clone()),
                ));
                columns.push(result_column());
                columns.push(self.duration_column());
                columns.push(self.date_column());
            }
            Family::Clip => {
                columns.push(text_column(
                    "Type",
                    false,
                    |r| r.kind.clone(),
                    sort_by(|r| r.kind.clone()),
                ));
                columns.push(text_column(
                    "Source activity",
                    true,
                    |r| r.source.clone(),
                    sort_by(|r| r.source.clone()),
                ));
                columns.push(self.duration_column());
                columns.push(self.date_column());
            }
            Family::Manual => {
                columns.push(text_column(
                    "Type",
                    true,
                    |r| r.kind.clone(),
                    sort_by(|r| r.kind.clone()),
                ));
                columns.push(self.duration_column());
                columns.push(self.date_column());
            }
        }
        columns
    }

    fn duration_column(&self) -> gtk4::ColumnViewColumn {
        text_column(
            "Duration",
            false,
            |r| format_duration(r.duration_ms),
            sort_by(|r| r.duration_ms),
        )
    }

    fn date_column(&self) -> gtk4::ColumnViewColumn {
        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let label = gtk4::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            // Keep the final text column clear of the vertical scrollbar.
            label.set_margin_end(6);
            item.downcast_ref::<gtk4::ListItem>()
                .unwrap()
                .set_child(Some(&label));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
            let label = item.child().and_downcast::<gtk4::Label>().unwrap();
            let row = row_of(&item.item().unwrap());
            label.set_text(&format_date(row.date_ms));
        });
        let column = gtk4::ColumnViewColumn::new(Some("Date"), Some(factory));
        column.set_resizable(true);
        column.set_sorter(Some(&sort_by(|r| r.date_ms)));
        column
    }

    fn star_column(self: &Rc<Self>) -> gtk4::ColumnViewColumn {
        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let button = gtk4::Button::new();
            button.add_css_class("flat");
            button.set_valign(gtk4::Align::Center);
            item.downcast_ref::<gtk4::ListItem>()
                .unwrap()
                .set_child(Some(&button));
        });
        let this = Rc::clone(self);
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
            let button = item.child().and_downcast::<gtk4::Button>().unwrap();
            let row = row_of(&item.item().unwrap());
            // Reflect the toggle action: a filled star means every correlated
            // viewpoint is protected, matching `toggle_protect`'s value.
            button.set_icon_name(if row.all_protected {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
            let label = if row.all_protected {
                "Unprotect"
            } else {
                "Protect"
            };
            button.set_tooltip_text(Some(label));
            button.update_property(&[gtk4::accessible::Property::Label(label)]);
            let this = Rc::clone(&this);
            let handler = button.connect_clicked(move |_| this.toggle_protect(&row));
            unsafe { button.set_data("wr-handler", handler) };
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
            let button = item.child().and_downcast::<gtk4::Button>().unwrap();
            if let Some(handler) =
                unsafe { button.steal_data::<glib::SignalHandlerId>("wr-handler") }
            {
                button.disconnect(handler);
            }
        });
        let column = gtk4::ColumnViewColumn::new(Some("★"), Some(factory));
        column.set_fixed_width(40);
        column
    }

    fn details_column(self: &Rc<Self>) -> gtk4::ColumnViewColumn {
        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let title = gtk4::Label::new(None);
            title.set_xalign(0.0);
            title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            let tag = gtk4::Label::new(None);
            tag.set_xalign(0.0);
            tag.add_css_class("dim-label");
            tag.add_css_class("caption");
            tag.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            let text = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            text.set_hexpand(true);
            text.set_valign(gtk4::Align::Center);
            text.append(&title);
            text.append(&tag);
            let container = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
            container.append(&text);
            item.downcast_ref::<gtk4::ListItem>()
                .unwrap()
                .set_child(Some(&container));
        });
        // Right-click menu with the full action set (protect/tag/reveal/delete).
        let this = Rc::clone(self);
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
            let container = item.child().and_downcast::<gtk4::Box>().unwrap();
            let text = container.first_child().and_downcast::<gtk4::Box>().unwrap();
            let title = text.first_child().and_downcast::<gtk4::Label>().unwrap();
            let tag = title.next_sibling().and_downcast::<gtk4::Label>().unwrap();
            let row = row_of(&item.item().unwrap());
            title.set_text(&row.details);
            if let Some(previous) = unsafe { title.steal_data::<&'static str>("wr-class") } {
                title.remove_css_class(previous);
            }
            if let Some(class) = row.class_css {
                title.add_css_class(class);
                unsafe { title.set_data("wr-class", class) };
            }
            match &row.tag {
                Some(value) => {
                    tag.set_text(value);
                    tag.set_visible(true);
                }
                None => tag.set_visible(false),
            }
            this.attach_row_menu(&container, &row);
        });
        let column = gtk4::ColumnViewColumn::new(Some("Details"), Some(factory));
        column.set_expand(true);
        column.set_resizable(true);
        column.set_sorter(Some(&sort_by(|r| r.details.clone())));
        column
    }

    fn attach_row_menu(self: &Rc<Self>, container: &gtk4::Box, row: &Rc<RowModel>) {
        if let Some(existing) = unsafe { container.steal_data::<gtk4::GestureClick>("wr-menu") } {
            container.remove_controller(&existing);
        }
        let gesture = gtk4::GestureClick::new();
        gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
        let this = Rc::clone(self);
        let row = Rc::clone(row);
        let container_weak = container.downgrade();
        gesture.connect_pressed(move |_, _, x, y| {
            let Some(container) = container_weak.upgrade() else {
                return;
            };
            let popover = this.row_menu(&row);
            popover.set_parent(&container);
            popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        });
        container.add_controller(gesture.clone());
        unsafe { container.set_data("wr-menu", gesture) };
    }

    fn row_menu(self: &Rc<Self>, row: &Rc<RowModel>) -> gtk4::Popover {
        let list = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let popover = gtk4::Popover::new();
        popover.set_child(Some(&list));
        let make = |label: &str| {
            let button = gtk4::Button::with_label(label);
            button.add_css_class("flat");
            button.set_hexpand(true);
            if let Some(child) = button.child().and_downcast::<gtk4::Label>() {
                child.set_xalign(0.0);
            }
            list.append(&button);
            button
        };
        let protect = make(if row.all_protected {
            "Unprotect"
        } else {
            "Protect"
        });
        let tag = make("Edit tag");
        let reveal = make("Reveal in folder");
        let delete = make("Delete");
        delete.add_css_class("destructive-action");

        let this = Rc::clone(self);
        let r = Rc::clone(row);
        let pop = popover.clone();
        protect.connect_clicked(move |_| {
            pop.popdown();
            this.toggle_protect(&r);
        });
        let this = Rc::clone(self);
        let r = Rc::clone(row);
        let pop = popover.clone();
        tag.connect_clicked(move |_| {
            pop.popdown();
            this.edit_tag(&r);
        });
        let this = Rc::clone(self);
        let r = Rc::clone(row);
        let pop = popover.clone();
        reveal.connect_clicked(move |_| {
            pop.popdown();
            this.reveal(&r);
        });
        let this = Rc::clone(self);
        let r = Rc::clone(row);
        let pop = popover.clone();
        delete.connect_clicked(move |_| {
            pop.popdown();
            this.confirm_delete(vec![Rc::clone(&r)]);
        });
        popover
    }
}

impl State {
    fn available_narrowed(&self) -> Vec<Chip> {
        self.available_narrowed_with("")
    }

    fn available_narrowed_with(&self, query: &str) -> Vec<Chip> {
        filters::narrow(
            &self.available.borrow(),
            query,
            &self.selected_chips.borrow(),
        )
    }
}

// --- free helpers -----------------------------------------------------------

fn viewpoint_ids(rows: &[Rc<RowModel>]) -> Vec<RecordingId> {
    let mut ids = Vec::new();
    for row in rows {
        for id in &row.correlated_ids {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    ids
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn level_label(row: &RowModel) -> String {
    if row.level > 0 {
        format!("+{}", row.level)
    } else {
        String::new()
    }
}

fn day_start_ms(date: &glib::DateTime) -> i64 {
    glib::DateTime::new(
        &glib::TimeZone::local(),
        date.year(),
        date.month(),
        date.day_of_month(),
        0,
        0,
        0.0,
    )
    .map(|start| start.to_unix() * 1000)
    .unwrap_or(0)
}

/// A cheap fingerprint of what the table displays, to skip no-op rebuilds while
/// still reacting to protect/tag/delete/finalize changes.
fn rows_signature(rows: &[Rc<RowModel>]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rows.len().hash(&mut hasher);
    for row in rows {
        row.id.as_str().hash(&mut hasher);
        row.protected.hash(&mut hasher);
        row.all_protected.hash(&mut hasher);
        row.tag.hash(&mut hasher);
        // Correlated-POV changes still refresh the player's viewpoint selector.
        row.correlated_ids.len().hash(&mut hasher);
        row.date_ms.hash(&mut hasher);
    }
    hasher.finish()
}

fn sort_by<K: Ord + 'static>(key: impl Fn(&RowModel) -> K + 'static) -> gtk4::CustomSorter {
    gtk4::CustomSorter::new(move |a, b| {
        let a = row_of(a);
        let b = row_of(b);
        gtk4::Ordering::from(key(&a).cmp(&key(&b)))
    })
}

fn text_column(
    title: &str,
    expand: bool,
    getter: impl Fn(&RowModel) -> String + 'static,
    sorter: gtk4::CustomSorter,
) -> gtk4::ColumnViewColumn {
    let getter = Rc::new(getter);
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk4::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        item.downcast_ref::<gtk4::ListItem>()
            .unwrap()
            .set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk4::Label>().unwrap();
        let row = row_of(&item.item().unwrap());
        label.set_text(&getter(&row));
    });
    let column = gtk4::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(expand);
    column.set_resizable(true);
    column.set_sorter(Some(&sorter));
    column
}

/// The Result column: same text cell as `text_column`, plus the win/loss
/// outcome color the legacy table used (label conveys the meaning; color is
/// reinforcement only).
fn result_column() -> gtk4::ColumnViewColumn {
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk4::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        item.downcast_ref::<gtk4::ListItem>()
            .unwrap()
            .set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk4::Label>().unwrap();
        let row = row_of(&item.item().unwrap());
        label.set_text(&row.result);
        label.remove_css_class("wr-result-win");
        label.remove_css_class("wr-result-loss");
        match row.outcome_order {
            0 => label.add_css_class("wr-result-win"),
            1 => label.add_css_class("wr-result-loss"),
            _ => {}
        }
    });
    let column = gtk4::ColumnViewColumn::new(Some("Result"), Some(factory));
    column.set_resizable(true);
    column.set_sorter(Some(&sort_by(|r| r.outcome_order)));
    column
}

fn chip_of(item: &glib::Object) -> Chip {
    item.downcast_ref::<BoxedAnyObject>()
        .expect("suggestions are BoxedAnyObject")
        .borrow::<Chip>()
        .clone()
}

fn suggestion_factory() -> gtk4::SignalListItemFactory {
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let icon = gtk4::Image::new();
        let label = gtk4::Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        content.set_margin_top(4);
        content.set_margin_bottom(4);
        content.set_margin_start(8);
        content.set_margin_end(8);
        content.append(&icon);
        content.append(&label);
        item.downcast_ref::<gtk4::ListItem>()
            .expect("factory item")
            .set_child(Some(&content));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk4::ListItem>().expect("factory item");
        let content = item.child().and_downcast::<gtk4::Box>().unwrap();
        let icon = content.first_child().and_downcast::<gtk4::Image>().unwrap();
        let label = icon.next_sibling().and_downcast::<gtk4::Label>().unwrap();
        let chip = chip_of(&item.item().unwrap());
        icon.set_icon_name(Some(chip.icon_name()));
        label.set_text(&chip.label);
    });
    factory
}

fn chip_pill(chip: &Chip) -> gtk4::Button {
    let icon = gtk4::Image::from_icon_name(chip.icon_name());
    let label = gtk4::Label::new(Some(&chip.label));
    let close = gtk4::Image::from_icon_name("window-close-symbolic");
    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    content.append(&icon);
    content.append(&label);
    content.append(&close);
    let button = gtk4::Button::new();
    button.set_child(Some(&content));
    button.add_css_class("wr-chip");
    button.add_css_class(chip.css_class());
    button.set_tooltip_text(Some(&format!("Remove {}", chip.label)));
    button
}

struct DateControl {
    button: gtk4::MenuButton,
    label: gtk4::Label,
    from_calendar: gtk4::Calendar,
    to_calendar: gtk4::Calendar,
    apply: gtk4::Button,
    clear: gtk4::Button,
}

fn build_date_control() -> DateControl {
    let from_calendar = gtk4::Calendar::new();
    let to_calendar = gtk4::Calendar::new();
    let from_box = labelled_calendar("From", &from_calendar);
    let to_box = labelled_calendar("To", &to_calendar);
    let calendars = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    calendars.append(&from_box);
    calendars.append(&to_box);

    // The range is opt-in: a calendar always has a selected day, so the filter
    // only applies when the user commits both endpoints with Apply. Clear
    // removes the range without touching the chip selection.
    let apply = gtk4::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    let clear = gtk4::Button::with_label("Clear");
    clear.add_css_class("flat");
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    actions.set_halign(gtk4::Align::End);
    actions.append(&clear);
    actions.append(&apply);

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    content.append(&calendars);
    content.append(&actions);
    let popover = gtk4::Popover::new();
    popover.set_child(Some(&content));

    let label = gtk4::Label::new(Some("Date range"));
    let icon = gtk4::Image::from_icon_name("x-office-calendar-symbolic");
    let button_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    button_content.append(&icon);
    button_content.append(&label);
    let button = gtk4::MenuButton::new();
    button.set_child(Some(&button_content));
    button.set_popover(Some(&popover));
    button.set_tooltip_text(Some("Filter by date range"));

    DateControl {
        button,
        label,
        from_calendar,
        to_calendar,
        apply,
        clear,
    }
}

fn labelled_calendar(title: &str, calendar: &gtk4::Calendar) -> gtk4::Box {
    let heading = gtk4::Label::new(Some(title));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    container.append(&heading);
    container.append(calendar);
    container
}

#[cfg(test)]
mod release_gate_tests {
    use super::*;
    use std::time::Instant;
    use warcraft_recorder::config::Config;
    use warcraft_recorder::domain::{RecorderStatus, StorageLimit};
    use warcraft_recorder::storage::Storage;

    fn snapshot() -> AppSnapshot {
        let root = PathBuf::from(std::env::var_os("WR015_CORPUS").expect("set WR015_CORPUS"));
        let index = Storage::new(root.clone(), root.join(".wr015-capture")).scan();
        assert_eq!(index.entries.len(), 2_000);
        AppSnapshot {
            entries: index.entries,
            correlations: index.correlations,
            category_counts: Vec::new(),
            status: RecorderStatus::WaitingForWow,
            active: None,
            config: Config::default(),
            setup_problems: Vec::new(),
            advanced_logging: Vec::new(),
            problems: Vec::new(),
            work: None,
            queued_jobs: 0,
            storage_used_bytes: 0,
            storage_limit: StorageLimit::Unlimited,
            protected_over_limit: false,
        }
    }

    fn measure(mut operation: impl FnMut()) -> (Vec<u128>, u128) {
        operation();
        let mut samples = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            operation();
            samples.push(started.elapsed().as_micros());
        }
        samples.sort_unstable();
        let median = samples[2];
        (samples, median)
    }

    #[test]
    #[ignore = "run manually with WR015_CORPUS after generating the WR-000 corpus"]
    fn measure_filter_and_sort_updates() {
        gtk4::init().expect("GTK display is required for the manual release gate");
        let mut snapshot = snapshot();
        snapshot.config.interface.selected_category = Category::MythicPlus;
        let library = Library::new(Rc::new(|_| true), Rc::new(|_| {}));
        library.apply(&snapshot);
        assert_eq!(library.inner.store.n_items(), 200);
        assert_eq!(
            library.inner.suggestion_model.n_items(),
            0,
            "a hidden popover must not materialize suggestion rows"
        );
        assert!(library.inner.state.suggestions_dirty.get());

        // Exercise the focused-search path without depending on compositor
        // focus policy in this manual harness.
        library.inner.state.suggestions_active.set(true);
        library.inner.refresh_suggestion_popover();
        assert!(!library.inner.state.available.borrow().is_empty());

        // A relevant snapshot while search is focused replaces the backing
        // store with one notification, rather than rescanning a growing model
        // once per row.
        let notifications = Rc::new(Cell::new(0u32));
        let notification_count = Rc::clone(&notifications);
        library
            .inner
            .store
            .connect_items_changed(move |_, _, _, _| {
                notification_count.set(notification_count.get() + 1);
            });
        let entries = Arc::make_mut(&mut snapshot.entries);
        entries
            .iter_mut()
            .find(|entry| entry.category == Category::MythicPlus)
            .expect("Mythic+ entry")
            .tag = Some("focused rebuild".to_owned());
        library.apply(&snapshot);
        assert_eq!(notifications.get(), 1);

        // Exercise the actual widgets and GtkFilterListModel/GtkSortListModel,
        // then enumerate the resulting selection model so lazy work is paid
        // inside each sample.
        let force_model = || {
            let count = library.inner.selection.n_items();
            for position in 0..count {
                std::hint::black_box(library.inner.selection.item(position));
            }
        };
        let mut suggestion_toggle = false;
        let (suggestion_samples, suggestion_median) = measure(|| {
            suggestion_toggle = !suggestion_toggle;
            library.inner.search.set_text(if suggestion_toggle {
                "player"
            } else {
                "dungeon"
            });
            library
                .inner
                .search
                .emit_by_name::<()>("search-changed", &[]);
            for position in 0..library.inner.suggestion_model.n_items() {
                std::hint::black_box(library.inner.suggestion_model.item(position));
            }
        });
        let normal_suggestions = library.inner.state.available.borrow().clone();
        *library.inner.state.available.borrow_mut() = (0..10_000)
            .map(|index| Chip {
                group: 200,
                label: format!(
                    "stress-{}-{index:05}",
                    if index % 2 == 0 { 'a' } else { 'b' }
                ),
            })
            .collect();
        let mut high_card_toggle = false;
        let (high_card_samples, high_card_median) = measure(|| {
            high_card_toggle = !high_card_toggle;
            library.inner.search.set_text(if high_card_toggle {
                "stress-a"
            } else {
                "stress-b"
            });
            library
                .inner
                .search
                .emit_by_name::<()>("search-changed", &[]);
            assert_eq!(library.inner.suggestion_model.n_items(), 5_000);
            for position in 0..library.inner.suggestion_model.n_items() {
                std::hint::black_box(library.inner.suggestion_model.item(position));
            }
        });
        *library.inner.state.available.borrow_mut() = normal_suggestions;
        library.inner.search.set_text("");
        let chip = library.inner.state.available.borrow()[0].clone();
        let mut chip_toggle = false;
        let (chip_samples, chip_median) = measure(|| {
            chip_toggle = !chip_toggle;
            let mut selected = library.inner.state.selected_chips.borrow_mut();
            selected.clear();
            if chip_toggle {
                selected.push(chip.clone());
            }
            drop(selected);
            library.inner.refilter();
            force_model();
        });
        let rows = build_rows(&snapshot, &Category::MythicPlus);
        let start = rows.last().map_or(0, |row| row.date_ms);
        let end = rows.first().map_or(0, |row| row.date_ms);
        let mut date_toggle = false;
        let (date_samples, date_median) = measure(|| {
            date_toggle = !date_toggle;
            library
                .inner
                .state
                .date_range
                .set(date_toggle.then_some((start, end)));
            library.inner.refilter();
            force_model();
        });
        let column = library
            .inner
            .column_view
            .columns()
            .item(1)
            .expect("details column")
            .downcast::<gtk4::ColumnViewColumn>()
            .expect("column type");
        let mut descending = false;
        let (sort_samples, sort_median) = measure(|| {
            descending = !descending;
            library.inner.column_view.sort_by_column(
                Some(&column),
                if descending {
                    gtk4::SortType::Descending
                } else {
                    gtk4::SortType::Ascending
                },
            );
            force_model();
        });
        println!(
            "suggestion_us={suggestion_samples:?} median={suggestion_median}; \
             high_card_suggestion_us={high_card_samples:?} median={high_card_median}; \
             chip_us={chip_samples:?} median={chip_median}; \
             date_us={date_samples:?} median={date_median}; \
             sort_us={sort_samples:?} median={sort_median}"
        );
    }
}
