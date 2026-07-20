// SPDX-License-Identifier: GPL-3.0-or-later

//! The category rail: product mark, status card, category rows in WR-000
//! order with derived counts, and Settings at the bottom. No Home/Recent
//! destinations exist.

use std::rc::Rc;

use gtk4::prelude::*;

use warcraft_recorder::coordinator::AppSnapshot;
use warcraft_recorder::domain::Category;

use super::status::StatusCard;
use super::{ActionSink, CATEGORIES, ShellAction};

/// One category row as the rail renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub category: Category,
    pub label: &'static str,
    pub icon_name: &'static str,
    pub count: usize,
    pub visible: bool,
    pub active: bool,
}

/// WR-000 visibility rule: `hide_empty_categories` hides only categories with
/// zero entries, only once the library holds any video, and Manual stays
/// visible whenever manual recording is enabled.
pub fn rows(snapshot: &AppSnapshot) -> Vec<RowView> {
    let total: usize = snapshot
        .category_counts
        .iter()
        .map(|(_, count)| count)
        .sum();
    CATEGORIES
        .iter()
        .map(|(category, label, icon_name)| {
            let count = snapshot
                .category_counts
                .iter()
                .find(|(candidate, _)| candidate == category)
                .map_or(0, |(_, count)| *count);
            let force_show = *category == Category::Manual && snapshot.config.manual.enabled;
            let visible = !snapshot.config.interface.hide_empty_categories
                || total == 0
                || count > 0
                || force_show;
            RowView {
                category: category.clone(),
                label,
                icon_name,
                count,
                visible,
                active: snapshot.config.interface.selected_category == *category,
            }
        })
        .collect()
}

pub struct Sidebar {
    pub widget: gtk4::Box,
    pub status_card: StatusCard,
    list: gtk4::ListBox,
    rows: Vec<(Category, gtk4::ListBoxRow, gtk4::Label)>,
    settings_warning: gtk4::Image,
}

impl Sidebar {
    pub fn new(sink: ActionSink) -> Self {
        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 6);

        let mark = gtk4::Image::from_icon_name("warcraft-recorder");
        mark.set_pixel_size(32);
        mark.set_valign(gtk4::Align::Center);
        let name = gtk4::Label::new(Some("Warcraft Recorder"));
        name.add_css_class("title-3");
        name.set_xalign(0.0);
        let brand = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        brand.set_margin_top(12);
        brand.set_margin_start(12);
        brand.set_margin_end(12);
        brand.append(&mark);
        brand.append(&name);
        widget.append(&brand);

        let status_card = StatusCard::new(Rc::clone(&sink));
        widget.append(&status_card.widget);

        let list = gtk4::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk4::SelectionMode::Single);
        list.set_vexpand(true);
        list.set_margin_start(6);
        list.set_margin_end(6);

        let mut rows = Vec::new();
        for (category, label, icon_name) in CATEGORIES {
            let icon = gtk4::Image::from_icon_name(icon_name);
            icon.set_pixel_size(20);
            icon.add_css_class("category-row");
            let text = gtk4::Label::new(Some(label));
            text.set_xalign(0.0);
            text.set_hexpand(true);
            let count = gtk4::Label::new(Some("0"));
            count.add_css_class("category-count");
            let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            row_box.set_margin_top(4);
            row_box.set_margin_bottom(4);
            row_box.set_margin_start(6);
            row_box.set_margin_end(6);
            row_box.append(&icon);
            row_box.append(&text);
            row_box.append(&count);
            let row = gtk4::ListBoxRow::new();
            row.set_child(Some(&row_box));
            row.set_tooltip_text(Some(label));
            list.append(&row);
            rows.push((category, row, count));
        }

        {
            let sink = Rc::clone(&sink);
            let rows_categories: Vec<Category> = rows
                .iter()
                .map(|(category, _, _)| category.clone())
                .collect();
            list.connect_row_activated(move |_, row| {
                let Some(category) = rows_categories.get(row.index() as usize) else {
                    return;
                };
                sink(ShellAction::Command(
                    warcraft_recorder::coordinator::Command::SetSelectedCategory {
                        category: category.clone(),
                    },
                ));
            });
        }

        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_child(Some(&list));
        scrolled.set_vexpand(true);
        scrolled.set_propagate_natural_height(true);
        widget.append(&scrolled);

        let settings_icon = gtk4::Image::from_icon_name("emblem-system-symbolic");
        settings_icon.set_pixel_size(20);
        let settings_label = gtk4::Label::new(Some("Settings"));
        settings_label.set_xalign(0.0);
        settings_label.set_hexpand(true);
        let settings_warning = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        settings_warning.set_visible(false);
        settings_warning.set_tooltip_text(Some("Settings need attention"));
        let settings_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        settings_box.set_margin_top(4);
        settings_box.set_margin_bottom(4);
        settings_box.set_margin_start(6);
        settings_box.set_margin_end(6);
        settings_box.append(&settings_icon);
        settings_box.append(&settings_label);
        settings_box.append(&settings_warning);
        let settings_button = gtk4::Button::new();
        settings_button.set_child(Some(&settings_box));
        settings_button.add_css_class("flat");
        settings_button.set_margin_start(6);
        settings_button.set_margin_end(6);
        settings_button.set_margin_bottom(6);
        settings_button.set_tooltip_text(Some("Settings"));
        {
            let sink = Rc::clone(&sink);
            settings_button.connect_clicked(move |_| {
                sink(ShellAction::OpenSettings);
            });
        }

        let separator = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        separator.set_margin_start(6);
        separator.set_margin_end(6);
        widget.append(&separator);
        widget.append(&settings_button);

        Self {
            widget,
            status_card,
            list,
            rows,
            settings_warning,
        }
    }

    pub fn apply(&self, snapshot: &AppSnapshot) {
        let views = rows(snapshot);
        for (view, (_, row, count)) in views.iter().zip(&self.rows) {
            row.set_visible(view.visible);
            count.set_label(&view.count.to_string());
            if view.active {
                self.list.select_row(Some(row));
            }
        }
        self.settings_warning
            .set_visible(!snapshot.setup_problems.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use warcraft_recorder::config::Config;
    use warcraft_recorder::domain::RecorderStatus;

    fn snapshot(counts: Vec<(Category, usize)>, hide_empty: bool, manual: bool) -> AppSnapshot {
        let mut config = Config::default();
        config.interface.hide_empty_categories = hide_empty;
        config.manual.enabled = manual;
        let mut snapshot =
            crate::ui::window::tests::snapshot_with(RecorderStatus::Ready, config, Vec::new());
        snapshot.category_counts = counts;
        snapshot
    }

    #[test]
    fn row_order_and_labels_match_the_baseline_rail() {
        let views = rows(&snapshot(Vec::new(), false, false));
        let labels: Vec<&str> = views.iter().map(|view| view.label).collect();
        assert_eq!(
            labels,
            [
                "2v2",
                "3v3",
                "5v5",
                "Skirmish",
                "Solo Shuffle",
                "Mythic+",
                "Raids",
                "Battlegrounds",
                "Manual",
                "Clips"
            ]
        );
        assert!(views.iter().all(|view| view.visible));
    }

    #[test]
    fn hide_empty_never_hides_anything_in_an_empty_library() {
        let views = rows(&snapshot(Vec::new(), true, false));
        assert!(views.iter().all(|view| view.visible));
    }

    #[test]
    fn hide_empty_hides_only_zero_categories_once_videos_exist() {
        let views = rows(&snapshot(
            vec![(Category::Raids, 3), (Category::Clip, 1)],
            true,
            false,
        ));
        let visible = |category: &Category| {
            views
                .iter()
                .find(|view| &view.category == category)
                .expect("row exists")
                .visible
        };
        assert!(visible(&Category::Raids));
        assert!(visible(&Category::Clip));
        assert!(!visible(&Category::TwoVTwo));
        assert!(!visible(&Category::Manual));
    }

    #[test]
    fn manual_stays_visible_when_manual_recording_is_enabled() {
        let views = rows(&snapshot(vec![(Category::Raids, 1)], true, true));
        let manual = views
            .iter()
            .find(|view| view.category == Category::Manual)
            .expect("manual row");
        assert!(manual.visible);
    }

    #[test]
    fn counts_and_active_row_come_from_the_snapshot() {
        let mut snapshot = snapshot(
            vec![(Category::Raids, 7), (Category::MythicPlus, 2)],
            false,
            false,
        );
        snapshot.config.interface.selected_category = Category::Raids;
        let views = rows(&snapshot);
        let raids = views
            .iter()
            .find(|view| view.category == Category::Raids)
            .expect("raids row");
        assert_eq!(raids.count, 7);
        assert!(raids.active);
        assert_eq!(views.iter().filter(|view| view.active).count(), 1);
    }
}
