//! Brings the Claude Code conversations that already exist on this machine
//! into the sidebar: one project per folder, one session per conversation.
//! Nothing starts until a session is selected, which then continues its
//! conversation with `claude --resume`.

use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use uuid::Uuid;

use crate::app::App;
use crate::claude::RecentSession;
use crate::model::{SessionProvider, TabGroup, TabItem, Workspace};
use crate::persistence::expand_tilde;
use crate::runtime::TabRuntime;

#[derive(Debug)]
pub struct PlannedGroup {
    /// The existing project for this folder, if there is one.
    pub existing: Option<Uuid>,
    pub directory: String,
    /// Newest first.
    pub sessions: Vec<RecentSession>,
}

/// `~/a/b/` and `/home/me/a/b` are the same folder.
fn normalize(dir: &str) -> String {
    let path = expand_tilde(dir);
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.to_string()
    }
}

/// Groups the conversations that have no session yet by folder. Projects
/// come in order of their newest conversation. Conversations whose folder
/// is gone are skipped.
pub fn plan(
    ws: &Workspace,
    mut sessions: Vec<RecentSession>,
    folder_exists: impl Fn(&str) -> bool,
) -> Vec<PlannedGroup> {
    sessions.sort_by_key(|s| std::cmp::Reverse(s.date));
    let mut seen: HashSet<Uuid> = ws
        .groups
        .iter()
        .flat_map(|g| g.tabs.iter())
        .map(|t| t.claude_session_id)
        .collect();
    let mut groups: Vec<PlannedGroup> = Vec::new();
    for session in sessions {
        if !seen.insert(session.id) || !folder_exists(&session.cwd) {
            continue;
        }
        let directory = normalize(&session.cwd);
        if let Some(group) = groups.iter_mut().find(|g| g.directory == directory) {
            group.sessions.push(session);
            continue;
        }
        let existing = ws
            .groups
            .iter()
            .find(|g| g.directory.as_deref().map(normalize).as_deref() == Some(directory.as_str()))
            .map(|g| g.id);
        groups.push(PlannedGroup {
            existing,
            directory,
            sessions: vec![session],
        });
    }
    groups
}

fn tab_for(session: &RecentSession) -> TabItem {
    let mut tab = TabItem::new(session.tab_name());
    tab.claude_session_id = session.id;
    tab.provider = Some(SessionProvider::Claude);
    tab.command = Some(SessionProvider::Claude.command().to_string());
    tab
}

/// Adds the planned sessions to the workspace. New projects start
/// collapsed so a long history doesn't flood the sidebar. Returns the
/// number of sessions and of new projects.
pub fn apply(ws: &mut Workspace, planned: Vec<PlannedGroup>) -> (Vec<Uuid>, usize) {
    let mut ids = Vec::new();
    let mut made = 0;
    for planned_group in planned {
        let tabs: Vec<TabItem> = planned_group.sessions.iter().map(tab_for).collect();
        ids.extend(tabs.iter().map(|t| t.id));
        let index = planned_group
            .existing
            .and_then(|id| ws.groups.iter().position(|g| g.id == id));
        match index {
            Some(index) => ws.groups[index].tabs.extend(tabs),
            None => {
                let name = Path::new(&planned_group.directory)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| planned_group.directory.clone());
                let mut group = TabGroup::new(name, Some(planned_group.directory), tabs);
                group.is_collapsed = true;
                ws.groups.push(group);
                made += 1;
            }
        }
    }
    (ids, made)
}

/// Imports every Claude Code conversation that has no session yet.
/// Returns the number of sessions and of new projects.
pub fn run(app: &Rc<App>) -> (usize, usize) {
    let planned = plan(&app.ws.borrow(), crate::claude::all_sessions(), |dir| {
        expand_tilde(dir).is_dir()
    });
    let (ids, made) = apply(&mut app.ws.borrow_mut(), planned);
    if ids.is_empty() {
        return (0, 0);
    }
    {
        let mut runtime = app.runtime.borrow_mut();
        for id in &ids {
            runtime.insert(*id, TabRuntime::default());
        }
    }
    app.schedule_save();
    app.refresh_sidebar();
    (ids.len(), made)
}

/// Main menu › Import Claude Code Conversations.
pub fn run_and_report(app: &Rc<App>) {
    let (sessions, projects) = run(app);
    let body = match (sessions, projects) {
        (0, _) => "Every Claude Code conversation on this computer already has a session.".to_string(),
        (1, _) => "Added 1 conversation. Select it to continue where you left off.".to_string(),
        (n, 0) => format!("Added {n} conversations. Select one to continue where you left off."),
        (n, 1) => format!(
            "Added {n} conversations in 1 new project. Select one to continue where you left off."
        ),
        (n, p) => format!(
            "Added {n} conversations in {p} new projects. Select one to continue where you left off."
        ),
    };
    let dialog = adw::AlertDialog::new(Some("Claude Code Conversations"), Some(&body));
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.present(Some(&app.ui.window));
}

/// Once, at launch: a new install, or an update from a version without
/// import, gets the existing conversations without asking.
pub fn first_launch(app: &Rc<App>) {
    if app.settings.borrow().imported_claude_conversations {
        return;
    }
    run(app);
    app.update_settings(|s| s.imported_claude_conversations = true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn session(id: u128, cwd: &str, age: u64) -> RecentSession {
        RecentSession {
            id: Uuid::from_u128(id),
            cwd: cwd.into(),
            prompt: format!("prompt {id}"),
            date: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age),
        }
    }

    #[test]
    fn groups_by_folder_newest_first() {
        let ws = Workspace::default();
        let planned = plan(
            &ws,
            vec![
                session(1, "/a", 30),
                session(2, "/b", 10),
                session(3, "/a/", 5),
                session(4, "/gone", 1),
            ],
            |dir| !dir.starts_with("/gone"),
        );
        let summary: Vec<(String, Vec<u128>)> = planned
            .iter()
            .map(|g| {
                (
                    g.directory.clone(),
                    g.sessions.iter().map(|s| s.id.as_u128()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![("/a".into(), vec![3, 1]), ("/b".into(), vec![2])]
        );
        assert!(planned.iter().all(|g| g.existing.is_none()));
    }

    #[test]
    fn skips_open_conversations_and_reuses_projects() {
        let mut tab = TabItem::new("open");
        tab.claude_session_id = Uuid::from_u128(1);
        let group = TabGroup::new("a", Some("/a/".into()), vec![tab]);
        let group_id = group.id;
        let mut ws = Workspace {
            groups: vec![group],
            selected_tab_id: None,
        };
        let planned = plan(
            &ws,
            vec![
                session(1, "/a", 3),
                session(2, "/a", 2),
                session(2, "/a", 2),
                session(3, "/c", 1),
            ],
            |_| true,
        );
        // Newest first: /c, then the existing /a project.
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].directory, "/c");
        assert_eq!(planned[1].existing, Some(group_id));
        assert_eq!(planned[1].sessions.len(), 1);

        let (ids, made) = apply(&mut ws, planned);
        assert_eq!((ids.len(), made), (2, 1));
        assert_eq!(ws.groups[0].tabs.len(), 2);
        let added = &ws.groups[0].tabs[1];
        assert_eq!(added.claude_session_id, Uuid::from_u128(2));
        assert_eq!(added.provider, Some(SessionProvider::Claude));
        assert_eq!(added.name, "prompt 2");
        let new_group = &ws.groups[1];
        assert_eq!(new_group.name, "c");
        assert_eq!(new_group.directory.as_deref(), Some("/c"));
        assert!(new_group.is_collapsed);

        // Running it again adds nothing.
        assert!(
            plan(&ws, vec![session(2, "/a", 2), session(3, "/c", 1)], |_| {
                true
            })
            .is_empty()
        );
    }
}
