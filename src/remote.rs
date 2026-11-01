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

#[cfg(test)]
mod tests {
    use super::*;

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
