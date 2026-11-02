//! What a Claude Code session is doing in the background: its subagents and
//! the shell commands it runs in the background, read from Claude Code's
//! files on this computer or, for a remote project, on its SSH host.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

const PROBE: &str = include_str!("activity_probe.py");

/// At most this much of a file is read at once.
const LIMIT: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Agent {
    pub id: String,
    pub kind: String,
    pub description: String,
    pub finished: bool,
    pub background: bool,
    pub activity: Option<String>,
    pub tools: usize,
    /// Its transcript.
    pub path: String,
    pub modified: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Shell {
    pub id: String,
    pub command: String,
    pub description: String,
    /// `running`, `completed`, `failed`, `killed`…
    pub status: String,
    /// Where its output goes.
    pub output: String,
    pub modified: f64,
}

impl Shell {
    pub fn running(&self) -> bool {
        self.status == "running"
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Snapshot {
    pub agents: Vec<Agent>,
    pub shells: Vec<Shell>,
}

/// `python3 - args…` here or on `host`, with the probe on stdin.
fn probe(host: Option<&str>, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = match host {
        Some(host) => {
            let mut c = Command::new("ssh");
            c.args(ssh_options()).arg(host).arg("python3").arg("-");
            // ssh joins the remote words with spaces; the args are ids and
            // paths, quoted for the remote shell.
            c.args(args.iter().map(|a| crate::remote::quote(a)));
            c
        }
        None => {
            let mut c = Command::new("python3");
            c.arg("-").args(args);
            c
        }
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(PROBE.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(output.stdout)
}

/// Reuses one connection per host for the frequent reads.
fn ssh_options() -> [&'static str; 10] {
    [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ControlMaster=auto",
        "-o",
        "ControlPath=~/.ssh/omux-%C",
        "-o",
        "ControlPersist=120",
    ]
}

/// The session's subagents and background shells. Blocking.
pub fn snapshot(host: Option<&str>, session: Uuid) -> Result<Snapshot, String> {
    let id = session.to_string().to_lowercase();
    let out = probe(host, &["snapshot", &id])?;
    serde_json::from_slice(&out).map_err(|e| e.to_string())
}

/// New bytes of a growing file. Returns where they start in the file and
/// the file's size; a file that shrank is read from the start again, and a
/// first read of a long file starts near its end. Blocking.
pub fn tail(host: Option<&str>, path: &str, offset: u64) -> Result<(u64, u64, Vec<u8>), String> {
    match host {
        None => tail_local(Path::new(path), offset).map_err(|e| e.to_string()),
        Some(host) => {
            let out = probe(Some(host), &["tail", path, &offset.to_string()])?;
            let newline = out.iter().position(|b| *b == b'\n').ok_or("no header")?;
            let header: Value =
                serde_json::from_slice(&out[..newline]).map_err(|e| e.to_string())?;
            let number = |k: &str| header.get(k).and_then(Value::as_u64).unwrap_or(0);
            Ok((number("start"), number("size"), out[newline + 1..].to_vec()))
        }
    }
}

fn start_at(offset: u64, size: u64) -> u64 {
    let start = if offset > size { 0 } else { offset };
    if start == 0 && size > LIMIT {
        size - LIMIT
    } else {
        start
    }
}

fn tail_local(path: &Path, offset: u64) -> std::io::Result<(u64, u64, Vec<u8>)> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let start = start_at(offset, size);
    let mut data = Vec::new();
    if size > start {
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        file.take(LIMIT).read_to_end(&mut data)?;
    }
    Ok((start, size, data))
}

/// Reads a growing JSON-lines file in steps: each call returns the rows
/// added since the last one.
pub struct JsonlTail {
    host: Option<String>,
    path: String,
    offset: u64,
    partial: Vec<u8>,
}

impl JsonlTail {
    pub fn new(host: Option<String>, path: String) -> Self {
        Self {
            host,
            path,
            offset: 0,
            partial: Vec::new(),
        }
    }

    pub fn read(&mut self) -> Vec<Value> {
        let Ok((start, _, data)) = tail(self.host.as_deref(), &self.path, self.offset) else {
            return Vec::new();
        };
        if start != self.offset {
            // Started over (or near the end): drop the cut first line.
            self.partial.clear();
        }
        let skip_first = start != self.offset && start > 0;
        self.offset = start + data.len() as u64;
        self.partial.extend_from_slice(&data);
        let Some(last) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Vec::new();
        };
        let complete: Vec<u8> = self.partial.drain(..=last).collect();
        complete
            .split(|b| *b == b'\n')
            .skip(usize::from(skip_first))
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_from_near_the_end() {
        assert_eq!(start_at(0, 100), 0);
        assert_eq!(start_at(40, 100), 40);
        assert_eq!(start_at(400, 100), 0);
        assert_eq!(start_at(0, LIMIT + 10), 10);
    }

    #[test]
    fn jsonl_tail_returns_new_rows() {
        let dir = std::env::temp_dir().join(format!("omux-tail-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.jsonl");
        std::fs::write(&path, "{\"n\":1}\n{\"n\":2").unwrap();
        let mut tail = JsonlTail::new(None, path.to_string_lossy().into_owned());
        let rows = tail.read();
        assert_eq!(rows.len(), 1);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"}\n{\"n\":3}\n").unwrap();
        let rows: Vec<i64> = tail
            .read()
            .iter()
            .filter_map(|r| r.get("n").and_then(Value::as_i64))
            .collect();
        assert_eq!(rows, [2, 3]);
        assert!(tail.read().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn probe_reads_a_session() {
        // The probe is plain python3 with no packages; run it on a fake
        // config folder.
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("omux-probe-{}", Uuid::new_v4()));
        let session = Uuid::new_v4();
        let id = session.to_string();
        let project = base.join("projects").join("-p");
        let agents = project.join(&id).join("subagents");
        std::fs::create_dir_all(&agents).unwrap();
        let output = base.join("task.output");
        std::fs::write(&output, "hello\n").unwrap();
        let rows = [
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Bash",
                 "input": {"command": "npm run dev", "description": "Dev server", "run_in_background": true}}]}}),
            serde_json::json!({"type": "user", "toolUseResult": {"backgroundTaskId": "b1"},
                "message": {"content": [{"type": "tool_result", "tool_use_id": "t1",
                "content": format!("Command running in background with ID: b1. Output is being written to: {}. You will be notified.", output.display())}]}}),
        ];
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(project.join(format!("{id}.jsonl")), text).unwrap();
        std::fs::write(
            agents.join("agent-a1.meta.json"),
            r#"{"agentType": "Explore", "description": "Find the bug", "finished": false}"#,
        )
        .unwrap();
        std::fs::write(
            agents.join("agent-a1.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Grep\",\"input\":{\"pattern\":\"panic\"}}]}}\n",
        )
        .unwrap();

        let mut child = Command::new("python3")
            .args(["-", "snapshot", &id])
            .env("CLAUDE_CONFIG_DIR", &base)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(PROBE.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let snapshot: Snapshot = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(snapshot.shells.len(), 1);
        let shell = &snapshot.shells[0];
        assert_eq!(shell.id, "b1");
        assert_eq!(shell.command, "npm run dev");
        assert!(shell.running());
        assert_eq!(Path::new(&shell.output), output);
        assert_eq!(snapshot.agents.len(), 1);
        let agent = &snapshot.agents[0];
        assert_eq!(agent.kind, "Explore");
        assert_eq!(agent.activity.as_deref(), Some("Grep panic"));
        assert_eq!(agent.tools, 1);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
