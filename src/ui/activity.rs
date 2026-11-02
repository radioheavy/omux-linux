//! The Activity tab: what the selected Claude Code session runs in the
//! background (subagents, workflows and background shells), live, on this computer or
//! its SSH host. The list follows the newest running item until you pick
//! one; the pane below streams it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use uuid::Uuid;

use crate::activity::{Agent, JsonlTail, Shell, Snapshot, Workflow};
use crate::app::App;
use crate::i18n::tr;
use crate::model::SessionProvider;

/// Characters a shell's output view keeps.
const KEEP: usize = 400_000;

struct State {
    status: gtk::Label,
    list: gtk::ListBox,
    title: gtk::Label,
    holder: gtk::Box,
    /// The session shown, and its host.
    session: Option<Uuid>,
    snapshot: Snapshot,
    /// `agent:<id>`, `shell:<id>` or `workflow:<id>`.
    chosen: Option<String>,
    /// Workflows opened or closed by hand; a running one starts open.
    toggled: HashSet<String>,
    picked: bool,
    detail: Option<String>,
    busy: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.set_wrap(true);
    status.set_margin_top(10);
    status.set_margin_bottom(10);
    status.set_margin_start(12);
    status.set_margin_end(12);
    root.append(&status);

    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(220)
        .build();
    root.append(&scroller);

    let hairline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hairline.add_css_class("omux-hairline");
    root.append(&hairline);
    let title = gtk::Label::new(None);
    title.add_css_class("omux-header-name");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_margin_start(12);
    title.set_margin_end(12);
    title.set_margin_top(6);
    title.set_margin_bottom(4);
    root.append(&title);
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.set_vexpand(true);
    root.append(&holder);

    let weak = Rc::downgrade(app);
    list.connect_row_activated(move |_, row| {
        let Some(app) = weak.upgrade() else { return };
        let key = row.widget_name().to_string();
        let workflow = key.starts_with("workflow:");
        with(|s| {
            if workflow && !s.toggled.remove(&key) {
                s.toggled.insert(key.clone());
            }
            s.chosen = Some(key);
            s.picked = true;
        });
        if workflow {
            // Shows or hides its agents.
            render(&app);
        } else {
            show_detail(&app);
        }
    });

    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            status,
            list,
            title,
            holder,
            session: None,
            snapshot: Snapshot::default(),
            chosen: None,
            toggled: HashSet::new(),
            picked: false,
            detail: None,
            busy: false,
        })
    });

    let weak = Rc::downgrade(app);
    glib::timeout_add_local(Duration::from_secs(2), move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        refresh(&app);
        glib::ControlFlow::Continue
    });
    root.upcast()
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

/// The selected session, if it's a Claude Code one: its tab id, Claude
/// session id and SSH host.
fn target(app: &App) -> Result<(Uuid, Uuid, Option<String>), &'static str> {
    let id = app.selected.get().ok_or("No session")?;
    let (group, tab) = app.tab(id).ok_or("No session")?;
    if tab.resolved_provider(&group) != SessionProvider::Claude {
        return Err("Activity shows what Claude Code sessions run in the background.");
    }
    Ok((id, tab.claude_session_id, group.ssh_host))
}

/// A new session was selected: start over.
pub fn selection_changed(app: &Rc<App>) {
    let id = app.selected.get();
    let changed = with(|s| {
        let changed = s.session != id;
        if changed {
            s.session = id;
            s.snapshot = Snapshot::default();
            s.chosen = None;
            s.toggled.clear();
            s.picked = false;
            s.detail = None;
        }
        changed
    })
    .unwrap_or(false);
    if changed {
        render(app);
        refresh(app);
    }
}

/// Opens the tab on one subagent (clicked in the sidebar or header).
pub fn focus_agent(app: &Rc<App>, agent_id: &str) {
    with(|s| {
        s.chosen = Some(format!("agent:{agent_id}"));
        s.picked = true;
    });
    crate::ui::panel::show_tab(app, "activity");
    show_detail(app);
    refresh(app);
}

fn refresh(app: &Rc<App>) {
    if !crate::ui::panel::is_showing(app, "activity") {
        return;
    }
    let (tab, session, host) = match target(app) {
        Ok(t) => t,
        Err(message) => {
            with(|s| {
                s.snapshot = Snapshot::default();
                s.status.set_text(tr(message));
            });
            render(app);
            return;
        }
    };
    if with(|s| std::mem::replace(&mut s.busy, true)).unwrap_or(true) {
        return;
    }
    let weak = Rc::downgrade(app);
    let probe_host = host.clone();
    crate::ui::panel::background(
        move || crate::activity::snapshot(probe_host.as_deref(), session),
        move |result| {
            with(|s| s.busy = false);
            let Some(app) = weak.upgrade() else { return };
            if app.selected.get() != Some(tab) {
                return;
            }
            match result {
                Ok(snapshot) => {
                    let changed = with(|s| {
                        let changed = s.snapshot != snapshot;
                        s.snapshot = snapshot;
                        changed
                    })
                    .unwrap_or(false);
                    if changed {
                        render(&app);
                    }
                }
                Err(err) => {
                    let text = match &host {
                        Some(host) => crate::trf!(
                            "Couldn't read the activity on {host}: {err}",
                            host = host,
                            err = err
                        ),
                        None => crate::trf!("Couldn't read the activity: {err}", err = err),
                    };
                    with(|s| s.status.set_text(&text));
                }
            }
        },
    );
}

/// One row of the list.
struct Item {
    key: String,
    title: String,
    detail: String,
    color: &'static str,
    running: bool,
    modified: f64,
    /// A workflow's agent, listed under it.
    nested: bool,
}

fn agent_item(a: &Agent) -> Item {
    let state = if a.finished {
        tr("Done").to_string()
    } else {
        a.activity
            .clone()
            .unwrap_or_else(|| tr("starting").to_string())
    };
    Item {
        key: format!("agent:{}", a.id),
        title: if a.description.is_empty() {
            a.kind.clone()
        } else {
            a.description.clone()
        },
        detail: format!("{} · {state}", a.phase.as_deref().unwrap_or(&a.kind)),
        color: if a.finished { "green" } else { "blue" },
        running: !a.finished,
        modified: a.modified,
        nested: a.phase.is_some(),
    }
}

fn shell_item(sh: &Shell) -> Item {
    let state = state_name(&sh.status);
    Item {
        key: format!("shell:{}", sh.id),
        title: if sh.description.is_empty() {
            sh.command.clone()
        } else {
            sh.description.clone()
        },
        detail: format!("$ {} · {state}", one_line(&sh.command)),
        color: match sh.status.as_str() {
            "running" => "blue",
            "completed" => "green",
            _ => "red",
        },
        running: sh.running(),
        modified: sh.modified,
        nested: false,
    }
}

fn state_name(status: &str) -> &str {
    match status {
        "running" => tr("Running…"),
        "completed" => tr("Done"),
        "failed" => tr("Failed"),
        "killed" => tr("Stopped"),
        other => other,
    }
}

fn workflow_item(w: &Workflow, open: bool) -> Item {
    let running = w.agents.iter().filter(|a| !a.finished).count();
    let done = w.agents.len() - running;
    let detail = if w.running() {
        let mut parts = Vec::new();
        if let Some(phase) = w.current_phase() {
            let n = w
                .phases
                .iter()
                .position(|p| p == phase)
                .map_or(0, |i| i + 1);
            parts.push(if n > 0 && w.phases.len() > 1 {
                crate::trf!(
                    "{phase} ({n}/{total})",
                    phase = phase,
                    n = n,
                    total = w.phases.len()
                )
            } else {
                phase.to_string()
            });
        }
        parts.push(crate::trf!("{n} running", n = running));
        parts.push(crate::trf!("{n} done", n = done));
        parts.join(" · ")
    } else {
        let mut parts = vec![
            state_name(&w.status).to_string(),
            crate::trf!("{n} agents", n = w.agents.len()),
        ];
        if let Some(ms) = w.duration_ms {
            parts.push(duration(ms));
        }
        parts.join(" · ")
    };
    Item {
        key: format!("workflow:{}", w.id),
        title: format!("{} {}", if open { "▾" } else { "▸" }, w.name),
        detail: format!("{} · {detail}", tr("Workflow")),
        color: match w.status.as_str() {
            "running" => "purple",
            "completed" => "green",
            _ => "red",
        },
        running: w.running(),
        modified: w.modified,
        nested: false,
    }
}

fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m) = (secs / 3600, secs / 60 % 60);
    if h > 0 {
        crate::trf!("{h}h {m}m", h = h, m = m)
    } else if m > 0 {
        crate::trf!("{m}m", m = m)
    } else {
        crate::trf!("{s}s", s = secs)
    }
}

/// A workflow's agents, phase by phase, in the order they started.
fn by_phase(w: &Workflow) -> Vec<&Agent> {
    let mut agents: Vec<&Agent> = w.agents.iter().collect();
    let rank = |a: &Agent| {
        a.phase
            .as_ref()
            .and_then(|p| w.phases.iter().position(|q| q == p))
            .unwrap_or(usize::MAX)
    };
    agents.sort_by_key(|a| rank(a));
    agents
}

fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(80).collect::<String>())
    } else {
        line
    }
}

fn is_open(w: &Workflow, toggled: &HashSet<String>) -> bool {
    w.running() != toggled.contains(&format!("workflow:{}", w.id))
}

/// Running first, then the latest; an open workflow's agents follow it.
fn items(snapshot: &Snapshot, toggled: &HashSet<String>) -> Vec<Item> {
    let mut top: Vec<(Item, Option<&Workflow>)> = snapshot
        .shells
        .iter()
        .map(|sh| (shell_item(sh), None))
        .chain(snapshot.agents.iter().map(|a| (agent_item(a), None)))
        .chain(
            snapshot
                .workflows
                .iter()
                .map(|w| (workflow_item(w, is_open(w, toggled)), Some(w))),
        )
        .collect();
    top.sort_by(|(a, _), (b, _)| {
        b.running
            .cmp(&a.running)
            .then(b.modified.total_cmp(&a.modified))
    });
    let mut items = Vec::new();
    for (item, workflow) in top {
        items.push(item);
        if let Some(w) = workflow.filter(|w| is_open(w, toggled)) {
            items.extend(by_phase(w).into_iter().map(|a| Item {
                nested: true,
                ..agent_item(a)
            }));
        }
    }
    items
}

/// Every key that can be chosen, hidden agents of closed workflows too.
fn known(snapshot: &Snapshot, key: &str) -> bool {
    match key.split_once(':') {
        Some(("agent", id)) => snapshot.agent(id).is_some(),
        Some(("shell", id)) => snapshot.shells.iter().any(|s| s.id == id),
        Some(("workflow", id)) => snapshot.workflows.iter().any(|w| w.id == id),
        _ => false,
    }
}

fn render(app: &Rc<App>) {
    let items = with(|s| items(&s.snapshot, &s.toggled)).unwrap_or_default();
    with(|s| {
        // Follow the newest running item until one is picked; a picked
        // one that's gone falls back too.
        let present = s.chosen.as_ref().is_some_and(|k| known(&s.snapshot, k));
        if !s.picked || (!items.is_empty() && !present) {
            s.chosen = items.first().map(|i| i.key.clone());
            s.picked = false;
        }
        // A workflow's summary is drawn again with each change.
        if s.chosen
            .as_ref()
            .is_some_and(|k| k.starts_with("workflow:"))
        {
            s.detail = None;
        }
        while let Some(row) = s.list.row_at_index(0) {
            s.list.remove(&row);
        }
        for item in &items {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&item.key);
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            hbox.set_margin_top(3);
            hbox.set_margin_bottom(3);
            if item.nested {
                hbox.set_margin_start(18);
            }
            let dot = crate::ui::sidebar::dot();
            crate::ui::sidebar::set_dot(&dot, Some(item.color));
            dot.set_valign(gtk::Align::Center);
            hbox.append(&dot);
            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 1);
            let title = gtk::Label::new(Some(&item.title));
            title.set_xalign(0.0);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            vbox.append(&title);
            let detail = gtk::Label::new(Some(&item.detail));
            detail.add_css_class("omux-status");
            detail.set_xalign(0.0);
            detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
            vbox.append(&detail);
            hbox.append(&vbox);
            row.set_child(Some(&hbox));
            s.list.append(&row);
            if s.chosen.as_deref() == Some(item.key.as_str()) {
                s.list.select_row(Some(&row));
            }
        }
        s.list.set_visible(!items.is_empty());
        if items.is_empty() {
            if s.status.text().is_empty() || s.snapshot == Snapshot::default() {
                s.status.set_text(tr(
                    "Nothing in the background. Subagents, workflows and shell commands this session starts in the background show up here, live.",
                ));
            }
            s.status.set_visible(true);
        } else {
            s.status.set_visible(false);
        }
    });
    show_detail(app);
}

/// Streams the chosen item below the list.
fn show_detail(app: &Rc<App>) {
    let Ok((_, _, host)) = target(app) else {
        with(clear_detail);
        return;
    };
    let chosen = with(|s| s.chosen.clone()).flatten();
    if with(|s| s.detail == chosen).unwrap_or(true) {
        return;
    }
    with(|s| {
        clear_detail(s);
        s.detail = chosen.clone();
        let Some(key) = &chosen else { return };
        if let Some(id) = key.strip_prefix("workflow:") {
            if let Some(w) = s.snapshot.workflows.iter().find(|w| w.id == id) {
                s.title.set_text(&w.name);
                s.holder.append(&workflow_view(w));
            }
        } else if let Some(id) = key.strip_prefix("agent:") {
            if let Some(agent) = s.snapshot.agent(id) {
                s.title.set_text(&agent_item(agent).title);
                let mut tail = JsonlTail::new(host.clone(), agent.path.clone());
                s.holder
                    .append(&crate::ui::agents::transcript_view_from(move || {
                        tail.read()
                    }));
            }
        } else if let Some(id) = key.strip_prefix("shell:") {
            if let Some(shell) = s.snapshot.shells.iter().find(|sh| sh.id == id) {
                s.title.set_text(&format!("$ {}", one_line(&shell.command)));
                s.holder
                    .append(&output_view(host.clone(), shell.output.clone()));
            }
        }
    });
}

/// A workflow at a glance: its phases and how far each got.
fn workflow_view(w: &Workflow) -> gtk::Widget {
    let text = |content: &str, class: Option<&str>| {
        let label = gtk::Label::new(Some(content));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_selectable(true);
        if let Some(class) = class {
            label.add_css_class(class);
        }
        label
    };
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(4);
    body.set_margin_bottom(12);
    if !w.summary.is_empty() {
        body.append(&text(&w.summary, None));
    }
    let mut facts = vec![state_name(&w.status).to_string()];
    if let Some(ms) = w.duration_ms {
        facts.push(duration(ms));
    }
    if let Some(n) = w.tokens {
        facts.push(crate::trf!("{n} tokens", n = n));
    }
    if let Some(n) = w.tool_calls {
        facts.push(crate::trf!("{n} tool calls", n = n));
    }
    body.append(&text(&facts.join(" · "), Some("dim-label")));
    if let Some(error) = &w.error {
        body.append(&text(error, Some("error")));
    }
    let mut phases = w.phases.clone();
    for a in &w.agents {
        let phase = a.phase.clone().unwrap_or_default();
        if !phases.contains(&phase) {
            phases.push(phase);
        }
    }
    for phase in &phases {
        let agents: Vec<&Agent> = w
            .agents
            .iter()
            .filter(|a| a.phase.as_deref().unwrap_or_default() == phase)
            .collect();
        let done = agents.iter().filter(|a| a.finished).count();
        let mark = if agents.is_empty() {
            "○"
        } else if done == agents.len() {
            "✓"
        } else {
            "●"
        };
        let name = if phase.is_empty() {
            tr("Agents")
        } else {
            phase
        };
        let line = text(
            &format!("{mark} {name}  {done}/{}", agents.len()),
            Some("heading"),
        );
        line.set_margin_top(6);
        body.append(&line);
        for a in agents {
            let state = if a.finished {
                tr("Done").to_string()
            } else {
                a.activity
                    .clone()
                    .unwrap_or_else(|| tr("starting").to_string())
            };
            let title = if a.description.is_empty() {
                &a.kind
            } else {
                &a.description
            };
            body.append(&text(&format!("   {title} · {state}"), Some("omux-status")));
        }
    }
    gtk::ScrolledWindow::builder()
        .child(&body)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build()
        .upcast()
}

fn clear_detail(s: &mut State) {
    while let Some(child) = s.holder.first_child() {
        s.holder.remove(&child);
    }
    s.title.set_text("");
    s.detail = None;
}

/// A shell's output, followed every second while on screen.
fn output_view(host: Option<String>, path: String) -> gtk::Widget {
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(6);
    view.set_bottom_margin(12);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let offset = Arc::new(Mutex::new(0u64));
    let busy = Rc::new(std::cell::Cell::new(false));
    let alive = scroller.downgrade();
    let load = move || {
        let Some(scroller) = alive.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !scroller.is_mapped() && *offset.lock().unwrap() > 0 {
            return glib::ControlFlow::Continue;
        }
        if busy.replace(true) {
            return glib::ControlFlow::Continue;
        }
        let (host, path, offset) = (host.clone(), path.clone(), offset.clone());
        let busy = busy.clone();
        crate::ui::panel::background(
            move || {
                let from = *offset.lock().unwrap();
                let read = crate::activity::tail(host.as_deref(), &path, from);
                if let Ok((start, _, data)) = &read {
                    *offset.lock().unwrap() = start + data.len() as u64;
                }
                read.map(|(start, _, data)| (start != from, data))
            },
            move |result| {
                busy.set(false);
                let Ok((restart, data)) = result else { return };
                let Some(view) = scroller
                    .child()
                    .and_then(|c| c.downcast::<gtk::TextView>().ok())
                else {
                    return;
                };
                if !restart && data.is_empty() {
                    return;
                }
                let buffer = view.buffer();
                let adjustment = scroller.vadjustment();
                let at_bottom =
                    adjustment.value() + adjustment.page_size() >= adjustment.upper() - 40.0;
                if restart {
                    buffer.set_text("");
                }
                let mut end = buffer.end_iter();
                buffer.insert(&mut end, &String::from_utf8_lossy(&data));
                let extra = buffer.char_count() as usize;
                if extra > KEEP {
                    let mut start = buffer.start_iter();
                    let mut cut = buffer.iter_at_offset((extra - KEEP) as i32);
                    buffer.delete(&mut start, &mut cut);
                }
                if at_bottom {
                    glib::idle_add_local_once(move || adjustment.set_value(adjustment.upper()));
                }
            },
        );
        glib::ControlFlow::Continue
    };
    let _ = (load.clone())();
    glib::timeout_add_local(Duration::from_secs(1), load);
    scroller.upcast()
}
