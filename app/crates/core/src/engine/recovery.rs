//! Recovering runs a backend restart interrupted, ported from the engine's `recover`,
//! `recover_node` and `_pending_completion`.
//!
//! At startup every active node is marked INTERRUPTED, with a message saying what a human can do:
//! resume the agent that survived the restart, submit a result flowctl saved to disk, or retry.
//! Nothing is launched or replayed automatically. Eligible pending workers become READY;
//! occupied slots still prevent a fourth worker from launching.

use std::path::Path;

use serde_json::{Map, Value};

use super::states::{NodeState, TaskStatus};
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::table;
use crate::db::{AgentAccessRow, NodeRunRow, TaskRow};
use crate::workspace as files;

/// A completion flowctl saved to disk (`pending-complete.json`) whose receipt the backend never
/// confirmed.
pub fn pending_completion(run: Option<&NodeRunRow>) -> Option<Map<String, Value>> {
    let run = run?;
    let run_dir = run.run_dir.as_deref().filter(|d| !d.is_empty())?;
    let path = Path::new(run_dir).join("pending-complete.json");
    if !path.exists() {
        return None;
    }
    let payload = files::read_json(&path).ok()?;
    let same_run = payload.get("runId").is_some_and(|id| super::agents::py_eq(id, &Value::from(run.id)));
    if !payload.is_object() || !same_run {
        return None;
    }
    match payload.get("result") {
        Some(Value::Object(result)) => Some(result.clone()),
        _ => None,
    }
}

fn latest_run(state: &TaskState, node: usize) -> Option<usize> {
    let node_id = state.nodes[node].id;
    (0..state.runs.len()).filter(|&i| state.runs[i].node_id == node_id).max_by_key(|&i| state.runs[i].id)
}

impl Engine {
    /// Reconcile persisted runs after a restart, without launching agents or replaying work.
    pub async fn recover(&self) -> Result<(), WorkflowError> {
        let tasks: Vec<TaskRow> = self.db.session(|tx| table::select(tx, "ORDER BY id", []))?;
        for task in tasks {
            let finished =
                TaskStatus::parse(&task.status).is_some_and(|s| s == TaskStatus::Cancelled || s.is_post_ready());
            if finished {
                continue;
            }
            let lock = self.lock(&task.jira_key);
            let _held = lock.lock().await;
            let mut state = self.db.session(|tx| TaskState::load(tx, &task.jira_key))?;
            for node in 0..state.nodes.len() {
                let node_state = state.node_state(node);
                if !node_state.is_some_and(|s| s.is_active() || s == NodeState::Interrupted) {
                    continue;
                }
                let run = latest_run(&state, node);
                let message = if pending_completion(run.map(|r| &state.runs[r].row)).is_some() {
                    // The agent finished and flowctl captured the result, but the backend never
                    // confirmed receipt. Don't apply it here: startup must not block on (or
                    // silently trigger) a checkpoint or verification. A human resumes it.
                    "Backend restarted. A finished result was saved to disk before the restart; Resume to submit it."
                        .to_string()
                } else {
                    let role = state.nodes[node].role.clone().unwrap_or_default();
                    match self.live_session(&mut state, &role, Some(node)).await {
                        Ok(None) => "Agent is no longer present. Retry explicitly to launch a new run.".to_string(),
                        Ok(Some(agent)) => {
                            self.ensure_screen_tracker(&state, agent);
                            "Backend restarted. Resume the existing agent or stop it and retry.".to_string()
                        }
                        Err(error) => {
                            format!("Could not inspect agent process: {error}. Recovery requires a human action.")
                        }
                    }
                };
                if let Some(run) = run {
                    state.runs[run].state = NodeState::Interrupted.as_str().into();
                }
                state.set_state(node, NodeState::Interrupted, Some(&message), None);
            }
            self.recover_workers(&mut state)?;
            self.db.session(|tx| state.flush(tx))?;
        }
        Ok(())
    }

    /// The human's choice for an INTERRUPTED node: resume the surviving agent (or submit the
    /// result saved to disk), or `retry` with a fresh run.
    pub async fn recover_node(&self, jira_key: &str, node_key: &str, retry: bool) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let node = state.node_index(node_key)?;
        if state.node_state(node) != Some(NodeState::Interrupted) || state.status() != Some(TaskStatus::Active) {
            return Err(WorkflowError::conflict("Node is not awaiting recovery"));
        }
        let run = latest_run(&state, node);
        let pending = if retry { None } else { pending_completion(run.map(|r| &state.runs[r].row)) };
        let role = state.nodes[node].role.clone().unwrap_or_default();
        if let (Some(pending), Some(run)) = (pending, run) {
            state.runs[run].state = NodeState::Running.as_str().into();
            state.set_state(
                node,
                NodeState::Running,
                Some("Human resumed; submitting the completion result saved to disk before the restart"),
                None,
            );
            self.apply_completion(&mut state, node, run, pending).await?;
            if let Some(run_dir) = state.runs[run].run_dir.clone().filter(|d| !d.is_empty()) {
                let _ = std::fs::remove_file(Path::new(&run_dir).join("pending-complete.json"));
            }
        } else if retry {
            let ids = super::launch::session_node_ids(&state, node);
            self.stop_sessions(&mut state, Some(&role), Some(&ids), true).await?;
            if let Some(run) = run {
                TaskState::finish_run(
                    &mut state.runs[run],
                    NodeState::Failed,
                    Some("Human retried after interruption".into()),
                );
            }
            self.start_agent_run(&mut state, node, None).await?;
        } else {
            let live = self.live_session(&mut state, &role, Some(node)).await?;
            let access: Option<AgentAccessRow> = match run {
                Some(run) => {
                    let run_id = state.runs[run].id;
                    self.db.session(|tx| table::select_one(tx, "WHERE run_id = ?1", [run_id]))?
                }
                None => None,
            };
            let (Some(agent), Some(_), Some(run)) = (live, access, run) else {
                return Err(WorkflowError::conflict("No authenticated agent session survives; use Stop & retry"));
            };
            self.ensure_screen_tracker(&state, agent);
            state.runs[run].state = NodeState::Running.as_str().into();
            state.set_state(node, NodeState::Running, Some("Human resumed the surviving agent"), None);
            let run_id = state.runs[run].id;
            let pane = state.agents[agent].zellij_pane_id.clone().unwrap_or_default();
            let message = format!(
                "AgentFlow recovered. Continue run {run_id}; use --run {run_id} on flowctl commands. If completion \
                 was rejected while offline, resubmit the existing result; do not redo completed work."
            );
            self.terminals
                .write_line(&state.task.zellij_session_name, &pane, &message)
                .await
                .map_err(|e| WorkflowError::conflict(e.0))?;
        }
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }
}
