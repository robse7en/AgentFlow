//! Bounded local diagnostics, ported from `backend/agentflow/diagnostics.py`. Never record request
//! bodies, credentials or command output.
//!
//! Events keep the Python names and detail keys (`worktree.create.requested`, `{"path": ...}`),
//! since `/api/diagnostics` serves them. They are written as JSON lines to `agentflow.jsonl` once
//! [`open_log`] has been called (the server does at startup), rotated at 2 MB with four backups like
//! Python's RotatingFileHandler, and always mirrored to `tracing` for the console.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

pub const TARGET: &str = "agentflow::diagnostics";
const MAX_BYTES: u64 = 2_000_000;
const BACKUPS: u32 = 4;

tokio::task_local! {
    /// The current HTTP request's id (the X-Request-ID header), stamped on every event it causes.
    pub static REQUEST_ID: String;
}

struct Rotating {
    path: PathBuf,
    file: File,
    size: u64,
}

static LOG: Mutex<Option<Rotating>> = Mutex::new(None);

/// Unit tests that open the (process-wide) log take this first.
#[cfg(test)]
pub(crate) static TEST_LOG: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Start writing events to `path` (creating its directory), replacing any log opened before.
pub fn open_log(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let size = file.metadata()?.len();
    *LOG.lock().unwrap_or_else(|p| p.into_inner()) = Some(Rotating { path: path.to_path_buf(), file, size });
    Ok(())
}

/// Stop writing to the file (the app is shutting down).
pub fn close_log() {
    LOG.lock().unwrap_or_else(|p| p.into_inner()).take();
}

/// Record a diagnostic event: `event("git.fetch.failed", json!({"branch": b, "exitCode": 1}))`.
pub fn event(name: &str, details: Value) {
    tracing::info!(target: TARGET, event = name, details = %details);
    write("INFO", name, details, None);
}

/// Record a failure. Only the error's kind and where it happened are kept, never its message:
/// messages can contain secrets (Python kept the exception type and stack locations, not the text).
pub fn failure(name: &str, details: Value, kind: &str, frames: Vec<Value>) {
    tracing::error!(target: TARGET, event = name, details = %details, kind);
    write("ERROR", name, details, Some((kind, frames)));
}

/// Where requests panicked, by request id, until the request's middleware logs it.
static PANICS: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());

/// Panics inside a request record only their location (for the request's `request.failed` entry)
/// and print no message, which could contain a prompt or a token. Other panics print as usual.
pub fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| match REQUEST_ID.try_with(Clone::clone) {
            Ok(request) => {
                let location = info.location();
                let frame = json!({"file": location.map(|l| l.file()), "line": location.map(|l| l.line()),
                                   "method": "<panic>"});
                eprintln!("request {request} failed at {}", location.map_or("<unknown>".into(), |l| l.to_string()));
                PANICS.lock().unwrap_or_else(|p| p.into_inner()).push((request, frame));
            }
            Err(_) => previous(info),
        }));
    });
}

/// The frames recorded for a request that panicked.
pub fn take_panic_frames(request: &str) -> Vec<Value> {
    let mut panics = PANICS.lock().unwrap_or_else(|p| p.into_inner());
    let (taken, kept): (Vec<_>, Vec<_>) = panics.drain(..).partition(|(id, _)| id == request);
    *panics = kept;
    taken.into_iter().map(|(_, frame)| frame).collect()
}

fn write(level: &str, name: &str, details: Value, failure: Option<(&str, Vec<Value>)>) {
    let request = REQUEST_ID.try_with(Clone::clone).ok();
    let mut entry = json!({
        "time": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string(),
        "level": level,
        "event": name,
        "requestId": request,
        "details": details,
    });
    if let Some((kind, frames)) = failure {
        entry["exceptionType"] = json!(kind);
        entry["frames"] = json!(frames);
    }
    let line = format!("{}\n", pyjson::dumps_compact(&entry));
    let mut log = LOG.lock().unwrap_or_else(|p| p.into_inner());
    let Some(log) = log.as_mut() else { return };
    // RotatingFileHandler.shouldRollover: rotate before a write that would reach the limit.
    if log.size > 0 && log.size + line.len() as u64 >= MAX_BYTES && rotate(log).is_err() {
        return;
    }
    if log.file.write_all(line.as_bytes()).is_ok() {
        log.size += line.len() as u64;
    }
}

/// agentflow.jsonl.3 -> .4, ..., agentflow.jsonl -> .1, then a fresh file.
fn rotate(log: &mut Rotating) -> std::io::Result<()> {
    let backup = |n: u32| PathBuf::from(format!("{}.{n}", log.path.display()));
    let _ = std::fs::remove_file(backup(BACKUPS));
    for n in (1..BACKUPS).rev() {
        if backup(n).exists() {
            std::fs::rename(backup(n), backup(n + 1))?;
        }
    }
    // Windows cannot rename an open file: reopen on a placeholder first.
    let placeholder = OpenOptions::new().create(true).append(true).open(backup(BACKUPS + 1))?;
    drop(std::mem::replace(&mut log.file, placeholder));
    std::fs::rename(&log.path, backup(1))?;
    log.file = OpenOptions::new().create(true).append(true).open(&log.path)?;
    let _ = std::fs::remove_file(backup(BACKUPS + 1));
    log.size = 0;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn log_is_structured_rotating_and_omits_messages() {
        let _serial = TEST_LOG.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("agentflow.jsonl");
        open_log(&path).unwrap();
        REQUEST_ID
            .scope("test-reference".into(), async {
                failure("test.failed", json!({"route": "/x"}), "ValueError", vec![json!({"file": "x.rs", "line": 1})]);
            })
            .await;
        let text = std::fs::read_to_string(&path).unwrap();
        let entry: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(entry["requestId"], "test-reference");
        assert_eq!(entry["exceptionType"], "ValueError");
        assert_eq!(entry["level"], "ERROR");
        LOG.lock().unwrap().as_mut().unwrap().size = MAX_BYTES;
        event("test.next", json!({}));
        assert!(dir.path().join("logs").join("agentflow.jsonl.1").is_file());
        assert!(std::fs::read_to_string(&path).unwrap().contains("test.next"));
        close_log();
    }
}
