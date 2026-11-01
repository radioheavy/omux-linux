//! Remote projects: sessions that run on another machine over SSH.
//!
//! Each session runs inside `dtach` on the host, named after the session,
//! so it keeps running when omux (or the computer) goes away, and the next
//! start attaches to it again. dtach passes every byte through, so Claude
//! Code's status reports (OSC 7501) still reach the sidebar; tmux would
//! swallow them.

use std::rc::Rc;

use adw::prelude::*;
use uuid::Uuid;

use crate::app::App;
use crate::claude::RecentSession;
use crate::model::{TabGroup, TabItem};
use crate::runtime::TabRuntime;

/// `'text'` for a POSIX shell.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// A folder for the remote shell: `~` and `~/x` stay relative to its home.
fn remote_dir(dir: &str) -> String {
    let dir = dir.trim();
    if dir.is_empty() || dir == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = dir.strip_prefix("~/") {
        format!("\"$HOME\"/{}", quote(rest))
    } else {
        quote(dir)
    }
}

/// The dtach socket of a session on the host.
fn socket(tab: Uuid) -> String {
    format!("~/.omux/{}.sock", tab.as_hyphenated())
}

/// The local command that starts or reattaches a session on `host`: makes
/// the folder if needed, then runs `inner` (a shell command) inside dtach.
pub fn command(host: &str, dir: Option<&str>, tab: Uuid, inner: &str) -> String {
    let dir = remote_dir(dir.unwrap_or("~"));
    let script = format!(
        "mkdir -p ~/.omux {dir} && cd {dir} && exec dtach -A {} -r winch -z bash -lc {}",
        socket(tab),
        quote(inner)
    );
    format!(
        "ssh -t -o ServerAliveInterval=30 {} {}",
        quote(host),
        quote(&script)
    )
}

/// Claude Code pinned to the session's conversation on the host: resumes it
/// if the host has its transcript, else starts it with that id. `args` are
/// the rest of the project's `claude` command (`--model opus` …).
pub fn claude(session: Uuid, args: &str) -> String {
    let id = session.as_hyphenated().to_string().to_lowercase();
    let args = if args.trim().is_empty() {
        String::new()
    } else {
        format!(" {}", args.trim())
    };
    format!(
        "if ls ~/.claude/projects/*/{id}.jsonl >/dev/null 2>&1; then exec claude --resume {id}{args}; else exec claude --session-id {id}{args}; fi"
    )
}

/// Ends a session on the host, in the background.
pub fn kill(host: &str, tab: Uuid) {
    let pattern = format!("omux/{}.sock", tab.as_hyphenated());
    let _ = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            host,
            &format!("pkill -f {}", quote(&pattern)),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The host used last, to prefill the dialog.
fn last_host(app: &App) -> String {
    app.ws
        .borrow()
        .groups
        .iter()
        .rev()
        .find_map(|g| g.ssh_host.clone())
        .unwrap_or_default()
}

/// Main menu › New Remote Project…
pub fn new_project(app: &Rc<App>) {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_description(Some(
        "Sessions run on the host over SSH and keep running when omux closes. \
         The host needs dtach and Claude Code. Use a name from ~/.ssh/config, or user@address.",
    ));
    let host = adw::EntryRow::new();
    host.set_title("SSH host");
    host.set_text(&last_host(app));
    group.add(&host);
    let folder = adw::EntryRow::new();
    folder.set_title("Folder on the host");
    folder.set_text("~/");
    group.add(&folder);
    let name = adw::EntryRow::new();
    name.set_title("Name (optional)");
    group.add(&name);
    page.add(&group);

    let (dialog, create) = crate::ui::dialogs::form("New Remote Project", "Create", &page);
    let weak = Rc::downgrade(app);
    let d = dialog.downgrade();
    create.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let host_text = host.text().trim().to_string();
        if host_text.is_empty() {
            host.grab_focus();
            return;
        }
        let dir = folder.text().trim().trim_end_matches('/').to_string();
        let dir = if dir.is_empty() { "~".to_string() } else { dir };
        let title = name.text().trim().to_string();
        let title = if title.is_empty() {
            let base = dir.rsplit('/').next().unwrap_or("").to_string();
            if base.is_empty() || base == "~" {
                host_text.clone()
            } else {
                base
            }
        } else {
            title
        };
        add_project(&app, &host_text, &dir, &title);
        if let Some(d) = d.upgrade() {
            d.close();
        }
    });
    dialog.present(Some(&app.ui.window));
}

fn add_project(app: &Rc<App>, host: &str, dir: &str, name: &str) {
    let tab = TabItem::new("Session 1");
    let id = tab.id;
    let mut group = TabGroup::new(name, Some(dir.to_string()), vec![tab]);
    group.ssh_host = Some(host.to_string());
    app.ws.borrow_mut().groups.push(group);
    app.runtime.borrow_mut().insert(id, TabRuntime::default());
    app.schedule_save();
    app.refresh_sidebar();
    app.select(id);
}

/// Lists the host's Claude Code conversations as JSON: id, folder, first
/// prompt, title and last write. Runs with the host's python3.
const LIST_SCRIPT: &str = r#"
import glob, json, os
out = []
for p in glob.glob(os.path.expanduser('~/.claude/projects/*/*.jsonl')):
    cwd = prompt = title = custom = None
    try:
        with open(p, errors='ignore') as f:
            for line in f:
                if prompt is None and '"type":"user"' in line:
                    try:
                        o = json.loads(line)
                    except Exception:
                        continue
                    if o.get('isSidechain'):
                        continue
                    cwd = cwd or o.get('cwd')
                    c = (o.get('message') or {}).get('content')
                    texts = [c] if isinstance(c, str) else [i.get('text', '') for i in (c or []) if isinstance(i, dict) and i.get('type') == 'text']
                    for t in texts:
                        t = ' '.join(t.split())
                        if t and not t.startswith('<'):
                            prompt = t[:300]
                            break
                if '"ai-title"' in line or '"custom-title"' in line:
                    try:
                        o = json.loads(line)
                    except Exception:
                        continue
                    if o.get('type') == 'ai-title' and o.get('aiTitle'):
                        title = o['aiTitle']
                    if o.get('type') == 'custom-title' and o.get('customTitle'):
                        custom = o['customTitle']
    except OSError:
        continue
    if cwd and prompt:
        out.append({'id': os.path.basename(p)[:-6], 'cwd': cwd, 'prompt': prompt,
                    'title': custom or title or '', 'date': int(os.path.getmtime(p))})
print(json.dumps(out))
"#;

#[derive(serde::Deserialize)]
struct Listed {
    id: String,
    cwd: String,
    prompt: String,
    #[serde(default)]
    title: String,
    date: u64,
}

/// The host's conversations, named by their title when they have one.
/// Blocking: runs `ssh host python3 -`.
fn list(host: &str) -> Result<Vec<RecentSession>, String> {
    use std::io::Write;
    let mut child = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=15",
            host,
            "python3",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run ssh: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(LIST_SCRIPT.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ssh {host}: {}", err.trim()));
    }
    parse_list(&output.stdout)
}

fn parse_list(json: &[u8]) -> Result<Vec<RecentSession>, String> {
    let listed: Vec<Listed> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(listed
        .into_iter()
        .filter_map(|l| {
            Some(RecentSession {
                id: Uuid::parse_str(&l.id).ok()?,
                cwd: l.cwd,
                prompt: if l.title.is_empty() {
                    l.prompt
                } else {
                    l.title
                },
                date: std::time::UNIX_EPOCH + std::time::Duration::from_secs(l.date),
            })
        })
        .collect())
}

thread_local! {
    static IMPORTING: std::cell::RefCell<std::collections::HashSet<String>> =
        Default::default();
}

/// Adds the host's Claude Code conversations that have no session yet, as
/// remote sessions in projects per folder. With `report`, says what it did.
pub fn import(app: &Rc<App>, host: &str, report: bool) {
    let busy = IMPORTING.with(|set| !set.borrow_mut().insert(host.to_string()));
    if busy {
        return;
    }
    let (tx, rx) = async_channel::bounded(1);
    let h = host.to_string();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(list(&h));
    });
    let weak = Rc::downgrade(app);
    let host = host.to_string();
    gtk::glib::spawn_future_local(async move {
        let result = rx.recv().await;
        IMPORTING.with(|set| set.borrow_mut().remove(&host));
        let (Some(app), Ok(result)) = (weak.upgrade(), result) else {
            return;
        };
        match result {
            Ok(sessions) => {
                let planned =
                    crate::import::plan(&app.ws.borrow(), sessions, Some(&host), |_| true);
                let (added, _) = crate::import::add(&app, planned, Some(&host));
                if report {
                    let body = match added {
                        0 => format!("Every conversation on {host} already has a session."),
                        1 => format!("Added 1 conversation from {host}."),
                        n => format!("Added {n} conversations from {host}."),
                    };
                    crate::ui::dialogs::info(&app, "Conversations on the Server", &body);
                }
            }
            Err(err) if report => {
                crate::ui::dialogs::info(&app, "Couldn't reach the server", &err);
            }
            Err(_) => {}
        }
    });
}

/// The SSH hosts of remote projects, in sidebar order, without repeats.
pub fn hosts(app: &App) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for group in &app.ws.borrow().groups {
        if let Some(host) = &group.ssh_host {
            if !hosts.contains(host) {
                hosts.push(host.clone());
            }
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing() {
        let json = br#"[{"id":"00000000-0000-0000-0000-000000000005","cwd":"/home/u/a","prompt":"hi there","title":"Expo plan","date":10},
            {"id":"bad","cwd":"/x","prompt":"p","date":1},
            {"id":"00000000-0000-0000-0000-000000000006","cwd":"/home/u/b","prompt":"first words","title":"","date":20}]"#;
        let sessions = parse_list(json).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].prompt, "Expo plan");
        assert_eq!(sessions[1].prompt, "first words");
        assert_eq!(sessions[1].cwd, "/home/u/b");
    }

    #[test]
    fn folders() {
        assert_eq!(remote_dir("~"), "\"$HOME\"");
        assert_eq!(remote_dir("~/my app"), "\"$HOME\"/'my app'");
        assert_eq!(remote_dir("/srv/it's"), "'/srv/it'\\''s'");
    }

    #[test]
    fn commands() {
        let tab = Uuid::from_u128(1);
        let line = command("gcp", Some("~/owx"), tab, "exec bash -l");
        assert_eq!(
            line,
            "ssh -t -o ServerAliveInterval=30 'gcp' 'mkdir -p ~/.omux \"$HOME\"/'\\''owx'\\'' && cd \"$HOME\"/'\\''owx'\\'' && exec dtach -A ~/.omux/00000000-0000-0000-0000-000000000001.sock -r winch -z bash -lc '\\''exec bash -l'\\'''"
        );
        let claude = claude(Uuid::from_u128(2), "--model opus ");
        assert!(claude
            .contains("exec claude --resume 00000000-0000-0000-0000-000000000002 --model opus;"));
        assert!(claude.contains(
            "else exec claude --session-id 00000000-0000-0000-0000-000000000002 --model opus; fi"
        ));
    }

    /// The quoting survives both shells: run the remote script with a
    /// stand-in dtach that runs its command.
    #[test]
    fn quoting_round_trips() {
        use std::os::unix::fs::PermissionsExt;
        let bin = std::env::temp_dir().join(format!("omux-remote-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("dtach");
        std::fs::write(&stub, "#!/bin/sh\nshift 5\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let inner = "printf '%s' \"it's ok\"";
        let line = command("host", Some("/tmp"), Uuid::from_u128(3), inner);
        let script = line
            .strip_prefix("ssh -t -o ServerAliveInterval=30 'host' ")
            .unwrap();
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("eval {script}")])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", &bin)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).ends_with("it's ok"),
            "{out:?}"
        );
        let _ = std::fs::remove_dir_all(&bin);
    }
}
