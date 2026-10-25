//! Codex usage (`scripts/usage_codex.py`): exact session totals from the rollout log.
//!
//! Codex reports tokens but no price (a ChatGPT login meters by quota), so cost is a list-price
//! estimate.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::pricing::{self, Tokens};
use super::{CollectorEnv, Outcome, Py, get, get_or, json_line, py_int, stripped_lines, truthy};

/// Bound the backward scan; a just-finished run is always recent.
const MAX_DAY_DIRECTORIES: usize = 30;

pub fn main(env: &CollectorEnv, session_id: &str) -> Py<Outcome> {
    let Some(session_file) = find_session_file(env, session_id)? else { return Ok(Outcome::Unavailable) };
    let Some(mut totals) = collect(env, &session_file)? else { return Ok(Outcome::Unavailable) };
    totals.insert("scope".into(), json!("session"));
    Ok(Outcome::Report(Value::Object(totals)))
}

fn subdirs_desc(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

/// Day directories (`sessions/YYYY/MM/DD`), newest first by name.
fn day_directories_desc(sessions_root: &Path) -> impl Iterator<Item = PathBuf> {
    subdirs_desc(sessions_root).into_iter().flat_map(|year| subdirs_desc(&year)).flat_map(|month| subdirs_desc(&month))
}

/// Whether a rollout's first line is the top-level CLI session with this id. Excludes Codex's own
/// internal guardian and subagent review sessions, and other front ends (vscode, exec), which can
/// log under the same cwd.
fn matches(path: &Path, session_id: &str) -> Py<bool> {
    let Ok(bytes) = std::fs::read(path) else { return Ok(false) };
    // readline(): up to the first line ending of any kind.
    let end = bytes.iter().position(|b| *b == b'\n' || *b == b'\r').unwrap_or(bytes.len());
    let first_line = String::from_utf8(bytes[..end].to_vec()).map_err(|_| super::Crash)?;
    let first_line = crate::engine::prompts::py_strip(&first_line);
    if first_line.is_empty() {
        return Ok(false);
    }
    let Some(entry) = json_line(first_line) else { return Ok(false) };
    if get(&entry, "type")? != Some(&json!("session_meta")) {
        return Ok(false);
    }
    let payload = get(&entry, "payload")?;
    if get_or(payload, "source")? != Some(&json!("cli")) {
        return Ok(false);
    }
    let identity = match get_or(payload, "session_id")? {
        Some(id) if truthy(Some(id)) => Some(id),
        _ => get_or(payload, "id")?,
    };
    Ok(identity == Some(&json!(session_id)))
}

pub fn find_session_file(env: &CollectorEnv, session_id: &str) -> Py<Option<PathBuf>> {
    if session_id.is_empty() {
        return Ok(None);
    }
    let sessions_root = env.home.join(".codex").join("sessions");
    for day in day_directories_desc(&sessions_root).take(MAX_DAY_DIRECTORIES) {
        let Ok(entries) = std::fs::read_dir(&day) else { continue };
        let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                !name.starts_with('.') && name.ends_with(".jsonl")
            })
            .map(|e| {
                let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                (e.path(), modified)
            })
            .collect();
        files.sort_by_key(|file| std::cmp::Reverse(file.1));
        for (path, _) in files {
            if matches(&path, session_id)? {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

pub fn collect(env: &CollectorEnv, session_file: &Path) -> Py<Option<Map<String, Value>>> {
    let mut model: Option<Value> = None;
    let mut last_usage: Option<Value> = None;
    for line in stripped_lines(session_file)? {
        if line.is_empty() {
            continue;
        }
        let Some(entry) = json_line(&line) else { continue };
        let kind = get(&entry, "type")?;
        if kind == Some(&json!("turn_context")) {
            let candidate = get_or(get(&entry, "payload")?, "model")?;
            if truthy(candidate) {
                model = candidate.cloned();
            }
            continue;
        }
        if kind != Some(&json!("event_msg")) {
            continue;
        }
        let payload = get(&entry, "payload")?;
        let payload_type = get_or(payload, "type")?;
        if payload_type == Some(&json!("thread_settings_applied")) {
            let candidate = get_or(get_or(payload, "thread_settings")?, "model")?;
            if truthy(candidate) {
                model = candidate.cloned();
            }
        } else if payload_type == Some(&json!("token_count")) {
            let usage = get_or(get_or(payload, "info")?, "total_token_usage")?;
            if truthy(usage) {
                last_usage = usage.cloned();
            }
        }
    }
    let (Some(usage), Some(model)) = (last_usage, model) else { return Ok(None) };
    let count = |key: &str| -> Py<i64> { py_int(get(&usage, key)?) };
    let (input, output, cached) = (count("input_tokens")?, count("output_tokens")?, count("cached_input_tokens")?);
    let mut report = Map::new();
    report.insert("inputTokens".into(), json!(input));
    // reasoning_output_tokens is a subset of output_tokens, not additional.
    report.insert("outputTokens".into(), json!(output));
    report.insert("cachedInputTokens".into(), json!(cached));
    report.insert("model".into(), model.clone());
    report.insert("source".into(), json!("codex-cli-rollout"));
    let catalog = pricing::load_catalog(env);
    if let Some(rates) = pricing::rates_for(catalog.as_ref(), &model)? {
        // input_tokens includes cached and cache-write tokens. Totals span many requests, so the
        // base (not long-context) rate applies.
        let cache_write = count("cache_write_input_tokens")?;
        let tokens = Tokens {
            uncached: input - cached - cache_write,
            cache_read: cached,
            cache_write,
            cache_write_1h: 0,
            output,
        };
        report.insert("costKind".into(), json!("estimated"));
        report.insert("costUsd".into(), json!(pricing::price(rates, tokens, false)?));
    }
    Ok(Some(report))
}
