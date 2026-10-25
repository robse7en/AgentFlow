//! OpenAI Codex CLI specifics, ported from `backend/agentflow/harnesses/codex.py`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

/// How long [`capture_session_id`] keeps looking: Python polls 40 times, 0.25 s apart.
#[derive(Debug, Clone, Copy)]
pub struct Polling {
    pub interval: Duration,
    pub attempts: u32,
}

impl Default for Polling {
    fn default() -> Self {
        Polling { interval: Duration::from_millis(250), attempts: 40 }
    }
}

/// Codex has no launch-time flag to assign its own session id (unlike Claude's --session-id), so
/// this polls `sessions_root` (`~/.codex/sessions/YYYY/MM/DD/*.jsonl`) for the rollout file this
/// exact launch just created: matched by cwd and a `session_meta` timestamp no older than `since`.
/// AgentFlow reuses the same cwd for every non-worker role on a task (code review and security
/// review can even run concurrently in it), so cwd alone is not a safe match: an ambiguous window
/// (more than one candidate) gives up rather than risk wiring a future resume to the wrong role's
/// conversation.
pub async fn capture_session_id(
    sessions_root: &Path,
    cwd: &Path,
    since: DateTime<Utc>,
    polling: Polling,
) -> Option<String> {
    let target = normcase(&cwd.to_string_lossy());
    for _ in 0..polling.attempts {
        tokio::time::sleep(polling.interval).await;
        let mut candidates: Vec<String> = Vec::new();
        for day in day_directories_desc(sessions_root).into_iter().take(2) {
            let Ok(entries) = std::fs::read_dir(&day) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                let Some(meta) = read_session_meta(&path) else { continue };
                if meta.get("source").and_then(Value::as_str) != Some("cli") {
                    continue;
                }
                let meta_cwd = meta.get("cwd").and_then(Value::as_str).unwrap_or_default();
                if normcase(meta_cwd) != target {
                    continue;
                }
                match meta.get("timestamp").and_then(Value::as_str).and_then(parse_timestamp) {
                    Some(started) if started >= since => {}
                    _ => continue,
                }
                let id = [meta.get("session_id"), meta.get("id")]
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .find(|id| !id.is_empty());
                if let Some(id) = id
                    && !candidates.iter().any(|c| c == id)
                {
                    candidates.push(id.to_string());
                }
            }
        }
        match candidates.len() {
            0 => {}
            1 => return candidates.pop(),
            _ => return None,
        }
    }
    None
}

/// `os.path.normcase(os.path.normpath(path))`: lexically normalized, and on Windows with
/// backslashes and lower case.
fn normcase(path: &str) -> String {
    let normalized = crate::paths::normalize(Path::new(path)).to_string_lossy().into_owned();
    if cfg!(windows) { normalized.replace('/', "\\").to_lowercase() } else { normalized }
}

fn subdirs_desc(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

/// Day directories, newest first (`YYYY/MM/DD` names sort by date).
fn day_directories_desc(sessions_root: &Path) -> Vec<PathBuf> {
    let mut days = Vec::new();
    for year in subdirs_desc(sessions_root) {
        for month in subdirs_desc(&year) {
            days.extend(subdirs_desc(&month));
            if days.len() >= 2 {
                return days;
            }
        }
    }
    days
}

/// The payload of a rollout file's first line when it is a `session_meta` entry.
fn read_session_meta(path: &Path) -> Option<Map<String, Value>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let entry: Value = serde_json::from_str(first.trim()).ok()?;
    if entry.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    match entry.get("payload") {
        Some(Value::Object(payload)) => Some(payload.clone()),
        _ => Some(Map::new()),
    }
}

/// An ISO 8601 timestamp with an offset (`Z` included). Python would crash comparing a naive
/// timestamp against `since`; here such an entry is skipped.
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value.replace('Z', "+00:00")).ok().map(|t| t.with_timezone(&Utc))
}
