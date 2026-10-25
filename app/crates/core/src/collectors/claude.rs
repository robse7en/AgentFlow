//! Claude Code usage (`scripts/usage_claude.py`): the session transcript's totals.
//!
//! Totals cover the whole conversation; the engine subtracts the baseline captured before each
//! run. While the latest turn is still inside a tool call (the agent's own `flowctl complete` is
//! one), the totals so far come back pending, so the engine collects again once the turn ends.
//! When Claude Code runs through the Copilot proxy (copilot-api), the proxy's ledger supplies the
//! cost GitHub billed; otherwise the transcript is priced at list rates as an estimate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::pricing::{self, Tokens};
use super::{
    CollectorEnv, Crash, Outcome, Py, get, get_or, json_line, py_int, safe_session_id, stripped_lines, truthy,
};

/// An assistant message ending in these continues the turn after the tool runs.
const OPEN_TURN_STOP_REASONS: [&str; 2] = ["tool_use", "pause_turn"];

pub fn main(env: &CollectorEnv, session_id: &str) -> Py<Outcome> {
    if session_id.is_empty() {
        return Ok(Outcome::Unavailable);
    }
    let Some(transcript) = find_transcript(env, session_id) else { return Ok(Outcome::Unavailable) };
    let (report, turn_open) = collect(env, &transcript)?;
    let Some(mut report) = report else { return Ok(Outcome::Unavailable) };
    if let Some(billed) = proxy_billed_cost(env, session_id) {
        report.insert("costUsd".into(), json!(billed));
        report.insert("costKind".into(), json!("billed"));
        report.insert("source".into(), json!("claude-code-transcript+copilot-api"));
    }
    let report = Value::Object(report);
    Ok(if turn_open { Outcome::Pending(Some(report)) } else { Outcome::Report(report) })
}

/// `~/.claude/projects/*/<session>.jsonl`, the first match in directory order (glob's `*` skips
/// hidden names).
pub fn find_transcript(env: &CollectorEnv, session_id: &str) -> Option<PathBuf> {
    if !safe_session_id(session_id) {
        return None;
    }
    let projects = std::fs::read_dir(env.home.join(".claude").join("projects")).ok()?;
    projects
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path().join(format!("{session_id}.jsonl")))
        .find(|path| std::fs::symlink_metadata(path).is_ok())
}

/// The session total from copilot-api's ledger; None unless every request carries a USD cost.
pub fn proxy_billed_cost(env: &CollectorEnv, session_id: &str) -> Option<f64> {
    let database = env.copilot_api_home.join("copilot-api.sqlite");
    if !database.is_file() {
        return None;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = rusqlite::Connection::open_with_flags(&database, flags).ok()?;
    connection.busy_timeout(std::time::Duration::from_secs(5)).ok()?;
    let (requests, costed, nanos): (i64, i64, rusqlite::types::Value) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(total_cost_nanos), COALESCE(SUM(total_cost_nanos), 0) \
             FROM token_usage_events WHERE session_id = ?1 AND cost_currency IS 'USD'",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()?;
    let other: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM token_usage_events WHERE session_id = ?1 AND cost_currency IS NOT 'USD'",
            [session_id],
            |row| row.get(0),
        )
        .ok()?;
    if requests == 0 || costed < requests || other != 0 {
        return None;
    }
    match nanos {
        rusqlite::types::Value::Integer(n) => Some(n as f64 / 1_000_000_000.0),
        rusqlite::types::Value::Real(f) => Some(f / 1_000_000_000.0),
        _ => None,
    }
}

/// A message id as a set member (Python hashes it; a list or dict id raises).
fn id_key(id: &Value) -> Py<String> {
    match id {
        Value::String(s) => Ok(format!("s:{s}")),
        Value::Bool(b) => Ok(format!("n:{}", u8::from(*b))),
        Value::Number(n) => Ok(match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => format!("n:{i}"),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9.2e18 => format!("n:{}", f as i64),
            _ => format!("n:{n}"),
        }),
        _ => Err(Crash),
    }
}

/// The session totals so far, and whether its latest turn is still open.
pub fn collect(env: &CollectorEnv, transcript: &Path) -> Py<(Option<Map<String, Value>>, bool)> {
    let mut seen_message_ids: HashSet<String> = HashSet::new();
    let (mut input_tokens, mut output_tokens, mut cache_creation, mut cache_read) = (0i64, 0i64, 0i64, 0i64);
    let mut model: Option<Value> = None;
    let mut turns = 0;
    let mut turn_open = false;
    let catalog = pricing::load_catalog(env);
    let mut estimate: Option<f64> = Some(0.0);

    for line in stripped_lines(transcript)? {
        if line.is_empty() {
            continue;
        }
        let Some(entry) = json_line(&line) else { continue };
        if get(&entry, "type")? != Some(&json!("assistant")) {
            continue;
        }
        let message = get(&entry, "message")?;
        let field = |key: &str| get_or(message, key);
        let stop_reason = field("stop_reason")?;
        turn_open = stop_reason.and_then(Value::as_str).is_some_and(|s| OPEN_TURN_STOP_REASONS.contains(&s));
        let usage = field("usage")?;
        let entry_model = field("model")?;
        if !truthy(usage) || !truthy(entry_model) || entry_model == Some(&json!("<synthetic>")) {
            continue;
        }
        let (usage, entry_model) = (usage.unwrap_or(&Value::Null), entry_model.unwrap_or(&Value::Null));
        // A single API turn can appear as several lines (one per content block), all sharing the
        // same message id and the same usage totals for that turn.
        let message_id = field("id")?;
        if truthy(message_id) {
            let key = id_key(message_id.unwrap_or(&Value::Null))?;
            if !seen_message_ids.insert(key) {
                continue;
            }
        }
        let count = |key: &str| -> Py<i64> { py_int(get(usage, key)?) };
        input_tokens += count("input_tokens")?;
        output_tokens += count("output_tokens")?;
        cache_creation += count("cache_creation_input_tokens")?;
        cache_read += count("cache_read_input_tokens")?;
        model = Some(entry_model.clone());
        turns += 1;
        match pricing::rates_for(catalog.as_ref(), entry_model)? {
            None => estimate = None,
            Some(rates) => {
                if let Some(total) = estimate {
                    let one_hour = py_int(get_or(get(usage, "cache_creation")?, "ephemeral_1h_input_tokens")?)?;
                    let written = count("cache_creation_input_tokens")?;
                    let tokens = Tokens {
                        uncached: count("input_tokens")?,
                        cache_read: count("cache_read_input_tokens")?,
                        cache_write: (written - one_hour).max(0),
                        cache_write_1h: one_hour.min(written),
                        output: count("output_tokens")?,
                    };
                    estimate = Some(total + pricing::price(rates, tokens, true)?);
                }
            }
        }
    }

    let Some(model) = model.filter(|_| turns > 0) else { return Ok((None, false)) };
    let mut report = Map::new();
    report.insert("inputTokens".into(), json!(input_tokens + cache_creation + cache_read));
    report.insert("outputTokens".into(), json!(output_tokens));
    report.insert("cachedInputTokens".into(), json!(cache_read));
    report.insert("model".into(), model);
    report.insert("source".into(), json!("claude-code-transcript"));
    report.insert("scope".into(), json!("session"));
    if let Some(estimate) = estimate {
        report.insert("costUsd".into(), json!(estimate));
        report.insert("costKind".into(), json!("estimated"));
    }
    Ok((Some(report), turn_open))
}
