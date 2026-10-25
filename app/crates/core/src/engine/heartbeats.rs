//! Observational agent reports. Silence never changes workflow or scheduling state.

use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use super::states::NodeState;
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::{DbTime, new_uid};

impl Engine {
    /// Accept only the latest active attempt, using the existing scoped callback authority.
    pub async fn agent_heartbeat(
        &self,
        jira_key: &str,
        role: &str,
        run_id: Option<i64>,
        phase: &str,
    ) -> Result<Value, WorkflowError> {
        if !matches!(phase, "working" | "investigating" | "implementing" | "testing" | "reviewing") {
            return Err(WorkflowError::new("Invalid heartbeat phase", 422));
        }
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let (node, run) = self.resolve_run(&state, role, run_id)?;
        if !NodeState::parse(&state.runs[run].state).is_some_and(NodeState::is_active)
            || !state.node_state(node).is_some_and(NodeState::is_active)
        {
            return Err(WorkflowError::conflict("Heartbeat requires an active run and node"));
        }
        let received_at = DbTime::now();
        let run_id = state.runs[run].id;
        self.db.session(|tx| {
            tx.execute(
                "INSERT INTO run_heartbeats (uid, run_id, received_at, phase) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(run_id) DO UPDATE SET received_at = excluded.received_at, phase = excluded.phase",
                (new_uid(), run_id, &received_at, phase),
            )
        })?;
        drop(held);
        self.publish(jira_key)?;
        Ok(json!({"ok": true, "runId": run_id, "receivedAt": super::snapshot::iso(Some(&received_at))}))
    }

    pub(super) fn heartbeat_snapshot(&self, run_id: i64, warning_since: &DbTime) -> Result<Value, WorkflowError> {
        let report = self.db.session(|tx| {
            tx.query_row("SELECT received_at, phase FROM run_heartbeats WHERE run_id = ?1", [run_id], |row| {
                Ok((row.get::<_, DbTime>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
        })?;
        Ok(json!({
            "receivedAt": report.as_ref().map(|(at, _)| super::snapshot::iso(Some(at))),
            "observedAt": super::snapshot::iso(Some(&DbTime::now())),
            "warningSince": super::snapshot::iso(Some(warning_since)),
            "phase": report.as_ref().map(|(_, phase)| phase),
            "warningSeconds": self.config().workflow.heartbeat_warning_seconds,
        }))
    }
}
