//! Searching inside Claude Code conversations.
//!
//! Transcripts are large (tool output, images), but the text people and
//! Claude wrote is small. The index keeps that text for every conversation
//! in `$XDG_CACHE_HOME/omux/conversations.json`, and reads only what was
//! appended to a transcript since the last time.
//!
//! Two ways to search: words (accent- and case-insensitive, so Turkish
//! typed without its letters still matches) and asking Claude, which
//! picks conversations by meaning from a digest of each one.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::persistence::{home_dir, write_atomic};

const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    pub id: Uuid,
    /// The transcript this entry was read from.
    pub path: PathBuf,
    pub cwd: String,
    /// The first real prompt, on one line.
    pub prompt: String,
    /// The transcript's last write, in seconds since the epoch.
    pub date: i64,
    /// How far the transcript has been read (always at a line end).
    pub offset: u64,
    /// The transcript's size when last read; a partial last line makes it
    /// larger than `offset`.
    #[serde(default)]
    pub size: u64,
    /// The user's messages, one per line.
    pub user: String,
    /// Claude's replies, one per line.
    pub assistant: String,
}

impl Entry {
    /// What words are searched in: the prompt, the folder and every message.
    fn haystack(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}",
            self.prompt, self.cwd, self.user, self.assistant
        )
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    entries: Vec<Entry>,
}

#[derive(Default)]
pub struct Index {
    pub entries: Vec<Entry>,
    /// `fold(haystack)` for each entry, same order.
    folded: Vec<String>,
}

/// One character in, one character out, so positions in folded text match
/// the original. Lowercases and drops Turkish and other common accents.
fn fold_char(c: char) -> char {
    match c {
        'ı' | 'I' | 'İ' | 'î' | 'Î' | 'í' | 'ì' | 'ï' => 'i',
        'ş' | 'Ş' => 's',
        'ç' | 'Ç' => 'c',
        'ğ' | 'Ğ' => 'g',
        'ö' | 'Ö' | 'ó' | 'ò' | 'ô' => 'o',
        'ü' | 'Ü' | 'û' | 'Û' | 'ú' | 'ù' => 'u',
        'â' | 'Â' | 'á' | 'à' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        _ => {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) => l,
                _ => c,
            }
        }
    }
}

pub fn fold(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

fn cache_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home_dir().join(".cache"),
    };
    base.join("omux").join("conversations.json")
}

fn seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The text items of a message's content.
fn content_texts(content: &Value) -> Vec<String> {
    match content {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reads one transcript line into the entry.
fn read_line(entry: &mut Entry, line: &str) {
    let Ok(object) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let kind = object.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("user" | "assistant")) {
        return;
    }
    if object.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return;
    }
    if entry.cwd.is_empty() {
        if let Some(cwd) = object.get("cwd").and_then(Value::as_str) {
            entry.cwd = cwd.to_string();
        }
    }
    let Some(content) = object.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    for text in content_texts(content) {
        let text = one_line(&text);
        // Slash commands, hooks and system notes start with a tag.
        if text.is_empty() || text.starts_with('<') {
            continue;
        }
        let target = if kind == Some("user") {
            if entry.prompt.is_empty() {
                entry.prompt = text.clone();
            }
            &mut entry.user
        } else {
            &mut entry.assistant
        };
        target.push_str(&text);
        target.push('\n');
    }
}

/// Reads what was appended to the transcript since `entry.offset`, up to
/// the last complete line.
fn read_new(entry: &mut Entry) {
    let Ok(mut file) = File::open(&entry.path) else {
        return;
    };
    if file.seek(SeekFrom::Start(entry.offset)).is_err() {
        return;
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if line.last() != Some(&b'\n') {
                    break; // a partial line waits for the next update
                }
                entry.offset += n as u64;
                if let Ok(text) = std::str::from_utf8(&line) {
                    read_line(entry, text);
                }
            }
        }
    }
}

/// Brings the entries up to date with the transcripts in `dirs`. Returns
/// whether anything changed.
fn update(entries: &mut Vec<Entry>, dirs: &[PathBuf]) -> bool {
    let mut by_path: HashMap<PathBuf, Entry> =
        entries.drain(..).map(|e| (e.path.clone(), e)).collect();
    let mut changed = false;
    for dir in dirs {
        let Ok(files) = std::fs::read_dir(dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let Ok(meta) = file.metadata() else { continue };
            let mut entry = by_path.remove(&path).unwrap_or_else(|| Entry {
                id,
                path: path.clone(),
                ..Entry::default()
            });
            if meta.len() < entry.offset {
                // Rewritten: start over.
                entry = Entry {
                    id,
                    path: path.clone(),
                    ..Entry::default()
                };
            }
            if meta.len() != entry.size {
                entry.size = meta.len();
                read_new(&mut entry);
                entry.date = meta.modified().map(seconds).unwrap_or(0);
                changed = true;
            }
            entries.push(entry);
        }
    }
    // Whatever is left was deleted.
    changed |= !by_path.is_empty();
    entries.sort_by_key(|e| std::cmp::Reverse(e.date));
    changed
}

fn load_stored() -> Vec<Entry> {
    std::fs::read(cache_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
        .filter(|stored| stored.version == VERSION)
        .map(|stored| stored.entries)
        .unwrap_or_default()
}

fn save_stored(entries: &[Entry]) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let stored = Stored {
        version: VERSION,
        entries: entries.to_vec(),
    };
    if let Ok(bytes) = serde_json::to_vec(&stored) {
        let _ = write_atomic(&path, &bytes);
    }
}

fn project_dirs() -> Vec<PathBuf> {
    std::fs::read_dir(crate::claude::projects_dir())
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

static INDEX: OnceLock<Mutex<Option<Arc<Index>>>> = OnceLock::new();

/// The up-to-date index. Blocking: the first call reads every transcript
/// (or the cache), later ones only what changed. Call it off the GTK thread.
pub fn current() -> Arc<Index> {
    let lock = INDEX.get_or_init(|| Mutex::new(None));
    let mut slot = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut entries = match slot.as_ref() {
        Some(index) => index.entries.clone(),
        None => load_stored(),
    };
    let changed = update(&mut entries, &project_dirs());
    if !changed {
        if let Some(index) = slot.as_ref() {
            return index.clone();
        }
    }
    if changed {
        save_stored(&entries);
    }
    let index = Arc::new(Index::new(entries));
    *slot = Some(index.clone());
    index
}

/// Builds the index in the background shortly after launch, so the first
/// search doesn't wait for it.
pub fn warm_up() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        current();
    });
}

#[derive(Debug, Clone)]
pub struct Hit {
    /// Index into `Index::entries`.
    pub entry: usize,
    /// Text around the first match, on one line.
    pub snippet: String,
}

impl Index {
    fn new(entries: Vec<Entry>) -> Self {
        let folded = entries.iter().map(|e| fold(&e.haystack())).collect();
        Self { entries, folded }
    }

    /// Conversations with every word of the query. More matches rank
    /// higher, a match in the first prompt most; ties go to the newest.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query = fold(query);
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, usize)> = Vec::new();
        for (i, hay) in self.folded.iter().enumerate() {
            if !words.iter().all(|w| hay.contains(w)) {
                continue;
            }
            let prompt = fold(&self.entries[i].prompt);
            let score = words
                .iter()
                .map(|w| hay.matches(w).take(20).count() + if prompt.contains(w) { 50 } else { 0 })
                .sum();
            scored.push((i, score));
        }
        // Stable: entries are newest first, so ties keep that order.
        scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        scored
            .into_iter()
            .take(limit)
            .map(|(i, _)| Hit {
                entry: i,
                snippet: self.snippet(i, words[0]),
            })
            .collect()
    }

    /// About 140 characters of the original text around the first match of
    /// `word` in a message (after the prompt and folder lines).
    fn snippet(&self, i: usize, word: &str) -> String {
        let entry = &self.entries[i];
        let hay = &self.folded[i];
        // Messages start after the prompt and folder lines.
        let body = entry.prompt.chars().count() + entry.cwd.chars().count() + 2;
        let body_byte = hay.char_indices().nth(body).map_or(hay.len(), |(b, _)| b);
        let Some(found) = hay[body_byte..].find(word).map(|b| b + body_byte) else {
            return String::new();
        };
        let at = hay[..found].chars().count();
        let original: Vec<char> = entry.haystack().chars().collect();
        let start = at.saturating_sub(50).max(body);
        let end = (at + 90).min(original.len());
        let mut text = one_line(&original[start..end].iter().collect::<String>());
        if start > body {
            text.insert(0, '…');
        }
        if end < original.len() {
            text.push('…');
        }
        text
    }

    /// Asks Claude (Haiku, no tools, nothing saved) which conversations
    /// match the question. Returns entry indexes, best first. Blocking.
    pub fn ask(&self, question: &str) -> Result<Vec<usize>, String> {
        let candidates: Vec<usize> = (0..self.entries.len())
            .filter(|i| !self.entries[*i].prompt.is_empty())
            .take(200)
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let prompt = ask_prompt(&self.entries, &candidates, question);
        let reply = run_claude(&prompt)?;
        Ok(parse_reply(&reply, candidates.len())
            .into_iter()
            .map(|n| candidates[n])
            .collect())
    }
}

fn truncate(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(chars).collect::<String>())
    }
}

fn ask_prompt(entries: &[Entry], candidates: &[usize], question: &str) -> String {
    let now = seconds(SystemTime::now());
    let mut text = String::from(
        "Below is a numbered list of Claude Code conversations: their folder, age, first message and later messages from the user. Find the conversations the question is about. Judge by meaning, not exact words; messages may mix languages.\n\n",
    );
    for (n, i) in candidates.iter().enumerate() {
        let e = &entries[*i];
        let folder = Path::new(&e.cwd)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| e.cwd.clone());
        let days = (now - e.date).max(0) / 86_400;
        let later: Vec<&str> = e.user.lines().skip(1).collect();
        text.push_str(&format!(
            "[{n}] folder: {folder} · {days} days ago\nfirst: {}\nlater: {}\n\n",
            truncate(&e.prompt, 200),
            truncate(&later.join(" / "), 500),
        ));
    }
    text.push_str(&format!(
        "Question: {question}\n\nReply with only a JSON array of up to 8 conversation numbers, best match first, for example [3, 0]. Reply [] if none match."
    ));
    text
}

/// The numbers in the first JSON array of the reply that are in range.
fn parse_reply(reply: &str, count: usize) -> Vec<usize> {
    let Some(start) = reply.find('[') else {
        return Vec::new();
    };
    let Some(end) = reply[start..].find(']') else {
        return Vec::new();
    };
    let mut numbers: Vec<usize> =
        serde_json::from_str::<Vec<usize>>(&reply[start..start + end + 1]).unwrap_or_default();
    numbers.retain(|n| *n < count);
    let mut seen = std::collections::HashSet::new();
    numbers.retain(|n| seen.insert(*n));
    numbers
}

/// `claude -p` through a login shell (for the user's PATH), outside any
/// project, with no tools and no saved session.
fn run_claude(prompt: &str) -> Result<String, String> {
    let dir = crate::persistence::private_temp_dir("omux-ask").ok_or("no temp folder")?;
    let mut child = Command::new(crate::session_env::shell())
        .args([
            "-lc",
            "exec claude -p --model haiku --tools '' --no-session-persistence --strict-mcp-config",
        ])
        .current_dir(&dir)
        .envs(crate::session_env::build(crate::session_env::Extra {
            codex_token: None,
            repo: None,
        }))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start Claude Code: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .map_err(|e| format!("couldn't ask Claude Code: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("Claude Code failed: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        let detail = [err.trim(), out.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("no output");
        return Err(format!("Claude Code failed: {}", truncate(detail, 300)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omux-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folding_keeps_positions() {
        assert_eq!(
            fold("Köpek IŞIK Çağrı İstanbul"),
            "kopek isik cagri istanbul"
        );
        for text in ["İİİ ışık", "ẞ straße", "日本語 ok"] {
            assert_eq!(fold(text).chars().count(), text.chars().count());
        }
    }

    #[test]
    fn indexes_appended_lines_only() {
        let dir = temp("index");
        let id = Uuid::new_v4();
        let path = dir.join(format!("{id}.jsonl"));
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","cwd":"/home/u/owx","message":{{"content":"<command-name>/clear</command-name>"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"user","cwd":"/home/u/owx","message":{{"content":"Güvercin oyunu yapalım"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Three.js ile İstanbul"}},{{"type":"tool_use","name":"Bash","input":{{}}}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","isSidechain":true,"message":{{"content":[{{"type":"text","text":"subagent noise"}}]}}}}"#
        )
        .unwrap();
        write!(file, r#"{{"type":"user","message""#).unwrap(); // partial
        file.flush().unwrap();

        let dirs = vec![dir.clone()];
        let mut entries = Vec::new();
        assert!(update(&mut entries, &dirs));
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.cwd, "/home/u/owx");
        assert_eq!(e.prompt, "Güvercin oyunu yapalım");
        assert_eq!(e.user, "Güvercin oyunu yapalım\n");
        assert_eq!(e.assistant, "Three.js ile İstanbul\n");
        assert!(!update(&mut entries, &dirs), "nothing new");

        writeln!(file, r#":{{"content":"render GCP'de olsun"}}}}"#).unwrap();
        file.flush().unwrap();
        assert!(update(&mut entries, &dirs));
        assert_eq!(
            entries[0].user,
            "Güvercin oyunu yapalım\nrender GCP'de olsun\n"
        );

        let index = Index::new(entries.clone());
        let hits = index.search("guvercin ISTANBUL", 10);
        assert_eq!(hits.len(), 1);
        let hits = index.search("istanbul", 10);
        let snippet = &hits[0].snippet;
        assert!(snippet.starts_with('…'), "{snippet}");
        assert!(
            snippet.ends_with("yapalım render GCP'de olsun Three.js ile İstanbul"),
            "{snippet}"
        );
        assert!(index.search("istanbul pizza", 10).is_empty());
        assert!(index.search("noise", 10).is_empty());

        std::fs::remove_file(&path).unwrap();
        assert!(update(&mut entries, &dirs));
        assert!(entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranking_prefers_the_prompt() {
        let entry = |prompt: &str, user: &str, date: i64| Entry {
            id: Uuid::new_v4(),
            prompt: prompt.into(),
            user: user.into(),
            date,
            ..Entry::default()
        };
        let index = Index::new(vec![
            entry("other", "expo expo expo", 3),
            entry("Expo build", "", 2),
            entry("nothing", "", 1),
        ]);
        let order: Vec<usize> = index.search("expo", 10).iter().map(|h| h.entry).collect();
        assert_eq!(order, vec![1, 0]);
    }

    #[test]
    fn replies() {
        assert_eq!(parse_reply("[3, 0, 3, 99]", 5), vec![3, 0]);
        assert_eq!(parse_reply("Here: [1]\nthanks", 5), vec![1]);
        assert!(parse_reply("none", 5).is_empty());
        assert!(parse_reply("[]", 5).is_empty());
    }

    #[test]
    fn prompt_lists_candidates() {
        let e = Entry {
            cwd: "/home/u/owx".into(),
            prompt: "first".into(),
            user: "first\nsecond\nthird".into(),
            date: seconds(SystemTime::now()),
            ..Entry::default()
        };
        let text = ask_prompt(&[e], &[0], "which?");
        assert!(text.contains("[0] folder: owx · 0 days ago\nfirst: first\nlater: second / third"));
        assert!(text.contains("Question: which?"));
    }
}
