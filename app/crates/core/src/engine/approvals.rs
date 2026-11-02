//! Human approval bound to the displayed requirements digest and reviewed checkpoint.
use super::snapshot::workspace;
use super::states::{NodeState, TaskStatus};
use super::store::TaskState;
use super::{Engine, WorkflowError, rules, spec};
use crate::db::table;
use crate::db::{DbTime, Json, RequirementsApprovalRow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub reason: Option<String>,
    pub expected_sha: Option<String>,
    pub expected_requirements_digest: Option<String>,
}

impl Engine {
    pub async fn approve(&self, key: &str, node_key: &str, request: ApprovalRequest) -> Result<Value, WorkflowError> {
        self.approve_via(key, node_key, request, None).await
    }

    /// A channel label supplied by an authorized human adapter, never by HTTP request data.
    pub async fn approve_via(
        &self,
        key: &str,
        node_key: &str,
        request: ApprovalRequest,
        via: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        let node = state.node_index(node_key)?;
        if state.status().is_some_and(|s| s == TaskStatus::Cancelled || s.is_post_ready()) {
            return Err(WorkflowError::conflict("Task no longer accepts approvals"));
        }
        let approval: Option<RequirementsApprovalRow> =
            self.db.session(|tx| table::select_one(tx, "WHERE task_id = ?1", [state.task.id]))?;
        let legacy = node_key == spec::REQUIREMENTS_GATE
            && state.node_state(node) == Some(NodeState::Completed)
            && approval.is_none();
        if state.node_state(node) != Some(NodeState::WaitingForApproval) && !legacy {
            return Err(WorkflowError::conflict(format!("{} is not waiting for approval", state.nodes[node].label)));
        }
        if node_key == spec::REQUIREMENTS_GATE {
            let digest = request
                .expected_requirements_digest
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| WorkflowError::conflict("Refresh and review the requirements before approving"))?;
            if legacy && state.nodes.iter().any(|n| NodeState::parse(&n.state).is_some_and(NodeState::is_active)) {
                return Err(WorkflowError::conflict(
                    "Stop active agents before approving a legacy requirements snapshot",
                ));
            }
            let docs =
                workspace(&state.task).requirement_files().map_err(|e| WorkflowError::new(e.to_string(), 500))?;
            if digest != rules::documents_digest(&docs) {
                return Err(WorkflowError::conflict(
                    "Requirements changed since they were displayed; refresh and review again",
                ));
            }
            for name in ["requirements", "acceptanceCriteria", "implementationPlan"] {
                if docs.get(name).is_none_or(|s| s.trim().is_empty()) {
                    return Err(WorkflowError::conflict(format!("A nonempty {name} document is required")));
                }
            }
            if approval.is_some() {
                return Err(WorkflowError::conflict(
                    "Requirements already approved; create a new task for changed scope",
                ));
            }
            let mut row = RequirementsApprovalRow {
                uid: crate::db::new_uid(),
                task_id: state.task.id,
                documents: Json(json!(docs)),
                digest: digest.into(),
                approved_at: DbTime::now(),
            };
            self.db.session(|tx| table::insert(tx, &mut row))?;
            state.set_state(node, NodeState::Completed, Some("Requirements approved"), None);
            if let Some(via) = via {
                state.log(format!("Requirements approved via {via}"), None, "info", Some(json!({"via":via})));
            } else {
                state.info("Requirements approved");
            }
            if !legacy {
                let next = if state.nodes.iter().any(|n| n.key == spec::ORCHESTRATOR) {
                    spec::ORCHESTRATOR
                } else {
                    spec::IMPLEMENTER
                };
                let next = state.node_index(next)?;
                state.nodes[next].iteration = state.task.review_iteration;
                state.set_state(next, NodeState::Ready, None, None);
                self.on_ready(&mut state, next).await?;
            }
        } else if node_key == spec::READY_FOR_PR {
            let drift = self.verify_checkpoint(&state, request.expected_sha.as_deref()).await?;
            let docs =
                workspace(&state.task).requirement_files().map_err(|e| WorkflowError::new(e.to_string(), 500))?;
            rules::check_requirements(approval.as_ref(), &docs).map_err(WorkflowError::conflict)?;
            self.db.session(|tx| state.flush(tx))?;
            let mut blockers = drift.clone();
            blockers.extend(self.current_final_blockers(key)?);
            let reason = request.reason.as_deref().unwrap_or("").trim();
            if !blockers.is_empty() && reason.is_empty() {
                let mut error = WorkflowError::conflict(format!("Override reason required: {}", blockers.join("; ")));
                if !drift.is_empty() {
                    error.code = Some(WorkflowError::WORKTREE_DRIFT);
                }
                return Err(error);
            }
            if !blockers.is_empty() {
                state.log(
                    "Human overrode final blockers",
                    None,
                    "warning",
                    Some(json!({"sha": state.task.current_sha, "reason": reason, "blockers": blockers})),
                );
            }
            state.set_state(node, NodeState::Completed, Some("Ready for PR"), None);
            state.set_status(TaskStatus::ReadyForPr);
            state.task.status_message =
                Some(format!("Ready for PR. Push branch {} and open the pull request.", state.task.branch_name));
            state.info("Task is READY_FOR_PR");
        } else {
            return Err(WorkflowError::conflict(format!("{} cannot be approved", state.nodes[node].label)));
        }
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }

    pub async fn request_requirements_changes(&self, key: &str, feedback: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.status().is_some_and(|s| s == TaskStatus::Cancelled || s.is_post_ready()) {
            return Err(WorkflowError::conflict("Task no longer accepts approvals"));
        }
        let gate = state.node_index(spec::REQUIREMENTS_GATE)?;
        let architect = state.node_index(spec::ARCHITECT)?;
        if state.node_state(gate) != Some(NodeState::WaitingForApproval) {
            return Err(WorkflowError::conflict("Requirements gate is not waiting for approval"));
        }
        if feedback.trim().is_empty() {
            return Err(WorkflowError::bad_request("Feedback is required to send requirements back for revision"));
        }
        if feedback.chars().count() > 4000 {
            return Err(WorkflowError::bad_request("Feedback must be at most 4000 characters"));
        }
        if state.nodes[architect].iteration + 1 > state.task.max_requirements_revisions {
            return Err(WorkflowError::conflict(format!(
                "Maximum requirement revisions reached ({}); approve as-is or edit the documents directly",
                state.task.max_requirements_revisions
            )));
        }
        state.nodes[architect].iteration += 1;
        let round = state.nodes[architect].iteration;
        let path = workspace(&state.task).requirements_dir.join(format!("feedback-{round:02}.md"));
        crate::pyfile::write_text(&path, &(feedback.trim().to_string() + "\n"))
            .map_err(|e| WorkflowError::new(e.to_string(), 500))?;
        state.set_state(gate, NodeState::Pending, Some(&format!("Sent back for revision (round {round})")), None);
        state.set_state(
            architect,
            NodeState::Ready,
            Some(&format!("Revise requirements: {}", feedback.trim().chars().take(200).collect::<String>())),
            None,
        );
        state.log(
            format!("Requirements sent back for revision (round {round}): {}", feedback.trim()),
            Some(spec::REQUIREMENTS_GATE),
            "warning",
            None,
        );
        self.on_ready(&mut state, architect).await?;
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }

    pub(crate) async fn verify_checkpoint(
        &self,
        state: &TaskState,
        expected: Option<&str>,
    ) -> Result<Vec<String>, WorkflowError> {
        let sha = state
            .task
            .current_sha
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WorkflowError::conflict("Checkpoint changed; refresh the task before approving"))?;
        if expected != Some(sha) {
            return Err(WorkflowError::conflict("Checkpoint changed; refresh the task before approving"));
        }
        let path = Path::new(&state.task.worktree_path);
        let head = self.git.head_sha(path).await.map_err(|e| WorkflowError::conflict(e.to_string()))?;
        let paths = self.git.dirty_paths(path, false).await.map_err(|e| WorkflowError::conflict(e.to_string()))?;
        let mut blockers = Vec::new();
        if head != sha {
            blockers.push(format!("Worktree changed after review: new commit(s) since the checkpoint (reviewed {}, worktree is now at {})", sha.chars().take(10).collect::<String>(), head.chars().take(10).collect::<String>()));
        }
        if !paths.is_empty() {
            let names = paths.iter().take(5).map(|p| p.get(3..).unwrap_or(p)).collect::<Vec<_>>().join(", ");
            let extra = if paths.len() > 5 { format!(", +{} more", paths.len() - 5) } else { String::new() };
            blockers.push(format!(
                "Worktree changed after review: {} file(s) touched since the checkpoint ({names}{extra})",
                paths.len()
            ));
        }
        Ok(blockers)
    }

    pub async fn discard_worktree_drift(&self, key: &str, expected: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.task.current_sha.as_deref() != Some(expected) || expected.is_empty() {
            return Err(WorkflowError::conflict("Checkpoint changed; refresh the task before discarding drift"));
        }
        let path = Path::new(&state.task.worktree_path);
        if self.git.head_sha(path).await.map_err(|e| WorkflowError::conflict(e.to_string()))? != expected {
            return Err(WorkflowError::conflict(
                "Worktree has new commits since the checkpoint; create a new checkpoint and review it",
            ));
        }
        let paths = self.git.dirty_paths(path, false).await.map_err(|e| WorkflowError::conflict(e.to_string()))?;
        if paths.is_empty() {
            return Err(WorkflowError::conflict("Worktree already matches the reviewed checkpoint"));
        }
        let untracked = paths.iter().filter(|p| p.starts_with("??")).map(|p| &p[3..]).take(5).collect::<Vec<_>>();
        if !untracked.is_empty() {
            return Err(WorkflowError::conflict(format!(
                "Untracked file(s) appeared since review; remove or commit them manually before retrying: {}",
                untracked.join(", ")
            )));
        }
        self.git.reset_hard(path, expected).await.map_err(|e| WorkflowError::conflict(e.to_string()))?;
        state.log(
            "Human discarded worktree drift to match the reviewed checkpoint",
            None,
            "warning",
            Some(json!({"sha": expected, "paths": paths.iter().map(|p| &p[3..]).collect::<Vec<_>>()})),
        );
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }

    pub async fn override_reviews(&self, key: &str, reason: &str, expected: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.status() != Some(TaskStatus::NeedsHumanReview) || reason.trim().is_empty() {
            return Err(WorkflowError::conflict("Review override requires a stopped review loop and a reason"));
        }
        let mut blockers = self.verify_checkpoint(&state, Some(expected)).await?;
        blockers.extend(self.current_final_blockers(key)?);
        state.log(
            "Human overrode review loop blockers",
            None,
            "warning",
            Some(json!({"sha": state.task.current_sha, "reason": reason.trim(), "blockers": blockers})),
        );
        let implementation = state.node_index(spec::IMPLEMENTER)?;
        state.set_state(
            implementation,
            NodeState::Completed,
            Some("Further fixes waived by human; findings retained"),
            None,
        );
        state.set_status(TaskStatus::Active);
        state.task.status_message = Some("Review blockers overridden; final assessment still required".into());
        let final_node = state.node_index(spec::FINAL_ARCHITECT)?;
        state.nodes[final_node].checkpoint_sha = state.task.current_sha.clone();
        state.nodes[final_node].iteration = state.task.review_iteration;
        state.set_state(final_node, NodeState::Ready, None, None);
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }
}
