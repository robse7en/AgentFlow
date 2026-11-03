//! Lessons agents propose for their role's repository guidance (`flowctl lesson`), ported from
//! `backend/agentflow/workflow/lessons.py` and the engine's `agent_lesson` / `decide_lesson`.
//!
//! A proposal is only a record on the task, in `<task>/lessons.json`. It reaches a briefing only
//! after a human approves it into `.agentflow-instructions/<role>.md` in the task's original
//! repository checkout, commits that file, and a later task's worktree is cut from the commit.
//! Lesson text is agent output: shown as plain text, and normalised to a single bullet line before
//! it is ever written. `normalize`, `validate` and `append_lesson` match Python byte for byte
//! (golden lessons-unit).

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::snapshot::{iso, workspace};
use super::states::NodeState;
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::{DbTime, TaskRow, new_uid};
use crate::paths::py_str;

pub const MAX_LESSON_CHARS: usize = 300;
pub const MAX_PER_RUN: usize = 5;
pub const MAX_PER_TASK: usize = 50;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
pub const HEADING: &str = "## Lessons from past tasks";
const FILE: &str = "lessons.json";

/// One line: every whitespace run (line breaks included) becomes one space, ends trimmed
/// (Python's `" ".join(text.split())`). A lesson can therefore never add structure of its own.
pub fn normalize(text: &str) -> String {
    text.split(super::prompts::py_space).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The normalised lesson, or what to fix.
pub fn validate(text: &str) -> Result<String, String> {
    let line = normalize(text);
    if line.is_empty() {
        return Err("A lesson needs text".into());
    }
    if line.contains('\0') {
        return Err("A lesson cannot contain NUL characters".into());
    }
    let length = line.chars().count();
    if length > MAX_LESSON_CHARS {
        return Err(format!("A lesson is at most {MAX_LESSON_CHARS} characters (this one has {length})"));
    }
    Ok(line)
}

/// `existing` guidance with `- line` added at the end, under the lessons heading. The heading is
/// added when the file's last heading is another one. The file's own line endings are kept.
pub fn append_lesson(existing: Option<&str>, line: &str) -> String {
    let bullet = format!("- {line}");
    let Some(existing) = existing.filter(|e| !e.is_empty()) else {
        return format!("{HEADING}\n\n{bullet}\n");
    };
    let newline = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let mut text = existing.to_string();
    if !text.ends_with('\n') {
        text.push_str(newline);
    }
    let last_heading =
        text.split('\n').filter(|row| row.starts_with('#')).map(|row| row.trim_end_matches('\r')).next_back();
    if last_heading != Some(HEADING) {
        text.push_str(&format!("{newline}{HEADING}{newline}{newline}"));
    }
    text + &bullet + newline
}

pub fn path(task_dir: &Path) -> PathBuf {
    task_dir.join(FILE)
}

/// The task's lessons, oldest first. A missing file is no lessons; an unreadable one fails closed.
pub fn read(task_dir: &Path) -> Result<Vec<Value>, String> {
    let target = path(task_dir);
    if !target.exists() {
        return Ok(Vec::new());
    }
    let unreadable = |e: &dyn std::fmt::Display| format!("The task's lessons file is unreadable: {e}");
    let mut raw = Vec::new();
    std::fs::File::open(&target)
        .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut raw))
        .map_err(|e| unreadable(&e))?;
    if raw.len() as u64 > MAX_FILE_BYTES {
        return Err("The task's lessons file is too large".into());
    }
    let text = String::from_utf8(raw).map_err(|e| unreadable(&e))?;
    let data: Value = serde_json::from_str(&text).map_err(|e| unreadable(&e))?;
    match (data.get("schema"), data.get("lessons")) {
        (Some(schema), Some(Value::Array(items))) if schema == &json!(1) => Ok(items.clone()),
        _ => Err("The task's lessons file has an unknown format".into()),
    }
}

impl Engine {
    fn read_lessons(task: &TaskRow) -> Result<Vec<Value>, WorkflowError> {
        read(&workspace(task).root).map_err(WorkflowError::conflict)
    }

    fn write_lessons(task: &TaskRow, items: Vec<Value>) -> Result<(), WorkflowError> {
        crate::instructions::atomic_json(&path(&workspace(task).root), &json!({"schema": 1, "lessons": items}))
            .map_err(|e| WorkflowError::conflict(format!("Could not save the task's lessons: {e}")))
    }

    /// The task's lessons for the UI. An unreadable file shows none here; every write fails closed.
    pub(super) fn snapshot_lessons(task: &TaskRow) -> Vec<Value> {
        read(&workspace(task).root).unwrap_or_default()
    }

    /// Record a lesson an agent proposes for its role's repository guidance. Only a proposal:
    /// nothing reaches any briefing until a human approves it (`decide_lesson`) and commits it.
    pub async fn agent_lesson(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        message: &str,
    ) -> Result<Value, WorkflowError> {
        let line = validate(message).map_err(|e| WorkflowError::new(e, 422))?;
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        if !NodeState::parse(&state.runs[run].state).is_some_and(NodeState::is_active)
            || !state.node_state(node).is_some_and(NodeState::is_active)
        {
            return Err(WorkflowError::conflict("Lessons require an active run and node"));
        }
        let mut items = Self::read_lessons(&state.task)?;
        let run_id = state.runs[run].id;
        let from_run = items.iter().filter(|item| item.get("runId") == Some(&json!(run_id))).count();
        if from_run >= MAX_PER_RUN {
            return Err(WorkflowError::conflict(format!("This run already proposed {MAX_PER_RUN} lessons")));
        }
        if items.len() >= MAX_PER_TASK {
            return Err(WorkflowError::conflict(format!("This task already has {MAX_PER_TASK} lessons")));
        }
        let (label, key) = (state.nodes[node].label.clone(), state.nodes[node].key.clone());
        let uid = new_uid();
        items.push(json!({
            "uid": uid, "role": state.nodes[node].role, "nodeKey": key, "nodeLabel": label,
            "runId": run_id, "runNumber": state.runs[run].run_number, "text": line, "status": "proposed",
            "createdAt": iso(Some(&DbTime::now())), "decidedAt": null, "approvedText": null, "path": null,
        }));
        Self::write_lessons(&state.task, items)?;
        state.log(format!("{label} proposed a lesson for human review"), Some(&key), "info", None);
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)?;
        Ok(json!({"ok": true, "uid": uid, "count": from_run + 1}))
    }

    /// A human approves (optionally edited) or dismisses a proposed lesson. Approving appends it to
    /// the role's `.agentflow-instructions` file in the task's original repository checkout: never
    /// the task worktree, never committed. It reaches future tasks only once a human commits it.
    pub async fn decide_lesson(
        &self,
        jira_key: &str,
        uid: &str,
        approve: bool,
        text: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let projects = self.projects_lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let mut items = Self::read_lessons(&state.task)?;
        let index = items
            .iter()
            .position(|item| item.get("uid").and_then(Value::as_str) == Some(uid))
            .ok_or_else(|| WorkflowError::not_found("Lesson not found"))?;
        let status = items[index]["status"].as_str().unwrap_or_default().to_string();
        if status != "proposed" {
            return Err(WorkflowError::conflict(format!("This lesson was already {status}")));
        }
        let node_key = items[index]["nodeKey"].as_str().map(String::from);
        let mut dirty: Vec<String> = Vec::new();
        if approve {
            let proposed = items[index]["text"].as_str().unwrap_or_default().to_string();
            let line = validate(text.unwrap_or(&proposed)).map_err(|e| WorkflowError::new(e, 422))?;
            let role = items[index]["role"].as_str().unwrap_or_default().to_string();
            let root = PathBuf::from(&state.task.repository_root);
            let current = crate::instructions::read_source(&root, &role)?;
            let saved = crate::instructions::save_source(
                &root,
                &role,
                &append_lesson(current.content.as_deref(), &line),
                current.hash.as_deref(),
            )?;
            let lesson = &mut items[index];
            lesson["status"] = json!("approved");
            lesson["approvedText"] = json!(line);
            lesson["path"] = json!(py_str(&root.join(&saved.path)));
            dirty = self.git.dirty_paths(&root, false).await.unwrap_or_default();
            state.log(
                format!("Human approved a lesson into {} (not committed)", saved.path),
                node_key.as_deref(),
                "info",
                None,
            );
        } else {
            items[index]["status"] = json!("dismissed");
            state.log("Human dismissed a proposed lesson", node_key.as_deref(), "info", None);
        }
        items[index]["decidedAt"] = iso(Some(&DbTime::now()));
        let lesson = items[index].clone();
        Self::write_lessons(&state.task, items)?;
        self.db.session(|tx| state.flush(tx))?;
        drop(projects);
        drop(held);
        self.publish(jira_key)?;
        Ok(json!({"lesson": lesson, "dirtyPaths": dirty}))
    }
}
