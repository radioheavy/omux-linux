//! The sidebar: projects with their sessions, each with its live status.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use uuid::Uuid;

use crate::app::App;
use crate::i18n::tr;
use crate::model::{TabColor, TabGroup, TabItem};
use crate::runtime::{StatusColor, TabRuntime};

pub struct Sidebar {
    pub root: gtk::Box,
    /// All · Local · one tab per SSH host; hidden without remote projects.
    machines: gtk::Box,
    pub list: gtk::ListBox,
    /// Holds the usage strip (filled by `usage`).
    pub footer: gtk::Box,
    rows: RefCell<HashMap<Uuid, SessionRow>>,
    menu: gtk::PopoverMenu,
    hovered: Cell<Option<Uuid>>,
    /// Set while the sidebar selects a row itself, so it isn't taken as a click.
    syncing: Cell<bool>,
    /// The rows as last built, to notice when the activity order changes.
    shown: RefCell<Vec<Uuid>>,
    /// How far back sessions are listed: today, a week, a month or (0) all.
    /// Load More steps through them.
    days: Cell<u32>,
}

/// The steps of Load More, in days; 0 is everything.
const DAY_STEPS: [u32; 4] = [1, 7, 30, 0];

/// The next Load More step, or None when everything shows.
fn next_days(days: u32) -> Option<u32> {
    let at = DAY_STEPS.iter().position(|d| *d == days)?;
    DAY_STEPS.get(at + 1).copied()
}

/// The start of the first day shown, in seconds since the epoch: local
/// midnight today, or that many days back. None shows everything.
fn since(days: u32) -> Option<u64> {
    if days == 0 {
        return None;
    }
    let now = glib::DateTime::now_local().ok()?;
    let today = now.hour() * 3600 + now.minute() * 60 + now.second();
    let start = now.to_unix() - i64::from(today) - i64::from(days - 1) * 86_400;
    Some(start.max(0) as u64)
}

struct SessionRow {
    row: gtk::ListBoxRow,
    dot: gtk::Box,
    name: gtk::Label,
    status: gtk::Label,
    permission: gtk::Box,
    trailing: gtk::Stack,
    start_stop: gtk::Button,
    /// Subagent rows and PR badges, filled by later layers.
    pub extras: gtk::Box,
}

const DOT_CLASSES: [&str; 14] = [
    "blue", "orange", "green", "red", "purple", "yellow", "mint", "teal", "cyan", "indigo", "pink",
    "brown", "gray", "ring",
];

pub fn set_dot(dot: &gtk::Box, color: Option<&str>) {
    for class in DOT_CLASSES {
        dot.remove_css_class(class);
    }
    if let Some(color) = color {
        dot.add_css_class(color);
    }
}

pub fn status_class(color: StatusColor) -> &'static str {
    match color {
        StatusColor::Orange => "orange",
        StatusColor::Blue => "blue",
        StatusColor::Green => "green",
        StatusColor::Red => "red",
    }
}

/// Fills a session row with its color.
fn set_tint(row: &gtk::ListBoxRow, color: TabColor) {
    for c in TabColor::ALL {
        row.remove_css_class(&format!("omux-tint-{}", c.raw()));
    }
    if color != TabColor::None {
        row.add_css_class(&format!("omux-tint-{}", color.raw()));
    }
}

/// Status color, else the session's color, else neutral.
pub fn dot_color(rt: &TabRuntime, tab: &TabItem) -> Option<&'static str> {
    rt.status_color()
        .map(status_class)
        .or_else(|| (tab.color != TabColor::None).then_some(tab.color.raw()))
}

pub fn dot() -> gtk::Box {
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("omux-dot");
    dot.set_valign(gtk::Align::Center);
    dot.set_halign(gtk::Align::Center);
    dot
}

impl Sidebar {
    pub fn build() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("omux-sidebar");
        root.set_size_request(200, -1);

        let bar = adw::HeaderBar::new();
        bar.add_css_class("omux-chrome");
        bar.set_show_end_title_buttons(false);
        bar.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some(tr("New Project")), Some("app.new-project"));
        section.append(
            Some(tr("New Remote Project…")),
            Some("app.new-remote-project"),
        );
        section.append(Some(tr("New Claude Code Session")), Some("app.new-claude"));
        section.append(Some(tr("New Codex Session")), Some("app.new-codex"));
        section.append(Some(tr("New Shell")), Some("app.new-shell"));
        section.append(
            Some(tr("New Session in Worktree…")),
            Some("app.new-worktree"),
        );
        menu.append_section(None, &section);
        let section = gio::Menu::new();
        let session = gio::Menu::new();
        for (label, action) in [
            (tr("Rename"), "app.rename"),
            (tr("Edit Session…"), "app.edit-session"),
            (tr("Start or Continue Session"), "app.start"),
            (tr("Restart Session"), "app.restart"),
            (tr("Interrupt (Ctrl-C)"), "app.interrupt"),
            (tr("Kill Session"), "app.kill"),
            (tr("Close Session"), "app.close-session"),
        ] {
            session.append(Some(label), Some(action));
        }
        section.append_submenu(Some(tr("Session")), &session);
        let view = gio::Menu::new();
        view.append(Some(tr("Files Panel")), Some("app.toggle-panel"));
        view.append(Some(tr("Files")), Some("app.panel-tab::files"));
        view.append(Some(tr("Changes")), Some("app.panel-tab::changes"));
        view.append(Some(tr("Browser")), Some("app.panel-tab::browser"));
        view.append(Some(tr("Next Session")), Some("app.next-session"));
        view.append(Some(tr("Previous Session")), Some("app.prev-session"));
        section.append_submenu(Some(tr("View")), &view);
        section.append(Some(tr("Jump to Session…")), Some("app.quick-switch"));
        section.append(Some(tr("Search Conversations…")), Some("app.open-recent"));
        section.append(
            Some(tr("Import Claude Code Conversations")),
            Some("app.import-claude"),
        );
        section.append(Some(tr("Settings")), Some("app.settings"));
        section.append(Some(tr("Keyboard Shortcuts")), Some("app.shortcuts"));
        section.append(Some(tr("Welcome to omux…")), Some("app.welcome"));
        section.append(Some(tr("omux on GitHub")), Some("app.github"));
        section.append(Some(tr("About omux")), Some("app.about"));
        menu.append_section(None, &section);
        let section = gio::Menu::new();
        section.append(Some(tr("Close Window")), Some("app.close-window"));
        section.append(Some(tr("Quit")), Some("app.quit"));
        menu.append_section(None, &section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text(tr("Main menu"))
            .build();
        bar.pack_end(&menu_button);
        let sort = gio::Menu::new();
        let view = gio::Menu::new();
        view.append(Some(tr("Projects")), Some("app.sidebar-view::projects"));
        view.append(
            Some(tr("All Conversations, Latest First")),
            Some("app.sidebar-view::recent"),
        );
        sort.append_section(Some(tr("Show")), &view);
        let order = gio::Menu::new();
        order.append(Some(tr("Manual")), Some("app.project-order::manual"));
        order.append(
            Some(tr("Latest Activity First")),
            Some("app.project-order::recent"),
        );
        sort.append_section(Some(tr("Order Projects")), &order);
        let sort_button = gtk::MenuButton::builder()
            .icon_name("view-sort-descending-symbolic")
            .menu_model(&sort)
            .tooltip_text(tr("Show and order sessions"))
            .build();
        bar.pack_end(&sort_button);
        for (icon, action, tip) in [
            (
                "document-open-recent-symbolic",
                "app.open-recent",
                tr("Search conversations (Ctrl+Alt+O)"),
            ),
            (
                "folder-new-symbolic",
                "app.open-folder",
                tr("Open folder as a project (Ctrl+Shift+O)"),
            ),
            (
                "list-add-symbolic",
                "app.new-session",
                tr("New session in current project (Ctrl+Shift+T)"),
            ),
        ] {
            let button = gtk::Button::from_icon_name(icon);
            button.set_action_name(Some(action));
            button.set_tooltip_text(Some(tip));
            bar.pack_end(&button);
        }
        root.append(&bar);

        let machines = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        machines.add_css_class("linked");
        machines.add_css_class("omux-machines");
        machines.set_halign(gtk::Align::Fill);
        machines.set_homogeneous(true);
        machines.set_margin_start(10);
        machines.set_margin_end(10);
        machines.set_margin_bottom(4);
        machines.set_visible(false);
        root.append(&machines);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.add_css_class("navigation-sidebar");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        root.append(&scroller);

        let hairline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hairline.add_css_class("omux-hairline");
        root.append(&hairline);
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&footer);

        let menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        menu.set_has_arrow(false);
        // Not parented to the list: ListBox::remove_all would treat it as a row.
        menu.set_parent(&root);

        Self {
            root,
            machines,
            list,
            footer,
            rows: RefCell::new(HashMap::new()),
            menu,
            hovered: Cell::new(None),
            syncing: Cell::new(false),
            shown: RefCell::new(Vec::new()),
            days: Cell::new(1),
        }
    }

    pub fn select_row(&self, id: Option<Uuid>) {
        self.syncing.set(true);
        match id.and_then(|id| self.rows.borrow().get(&id).map(|r| r.row.clone())) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
        self.syncing.set(false);
    }

    pub fn update_row(&self, app: &App, id: Uuid) {
        let rows = self.rows.borrow();
        let Some(row) = rows.get(&id) else { return };
        let Some((group, tab)) = app.tab(id) else {
            return;
        };
        let rt = app.rt(id);
        // The row wears the session's color; the dot only shows status.
        let status = rt.status_color().map(status_class);
        set_dot(&row.dot, status);
        row.dot
            .set_opacity(if status.is_some() { 1.0 } else { 0.0 });
        set_tint(&row.row, tab.color);
        row.name.set_text(&tab.name);
        row.status.set_text(&secondary_line(app, &group, &tab, &rt));
        row.status.set_visible(!row.status.text().is_empty());
        let hovered = self.hovered.get() == Some(id);
        if rt.exited && !hovered {
            row.row.add_css_class("omux-stopped");
        } else {
            row.row.remove_css_class("omux-stopped");
        }
        let codex_pending = crate::codex::pending_approval(app, id).is_some();
        row.permission
            .set_visible(rt.awaits_permission() || codex_pending);
        // Trailing: on hover ■ (busy) or ▶ (not connected); else bell or activity.
        if hovered && (rt.busy || !rt.agent_connected) {
            let busy = rt.busy;
            row.start_stop.set_icon_name(if busy {
                "media-playback-stop-symbolic"
            } else {
                "media-playback-start-symbolic"
            });
            row.start_stop.set_tooltip_text(Some(if busy {
                tr("Interrupt (Ctrl-C)")
            } else {
                tr("Start or continue this session")
            }));
            row.trailing.set_visible_child_name("button");
        } else if rt.needs_attention {
            row.trailing.set_visible_child_name("bell");
        } else if rt.has_activity {
            row.trailing.set_visible_child_name("activity");
        } else {
            row.trailing.set_visible_child_name("none");
        }
        crate::ui::agents::update_sidebar(app, id, &row.extras);
    }
}

/// The second line: branch for worktree sessions, status, agent count.
fn secondary_line(app: &App, group: &TabGroup, tab: &TabItem, rt: &TabRuntime) -> String {
    let show_folder = app.settings.borrow().show_folder_path;
    let mut parts = Vec::new();
    if let Some(seconds) = rt.closes_in {
        parts.push(crate::trf!(
            "Closes in {time}",
            time = format!("{}:{:02}", seconds / 60, seconds % 60)
        ));
    }
    // Without project headers, each row names its project.
    if app.settings.borrow().sidebar_view == "recent" {
        parts.push(group.name.clone());
    }
    if let Some(worktree) = &tab.worktree {
        parts.push(format!("⎇ {}", worktree.branch));
    }
    let status = rt.status_line(show_folder && tab.worktree.is_none());
    if !status.is_empty() {
        parts.push(status);
    }
    if app.selected.get() != Some(tab.id) {
        let agents = crate::ui::agents::running_count(app, tab.id);
        if agents > 0 {
            parts.push(if agents == 1 {
                tr("1 agent").to_string()
            } else {
                crate::trf!("{agents} agents", agents = agents)
            });
        }
    }
    parts.join(" · ")
}

/// A sidebar row: a project header, or a session.
#[derive(Debug, Clone)]
pub enum Item {
    Group(TabGroup),
    Tab(TabItem),
}

impl Item {
    fn id(&self) -> Uuid {
        match self {
            Item::Group(g) => g.id,
            Item::Tab(t) => t.id,
        }
    }
}

/// Seconds since the epoch.
fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// When each session last did something: its Claude Code transcript's last
/// write, or the last status change seen this run, whichever is later.
fn activity_times(app: &App) -> HashMap<Uuid, u64> {
    let transcripts = crate::claude::transcript_times();
    let runtime = app.runtime.borrow();
    app.ws
        .borrow()
        .groups
        .iter()
        .flat_map(|g| g.tabs.iter())
        .map(|tab| {
            let transcript = transcripts.get(&tab.claude_session_id).copied();
            let seen = runtime.get(&tab.id).and_then(|rt| rt.touched);
            let time = transcript.max(seen).map_or(0, seconds);
            (tab.id, time.max(tab.last_active.unwrap_or(0)))
        })
        .collect()
}

/// Which sessions the sidebar lists: those active since `since` (all when
/// None), and always the ones in `keep` (selected, running, needing you).
pub struct Filter<'a> {
    pub since: Option<u64>,
    pub keep: &'a HashSet<Uuid>,
}

/// The rows in display order, and how many sessions the filter hid.
/// `recent` lists every session, latest activity first, with no project
/// headers. Otherwise projects with their sessions, either in their saved
/// order or, with `by_activity`, latest first. Ties keep the saved order.
/// A project whose sessions are all hidden is hidden too.
pub fn layout(
    groups: &[TabGroup],
    times: &HashMap<Uuid, u64>,
    recent: bool,
    by_activity: bool,
    filter: &Filter,
) -> (Vec<Item>, usize) {
    let time = |tab: &TabItem| times.get(&tab.id).copied().unwrap_or(0);
    let listed = |tab: &TabItem| {
        filter.since.is_none_or(|s| time(tab) >= s) || filter.keep.contains(&tab.id)
    };
    let total: usize = groups.iter().map(|g| g.tabs.len()).sum();
    if recent {
        let mut tabs: Vec<&TabItem> = groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .filter(|t| listed(t))
            .collect();
        tabs.sort_by_key(|t| std::cmp::Reverse(time(t)));
        let hidden = total - tabs.len();
        return (
            tabs.into_iter().map(|t| Item::Tab(t.clone())).collect(),
            hidden,
        );
    }
    let mut groups: Vec<TabGroup> = groups
        .iter()
        .filter_map(|g| {
            let mut group = g.clone();
            group.tabs.retain(|t| listed(t));
            (!group.tabs.is_empty() || g.tabs.is_empty()).then_some(group)
        })
        .collect();
    let hidden = total - groups.iter().map(|g| g.tabs.len()).sum::<usize>();
    if by_activity {
        for group in &mut groups {
            group.tabs.sort_by_key(|t| std::cmp::Reverse(time(t)));
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g.tabs.iter().map(time).max().unwrap_or(0)));
    }
    let mut items = Vec::new();
    for group in groups {
        let tabs = if group.is_collapsed {
            Vec::new()
        } else {
            group.tabs.clone()
        };
        items.push(Item::Group(group));
        items.extend(tabs.into_iter().map(Item::Tab));
    }
    (items, hidden)
}

/// Sessions that show whatever their age: the selected one, running ones
/// and ones waiting for the user.
fn always_listed(app: &App) -> HashSet<Uuid> {
    let terminals = app.terminals.borrow();
    let runtime = app.runtime.borrow();
    let mut keep: HashSet<Uuid> = runtime
        .iter()
        .filter(|(id, rt)| rt.needs_attention || (terminals.contains_key(id) && !rt.exited))
        .map(|(id, _)| *id)
        .collect();
    keep.extend(app.selected.get());
    keep
}

/// The machine tab in effect: `all` when there are no remote projects or
/// the chosen host has none left.
fn machine(app: &App) -> String {
    let chosen = app.settings.borrow().sidebar_machine.clone();
    let hosts = crate::remote::hosts(app);
    if hosts.is_empty() || (chosen != "local" && chosen != "all" && !hosts.contains(&chosen)) {
        "all".into()
    } else {
        chosen
    }
}

/// The projects on the chosen machine tab.
fn visible_groups(app: &App) -> Vec<TabGroup> {
    let machine = machine(app);
    app.ws
        .borrow()
        .groups
        .iter()
        .filter(|g| match machine.as_str() {
            "all" => true,
            "local" => g.ssh_host.is_none(),
            host => g.ssh_host.as_deref() == Some(host),
        })
        .cloned()
        .collect()
}

/// Rebuilds the machine tabs (All, Local, each host) when hosts change.
fn fill_machines(app: &Rc<App>) {
    let sidebar = &app.ui.sidebar;
    let hosts = crate::remote::hosts(app);
    sidebar.machines.set_visible(!hosts.is_empty());
    let mut wanted = vec![("all".to_string(), tr("All").to_string())];
    wanted.push(("local".into(), tr("Local").into()));
    wanted.extend(hosts.iter().map(|h| (h.clone(), format!("☁ {h}"))));
    let current = machine(app);
    // Reuse the buttons when the hosts are the same; just sync the state.
    let mut existing = Vec::new();
    let mut child = sidebar.machines.first_child();
    while let Some(widget) = child {
        existing.push(widget.widget_name().to_string());
        child = widget.next_sibling();
    }
    let keys: Vec<String> = wanted.iter().map(|(k, _)| k.clone()).collect();
    if existing != keys {
        while let Some(widget) = sidebar.machines.first_child() {
            sidebar.machines.remove(&widget);
        }
        let mut first: Option<gtk::ToggleButton> = None;
        for (key, label) in &wanted {
            let button = gtk::ToggleButton::with_label(label);
            button.set_widget_name(key);
            button.add_css_class("omux-small");
            if let Some(first) = &first {
                button.set_group(Some(first));
            } else {
                first = Some(button.clone());
            }
            button.set_active(*key == current);
            let weak = Rc::downgrade(app);
            let key = key.clone();
            button.connect_toggled(move |button| {
                let Some(app) = weak.upgrade() else { return };
                if !button.is_active() || app.settings.borrow().sidebar_machine == key {
                    return;
                }
                app.update_settings(|s| s.sidebar_machine = key.clone());
                if key != "all" && key != "local" {
                    crate::remote::import(&app, &key, false);
                }
                // Not from inside the button's own signal: rebuilding may
                // replace it.
                let weak = Rc::downgrade(&app);
                glib::idle_add_local_once(move || {
                    if let Some(app) = weak.upgrade() {
                        app.refresh_sidebar();
                    }
                });
            });
            sidebar.machines.append(&button);
        }
    } else {
        let mut child = sidebar.machines.first_child();
        while let Some(widget) = child {
            if let Ok(button) = widget.clone().downcast::<gtk::ToggleButton>() {
                if button.widget_name() == current.as_str() && !button.is_active() {
                    button.set_active(true);
                }
            }
            child = widget.next_sibling();
        }
    }
}

/// The sidebar's rows and the number of sessions Load More would add.
/// With `expand`, collapsed projects list their sessions too.
fn current_layout(app: &App, expand: bool) -> (Vec<Item>, usize) {
    let (recent, by_activity) = {
        let settings = app.settings.borrow();
        (
            settings.sidebar_view == "recent",
            settings.project_order == "recent",
        )
    };
    let since = since(app.ui.sidebar.days.get());
    let times = if recent || by_activity || since.is_some() {
        activity_times(app)
    } else {
        HashMap::new()
    };
    let keep = always_listed(app);
    let mut groups = visible_groups(app);
    if expand {
        for group in &mut groups {
            group.is_collapsed = false;
        }
    }
    let filter = Filter { since, keep: &keep };
    layout(&groups, &times, recent, by_activity, &filter)
}

/// Sessions in the order the sidebar shows them, collapsed ones included,
/// for next / previous session.
pub fn tab_order(app: &App) -> Vec<Uuid> {
    current_layout(app, true)
        .0
        .into_iter()
        .filter_map(|item| match item {
            Item::Tab(t) => Some(t.id),
            Item::Group(_) => None,
        })
        .collect()
}

pub fn rebuild(app: &Rc<App>) {
    let colored = crate::model::assign_colors(&mut app.ws.borrow_mut());
    if colored {
        app.schedule_save();
    }
    fill_machines(app);
    let sidebar = &app.ui.sidebar;
    sidebar.syncing.set(true);
    sidebar.list.remove_all();
    sidebar.rows.borrow_mut().clear();
    let (items, hidden) = current_layout(app, false);
    *sidebar.shown.borrow_mut() = items.iter().map(Item::id).collect();
    for item in &items {
        match item {
            Item::Group(group) => sidebar.list.append(&group_row(app, group)),
            Item::Tab(tab) => {
                let row = session_row(app, tab);
                sidebar.list.append(&row.row);
                sidebar.rows.borrow_mut().insert(tab.id, row);
            }
        }
    }
    if let Some(row) = more_row(sidebar.days.get(), hidden) {
        sidebar.list.append(&row);
    }
    sidebar.syncing.set(false);
    let ids: Vec<Uuid> = sidebar.rows.borrow().keys().copied().collect();
    for id in ids {
        sidebar.update_row(app, id);
    }
    sidebar.select_row(app.selected.get());
}

/// Load More at the end of the list, or Show Today Only once everything
/// older is listed.
fn more_row(days: u32, hidden: usize) -> Option<gtk::ListBoxRow> {
    let label = match (hidden, next_days(days)) {
        (0, _) if days == 1 => return None,
        (0, _) | (_, None) => tr("Show Today Only").to_string(),
        (n, Some(_)) => crate::trf!("Load More ({n} older)", n = n),
    };
    let row = gtk::ListBoxRow::new();
    row.set_widget_name("more");
    row.set_selectable(false);
    let label = gtk::Label::new(Some(&label));
    label.add_css_class("dim-label");
    label.add_css_class("omux-provider");
    label.set_margin_top(8);
    label.set_margin_bottom(8);
    row.set_child(Some(&label));
    Some(row)
}

/// Moves rows when the activity order changes, or a session becomes active
/// or a new day starts, unless the user is in the middle of something in
/// the sidebar (renaming, hovering a row).
fn resort_if_needed(app: &Rc<App>) {
    let sidebar = &app.ui.sidebar;
    let sorted = {
        let settings = app.settings.borrow();
        settings.sidebar_view == "recent"
            || settings.project_order == "recent"
            || sidebar.days.get() != 0
    };
    if !sorted
        || app.renaming.get().is_some()
        || app.renaming_group.get().is_some()
        || sidebar.hovered.get().is_some()
    {
        return;
    }
    let ids: Vec<Uuid> = current_layout(app, false).0.iter().map(Item::id).collect();
    if *sidebar.shown.borrow() != ids {
        rebuild(app);
    }
}

fn group_row(app: &Rc<App>, group: &TabGroup) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&format!("group:{}", group.id));
    row.set_selectable(false);
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    hbox.set_margin_start(6);
    hbox.set_margin_end(4);
    hbox.set_margin_top(10);
    hbox.set_margin_bottom(2);
    let chevron = gtk::Image::from_icon_name(if group.is_collapsed {
        "pan-end-symbolic"
    } else {
        "pan-down-symbolic"
    });
    chevron.set_opacity(0.5);
    hbox.append(&chevron);

    if app.renaming_group.get() == Some(group.id) {
        let entry = rename_entry(&group.name);
        let weak = Rc::downgrade(app);
        let id = group.id;
        commit_on_finish(&entry, move |text| {
            let Some(app) = weak.upgrade() else { return };
            app.renaming_group.set(None);
            if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                app.update_group(id, |g| g.name = text.trim().to_string());
            }
            app.refresh_sidebar();
        });
        hbox.append(&entry);
        glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    } else {
        let name = gtk::Label::new(Some(&group.name));
        name.add_css_class("omux-project");
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hbox.append(&name);
        if let Some(host) = &group.ssh_host {
            hbox.append(&host_pill(host));
        }
    }

    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.add_css_class("circular");
    add.set_tooltip_text(Some(tr("New session")));
    add.set_opacity(0.0);
    add.set_action_name(Some("app.group-new-session"));
    add.set_action_target_value(Some(&group.id.to_string().to_variant()));
    hbox.append(&add);
    let motion = gtk::EventControllerMotion::new();
    let shown = add.clone();
    motion.connect_enter(move |_, _, _| shown.set_opacity(1.0));
    let hidden = add.clone();
    motion.connect_leave(move |_| hidden.set_opacity(0.0));
    row.add_controller(motion);

    row.set_child(Some(&hbox));
    drag_source(&row, format!("group:{}", group.id));
    row
}

/// A small "☁ host" sticker for remote projects and sessions.
fn host_pill(host: &str) -> gtk::Label {
    let pill = gtk::Label::new(Some(&format!("☁ {host}")));
    pill.add_css_class("omux-pill");
    pill.set_valign(gtk::Align::Center);
    pill.set_tooltip_text(Some(&crate::trf!("Runs on {host}", host = host)));
    pill
}

fn session_row(app: &Rc<App>, tab: &TabItem) -> SessionRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&format!("tab:{}", tab.id));
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_margin_start(12);
    outer.set_margin_end(8);
    outer.set_margin_top(5);
    outer.set_margin_bottom(5);

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let dot = dot();
    line.append(&dot);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_hexpand(true);

    let name_stack = gtk::Stack::new();
    let name = gtk::Label::new(Some(&tab.name));
    name.add_css_class("omux-session-name");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_stack.add_named(&name, Some("label"));
    if app.renaming.get() == Some(tab.id) {
        let entry = rename_entry(&tab.name);
        let weak = Rc::downgrade(app);
        let id = tab.id;
        commit_on_finish(&entry, move |text| {
            let Some(app) = weak.upgrade() else { return };
            app.renaming.set(None);
            if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                app.update_tab(id, |t| t.name = text.trim().to_string());
            }
            app.refresh_sidebar();
            app.ui.header.update(&app);
        });
        name_stack.add_named(&entry, Some("entry"));
        name_stack.set_visible_child_name("entry");
        glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    }
    text.append(&name_stack);

    let status = gtk::Label::new(None);
    status.add_css_class("omux-status");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&status);
    line.append(&text);

    let trailing = gtk::Stack::new();
    trailing.set_valign(gtk::Align::Center);
    trailing.add_named(
        &gtk::Box::new(gtk::Orientation::Horizontal, 0),
        Some("none"),
    );
    let bell = gtk::Image::from_icon_name("preferences-system-notifications-symbolic");
    bell.add_css_class("omux-bell");
    trailing.add_named(&bell, Some("bell"));
    let activity = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    activity.add_css_class("omux-activity");
    activity.set_halign(gtk::Align::Center);
    activity.set_valign(gtk::Align::Center);
    trailing.add_named(&activity, Some("activity"));
    let start_stop = gtk::Button::from_icon_name("media-playback-start-symbolic");
    start_stop.add_css_class("flat");
    start_stop.add_css_class("circular");
    let weak = Rc::downgrade(app);
    let id = tab.id;
    start_stop.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        if app.rt(id).busy {
            app.stop(id);
        } else {
            app.start(id);
        }
    });
    trailing.add_named(&start_stop, Some("button"));
    trailing.set_hhomogeneous(false);
    // Without project headers, a remote session shows its host.
    if app.settings.borrow().sidebar_view == "recent" && machine(app) == "all" {
        if let Some(host) = app.tab(tab.id).and_then(|(g, _)| g.ssh_host) {
            line.append(&host_pill(&host));
        }
    }
    line.append(&trailing);
    outer.append(&line);

    let permission = permission_buttons(app, tab.id);
    permission.set_margin_start(16);
    outer.append(&permission);

    let extras = gtk::Box::new(gtk::Orientation::Vertical, 2);
    extras.set_margin_start(16);
    outer.append(&extras);
    row.set_child(Some(&outer));

    let motion = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(app);
    motion.connect_enter(move |_, _, _| {
        if let Some(app) = weak.upgrade() {
            app.ui.sidebar.hovered.set(Some(id));
            app.refresh_busy(id);
            app.ui.sidebar.update_row(&app, id);
        }
    });
    let weak = Rc::downgrade(app);
    motion.connect_leave(move |_| {
        if let Some(app) = weak.upgrade() {
            if app.ui.sidebar.hovered.get() == Some(id) {
                app.ui.sidebar.hovered.set(None);
            }
            app.ui.sidebar.update_row(&app, id);
        }
    });
    row.add_controller(motion);
    drag_source(&row, format!("tab:{}", tab.id));

    SessionRow {
        row,
        dot,
        name,
        status,
        permission,
        trailing,
        start_stop,
        extras,
    }
}

/// Allow / Deny (plus details for Codex), shown while a session waits for
/// permission.
pub fn permission_buttons(app: &Rc<App>, id: Uuid) -> gtk::Box {
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    hbox.set_visible(false);
    for (label, allow) in [(tr("Allow"), true), (tr("Deny"), false)] {
        let button = gtk::Button::with_label(label);
        button.add_css_class("omux-small");
        if allow {
            button.add_css_class("suggested-action");
        }
        let weak = Rc::downgrade(app);
        button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.answer_permission(id, allow);
            }
        });
        hbox.append(&button);
    }
    let info = gtk::MenuButton::builder()
        .icon_name("dialog-information-symbolic")
        .tooltip_text(tr("What Codex asks for"))
        .build();
    info.add_css_class("flat");
    info.add_css_class("omux-small");
    let weak = Rc::downgrade(app);
    info.set_create_popup_func(move |button| {
        let Some(app) = weak.upgrade() else { return };
        let detail = crate::codex::pending_approval(&app, id).unwrap_or_default();
        let label = gtk::Label::new(Some(&detail));
        label.add_css_class("omux-mono");
        label.set_selectable(true);
        label.set_wrap(true);
        label.set_max_width_chars(60);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&label)
            .min_content_width(420)
            .min_content_height(120)
            .max_content_height(180)
            .build();
        let popover = gtk::Popover::new();
        popover.set_child(Some(&scroller));
        button.set_popover(Some(&popover));
    });
    hbox.append(&info);
    let weak = Rc::downgrade(app);
    hbox.connect_map(move |_| {
        if let Some(app) = weak.upgrade() {
            info.set_visible(crate::codex::pending_approval(&app, id).is_some());
        }
    });
    hbox
}

fn rename_entry(text: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_text(text);
    entry.set_hexpand(true);
    entry.add_css_class("omux-small");
    entry
}

/// Enter or leaving the field commits; Escape cancels (`None`).
fn commit_on_finish(entry: &gtk::Entry, done: impl Fn(Option<String>) + 'static) {
    let done = Rc::new(done);
    let finished = Rc::new(Cell::new(false));
    let (d, f) = (done.clone(), finished.clone());
    entry.connect_activate(move |entry| {
        if !f.replace(true) {
            d(Some(entry.text().to_string()));
        }
    });
    let keys = gtk::EventControllerKey::new();
    let (d, f) = (done.clone(), finished.clone());
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if !f.replace(true) {
                d(None);
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(keys);
    let focus = gtk::EventControllerFocus::new();
    let entry_ref = entry.downgrade();
    focus.connect_leave(move |_| {
        if let Some(entry) = entry_ref.upgrade() {
            if !finished.replace(true) {
                let text = entry.text().to_string();
                // Leaving rebuilds the sidebar; do it after GTK finishes the focus change.
                let done = done.clone();
                glib::idle_add_local_once(move || done(Some(text)));
            }
        }
    });
    entry.add_controller(focus);
}

fn drag_source(row: &gtk::ListBoxRow, payload: String) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    source
        .connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
    row.add_controller(source);
}

fn row_target(list: &gtk::ListBox, y: f64) -> Option<(String, Uuid)> {
    let row = list.row_at_y(y as i32)?;
    let name = row.widget_name();
    let (kind, id) = name.split_once(':')?;
    Some((kind.to_string(), Uuid::parse_str(id).ok()?))
}

fn group_of_row(app: &Rc<App>, kind: &str, id: Uuid) -> Option<Uuid> {
    match kind {
        "group" => Some(id),
        _ => app.tab(id).map(|(g, _)| g.id),
    }
}

pub fn connect(app: &Rc<App>) {
    let sidebar = &app.ui.sidebar;
    let weak = Rc::downgrade(app);
    glib::timeout_add_local(Duration::from_secs(5), move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        resort_if_needed(&app);
        glib::ControlFlow::Continue
    });
    let weak = Rc::downgrade(app);
    sidebar.list.connect_row_activated(move |_, row| {
        let Some(app) = weak.upgrade() else { return };
        if app.ui.sidebar.syncing.get() {
            return;
        }
        let name = row.widget_name();
        if name == "more" {
            let sidebar = &app.ui.sidebar;
            let days = sidebar.days.get();
            let hidden = current_layout(&app, false).1;
            // Nothing older left: back to today.
            let mut next = if hidden == 0 {
                1
            } else {
                next_days(days).unwrap_or(1)
            };
            // Skip a step that adds nothing (nothing from this week, but
            // some from this month).
            while next != 0 && next != 1 {
                sidebar.days.set(next);
                if current_layout(&app, false).1 < hidden {
                    break;
                }
                next = next_days(next).unwrap_or(0);
            }
            sidebar.days.set(next);
            // Not from inside the row's own signal: rebuilding removes it.
            let weak = Rc::downgrade(&app);
            glib::idle_add_local_once(move || {
                if let Some(app) = weak.upgrade() {
                    app.refresh_sidebar();
                }
            });
            return;
        }
        let Some((kind, id)) = name.split_once(':') else {
            return;
        };
        let Ok(id) = Uuid::parse_str(id) else { return };
        if kind == "group" {
            app.update_group(id, |g| g.is_collapsed = !g.is_collapsed);
            app.refresh_sidebar();
        } else {
            app.select(id);
        }
    });

    // Arrow keys move the selection; follow it like a click.
    let weak = Rc::downgrade(app);
    sidebar.list.connect_row_selected(move |_, row| {
        let (Some(app), Some(row)) = (weak.upgrade(), row) else {
            return;
        };
        if app.ui.sidebar.syncing.get() {
            return;
        }
        let name = row.widget_name();
        if let Some(id) = name
            .strip_prefix("tab:")
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            if app.selected.get() != Some(id) {
                app.select(id);
            }
        }
    });

    // Right click: session, project or empty-area menu.
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let weak = Rc::downgrade(app);
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(app) = weak.upgrade() else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let model = match row_target(&app.ui.sidebar.list, y) {
            Some((kind, id)) if kind == "tab" => tab_menu(&app, id),
            Some((_, id)) => group_menu(&app, id),
            None => empty_menu(),
        };
        let sidebar = &app.ui.sidebar;
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let (x, y) = sidebar
            .list
            .compute_point(&sidebar.root, &point)
            .map_or((x, y), |p| (p.x() as f64, p.y() as f64));
        let menu = &sidebar.menu;
        menu.set_menu_model(Some(&model));
        menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        menu.popup();
    });
    sidebar.list.add_controller(click);

    // Reordering by drag.
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let weak = Rc::downgrade(app);
    target.connect_drop(move |_, value, _, y| {
        let Some(app) = weak.upgrade() else {
            return false;
        };
        // The recent list has no projects to drop into.
        if app.settings.borrow().sidebar_view == "recent" {
            return false;
        }
        let Ok(payload) = value.get::<String>() else {
            return false;
        };
        let Some((kind, id)) = payload.split_once(':') else {
            return false;
        };
        let Ok(id) = Uuid::parse_str(id) else {
            return false;
        };
        let target = row_target(&app.ui.sidebar.list, y);
        match (kind, target) {
            ("tab", Some((target_kind, target_id))) => {
                let Some(group) = group_of_row(&app, &target_kind, target_id) else {
                    return false;
                };
                let after = (target_kind == "tab").then_some(target_id);
                app.drop_tab(id, group, after);
            }
            ("tab", None) => {
                if let Some(last) = app.ws.borrow().groups.last().map(|g| g.id) {
                    let after = app
                        .ws
                        .borrow()
                        .groups
                        .last()
                        .and_then(|g| g.tabs.last().map(|t| t.id));
                    drop_later(&app, move |app| app.drop_tab(id, last, after));
                }
            }
            ("group", Some((target_kind, target_id))) => {
                let Some(before) = group_of_row(&app, &target_kind, target_id) else {
                    return false;
                };
                app.drop_group(id, before);
            }
            ("group", None) => app.drop_group(id, Uuid::nil()),
            _ => return false,
        }
        true
    });
    sidebar.list.add_controller(target);

    // Folders dropped from the file manager open as projects.
    let files = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let weak = Rc::downgrade(app);
    files.connect_drop(move |_, value, _, _| {
        let Some(app) = weak.upgrade() else {
            return false;
        };
        let Ok(list) = value.get::<gdk::FileList>() else {
            return false;
        };
        for file in list.files() {
            if let Some(path) = file.path().filter(|p| p.is_dir()) {
                app.open_folder(&path);
            }
        }
        true
    });
    sidebar.list.add_controller(files);
}

fn drop_later(app: &Rc<App>, f: impl FnOnce(&Rc<App>) + 'static) {
    let weak = Rc::downgrade(app);
    glib::idle_add_local_once(move || {
        if let Some(app) = weak.upgrade() {
            f(&app);
        }
    });
}

fn item(menu: &gio::Menu, label: &str, action: &str, target: &str) {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
    menu.append_item(&item);
}

fn tab_menu(app: &Rc<App>, id: Uuid) -> gio::Menu {
    let menu = gio::Menu::new();
    let target = id.to_string();
    let top = gio::Menu::new();
    item(&top, tr("Rename"), "app.tab-rename", &target);
    item(&top, tr("Edit Session…"), "app.tab-edit", &target);
    let colors = gio::Menu::new();
    // No "None": a session without a color gets one again.
    for color in TabColor::ALL.into_iter().skip(1) {
        item(
            &colors,
            tr(&color.label()),
            "app.tab-color",
            &format!("{id}:{}", color.raw()),
        );
    }
    top.append_submenu(Some(tr("Color")), &colors);
    let groups = app.ws.borrow().groups.clone();
    if groups.len() > 1 {
        let move_to = gio::Menu::new();
        let own = app.tab(id).map(|(g, _)| g.id);
        for group in groups.iter().filter(|g| Some(g.id) != own) {
            item(
                &move_to,
                &group.name,
                "app.tab-move",
                &format!("{id}:{}", group.id),
            );
        }
        top.append_submenu(Some(tr("Move to")), &move_to);
    }
    menu.append_section(None, &top);
    if let Some(section) = crate::ui::worktree_actions::menu_section(app, id) {
        menu.append_section(None, &section);
    }
    let process = gio::Menu::new();
    item(
        &process,
        tr("Open Terminal Here"),
        "app.tab-terminal",
        &target,
    );
    item(
        &process,
        tr("Interrupt (Ctrl-C)"),
        "app.tab-interrupt",
        &target,
    );
    item(&process, tr("Restart Session"), "app.tab-restart", &target);
    item(&process, tr("Kill Session"), "app.tab-kill", &target);
    item(&process, tr("Close Session"), "app.tab-close", &target);
    menu.append_section(None, &process);
    menu
}

fn group_menu(app: &Rc<App>, group_id: Uuid) -> gio::Menu {
    let menu = gio::Menu::new();
    let target = group_id.to_string();
    let new = gio::Menu::new();
    item(&new, tr("New Session"), "app.group-new-session", &target);
    item(
        &new,
        tr("New Session in Worktree…"),
        "app.group-new-worktree",
        &target,
    );
    item(&new, tr("New Shell"), "app.group-new-shell", &target);
    if let Some(host) = app
        .ws
        .borrow()
        .groups
        .iter()
        .find(|g| g.id == group_id)
        .and_then(|g| g.ssh_host.clone())
    {
        item(
            &new,
            &crate::trf!("Import Conversations from {host}", host = host),
            "app.import-remote",
            &host,
        );
    }
    menu.append_section(None, &new);
    let edit = gio::Menu::new();
    item(&edit, tr("Rename"), "app.group-rename", &target);
    item(&edit, tr("Edit Project…"), "app.group-edit", &target);
    menu.append_section(None, &edit);
    let order = gio::Menu::new();
    let index = app.group_index(group_id).unwrap_or(0);
    let count = app.ws.borrow().groups.len();
    if index > 0 {
        item(&order, tr("Move Up"), "app.group-up", &target);
    }
    if index + 1 < count {
        item(&order, tr("Move Down"), "app.group-down", &target);
    }
    menu.append_section(None, &order);
    let delete = gio::Menu::new();
    item(&delete, tr("Delete Project"), "app.group-delete", &target);
    menu.append_section(None, &delete);
    menu
}

fn empty_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let top = gio::Menu::new();
    top.append(Some(tr("New Project")), Some("app.new-project"));
    top.append(Some(tr("Open Folder as Project…")), Some("app.open-folder"));
    top.append(
        Some(tr("New Remote Project…")),
        Some("app.new-remote-project"),
    );
    top.append(
        Some(tr("Open Recent Conversation…")),
        Some("app.open-recent"),
    );
    top.append(
        Some(tr("Import Claude Code Conversations")),
        Some("app.import-claude"),
    );
    top.append(Some(tr("New Codex Session")), Some("app.new-codex"));
    menu.append_section(None, &top);
    let bottom = gio::Menu::new();
    bottom.append(Some(tr("New Session")), Some("app.new-session"));
    bottom.append(Some(tr("New Shell")), Some("app.new-shell"));
    menu.append_section(None, &bottom);
    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, tabs: &[u128]) -> TabGroup {
        let tabs = tabs
            .iter()
            .map(|n| {
                let mut tab = TabItem::new(format!("t{n}"));
                tab.id = Uuid::from_u128(*n);
                tab
            })
            .collect();
        TabGroup::new(name, None, tabs)
    }

    fn names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Group(g) => g.name.clone(),
                Item::Tab(t) => t.name.clone(),
            })
            .collect()
    }

    #[test]
    fn layouts() {
        let mut collapsed = group("c", &[5]);
        collapsed.is_collapsed = true;
        let groups = vec![group("a", &[1, 2]), group("b", &[3, 4]), collapsed];
        // 4 is the latest, then 1, 5, 2; 3 never ran.
        let times: HashMap<Uuid, u64> = [(1, 30), (2, 10), (4, 40), (5, 20)]
            .into_iter()
            .map(|(n, t)| (Uuid::from_u128(n), t))
            .collect();

        let none = HashSet::new();
        let all = Filter {
            since: None,
            keep: &none,
        };
        assert_eq!(
            names(&layout(&groups, &times, false, false, &all).0),
            ["a", "t1", "t2", "b", "t3", "t4", "c"]
        );
        assert_eq!(
            names(&layout(&groups, &times, false, true, &all).0),
            ["b", "t4", "t3", "a", "t1", "t2", "c"]
        );
        assert_eq!(
            names(&layout(&groups, &times, true, false, &all).0),
            ["t4", "t1", "t5", "t2", "t3"]
        );
    }

    #[test]
    fn only_recent_sessions() {
        let groups = vec![group("a", &[1, 2]), group("b", &[3]), group("empty", &[])];
        let times: HashMap<Uuid, u64> = [(1, 100), (2, 10), (3, 5)]
            .into_iter()
            .map(|(n, t)| (Uuid::from_u128(n), t))
            .collect();
        let none = HashSet::new();
        let today = Filter {
            since: Some(50),
            keep: &none,
        };
        // b has nothing recent and goes; an empty project stays.
        let (items, hidden) = layout(&groups, &times, false, false, &today);
        assert_eq!(names(&items), ["a", "t1", "empty"]);
        assert_eq!(hidden, 2);
        let (items, hidden) = layout(&groups, &times, true, false, &today);
        assert_eq!(names(&items), ["t1"]);
        assert_eq!(hidden, 2);
        // The selected or a running session shows whatever its age.
        let keep: HashSet<Uuid> = [Uuid::from_u128(3)].into_iter().collect();
        let kept = Filter {
            since: Some(50),
            keep: &keep,
        };
        let (items, hidden) = layout(&groups, &times, false, false, &kept);
        assert_eq!(names(&items), ["a", "t1", "b", "t3", "empty"]);
        assert_eq!(hidden, 1);
    }

    #[test]
    fn load_more_steps() {
        assert_eq!(next_days(1), Some(7));
        assert_eq!(next_days(7), Some(30));
        assert_eq!(next_days(30), Some(0));
        assert_eq!(next_days(0), None);
    }
}
