//! Names Claude Code sessions after the title Claude Code gives their
//! conversation ("Expo build plan" instead of the first words of the first
//! prompt). A name the user chose is never replaced.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use uuid::Uuid;

use crate::app::App;
use crate::claude::RecentSession;
use crate::model::SessionProvider;
use crate::search::{Entry, Index};

/// `Session`, `Session 3`: what new sessions are called.
fn is_default_name(name: &str) -> bool {
    match name.strip_prefix("Session") {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix(' ')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())),
        None => false,
    }
}

/// The name a session should get from its conversation, if its current
/// name was given automatically (a default name, the first words of the
/// first prompt, or an earlier title) and differs from the title.
pub fn new_name(current: &str, entry: &Entry) -> Option<String> {
    let title = entry.title();
    if title.is_empty() || title == current {
        return None;
    }
    let from_prompt = RecentSession {
        id: entry.id,
        cwd: String::new(),
        prompt: entry.prompt.clone(),
        date: std::time::SystemTime::UNIX_EPOCH,
    }
    .tab_name();
    let automatic = is_default_name(current)
        || current == from_prompt
        || current == entry.prompt.chars().take(60).collect::<String>()
        || entry.ai_titles.iter().any(|t| t == current);
    automatic.then(|| title.to_string())
}

/// Renames Claude Code sessions whose conversation has a newer title.
pub fn apply(app: &Rc<App>, index: &Arc<Index>) {
    let by_id: HashMap<Uuid, &Entry> = index.entries.iter().map(|e| (e.id, e)).collect();
    let mut changed = false;
    {
        let mut ws = app.ws.borrow_mut();
        for group in &mut ws.groups {
            let group_provider = group.provider;
            let group_command = group.command.clone();
            for tab in &mut group.tabs {
                let provider = tab
                    .provider
                    .or_else(|| {
                        tab.command
                            .as_deref()
                            .or(group_command.as_deref())
                            .map(|c| SessionProvider::infer(Some(c)))
                    })
                    .unwrap_or(group_provider);
                if provider != SessionProvider::Claude {
                    continue;
                }
                let Some(entry) = by_id.get(&tab.claude_session_id) else {
                    continue;
                };
                if let Some(name) = new_name(&tab.name, entry) {
                    tab.name = name;
                    changed = true;
                }
            }
        }
    }
    if changed && app.renaming.get().is_none() {
        app.schedule_save();
        app.refresh_sidebar();
        app.ui.header.update(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prompt: &str, titles: &[&str], custom: &str) -> Entry {
        Entry {
            prompt: prompt.into(),
            ai_title: titles.last().copied().unwrap_or("").into(),
            ai_titles: titles.iter().map(|t| t.to_string()).collect(),
            custom_title: custom.into(),
            ..Entry::default()
        }
    }

    #[test]
    fn defaults() {
        assert!(is_default_name("Session"));
        assert!(is_default_name("Session 12"));
        assert!(!is_default_name("Session notes"));
        assert!(!is_default_name("Sessions"));
    }

    #[test]
    fn only_automatic_names_change() {
        let e = entry(
            "Kankacım dur bir bakar mısın şuna",
            &["Şimdi kanka", "omux Linux portu"],
            "",
        );
        assert_eq!(
            new_name("Kankacım dur bir bakar mısın", &e).as_deref(),
            Some("omux Linux portu")
        );
        assert_eq!(
            new_name("Session 2", &e).as_deref(),
            Some("omux Linux portu")
        );
        assert_eq!(
            new_name("Şimdi kanka", &e).as_deref(),
            Some("omux Linux portu")
        );
        assert_eq!(new_name("omux Linux portu", &e), None);
        assert_eq!(new_name("My own name", &e), None);
        let renamed = entry("x", &["omux Linux portu"], "Port");
        assert_eq!(
            new_name("omux Linux portu", &renamed).as_deref(),
            Some("Port")
        );
        assert_eq!(new_name("Session", &entry("x", &[], "")), None);
    }
}
