//! Local application permissions (not an OS sandbox against same-user processes), ported from
//! `backend/agentflow/api/security.py`.

use std::path::PathBuf;

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::ApiError;
use crate::engine::Engine;
use crate::engine::states::TaskStatus;

pub const COOKIE: &str = "agentflow_human";

pub fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Python's `secrets.token_urlsafe(n)`: n random bytes, URL-safe base64 without padding.
pub fn token_urlsafe(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the OS random source is available");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buffer)
}

/// Python's `secrets.token_hex(n)`: n random bytes as lowercase hex.
pub fn token_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the OS random source is available");
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}

/// `secrets.compare_digest`: equal-length comparison that takes the same time wherever it differs.
fn same(left: &str, right: &str) -> bool {
    left.len() == right.len() && left.bytes().zip(right.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// One browser session that survives restarts. Only the token's hash is written to disk, so
/// reading the file does not grant access; pairing again replaces the session. The file is the one
/// the Python backend wrote, so a browser paired with either stays paired.
pub struct HumanAccess {
    pub pairing_code: String,
    session_file: PathBuf,
    session_hash: String,
    /// Bumped whenever pairing replaces the session; open event streams of an older one close
    /// (AF-AUDIT-004).
    generation: tokio::sync::watch::Sender<u64>,
}

impl HumanAccess {
    pub fn new(session_file: PathBuf) -> Self {
        let session_hash = std::fs::read_to_string(&session_file).map(|s| s.trim().to_string()).unwrap_or_default();
        HumanAccess {
            pairing_code: token_urlsafe(24),
            session_file,
            session_hash,
            generation: tokio::sync::watch::Sender::new(0),
        }
    }

    /// For an authenticated cookie, the session generation it belongs to and a receiver that
    /// sees it replaced. Checked and recorded under the same lock pairing takes, so a stream
    /// opened just before a pairing is revoked by it and one opened after fails here.
    pub fn stream_access(&self, cookie_header: Option<&str>) -> Option<(u64, tokio::sync::watch::Receiver<u64>)> {
        if !self.authenticated(cookie_header) {
            return None;
        }
        let receiver = self.generation.subscribe();
        let generation = *receiver.borrow();
        Some((generation, receiver))
    }

    pub fn authenticated(&self, cookie_header: Option<&str>) -> bool {
        let token = cookie(cookie_header, COOKIE).unwrap_or_default();
        !token.is_empty() && !self.session_hash.is_empty() && same(&digest(&token), &self.session_hash)
    }

    /// Exchange the single-use pairing code for a new session token.
    pub fn pair(&mut self, code: &str) -> Result<String, ApiError> {
        if self.pairing_code.is_empty() || !same(code, &self.pairing_code) {
            return Err(ApiError::new(401, "Invalid or already used pairing code. Restart to pair again."));
        }
        self.pairing_code.clear();
        let token = token_urlsafe(32);
        self.session_hash = digest(&token);
        let write = || -> std::io::Result<()> {
            if let Some(parent) = self.session_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&self.session_file, &self.session_hash)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&self.session_file, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        };
        write().map_err(|e| ApiError::new(500, format!("Cannot save the browser session: {e}")))?;
        self.generation.send_modify(|generation| *generation += 1);
        Ok(token)
    }
}

/// A cookie's value from a `Cookie` header.
pub fn cookie(header: Option<&str>, name: &str) -> Option<String> {
    header?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// The local UI (served by the backend itself, at the Host the browser used) or the Vite dev server.
pub fn allowed_origin(origin: Option<&str>, host: &str) -> bool {
    let Some(origin) = origin.filter(|o| !o.is_empty()) else { return false };
    if origin == "http://localhost:5173" || origin == "http://127.0.0.1:5173" {
        return true;
    }
    let Some(netloc) = origin.strip_prefix("http://") else { return false };
    let netloc = netloc.split(['/', '?', '#']).next().unwrap_or_default();
    netloc == host && ["localhost", "127.0.0.1", "::1"].contains(&hostname(netloc).as_str())
}

/// The host part of `host[:port]` or `[v6]:port`, lower-cased (urllib's `hostname`).
fn hostname(netloc: &str) -> String {
    let host = match netloc.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => netloc.rsplit_once(':').map_or(netloc, |(h, _)| h),
    };
    host.to_lowercase()
}

/// A bearer token grants callbacks only for its exact task, role and run.
pub async fn authorize_agent(
    engine: &Engine,
    jira_key: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<(), ApiError> {
    // Launching may deliver the prompt before its transaction commits. Wait for that transaction
    // before looking up the credential, then let the engine validate freshness.
    let lock = engine.lock(jira_key);
    let _held = lock.lock().await;
    let hash = digest(token);
    let key = jira_key.to_uppercase();
    let (access, task) = engine
        .db
        .session(|tx| -> rusqlite::Result<_> {
            use rusqlite::OptionalExtension;
            let access = tx
                .query_row("SELECT task_id, role, run_id FROM agent_access WHERE token_hash = ?1", [&hash], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
                })
                .optional()?;
            let task = tx
                .query_row("SELECT id, status FROM tasks WHERE jira_key = ?1", [&key], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?;
            Ok((access, task))
        })
        .map_err(|_| ApiError::new(500, "Database error"))?;
    let (Some((access_task, role, run_id)), Some((task_id, status))) = (access, task) else {
        return Err(ApiError::new(403, "Agent credential does not authorize this task"));
    };
    if access_task != task_id {
        return Err(ApiError::new(403, "Agent credential does not authorize this task"));
    }
    if TaskStatus::parse(&status).is_some_and(|s| s == TaskStatus::Cancelled || s.is_post_ready()) {
        return Err(ApiError::new(409, "Task no longer accepts agent requests"));
    }
    if let Some(body) = body {
        let role_matches = body.get("role").and_then(Value::as_str) == Some(role.as_str());
        // Python compared with ==, so 3 and 3.0 are the same run.
        let run_matches = body.get("runId").and_then(Value::as_f64) == Some(run_id as f64)
            && !body.get("runId").is_some_and(Value::is_boolean);
        if !role_matches || !run_matches {
            return Err(ApiError::new(403, "Agent credential does not authorize this role or run"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_the_local_ui_or_the_vite_dev_server() {
        let host = "127.0.0.1:8765";
        assert!(allowed_origin(Some("http://127.0.0.1:8765"), host));
        assert!(allowed_origin(Some("http://localhost:5173"), host));
        assert!(allowed_origin(Some("http://127.0.0.1:5173"), "anything"));
        assert!(!allowed_origin(None, host));
        assert!(!allowed_origin(Some(""), host));
        assert!(!allowed_origin(Some("http://evil.example"), host));
        assert!(!allowed_origin(Some("https://127.0.0.1:8765"), host));
        assert!(!allowed_origin(Some("http://127.0.0.1:9999"), host));
        assert!(!allowed_origin(Some("http://evil.example:8765"), "evil.example:8765"));
        assert!(allowed_origin(Some("http://[::1]:8765"), "[::1]:8765"));
    }

    #[test]
    fn tokens_look_like_python_token_urlsafe() {
        let token = token_urlsafe(24);
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(token, token_urlsafe(24));
    }

    #[test]
    fn a_session_is_persisted_as_a_hash_and_the_code_is_single_use() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("browser-session");
        let mut access = HumanAccess::new(file.clone());
        let code = access.pairing_code.clone();
        assert!(access.pair("wrong").is_err());
        let token = access.pair(&code).unwrap();
        assert!(access.pair(&code).is_err(), "single use");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), digest(&token));
        let header = format!("other=1; {COOKIE}={token}");
        assert!(access.authenticated(Some(&header)));
        // A restart reads the hash back.
        let restarted = HumanAccess::new(file);
        assert!(restarted.authenticated(Some(&header)));
        assert!(!restarted.authenticated(Some(&format!("{COOKIE}=forged"))));
        assert!(!restarted.authenticated(None));
    }

    /// AF-AUDIT-004: a stream opened by a session sees that session replaced, and the replaced
    /// session's cookie opens no new stream.
    #[test]
    fn pairing_revokes_the_streams_of_the_session_it_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("browser-session");
        let mut first = HumanAccess::new(file.clone());
        let code = first.pairing_code.clone();
        let old = format!("{COOKIE}={}", first.pair(&code).unwrap());
        // A restart keeps the session; its new pairing code replaces it.
        let mut access = HumanAccess::new(file);
        let (generation, stream) = access.stream_access(Some(&old)).expect("the old session streams");
        assert!(!stream.has_changed().unwrap());
        let code = access.pairing_code.clone();
        let new = format!("{COOKIE}={}", access.pair(&code).unwrap());
        assert!(stream.has_changed().unwrap() && *stream.borrow() != generation, "the open stream is revoked");
        assert!(access.stream_access(Some(&old)).is_none());
        let (current, _) = access.stream_access(Some(&new)).expect("the new session streams");
        assert_eq!(current, *stream.borrow());
    }
}
