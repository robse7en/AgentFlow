//! What running agents report back through flowctl, ported from the engine's `agent_*` methods,
//! `answer_question`, `terminal_target`, `_apply_completion`, `_complete_discovery` and usage
//! collection (`_collect_usage` and its background retry).
//!
//! Implementer, reviewer and final-assessment completions are handled by the slice 4 modules.
//! Parallel workers use scoped runs, ownership-checked checkpoints and bounded dispatch.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::snapshot::workspace;
use super::spec;
use super::states::{NodeState, Role, TaskStatus};
use super::store::TaskState;
use super::usage::{self, UsageError, UsageReport};
use super::{Engine, WorkflowError};
use crate::db::table;
use crate::db::{
    AgentQuestionRow, AgentSessionRow, DbTime, Json, NodeRunRow, RunEvidenceRow, TaskArtifactRow, new_uid,
};
use crate::paths::py_str;
use crate::terminal::TerminalError;
use crate::workspace as files;

/// Seconds between attempts to collect a run's usage that its provider had not finished writing.
pub(crate) const USAGE_RETRY_DELAYS: [u64; 6] = [5, 10, 15, 30, 60, 120];
/// How long stopping an agent waits for a harness asked to exit to write its final usage.
const EXIT_USAGE_WAIT: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------------------ Python semantics

/// Python's `str.isprintable()` for one character: not a control, format, surrogate, private-use,
/// unassigned or separator character (the ASCII space excepted).
pub fn py_isprintable(c: char) -> bool {
    use unicode_general_category::{GeneralCategory as G, get_general_category};
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        G::Control
            | G::Format
            | G::Surrogate
            | G::PrivateUse
            | G::Unassigned
            | G::SpaceSeparator
            | G::LineSeparator
            | G::ParagraphSeparator
    )
}

pub const MAX_ANSWER_LENGTH: usize = 4000;

/// An answer is typed into the agent's pane followed by Enter, so it must be one printable line
/// (`clean_answer`).
pub fn clean_answer(value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_ANSWER_LENGTH {
        return Err(format!("Answer must be at most {MAX_ANSWER_LENGTH} characters"));
    }
    if !value.chars().all(py_isprintable) {
        return Err("Answer must be a single line".into());
    }
    let value = super::prompts::py_strip(value);
    if value.is_empty() {
        return Err("Answer cannot be empty".into());
    }
    Ok(value.to_string())
}

/// Python's `==` on JSON data: maps compare regardless of order, and `1 == 1.0`.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Bool(x), Value::Number(n)) | (Value::Number(n), Value::Bool(x)) => {
            n.as_f64() == Some(f64::from(u8::from(*x)))
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => a == b,
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ------------------------------------------------------------------------------ result contracts

/// A finding as an agent reports it (`Finding`), dumped by alias without nulls: the declared
/// fields in order, then any extra keys as given.
pub fn finding_dump(value: &Value) -> Result<Map<String, Value>, String> {
    let Some(data) = value.as_object() else { return Err("finding: must be an object".into()) };
    let text = |keys: &[&str]| -> Result<Option<String>, String> {
        match keys.iter().find_map(|k| data.get(*k)) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("finding.{}: must be a string", keys[0])),
        }
    };
    let severity = match data.get("severity") {
        None => "medium".to_string(),
        Some(Value::String(s)) => {
            let s = s.trim().to_lowercase();
            let s = match s.as_str() {
                "informational" => "info",
                "warning" => "low",
                "moderate" => "medium",
                "major" => "high",
                "blocker" => "critical",
                other => other,
            }
            .to_string();
            if !["info", "low", "medium", "high", "critical"].contains(&s.as_str()) {
                return Err("finding.severity: must be info, low, medium, high or critical".into());
            }
            s
        }
        Some(_) => return Err("finding.severity: must be info, low, medium, high or critical".into()),
    };
    let blocking = match data.get("blocking") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) || n.as_f64() == Some(1.0) => Some(n.as_f64() == Some(1.0)),
        Some(Value::String(s)) => match s.to_lowercase().as_str() {
            "true" | "yes" | "on" | "1" | "t" | "y" => Some(true),
            "false" | "no" | "off" | "0" | "f" | "n" => Some(false),
            _ => return Err("finding.blocking: must be a boolean".into()),
        },
        Some(_) => return Err("finding.blocking: must be a boolean".into()),
    };
    let line = match data.get("line") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        Some(Value::Number(n)) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Some(i),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => Some(f as i64),
            _ => return Err("finding.line: must be an integer".into()),
        },
        Some(Value::String(s)) => Some(s.trim().parse().map_err(|_| "finding.line: must be an integer".to_string())?),
        Some(_) => return Err("finding.line: must be an integer".into()),
    };
    let mut out = Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            out.insert(key.into(), value);
        }
    };
    put("id", text(&["id"])?.map(Value::from));
    put("type", text(&["type"])?.map(Value::from));
    put("severity", Some(json!(severity)));
    put("blocking", blocking.map(Value::from));
    put("file", text(&["file"])?.map(Value::from));
    put("line", line.map(Value::from));
    put("title", Some(json!(text(&["title"])?.unwrap_or_default())));
    put("description", text(&["description"])?.map(Value::from));
    put("recommendation", text(&["recommendation"])?.map(Value::from));
    put("priorId", text(&["priorId", "prior_id"])?.map(Value::from));
    const DECLARED: [&str; 11] = [
        "id",
        "type",
        "severity",
        "blocking",
        "file",
        "line",
        "title",
        "description",
        "recommendation",
        "priorId",
        "prior_id",
    ];
    for (key, value) in data {
        if !DECLARED.contains(&key.as_str()) && !value.is_null() {
            out.insert(key.clone(), value.clone());
        }
    }
    Ok(out)
}

/// `Finding.is_blocking()` on a dumped finding: its explicit `blocking`, else medium severity or
/// worse.
pub fn finding_is_blocking(finding: &Map<String, Value>) -> bool {
    if let Some(Value::Bool(blocking)) = finding.get("blocking") {
        return *blocking;
    }
    let order = |s: &str| ["info", "low", "medium", "high", "critical"].iter().position(|x| *x == s);
    finding.get("severity").and_then(Value::as_str).and_then(order).unwrap_or(2) >= 2
}

/// A question as `QuestionRequest` accepts it: the message trimmed (1 to 4000 characters before
/// trimming, not blank after), and at most 8 trimmed, printable, distinct choices of 1 to 250
/// characters. The problem names the field.
pub fn clean_question(message: &str, choices: Option<&Value>) -> Result<(String, Vec<String>), String> {
    let (message, choices, _) = clean_ask(message, choices, None, None)?;
    Ok((message, choices))
}

/// A flat question or a decision tree as `QuestionRequest` accepts it: the message, the
/// top-level labels, and the validated tree (`decisions::clean_decision`) when object choices, a
/// `context` or a `risk` make it one.
pub fn clean_ask(
    message: &str,
    choices: Option<&Value>,
    context: Option<&Value>,
    risk: Option<&Value>,
) -> Result<(String, Vec<String>, Option<Value>), String> {
    if !(1..=4000).contains(&message.chars().count()) {
        return Err("message: must be 1 to 4000 characters".into());
    }
    let message = super::prompts::py_strip(message).to_string();
    if message.is_empty() {
        return Err("message: Question message cannot be empty".into());
    }
    let none = Vec::new();
    let items = match choices {
        None => &none,
        Some(Value::Array(items)) => items,
        Some(_) => return Err("choices: must be a list".into()),
    };
    if items.len() > 8 {
        return Err("choices: at most 8 choices".into());
    }
    if super::decisions::is_tree(items, context, risk) {
        let decision = super::decisions::clean_decision(items, context, risk)?;
        let labels = decision["choices"].as_array().into_iter().flatten();
        let labels = labels.map(|c| c["label"].as_str().unwrap_or_default().to_string()).collect();
        return Ok((message, labels, Some(decision)));
    }
    Ok((message, clean_text_choices(items)?, None))
}

fn clean_text_choices(items: &[Value]) -> Result<Vec<String>, String> {
    let mut cleaned = Vec::new();
    for item in items {
        let Some(choice) = item.as_str() else { return Err("choices: must be strings".into()) };
        let choice = super::prompts::py_strip(choice);
        if choice.is_empty() || choice.chars().count() > 250 || !choice.chars().all(py_isprintable) {
            return Err("choices: Each choice must contain 1 to 250 characters".into());
        }
        cleaned.push(choice.to_string());
    }
    // Python compares casefold()ed choices; lower case is the same for everything but a few
    // special cases (ß).
    let mut folded: Vec<String> = cleaned.iter().map(|c| c.to_lowercase()).collect();
    folded.sort();
    folded.dedup();
    if folded.len() != cleaned.len() {
        return Err("choices: Question choices must be unique".into());
    }
    Ok(cleaned)
}

/// The parts of a discovery result (`DiscoveryResult`) the engine acts on.
struct Discovery {
    status: String,
    summary: String,
    open_questions: Vec<Value>,
}

fn discovery(raw: &Map<String, Value>) -> Result<Discovery, String> {
    let status = match raw.get("status") {
        None => "completed".to_string(),
        Some(Value::String(s)) if ["completed", "failed", "blocked"].contains(&s.as_str()) => s.clone(),
        Some(_) => return Err("status: Input should be 'completed', 'failed' or 'blocked'".into()),
    };
    let summary = match raw.get("summary") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err("summary: Input should be a valid string".into()),
    };
    if !matches!(raw.get("complexity"), None | Some(Value::Null) | Some(Value::String(_))) {
        return Err("complexity: Input should be a valid string".into());
    }
    let mut open_questions = Vec::new();
    for (alias, name) in [
        ("affectedComponents", "affected_components"),
        ("assumptions", "assumptions"),
        ("risks", "risks"),
        ("openQuestions", "open_questions"),
    ] {
        let Some(value) = raw.get(alias).or_else(|| raw.get(name)) else { continue };
        let Some(items) = value.as_array().filter(|items| items.iter().all(Value::is_string)) else {
            return Err(format!("{alias}: Input should be a list of strings"));
        };
        if alias == "openQuestions" {
            open_questions = items.clone();
        }
    }
    Ok(Discovery { status, summary, open_questions })
}

// ------------------------------------------------------------------------------ the engine

impl Engine {
    /// The node and run an agent signal is for, refusing stale runs and other roles' runs.
    pub(crate) fn resolve_run(
        &self,
        state: &TaskState,
        role: &str,
        run_id: Option<i64>,
    ) -> Result<(usize, usize), WorkflowError> {
        if state.status().is_some_and(|s| s == TaskStatus::Cancelled || s.is_post_ready()) {
            return Err(WorkflowError::conflict("Task no longer accepts agent signals"));
        }
        let Some(run_id) = run_id else { return Err(WorkflowError::bad_request("An explicit run ID is required")) };
        let Some(run_index) = state.runs.iter().position(|r| r.id == run_id) else {
            return Err(WorkflowError::not_found(format!("Run {run_id} not found for task {}", state.task.jira_key)));
        };
        let node_id = state.runs[run_index].node_id;
        let node_index = state
            .nodes
            .iter()
            .position(|n| n.id == node_id)
            .ok_or_else(|| WorkflowError::not_found(format!("Node for run {run_id} not found")))?;
        if state.nodes[node_index].role.as_deref() != Some(role) {
            return Err(WorkflowError::new("Run does not belong to this role", 403));
        }
        let latest = state.runs.iter().filter(|r| r.node_id == node_id).map(|r| r.id).max();
        if latest != Some(run_id) {
            return Err(WorkflowError::conflict("Stale run; signal rejected"));
        }
        Ok((node_index, run_index))
    }

    fn open_question_count(&self, run_id: i64) -> Result<i64, WorkflowError> {
        Ok(self.db.session(|tx| {
            tx.query_row(
                "SELECT COUNT(*) FROM agent_questions WHERE node_run_id = ?1 AND answered = 0",
                [run_id],
                |row| row.get(0),
            )
        })?)
    }

    fn run_is_active(state: &TaskState, run_index: usize) -> bool {
        NodeState::parse(&state.runs[run_index].state).is_some_and(NodeState::is_active)
    }

    fn node_is_active(state: &TaskState, node_index: usize) -> bool {
        state.node_state(node_index).is_some_and(NodeState::is_active)
    }

    pub async fn agent_question(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        message: &str,
        choices: &[String],
    ) -> Result<Value, WorkflowError> {
        self.agent_decision(jira_key, role, run_id, message, choices, None).await
    }

    /// `agent_question` with a validated decision tree (`decisions::clean_decision`), stored beside
    /// its top-level labels in `choices`.
    pub async fn agent_decision(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        message: &str,
        choices: &[String],
        decision: Option<Value>,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        if !Self::run_is_active(&state, run) {
            return Err(WorkflowError::conflict("Run is no longer active"));
        }
        if !Self::node_is_active(&state, node) {
            return Err(WorkflowError::conflict(format!(
                "{} is {}; questions are only accepted while it is running",
                state.nodes[node].label, state.nodes[node].state
            )));
        }
        let mut question = AgentQuestionRow {
            id: 0,
            uid: new_uid(),
            task_id: state.task.id,
            node_run_id: Some(state.runs[run].id),
            role: state.nodes[node].role.clone().filter(|r| !r.is_empty()).unwrap_or_else(|| role.to_string()),
            message: message.to_string(),
            choices: Json(json!(choices)),
            answered: false,
            answer: None,
            created_at: DbTime::now(),
            answered_at: None,
            decision: Json(decision.unwrap_or(Value::Null)),
        };
        state.set_state(node, NodeState::WaitingForUser, Some("Input required"), None);
        state.runs[run].state = NodeState::WaitingForUser.as_str().into();
        let (label, key) = (state.nodes[node].label.clone(), state.nodes[node].key.clone());
        state.log(format!("{label} asked: {message}"), Some(&key), "warning", None);
        let result = json!({"ok": true, "state": state.nodes[node].state, "runId": state.runs[run].id});
        self.db.session(|tx| -> rusqlite::Result<()> {
            table::insert(tx, &mut question)?;
            state.flush(tx)
        })?;
        drop(held);
        self.publish(jira_key)?;
        Ok(result)
    }

    /// Free text; `via` names the channel the human answered through (e.g. "Slack") for the
    /// event log.
    pub async fn answer_question(
        &self,
        jira_key: &str,
        question_uid: &str,
        answer: &str,
        via: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        self.answer(jira_key, question_uid, Some(answer), None, via).await
    }

    /// A path of 0-based choice indexes (branch, then follow-up option).
    pub async fn answer_question_path(
        &self,
        jira_key: &str,
        question_uid: &str,
        path: &[usize],
        via: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        self.answer(jira_key, question_uid, None, Some(path), via).await
    }

    /// The line typed into the pane is built from the stored question (`decisions::answer_line`),
    /// so the same choice sent twice is one answer.
    async fn answer(
        &self,
        jira_key: &str,
        question_uid: &str,
        text: Option<&str>,
        path: Option<&[usize]>,
        via: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        let invalid = |problem: String| WorkflowError::new(problem, 422);
        let text = match path {
            None => Some(clean_answer(text.unwrap_or_default()).map_err(invalid)?),
            Some(_) => None,
        };
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let question: Option<AgentQuestionRow> = self.db.session(|tx| {
            table::select_one(tx, "WHERE task_id = ?1 AND uid = ?2", rusqlite::params![state.task.id, question_uid])
        })?;
        let Some(original) = question else {
            return Err(WorkflowError::not_found(format!("Question {question_uid} not found")));
        };
        let choices: Vec<String> =
            original.choices.0.as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect();
        let decision = Some(&original.decision.0).filter(|d| !d.is_null());
        let answer = super::decisions::answer_line(&choices, decision, path, text.as_deref())
            .and_then(|line| clean_answer(&line))
            .map_err(invalid)?;
        if original.answered {
            if original.answer.as_deref() != Some(answer.as_str()) {
                return Err(WorkflowError::conflict("This question has already been answered"));
            }
            drop(held);
            return self.snapshot(jira_key);
        }
        let Some(run_id) = original.node_run_id else {
            return Err(WorkflowError::conflict("Question is not attached to an active agent run"));
        };
        let (node, run) = self.resolve_run(&state, &original.role, Some(run_id))?;
        let waiting = NodeState::WaitingForUser.as_str();
        if state.runs[run].state != waiting || state.nodes[node].state != waiting {
            return Err(WorkflowError::conflict("The agent is no longer waiting for this answer"));
        }
        let agent = state
            .agents
            .iter()
            .rev()
            .find(|a| a.role == original.role && a.current_run_id == Some(run_id) && a.status == "running")
            .filter(|a| a.zellij_pane_id.as_deref().is_some_and(|p| !p.is_empty()))
            .map(|a| a.zellij_pane_id.clone().unwrap_or_default());
        let Some(pane_id) = agent else {
            return Err(WorkflowError::conflict("The waiting agent pane is unavailable"));
        };
        let session = state.task.zellij_session_name.clone();
        let send = async {
            if !self.terminals.session_exists(&session).await? {
                return Ok(Err(WorkflowError::conflict("The Zellij session for this agent is unavailable")));
            }
            let pane = self.terminals.pane_state(&session, &pane_id).await?;
            if pane.is_none_or(|p| p.exited) {
                return Ok(Err(WorkflowError::conflict("The waiting agent pane is no longer active")));
            }
            self.terminals.write_line(&session, &pane_id, &answer).await?;
            Ok::<_, TerminalError>(Ok(()))
        };
        match send.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                return Err(WorkflowError::conflict(format!("Could not send the answer to the agent pane: {error}")));
            }
        }
        let mut answered = original.clone();
        answered.answer = Some(answer.clone());
        answered.answered = true;
        answered.answered_at = Some(DbTime::now());
        self.db.session(|tx| table::update(tx, &original, &answered))?;
        let still_open = self.open_question_count(run_id)?;
        if still_open > 0 {
            state.set_state(
                node,
                NodeState::WaitingForUser,
                Some(&format!("Reply sent; waiting for {still_open} more question(s)")),
                None,
            );
        } else {
            state.set_state(node, NodeState::Running, Some("All replies sent; agent resumed"), None);
            state.runs[run].state = NodeState::Running.as_str().into();
        }
        let key = state.nodes[node].key.clone();
        let suffix = via.map(|v| format!(" via {v}")).unwrap_or_default();
        state.log(
            format!("Human replied to question {}{suffix}", original.uid),
            Some(&key),
            "info",
            via.map(|v| json!({"via": v})),
        );
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    /// The zellij session and pane of a role's (or node's) active agent, for a read-only stream.
    pub async fn terminal_target(&self, jira_key: &str, role: &str) -> Result<(String, String), WorkflowError> {
        let (session, pane_id) = {
            let lock = self.lock(jira_key);
            let _held = lock.lock().await;
            let state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
            let by_key =
                if Role::parse(role).is_some() { None } else { state.nodes.iter().position(|n| n.key == role) };
            let node = match by_key {
                Some(index) => index,
                None => {
                    let role = Role::parse(role)
                        .ok_or_else(|| WorkflowError::not_found(format!("Agent role {role} not found")))?;
                    node_by_role(&state, role.as_str())?
                }
            };
            let role = state.nodes[node].role.clone().unwrap_or_default();
            let usable = |s: &str| {
                NodeState::parse(s)
                    .is_some_and(|s| s.is_active() || s == NodeState::Interrupted || s == NodeState::Completed)
            };
            if !usable(&state.nodes[node].state) {
                return Err(WorkflowError::conflict("This agent does not have an active pane"));
            }
            let node_id = state.nodes[node].id;
            let run = state.runs.iter().filter(|r| r.node_id == node_id).max_by_key(|r| r.id);
            let Some(run) = run.filter(|r| usable(&r.state)) else {
                return Err(WorkflowError::conflict("This agent does not have an active run"));
            };
            let agent = state
                .agents
                .iter()
                .rev()
                .find(|a| a.role == role && a.current_run_id == Some(run.id) && a.status == "running")
                .and_then(|a| a.zellij_pane_id.clone().filter(|p| !p.is_empty()));
            let Some(pane_id) = agent else {
                return Err(WorkflowError::conflict("The active agent pane is unavailable"));
            };
            (state.task.zellij_session_name.clone(), pane_id)
        };
        let check = async {
            self.terminals.require_subscribe_support().await?;
            if !self.terminals.session_exists(&session).await? {
                return Ok(Err(WorkflowError::conflict("The Zellij session for this agent is unavailable")));
            }
            let pane = self.terminals.pane_state(&session, &pane_id).await?;
            if pane.is_none_or(|p| p.exited) {
                return Ok(Err(WorkflowError::conflict("The active agent pane is no longer available")));
            }
            Ok::<_, TerminalError>(Ok(()))
        };
        match check.await {
            Ok(Ok(())) => Ok((session, pane_id)),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(WorkflowError::conflict(error.0)),
        }
    }

    pub async fn agent_running(&self, jira_key: &str, role: &str, run_id: Option<i64>) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        if !Self::run_is_active(&state, run) {
            return Err(WorkflowError::conflict("Run is no longer active"));
        }
        if !Self::node_is_active(&state, node) {
            return Err(WorkflowError::conflict(format!(
                "{} is {}; nothing to resume",
                state.nodes[node].label, state.nodes[node].state
            )));
        }
        let open = self.open_question_count(state.runs[run].id)?;
        if open > 0 {
            return Err(WorkflowError::conflict(format!(
                "{open} question(s) still awaiting a reply; wait for them in this pane or in AgentFlow, then run \
                 flowctl running again"
            )));
        }
        // A reply to the last open question already resumed this run; this call is then just the
        // agent confirming what already happened.
        let running = NodeState::Running.as_str();
        let already_running = state.runs[run].state == running && state.nodes[node].state == running;
        if !already_running {
            state.set_state(node, NodeState::Running, Some("Running"), None);
            state.runs[run].state = running.into();
            let (label, key) = (state.nodes[node].label.clone(), state.nodes[node].key.clone());
            state.log(format!("{label} resumed (questions answered)"), Some(&key), "info", None);
        }
        let result = json!({"ok": true, "state": state.nodes[node].state, "runId": state.runs[run].id});
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        if !already_running {
            self.publish(jira_key)?;
        }
        Ok(result)
    }

    /// Incremental finding submission (an alternative to embedding findings in the result).
    pub async fn agent_finding(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        finding: Map<String, Value>,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        let reviewer = role == Role::CodeReviewer.as_str() || role == Role::SecurityReviewer.as_str();
        if !Self::run_is_active(&state, run) || !reviewer {
            return Err(WorkflowError::conflict("Findings require an active reviewer run"));
        }
        if !Self::node_is_active(&state, node) {
            return Err(WorkflowError::conflict(format!(
                "{} is {}; findings are only accepted while it is running",
                state.nodes[node].label, state.nodes[node].state
            )));
        }
        let run_dir = match state.runs[run].run_dir.as_deref().filter(|d| !d.is_empty()) {
            Some(dir) => Path::new(dir).to_path_buf(),
            None => {
                let role_dir = Role::parse(role).map(Role::run_dir).unwrap_or_default();
                workspace(&state.task).run_dir(role_dir, state.runs[run].run_number as u32).map_err(io_error)?
            }
        };
        let pending_path = run_dir.join("findings-pending.json");
        let mut pending = if pending_path.exists() {
            match files::read_json(&pending_path).map_err(io_error)? {
                Value::Array(items) => items,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let id = finding.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let title = finding.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
        pending.push(Value::Object(finding));
        files::write_json(&pending_path, &Value::Array(pending.clone())).map_err(io_error)?;
        let (label, key) = (state.nodes[node].label.clone(), state.nodes[node].key.clone());
        state.log(format!("{label} reported finding {id}: {title}"), Some(&key), "info", None);
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)?;
        Ok(json!({"ok": true, "count": pending.len()}))
    }

    pub async fn agent_fail(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        message: &str,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        if state.runs[run].state == NodeState::Failed.as_str() && state.runs[run].error.as_deref() == Some(message) {
            return Ok(json!({"ok": true, "state": state.nodes[node].state}));
        }
        if !Self::run_is_active(&state, run) || !Self::node_is_active(&state, node) {
            return Err(WorkflowError::conflict("Run is no longer active"));
        }
        TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(message.to_string()));
        self.collect_usage(&mut state, run, None).await?;
        state.set_state(node, NodeState::Failed, Some("Agent reported failure"), Some(message.to_string()));
        let result = json!({"ok": true, "state": state.nodes[node].state});
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)?;
        Ok(result)
    }

    /// The launcher script reports that the harness process ended.
    pub async fn agent_exited(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        exit_code: i64,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let launched = run_id.and_then(|id| state.runs.iter().position(|r| r.id == id));
        let owner_role = |state: &TaskState, run: usize| {
            state.nodes.iter().find(|n| n.id == state.runs[run].node_id).and_then(|n| n.role.clone())
        };
        let Some(mut run) = launched.filter(|&run| owner_role(&state, run).as_deref() == Some(role)) else {
            return Err(WorkflowError::conflict("Unknown launcher run"));
        };
        let launch_run_id = state.runs[run].id;
        state.runs[run].exit_code = Some(exit_code);
        self.collect_usage(&mut state, run, None).await?;
        let leased: Option<i64> = self.db.session(|tx| {
            use rusqlite::OptionalExtension;
            tx.query_row(
                "SELECT agent_sessions.id FROM agent_sessions JOIN process_leases \
                 ON process_leases.session_id = agent_sessions.id \
                 WHERE process_leases.launch_run_id = ?1 AND agent_sessions.status = 'running' LIMIT 1",
                [launch_run_id],
                |row| row.get(0),
            )
            .optional()
        })?;
        if let Some(agent) = leased.and_then(|id| state.agents.iter().position(|a| a.id == id)) {
            state.agents[agent].status = "exited".into();
            state.agents[agent].ended_at = Some(DbTime::now());
            self.screen_trackers.cancel(state.agents[agent].id);
            // A persistent session may be serving a later run than the one it was launched for.
            if let Some(current) = state.agents[agent].current_run_id.filter(|&id| id != launch_run_id)
                && let Some(later) = state.runs.iter().position(|r| r.id == current)
            {
                run = later;
            }
        }
        let node_id = state.runs[run].node_id;
        let node = state
            .nodes
            .iter()
            .position(|n| n.id == node_id)
            .ok_or_else(|| WorkflowError::not_found("Node not found"))?;
        if Self::run_is_active(&state, run) && Self::node_is_active(&state, node) {
            let error = format!("Harness exited with code {exit_code} before reporting completion");
            state.runs[run].state = NodeState::Failed.as_str().into();
            state.runs[run].completed_at = Some(DbTime::now());
            state.runs[run].error = Some(error.clone());
            state.set_state(node, NodeState::Failed, Some(&format!("Harness exited (code {exit_code})")), Some(error));
        } else {
            let (label, key) = (state.nodes[node].label.clone(), state.nodes[node].key.clone());
            state.log(format!("{label} harness process exited (code {exit_code})"), Some(&key), "info", None);
        }
        let result = json!({"ok": true, "state": state.nodes[node].state});
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)?;
        Ok(result)
    }

    pub async fn agent_complete(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        result: Map<String, Value>,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        let response = self.apply_completion(&mut state, node, run, result).await?;
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)?;
        Ok(response)
    }

    /// Shared by the live `flowctl complete` and a human replaying a persisted result after
    /// recovery; runs under the task's lock.
    pub(crate) async fn apply_completion(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        result: Map<String, Value>,
    ) -> Result<Value, WorkflowError> {
        let raw = Value::Object(result.clone());
        let done =
            |state: &TaskState| json!({"ok": true, "state": state.nodes[node].state, "taskStatus": state.task.status});
        if state.runs[run].state == NodeState::Completed.as_str() && py_eq(&state.runs[run].result.0, &raw) {
            return Ok(done(state));
        }
        if state.runs[run].state == "COMPLETED"
            && state.nodes[node].key.starts_with("worker_")
            && let Ok(mut repeated) = super::result_models::normalize(&result, "implementation")
        {
            repeated.insert("checkpointSha".into(), json!(state.nodes[node].checkpoint_sha));
            if py_eq(&json!(repeated), &state.runs[run].result.0) {
                return Ok(done(state));
            }
        }
        if !Self::run_is_active(state, run) {
            return Err(WorkflowError::conflict("Run is no longer active"));
        }
        if !Self::node_is_active(state, node) {
            return Err(WorkflowError::conflict(format!(
                "{} is {}; completion is only accepted while it is running",
                state.nodes[node].label, state.nodes[node].state
            )));
        }
        let key = state.nodes[node].key.clone();
        if ![
            spec::ARCHITECT,
            spec::ORCHESTRATOR,
            spec::IMPLEMENTER,
            spec::CODE_REVIEW,
            spec::SECURITY_REVIEW,
            spec::FINAL_ARCHITECT,
        ]
        .contains(&key.as_str())
            && !key.starts_with("worker_")
        {
            return Err(WorkflowError::conflict(format!(
                "Completing the {key} node is not supported by this backend yet"
            )));
        }
        self.completion_rejection(state, node, run, &result)?;
        let run_id = state.runs[run].id;
        if let Some(run_dir) = state.runs[run].run_dir.clone().filter(|d| !d.is_empty()) {
            let path = files::write_json(&Path::new(&run_dir).join("result.json"), &raw).map_err(io_error)?;
            state.runs[run].result_path = Some(py_str(&path));
            let mut artifact = TaskArtifactRow {
                id: 0,
                task_id: state.task.id,
                node_run_id: Some(run_id),
                kind: format!("{key}-result"),
                path: py_str(&path),
                created_at: DbTime::now(),
            };
            self.db.session(|tx| table::insert(tx, &mut artifact))?;
        }
        state.runs[run].result = Json(raw);
        self.collect_usage(state, run, None).await?;
        let outcome = match key.as_str() {
            spec::ARCHITECT => self.complete_discovery(state, node, run, &result),
            spec::ORCHESTRATOR => self.accept_plan(state, node, run, &result).await,
            key if key.starts_with("worker_") => self.complete_worker(state, node, run, &result).await,
            spec::IMPLEMENTER => self.complete_implementation(state, node, run, &result).await,
            spec::CODE_REVIEW | spec::SECURITY_REVIEW => self.complete_review(state, node, run, &result).await,
            spec::FINAL_ARCHITECT => self.complete_final(state, node, run, &result).await,
            _ => unreachable!(),
        };
        let error = match outcome {
            Ok(()) => None,
            Err(problem) => {
                let run_row = &mut state.runs[run];
                run_row.state = NodeState::Failed.as_str().into();
                run_row.error = Some(format!("Result could not be accepted: {problem}"));
                run_row.completed_at = Some(DbTime::now());
                state.set_state(node, NodeState::Failed, Some("Result validation failed"), Some(problem.clone()));
                Some(problem)
            }
        };
        let mut response =
            json!({"ok": error.is_none(), "state": state.nodes[node].state, "taskStatus": state.task.status});
        if let Some(error) = error {
            response["error"] = json!(error);
        }
        Ok(response)
    }

    /// Architect discovery: the requirement documents must exist; then the requirements gate
    /// waits for the human. A validation problem is returned as the error text.
    fn complete_discovery(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), String> {
        let result = discovery(raw)?;
        if result.status != "completed" {
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(result.summary.clone()));
            state.set_state(
                node,
                NodeState::Failed,
                Some(&format!("Architect reported {}", result.status)),
                Some(result.summary),
            );
            return Ok(());
        }
        let task_workspace = workspace(&state.task);
        let mut missing = Vec::new();
        for name in ["requirements.md", "acceptance-criteria.md", "implementation-plan.md"] {
            let text = task_workspace.read_text(&format!("requirements/{name}"), "").map_err(|e| e.to_string())?;
            if super::prompts::py_strip(&text).is_empty() {
                missing.push(name);
            }
        }
        if !missing.is_empty() {
            let error = format!("Missing deliverables: {}", missing.join(", "));
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(error.clone()));
            state.set_state(node, NodeState::Failed, Some("Deliverables missing"), Some(error));
            return Ok(());
        }
        if !result.open_questions.is_empty() {
            files::write_json(
                &task_workspace.requirements_dir.join("questions.json"),
                &Value::Array(result.open_questions),
            )
            .map_err(|e| e.to_string())?;
        }
        TaskState::finish_run(&mut state.runs[run], NodeState::Completed, None);
        let summary: String = result.summary.chars().take(200).collect();
        let message = if summary.is_empty() { "Discovery complete".to_string() } else { summary };
        state.set_state(node, NodeState::Completed, Some(&message), None);
        let gate = state.node_index(spec::REQUIREMENTS_GATE).map_err(|e| e.message)?;
        state.set_state(gate, NodeState::WaitingForApproval, Some("Review requirements and approve"), None);
        Ok(())
    }

    // ------------------------------------------------------------------------------ usage

    /// Collect a run's provider usage. Returns "collected", "pending", "failed" or "skipped".
    /// `until` keeps polling a collector whose usage is not written yet; otherwise pending usage
    /// is collected again in the background.
    pub(crate) async fn collect_usage(
        &self,
        state: &mut TaskState,
        run: usize,
        until: Option<Instant>,
    ) -> Result<&'static str, WorkflowError> {
        let config = self.config();
        let harness = state.runs[run].harness.clone().unwrap_or_default();
        let Some(command) = config.harnesses.get(&harness).and_then(|h| h.usage_command.clone()) else {
            return Ok("skipped");
        };
        let run_id = state.runs[run].id;
        let evidence: Option<RunEvidenceRow> =
            self.db.session(|tx| table::select_one(tx, "WHERE run_id = ?1", [run_id]))?;
        if let Some(usage) = evidence.as_ref().map(|e| &e.usage.0).filter(|u| truthy(Some(u))) {
            // A late exit callback must not replace accepted usage with totals from a later run
            // that has already resumed this conversation. Usage recorded while the run's last
            // turn was still open is replaced once the turn has ended.
            if usage.get("provenance").and_then(Value::as_str) == Some("human-imported-provider-report") {
                return Ok("skipped");
            }
            if !Self::run_is_active(state, run) && !truthy(usage.get("partial")) {
                return Ok("skipped");
            }
        }
        let agent: Option<AgentSessionRow> =
            state.agents.iter().find(|a| a.current_run_id == Some(run_id)).map(|a| a.row.clone());
        if let Some(agent) = agent.as_ref().filter(|a| a.harness_session_id.as_deref().is_some_and(|s| !s.is_empty())) {
            let advanced = state.agents.iter().any(|a| {
                a.harness == agent.harness && a.harness_session_id == agent.harness_session_id && a.id > agent.id
            });
            if advanced {
                state.log(
                    "Usage unavailable: this conversation has already advanced to another run",
                    None,
                    "warning",
                    None,
                );
                return Ok("skipped");
            }
        }
        // The collector may take up to 15s; don't hold the pending writes meanwhile.
        self.db.session(|tx| state.flush(tx))?;
        let node = state
            .nodes
            .iter()
            .position(|n| n.id == state.runs[run].node_id)
            .ok_or_else(|| WorkflowError::conflict("Run node missing"))?;
        let cwd = self.run_cwd(state, node)?;
        let session_id = agent.as_ref().and_then(|a| a.harness_session_id.clone());
        let source = self.usage_source();
        let collected = loop {
            match source.collect(&command, &cwd, run_id, session_id.as_deref()).await {
                Err(UsageError::Pending(_)) if until.is_some_and(|until| Instant::now() < until) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                other => break other,
            }
        };
        let report = collected.and_then(|report| self.run_usage(&state.runs[run], &report));
        match report {
            Ok(report) => {
                self.record_usage(run_id, &report, false)?;
                Ok("collected")
            }
            Err(UsageError::Pending(partial)) => {
                // The agent's own `flowctl complete` runs mid-turn, so the rest of that turn is
                // written after it. A background retry collects it once the turn has ended.
                let partial = partial.and_then(|p| self.run_usage(&state.runs[run], &p).ok());
                if let Some(partial) = &partial {
                    self.record_usage(run_id, partial, true)?;
                }
                if self.schedule_usage_retry(&state.task.jira_key, run_id) {
                    let so_far = if partial.is_some() { "recorded its usage so far and " } else { "" };
                    state.log(
                        format!("{harness} is still finishing this run's turn; {so_far}collecting it again shortly"),
                        None,
                        "info",
                        None,
                    );
                }
                Ok("pending")
            }
            Err(UsageError::Failed(_)) => {
                state.log("Provider usage unavailable; usage remains unknown", None, "warning", None);
                Ok("failed")
            }
        }
    }

    /// A session-scoped report minus the run's launch baseline.
    fn run_usage(&self, run: &NodeRunRow, report: &UsageReport) -> Result<UsageReport, UsageError> {
        if report.scope != "session" {
            return Ok(report.clone());
        }
        let baseline_path = run.run_dir.as_deref().map(|dir| Path::new(dir).join("usage-baseline.json"));
        let baseline = match baseline_path.filter(|p| p.is_file()) {
            Some(path) => match files::read_json(&path) {
                Ok(Value::Object(map)) => Some(map),
                Ok(Value::Null) => None,
                Ok(_) => return Err(UsageError::Failed("Usage baseline is not an object".into())),
                Err(error) => return Err(UsageError::Failed(error.to_string())),
            },
            None => None,
        };
        usage::run_usage(report, baseline.as_ref())
    }

    fn record_usage(&self, run_id: i64, report: &UsageReport, partial: bool) -> Result<(), WorkflowError> {
        let mut record = report.record("configured-provider-collector");
        if partial {
            record.insert("partial".into(), json!(true));
        }
        self.db.session(|tx| -> rusqlite::Result<()> {
            match table::select_one::<RunEvidenceRow>(tx, "WHERE run_id = ?1", [run_id])? {
                Some(original) => {
                    let mut changed = original.clone();
                    changed.usage = Json(Value::Object(record));
                    changed.confirmed_model = Some(report.model.clone());
                    table::update(tx, &original, &changed)?;
                }
                None => table::insert(
                    tx,
                    &mut RunEvidenceRow {
                        uid: new_uid(),
                        run_id,
                        requirements_digest: None,
                        verification: Json(Value::Null),
                        usage: Json(Value::Object(record)),
                        confirmed_model: Some(report.model.clone()),
                        billing_mode: "unknown".into(),
                    },
                )?,
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Start a background retry for a run's pending usage. False when one is already scheduled.
    fn schedule_usage_retry(&self, jira_key: &str, run_id: i64) -> bool {
        let mut retries = self.usage_retries.lock().unwrap_or_else(|e| e.into_inner());
        if retries.get(&run_id).is_some_and(|task| !task.is_finished()) {
            return false;
        }
        let (engine, key) = (self.me.clone(), jira_key.to_string());
        let task = tokio::spawn(async move {
            retry_usage(engine.clone(), &key, run_id).await;
            if let Some(engine) = engine.upgrade() {
                engine.usage_retries.lock().unwrap_or_else(|e| e.into_inner()).remove(&run_id);
            }
        });
        retries.insert(run_id, task);
        true
    }

    /// Type the harness's exit command into each idle agent, then collect the usage it writes on
    /// exit, all within one shared deadline. Returns whether any agent was asked to exit.
    ///
    /// Only an idle screen is safe: typed into a dialog, the Enter would pick an option, and into
    /// a working agent, the text may be queued as its next prompt.
    pub(crate) async fn exit_for_usage(&self, state: &mut TaskState, agents: &[usize]) -> Result<bool, WorkflowError> {
        let config = self.config();
        let registry = self.harnesses();
        let session = state.task.zellij_session_name.clone();
        let mut runs = Vec::new();
        for &index in agents {
            let agent = &state.agents[index];
            let has_command = config.harnesses.get(&agent.harness).is_some_and(|h| h.usage_command.is_some());
            let run = agent.current_run_id.and_then(|id| state.runs.iter().position(|r| r.id == id));
            let Some(pane_id) = agent.zellij_pane_id.clone().filter(|p| !p.is_empty()) else { continue };
            let (true, Some(run)) = (has_command, run) else { continue };
            let Ok(adapter) = registry.get(&agent.harness) else { continue };
            let Some(exit) = adapter.kind.exit_command() else { continue };
            let Ok(screen) = self.terminals.screen(&session, &pane_id).await else { continue };
            if adapter.classify_screen(&screen).as_str() != "idle" {
                continue;
            }
            if self.terminals.write_line(&session, &pane_id, exit).await.is_err() {
                continue; // the tab is closed as before
            }
            runs.push(run);
        }
        let until = Instant::now() + EXIT_USAGE_WAIT;
        for &run in &runs {
            self.collect_usage(state, run, Some(until)).await?;
        }
        Ok(!runs.is_empty())
    }
}

/// The node currently owning a role (final verification and discovery share the architect).
fn node_by_role(state: &TaskState, role: &str) -> Result<usize, WorkflowError> {
    let candidates: Vec<usize> =
        (0..state.nodes.len()).filter(|&i| state.nodes[i].role.as_deref() == Some(role)).collect();
    let in_states = |states: &[NodeState]| {
        candidates.iter().copied().find(|&i| state.node_state(i).is_some_and(|s| states.contains(&s)))
    };
    in_states(&[NodeState::Running, NodeState::WaitingForUser])
        .or_else(|| in_states(&[NodeState::Ready, NodeState::Failed]))
        .or_else(|| candidates.last().copied())
        .ok_or_else(|| WorkflowError::not_found(format!("No node for role {role}")))
}

fn io_error(error: std::io::Error) -> WorkflowError {
    WorkflowError::new(format!("Could not access the run files: {error}"), 500)
}

/// Collect a run's pending usage again once its turn has had time to end.
async fn retry_usage(engine: std::sync::Weak<Engine>, jira_key: &str, run_id: i64) {
    let Some(delays) = engine.upgrade().map(|e| e.usage_retry_delays()) else { return };
    for (attempt, delay) in delays.iter().enumerate() {
        tokio::time::sleep(*delay).await;
        let Some(engine) = engine.upgrade() else { return };
        let status = {
            let lock = engine.lock(jira_key);
            let _held = lock.lock().await;
            let Ok(mut state) = engine.db.session(|tx| TaskState::load(tx, jira_key)) else { return };
            let Some(run) = state.runs.iter().position(|r| r.id == run_id) else { return };
            let Ok(status) = engine.collect_usage(&mut state, run, None).await else { return };
            if status == "pending" && attempt + 1 == delays.len() {
                state.log(
                    "Provider usage still incomplete; it is collected again when the agent session closes",
                    None,
                    "warning",
                    None,
                );
            }
            if engine.db.session(|tx| state.flush(tx)).is_err() {
                return;
            }
            status
        };
        let _ = engine.publish(jira_key);
        if status != "pending" {
            return;
        }
    }
}

/// Retries in flight, by run id.
pub type UsageRetries = std::sync::Mutex<HashMap<i64, tokio::task::JoinHandle<()>>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_follows_python() {
        assert!("Yes, all tenants é 中".chars().all(py_isprintable));
        for c in ['\n', '\t', '\u{a0}', '\u{200b}', '\u{2028}', '\u{e000}', '\u{378}', '\u{feff}'] {
            assert!(!py_isprintable(c), "{c:?}");
        }
        assert_eq!(clean_answer("  All  "), Ok("All".into()));
        assert!(clean_answer("a\nb").is_err());
        assert!(clean_answer("   ").is_err());
    }

    #[test]
    fn python_equality() {
        assert!(py_eq(&json!({"a": 1, "b": [1.0]}), &json!({"b": [1], "a": 1.0})));
        assert!(!py_eq(&json!({"a": 1}), &json!({"a": 2})));
    }

    #[test]
    fn findings_dump_like_pydantic() {
        let dumped = finding_dump(&json!({"title": "t", "severity": " Major ", "extra": 1, "line": "42"})).unwrap();
        assert_eq!(Value::Object(dumped), json!({"severity": "high", "line": 42, "title": "t", "extra": 1}));
        assert!(finding_dump(&json!({"severity": "huge"})).is_err());
    }
}
