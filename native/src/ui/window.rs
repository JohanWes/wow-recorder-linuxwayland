// SPDX-License-Identifier: GPL-3.0-or-later

//! The one `AdwApplicationWindow`: `AdwNavigationSplitView` with the compact
//! sidebar and a content pane holding banners, the player placeholder above a
//! draggable divider, and the toolbar/table placeholders below. Full table,
//! player, and settings behavior arrive with WR-010–012.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use warcraft_recorder::coordinator::{AppSnapshot, Command, CoordinatorHandle};
use warcraft_recorder::domain::RecoveryAction;

use super::sidebar::Sidebar;
use super::tray_backend::TrayBackend;
use super::{ActionSink, ShellAction, category_label, install_actions, primary_menu, tray};

/// The content pane as rendered from one snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentView {
    pub title: String,
    /// The newest entry the player will open once WR-011 lands, if any.
    pub player_hint: Option<String>,
    pub table_empty: bool,
    pub setup_banner: Option<String>,
    pub problem_banner: Option<ProblemBanner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemBanner {
    pub summary: String,
    pub action: Option<RecoveryAction>,
}

pub fn content_view(snapshot: &AppSnapshot) -> ContentView {
    let selected = &snapshot.config.interface.selected_category;
    let newest = snapshot
        .entries
        .iter()
        .filter(|entry| &entry.category == selected)
        .max_by_key(|entry| entry.start_unix_ms);
    ContentView {
        title: category_label(selected).to_owned(),
        player_hint: newest.map(|entry| format!("Newest: {}", entry.title)),
        table_empty: newest.is_none(),
        setup_banner: snapshot
            .setup_problems
            .first()
            .map(|problem| problem.message.clone()),
        problem_banner: snapshot.problems.last().map(|problem| ProblemBanner {
            summary: problem.summary.clone(),
            action: problem.recovery_action,
        }),
    }
}

fn recovery_label(action: RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::OpenSettings => "Open Settings",
        RecoveryAction::ReselectCaptureTarget => "Reselect capture target",
        RecoveryAction::Retry => "Try again",
        RecoveryAction::OpenLogs => "Open logs",
        RecoveryAction::Quit => "Quit",
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

pub struct Shell {
    window: adw::ApplicationWindow,
    sidebar: Sidebar,
    title: adw::WindowTitle,
    nav_page: adw::NavigationPage,
    setup_banner: adw::Banner,
    problem_banner: adw::Banner,
    player_hint: gtk4::Label,
    table_stack: gtk4::Stack,
    close_to_tray: Rc<Cell<bool>>,
    minimize_to_tray: Rc<Cell<bool>>,
    tray_available: Rc<Cell<bool>>,
    hold_guard: Rc<RefCell<Option<gtk4::gio::ApplicationHoldGuard>>>,
}

impl Shell {
    pub fn build(
        application: &adw::Application,
        coordinator: Rc<RefCell<CoordinatorHandle>>,
        tray: Option<Rc<TrayBackend>>,
        data_dir: &Path,
        config_dir: &Path,
        close_to_tray: bool,
        minimize_to_tray: bool,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Warcraft Recorder")
            .default_width(1440)
            .default_height(900)
            .width_request(640)
            .height_request(480)
            .build();

        let busy_banner = adw::Banner::new("The app is busy — try again in a moment.");
        let sink = make_sink(
            &window,
            application,
            &coordinator,
            &busy_banner,
            data_dir,
            config_dir,
        );
        install_actions(application, Rc::clone(&sink));

        let sidebar = Sidebar::new(Rc::clone(&sink));

        // Content header: category title and the one primary menu.
        let title = adw::WindowTitle::new("Warcraft Recorder", "");
        let menu_button = gtk4::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&primary_menu()));
        menu_button.set_tooltip_text(Some("Main menu"));
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        header.pack_end(&menu_button);

        // Banners: setup, newest problem, and one transient Busy notice.
        let setup_banner = adw::Banner::new("");
        setup_banner.set_button_label(Some("Open Settings"));
        {
            let sink = Rc::clone(&sink);
            setup_banner.connect_button_clicked(move |_| {
                sink(ShellAction::OpenSettings);
            });
        }
        let problem_banner = adw::Banner::new("");
        {
            let sink = Rc::clone(&sink);
            let banner = problem_banner.clone();
            problem_banner.connect_button_clicked(move |_| {
                if let Some(action) = banner
                    .button_label()
                    .as_deref()
                    .and_then(recovery_from_label)
                {
                    sink(recovery_shell_action(action));
                }
            });
        }

        // Player placeholder above; WR-011 replaces it with the Clapper player.
        let player_headline = gtk4::Label::new(Some("No recording selected"));
        player_headline.add_css_class("title-2");
        let player_hint = gtk4::Label::new(None);
        player_hint.add_css_class("dim-label");
        let player_text = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        player_text.set_valign(gtk4::Align::Center);
        player_text.append(&player_headline);
        player_text.append(&player_hint);
        let player_area = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        player_area.add_css_class("player-area");
        player_area.set_hexpand(true);
        player_area.set_vexpand(true);
        player_area.set_height_request(240);
        player_area.append(&player_text);

        // Toolbar/table placeholders below; WR-010 owns filters and rows.
        let search = gtk4::SearchEntry::new();
        search.set_placeholder_text(Some("Search recordings"));
        search.set_sensitive(false);
        search.set_hexpand(true);
        let date_range = gtk4::Button::with_label("Date range");
        date_range.set_sensitive(false);
        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        toolbar.set_margin_top(6);
        toolbar.set_margin_bottom(6);
        toolbar.set_margin_start(12);
        toolbar.set_margin_end(12);
        toolbar.append(&search);
        toolbar.append(&date_range);

        let column_view = gtk4::ColumnView::new(None::<gtk4::SelectionModel>);
        column_view.append_column(&placeholder_column());
        column_view.set_hexpand(true);
        column_view.set_vexpand(true);
        let table_scroll = gtk4::ScrolledWindow::new();
        table_scroll.set_child(Some(&column_view));
        table_scroll.set_vexpand(true);

        let empty_page = adw::StatusPage::new();
        empty_page.set_title("No recordings in this category");
        empty_page.set_description(Some("Recordings in the selected category appear here."));
        empty_page.set_vexpand(true);

        let table_stack = gtk4::Stack::new();
        table_stack.add_named(&table_scroll, Some("table"));
        table_stack.add_named(&empty_page, Some("empty"));
        table_stack.set_visible_child_name("empty");

        let table_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        table_box.append(&toolbar);
        table_box.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        table_box.append(&table_stack);

        let paned = gtk4::Paned::new(gtk4::Orientation::Vertical);
        paned.set_wide_handle(true);
        paned.set_vexpand(true);
        paned.set_start_child(Some(&player_area));
        paned.set_end_child(Some(&table_box));
        paned.set_resize_start_child(true);
        paned.set_shrink_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(false);
        paned.connect_realize(|paned| {
            let paned = paned.clone();
            gtk4::glib::idle_add_local_once(move || paned.set_position(400));
        });

        let banner_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        banner_box.append(&setup_banner);
        banner_box.append(&problem_banner);
        banner_box.append(&busy_banner);
        let content_body = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        content_body.append(&banner_box);
        content_body.append(&paned);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&content_body));
        let nav_page = adw::NavigationPage::new(&toolbar_view, "Warcraft Recorder");

        let sidebar_page = adw::NavigationPage::new(&sidebar.widget, "Categories");
        let split = adw::NavigationSplitView::new();
        split.set_sidebar(Some(&sidebar_page));
        split.set_content(Some(&nav_page));
        window.set_content(Some(&split));

        let shell = Self {
            window: window.clone(),
            sidebar,
            title,
            nav_page,
            setup_banner,
            problem_banner,
            player_hint,
            table_stack,
            close_to_tray: Rc::new(Cell::new(close_to_tray)),
            minimize_to_tray: Rc::new(Cell::new(minimize_to_tray)),
            tray_available: Rc::new(Cell::new(
                tray.as_ref().is_some_and(|tray| tray.is_available()),
            )),
            hold_guard: Rc::new(RefCell::new(None)),
        };
        shell.connect_close_request();
        shell.connect_minimize();
        shell
    }

    pub fn present(&self) {
        self.hold_guard.borrow_mut().take();
        self.window.present();
    }

    /// Hide to the tray and hold the application while hidden. The
    /// coordinator and screen capture keep running.
    pub fn hide_to_tray(&self) {
        hide_and_hold(&self.window, &self.hold_guard);
    }

    pub fn set_tray_available(&self, available: bool) {
        self.tray_available.set(available);
        self.sidebar.status_card.set_tray_available(available);
        // If the tray vanished while the window was hidden to it, the window is
        // the only way back in — reveal it so the process can't be stranded
        // with no visible window and no tray menu (WR-009 step 11). Bind the
        // borrow to a local first so it is released before `present` takes the
        // guard mutably.
        let hidden = self.hold_guard.borrow().is_some();
        if !available && hidden {
            self.present();
        }
    }

    pub fn apply_snapshot(&self, snapshot: &AppSnapshot) {
        self.close_to_tray
            .set(snapshot.config.interface.close_to_tray);
        self.minimize_to_tray
            .set(snapshot.config.interface.minimize_to_tray);

        self.sidebar.apply(snapshot);
        self.sidebar.status_card.apply(snapshot, now_unix_ms());

        let view = content_view(snapshot);
        self.title.set_title(&view.title);
        self.nav_page.set_title(&view.title);
        self.player_hint
            .set_label(view.player_hint.as_deref().unwrap_or(""));
        self.table_stack
            .set_visible_child_name(if view.table_empty { "empty" } else { "table" });

        self.setup_banner.set_revealed(view.setup_banner.is_some());
        if let Some(message) = &view.setup_banner {
            self.setup_banner.set_title(message);
        }
        match &view.problem_banner {
            Some(problem) => {
                self.problem_banner.set_title(&problem.summary);
                self.problem_banner
                    .set_button_label(problem.action.map(recovery_label));
                self.problem_banner.set_revealed(true);
            }
            None => self.problem_banner.set_revealed(false),
        }
    }

    fn connect_close_request(&self) {
        let tray_available = Rc::clone(&self.tray_available);
        let close_to_tray = Rc::clone(&self.close_to_tray);
        let hold_guard = Rc::clone(&self.hold_guard);
        self.window.connect_close_request(move |window| {
            if tray::close_hides(tray_available.get(), close_to_tray.get()) {
                hide_and_hold(window, &hold_guard);
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
    }

    fn connect_minimize(&self) {
        let tray_available = Rc::clone(&self.tray_available);
        let minimize_to_tray = Rc::clone(&self.minimize_to_tray);
        let hold_guard = Rc::clone(&self.hold_guard);
        self.window.connect_realize(move |window| {
            let tray_available = Rc::clone(&tray_available);
            let minimize_to_tray = Rc::clone(&minimize_to_tray);
            let hold_guard = Rc::clone(&hold_guard);
            let window = window.clone();
            let Some(toplevel) = window.surface().and_downcast::<gtk4::gdk::Toplevel>() else {
                return;
            };
            toplevel.connect_state_notify(move |toplevel| {
                let minimized = toplevel
                    .state()
                    .contains(gtk4::gdk::ToplevelState::MINIMIZED);
                if minimized && tray::minimize_hides(tray_available.get(), minimize_to_tray.get()) {
                    hide_and_hold(&window, &hold_guard);
                }
            });
        });
    }
}

fn hide_and_hold(
    window: &adw::ApplicationWindow,
    hold_guard: &Rc<RefCell<Option<gtk4::gio::ApplicationHoldGuard>>>,
) {
    window.set_visible(false);
    let mut guard = hold_guard.borrow_mut();
    if guard.is_none() {
        *guard = Some(window.application().expect("shell application").hold());
    }
}

fn placeholder_column() -> gtk4::ColumnViewColumn {
    let factory = gtk4::SignalListItemFactory::new();
    let column = gtk4::ColumnViewColumn::new(Some("Recordings"), Some(factory));
    column.set_expand(true);
    column
}

fn make_sink(
    window: &adw::ApplicationWindow,
    application: &adw::Application,
    coordinator: &Rc<RefCell<CoordinatorHandle>>,
    busy_banner: &adw::Banner,
    data_dir: &Path,
    config_dir: &Path,
) -> ActionSink {
    let window = window.clone();
    let application = application.clone();
    let coordinator = Rc::clone(coordinator);
    let busy_banner = busy_banner.clone();
    let data_dir: PathBuf = data_dir.to_owned();
    let config_dir: PathBuf = config_dir.to_owned();
    Rc::new(move |action| {
        let command = match action {
            ShellAction::Command(command) => command,
            ShellAction::Retry => Command::Arm,
            ShellAction::OpenSettings => {
                present_settings(&window);
                return true;
            }
            ShellAction::OpenLogs => {
                open_logs(&window, &data_dir, &config_dir);
                return true;
            }
            ShellAction::About => {
                present_about(&window, &application);
                return true;
            }
            ShellAction::Quit => Command::Shutdown,
        };
        let sent = coordinator.borrow().send(command);
        if !sent {
            busy_banner.set_revealed(true);
            let busy_banner = busy_banner.clone();
            gtk4::glib::timeout_add_local_once(Duration::from_secs(2), move || {
                busy_banner.set_revealed(false);
            });
        }
        sent
    })
}

fn recovery_from_label(label: &str) -> Option<RecoveryAction> {
    match label {
        "Open Settings" => Some(RecoveryAction::OpenSettings),
        "Reselect capture target" => Some(RecoveryAction::ReselectCaptureTarget),
        "Try again" => Some(RecoveryAction::Retry),
        "Open logs" => Some(RecoveryAction::OpenLogs),
        "Quit" => Some(RecoveryAction::Quit),
        _ => None,
    }
}

fn recovery_shell_action(action: RecoveryAction) -> ShellAction {
    match action {
        RecoveryAction::OpenSettings => ShellAction::OpenSettings,
        RecoveryAction::ReselectCaptureTarget => {
            ShellAction::Command(Command::ReselectCaptureTarget)
        }
        RecoveryAction::Retry => ShellAction::Retry,
        RecoveryAction::OpenLogs => ShellAction::OpenLogs,
        RecoveryAction::Quit => ShellAction::Quit,
    }
}

/// WR-012 owns the real settings dialog; this is the shell's placeholder.
fn present_settings(parent: &impl IsA<gtk4::Widget>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Settings");
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let row = adw::ActionRow::new();
    row.set_title("Native settings arrive with the settings milestone.");
    group.add(&row);
    page.add(&group);
    dialog.add(&page);
    dialog.present(Some(parent));
}

fn present_about(parent: &impl IsA<gtk4::Widget>, application: &adw::Application) {
    let about = adw::AboutDialog::new();
    about.set_application_name("Warcraft Recorder");
    about.set_application_icon(
        application
            .application_id()
            .as_deref()
            .unwrap_or("warcraft-recorder"),
    );
    about.set_developer_name("JohanWes");
    about.set_version(env!("CARGO_PKG_VERSION"));
    about.set_license_type(gtk4::License::Gpl30);
    about.set_comments(
        "Automatic World of Warcraft recording and review.\n\nCategory icons: \
         Lucide contributors (ISC License); dragon and dungeon icons by \
         Fonticons, Inc. (CC BY 4.0). Full notices are bundled with the \
         application.",
    );
    about.set_website("https://github.com/JohanWes/wow-recorder-linuxwayland");
    about.set_issue_url("https://github.com/JohanWes/wow-recorder-linuxwayland/issues");
    about.present(Some(parent));
}

fn open_logs(parent: &impl IsA<gtk4::Widget>, data_dir: &Path, config_dir: &Path) {
    let parent_window = parent.root().and_downcast::<gtk4::Window>();
    let fallback = config_dir.to_owned();
    let fallback_parent = parent_window.clone();
    let launcher = gtk4::FileLauncher::new(Some(&gtk4::gio::File::for_path(data_dir)));
    launcher.launch(
        parent_window.as_ref(),
        None::<&gtk4::gio::Cancellable>,
        move |result| {
            if result.is_ok() {
                return;
            }
            let launcher = gtk4::FileLauncher::new(Some(&gtk4::gio::File::for_path(&fallback)));
            launcher.launch(
                fallback_parent.as_ref(),
                None::<&gtk4::gio::Cancellable>,
                |result| {
                    if let Err(error) = result {
                        tracing::warn!(%error, "could not open the logs directory");
                    }
                },
            );
        },
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use warcraft_recorder::config::Config;
    use warcraft_recorder::domain::{
        ActivityDetails, Category, Codec, CorrelatedActivity, GameFlavor, LibraryEntry, MediaFacts,
        Outcome, Problem, RecorderStatus, RecordingId, StorageLimit,
    };

    pub(crate) fn entry(category: Category, title: &str, start_unix_ms: i64) -> LibraryEntry {
        LibraryEntry {
            id: RecordingId::new(),
            media_path: PathBuf::from("/recordings/video.mkv"),
            sidecar_path: PathBuf::from("/recordings/video.json"),
            category,
            flavor: GameFlavor::Retail,
            title: title.to_owned(),
            start_unix_ms,
            duration_ms: 60_000,
            outcome: Outcome::Win,
            protected: false,
            tag: None,
            activity_hash: None,
            player: None,
            combatants: Vec::new(),
            details: ActivityDetails::Manual,
            timeline: Vec::new(),
            media: MediaFacts {
                fps: Some(60),
                width: Some(1920),
                height: Some(1080),
                codec: Some(Codec::H264),
            },
        }
    }

    pub(crate) fn snapshot_with(
        status: RecorderStatus,
        config: Config,
        entries: Vec<LibraryEntry>,
    ) -> AppSnapshot {
        let mut category_counts: Vec<(Category, usize)> = Vec::new();
        for entry in &entries {
            match category_counts
                .iter_mut()
                .find(|(category, _)| category == &entry.category)
            {
                Some((_, count)) => *count += 1,
                None => category_counts.push((entry.category.clone(), 1)),
            }
        }
        AppSnapshot {
            entries: Arc::from(entries),
            correlations: Arc::<[CorrelatedActivity]>::from(Vec::new()),
            category_counts,
            status,
            active: None,
            config,
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

    fn snapshot_with_entries(entries: Vec<LibraryEntry>) -> AppSnapshot {
        snapshot_with(RecorderStatus::Ready, Config::default(), entries)
    }

    #[test]
    fn content_title_and_empty_state_follow_the_selected_category() {
        let snapshot = snapshot_with_entries(Vec::new());
        let view = content_view(&snapshot);
        assert_eq!(view.title, "3v3");
        assert!(view.table_empty);
        assert_eq!(view.player_hint, None);

        let mut snapshot = snapshot_with_entries(vec![entry(Category::Raids, "Boss", 10)]);
        snapshot.config.interface.selected_category = Category::Raids;
        let view = content_view(&snapshot);
        assert_eq!(view.title, "Raids");
        assert!(!view.table_empty);
        assert_eq!(view.player_hint.as_deref(), Some("Newest: Boss"));
    }

    #[test]
    fn player_hint_uses_the_newest_entry_of_the_category() {
        let entries = vec![
            entry(Category::Raids, "Older", 10),
            entry(Category::Raids, "Newest", 99),
            entry(Category::MythicPlus, "Other category", 100),
        ];
        let mut snapshot = snapshot_with_entries(entries);
        snapshot.config.interface.selected_category = Category::Raids;
        assert_eq!(
            content_view(&snapshot).player_hint.as_deref(),
            Some("Newest: Newest")
        );
    }

    #[test]
    fn banners_come_from_setup_problems_and_the_newest_problem() {
        let mut snapshot = snapshot_with_entries(Vec::new());
        assert_eq!(content_view(&snapshot).setup_banner, None);
        assert_eq!(content_view(&snapshot).problem_banner, None);

        snapshot.setup_problems = vec![
            warcraft_recorder::config::ValidationProblem {
                field: "storage.recording_dir",
                message: "Choose a recording directory.".to_owned(),
            },
            warcraft_recorder::config::ValidationProblem {
                field: "flavors",
                message: "Enable at least one World of Warcraft flavor.".to_owned(),
            },
        ];
        snapshot.problems = vec![
            Problem {
                summary: "older".to_owned(),
                safe_detail: None,
                occurred_unix_ms: 1,
                recovery_action: None,
            },
            Problem {
                summary: "newest".to_owned(),
                safe_detail: None,
                occurred_unix_ms: 2,
                recovery_action: Some(RecoveryAction::OpenLogs),
            },
        ];
        let view = content_view(&snapshot);
        assert_eq!(
            view.setup_banner.as_deref(),
            Some("Choose a recording directory.")
        );
        assert_eq!(
            view.problem_banner,
            Some(ProblemBanner {
                summary: "newest".to_owned(),
                action: Some(RecoveryAction::OpenLogs),
            })
        );
    }

    #[test]
    fn a_two_thousand_entry_snapshot_maps_without_rework() {
        let entries: Vec<LibraryEntry> = (0..2_000)
            .map(|index| {
                let category = if index % 2 == 0 {
                    Category::Raids
                } else {
                    Category::MythicPlus
                };
                entry(category, "Recording", i64::from(index))
            })
            .collect();
        let mut snapshot = snapshot_with_entries(entries);
        snapshot.config.interface.selected_category = Category::Raids;

        let view = content_view(&snapshot);
        assert!(!view.table_empty);
        assert_eq!(view.player_hint.as_deref(), Some("Newest: Recording"));
        let rows = super::super::sidebar::rows(&snapshot);
        let raids = rows
            .iter()
            .find(|row| row.category == Category::Raids)
            .expect("raids row");
        assert_eq!(raids.count, 1_000);
    }
}
