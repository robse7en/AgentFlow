//! Copilot usage (`scripts/usage_copilot.py`): the AI credits GitHub billed for a session.
//!
//! Copilot records the session's cumulative credits in a usage checkpoint after each turn and
//! again at shutdown. Neither carries token counts, so this reports cost only. A turn's credits
//! are written after the turn ends, which is after the agent's own `flowctl complete`: until then
//! the result is pending, so the engine collects again later instead of recording the previous
//! snapshot.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{CollectorEnv, Crash, Outcome, Py, get, get_or, json_line, safe_session_id, stripped_lines, truthy};

const NANO_AIU_PER_USD: f64 = 100_000_000_000.0;
const SNAPSHOTS: [&str; 2] = ["session.usage_checkpoint", "session.shutdown"];
/// Events that happen inside a turn, so its credits are not in the latest snapshot yet.
const ACTIVITY: [&str; 5] = ["user.", "assistant.", "model.", "tool.", "subagent."];

pub fn main(env: &CollectorEnv, session_id: &str) -> Py<Outcome> {
    if session_id.is_empty() {
        return Ok(Outcome::Unavailable);
    }
    let Some(events_file) = find_events_file(env, session_id) else { return Ok(Outcome::Unavailable) };
    match collect_cost(&events_file)? {
        Cost::Pending => Ok(Outcome::Pending(None)),
        Cost::Unknown => Ok(Outcome::Unavailable),
        Cost::Billed(mut report) => {
            report.insert("scope".into(), json!("session"));
            report.insert("source".into(), json!("copilot-cli-events"));
            Ok(Outcome::Report(Value::Object(report)))
        }
    }
}

pub fn find_events_file(env: &CollectorEnv, session_id: &str) -> Option<PathBuf> {
    if !safe_session_id(session_id) {
        return None;
    }
    let path = env.home.join(".copilot").join("session-state").join(session_id).join("events.jsonl");
    path.is_file().then_some(path)
}

/// A credit count: a non-negative int (not a bool, not a float).
fn credits(value: Option<&Value>) -> Option<u128> {
    match value? {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_u64().map(u128::from),
        _ => None,
    }
}

/// The snapshot's session total in nano-AIU, else the sum of its per-model totals.
pub fn snapshot_credits(data: Option<&Value>) -> Py<Option<u128>> {
    if let Some(total) = credits(get_or(data, "totalNanoAiu")?) {
        return Ok(Some(total));
    }
    let per_model: Vec<Option<u128>> = match get_or(data, "modelMetrics")? {
        Some(metrics) if truthy(Some(metrics)) => {
            let metrics = metrics.as_object().ok_or(Crash)?;
            metrics.values().map(|m| get_or(Some(m), "totalNanoAiu").map(credits)).collect::<Py<_>>()?
        }
        _ => Vec::new(),
    };
    if per_model.is_empty() || per_model.contains(&None) {
        return Ok(None);
    }
    Ok(Some(per_model.into_iter().flatten().sum()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cost {
    Billed(Map<String, Value>),
    Pending,
    Unknown,
}

/// The session's cumulative billed cost, pending while a turn is unaccounted for, or unknown.
pub fn collect_cost(events_file: &Path) -> Py<Cost> {
    let mut nano_aiu: Option<u128> = None;
    let mut pending = false;
    let mut models: Vec<String> = Vec::new();
    let mut selected: Option<String> = None;

    for line in stripped_lines(events_file)? {
        if line.is_empty() {
            continue;
        }
        let Some(entry) = json_line(&line) else { continue };
        let kind = match get(&entry, "type")? {
            Some(kind) if truthy(Some(kind)) => kind.as_str().ok_or(Crash)?.to_string(),
            _ => String::new(),
        };
        let data = get(&entry, "data")?;
        let data_text =
            |key: &str| -> Py<Option<String>> { Ok(get_or(data, key)?.and_then(Value::as_str).map(String::from)) };
        if SNAPSHOTS.contains(&kind.as_str()) {
            // A snapshot without credits (older Copilot releases) leaves the total unknown.
            nano_aiu = snapshot_credits(data)?;
            pending = false;
        } else if ACTIVITY.iter().any(|prefix| kind.starts_with(prefix)) {
            pending = true;
        }
        if kind == "assistant.message" {
            if let Some(model) = data_text("model")?
                && !models.contains(&model)
            {
                models.push(model);
            }
        } else if kind == "session.start" {
            if let Some(model) = data_text("selectedModel")? {
                selected = Some(model);
            }
        } else if kind == "session.model_change"
            && let Some(model) = data_text("newModel")?
        {
            selected = Some(model);
        }
    }

    if pending {
        return Ok(Cost::Pending);
    }
    let Some(nano_aiu) = nano_aiu else { return Ok(Cost::Unknown) };
    models.sort();
    let joined = models.join("+");
    let model = [Some(joined), selected].into_iter().flatten().find(|m| !m.is_empty()).unwrap_or("copilot".into());
    let mut report = Map::new();
    report.insert("model".into(), json!(model));
    // GitHub AI credits: 1 credit = 1e9 nano-AIU = US$0.01.
    report.insert("costUsd".into(), json!(nano_aiu as f64 / NANO_AIU_PER_USD));
    report.insert("costKind".into(), json!("billed"));
    Ok(Cost::Billed(report))
}
