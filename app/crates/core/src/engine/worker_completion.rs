//! Ownership-checked worker checkpoints and the coordinator's integration gate.
use super::result_models::normalize;
use super::states::NodeState;
use super::store::TaskState;
use super::work_plans::owns_path;
use super::{Engine, WorkflowError};
use crate::db::Json;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

impl Engine {
    pub(crate) async fn complete_worker(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), String> {
        let mut result = normalize(raw, "implementation")?;
        let summary = result["summary"].as_str().unwrap_or("").to_string();
        if result["status"] != "completed" {
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(summary.clone()));
            state.set_state(node, NodeState::Failed, Some(&summary), None);
            return Ok(());
        }
        let item = self.worker_item(state, node).map_err(|e| e.to_string())?;
        let path = Path::new(&item.worktree_path);
        self.git
            .verify_worker_worktree(Path::new(&state.task.repository_root), path, &item.branch_name)
            .await
            .map_err(|e| e.to_string())?;
        let (sha, _) = self
            .git
            .checkpoint(path, &format!("AgentFlow worker {}", item.uid), true)
            .await
            .map_err(|e| e.to_string())?;
        let paths = self.git.changed_paths(path, &item.base_sha, &sha).await.map_err(|e| e.to_string())?;
        let outside = paths
            .iter()
            .filter(|p| {
                !item
                    .owned_paths
                    .0
                    .as_array()
                    .is_some_and(|a| a.iter().any(|o| o.as_str().is_some_and(|o| owns_path(o, p))))
            })
            .cloned()
            .collect::<Vec<_>>();
        if !outside.is_empty() {
            return Err(format!("Worker changed files outside its ownership: {}", outside.join(", ")));
        }
        // This worker is blocked inside its completion request; close only its own tracked pane.
        self.stop_sessions(state, Some("implementer"), Some(&[state.nodes[node].id]), false)
            .await
            .map_err(|e| e.to_string())?;
        state.nodes[node].checkpoint_sha = Some(sha.clone());
        result.insert("checkpointSha".into(), json!(sha));
        state.runs[run].result = Json(json!(result));
        if let Some(dir) = &state.runs[run].run_dir {
            crate::workspace::write_json(&Path::new(dir).join("result.json"), &json!(result))
                .map_err(|e| e.to_string())?;
        }
        TaskState::finish_run(&mut state.runs[run], NodeState::Completed, None);
        state.set_state(node, NodeState::Completed, Some(&summary), None);
        self.db.session(|tx| state.flush(tx)).map_err(|e| e.to_string())?;
        self.dispatch_workers(state).await.map_err(|e| e.to_string())
    }
    pub(crate) async fn check_worker_integration(&self, state: &TaskState) -> Result<(), String> {
        for item in self.work_items(state).map_err(|e| e.to_string())? {
            let sha = state
                .nodes
                .iter()
                .find(|n| n.id == item.node_id && n.state == "COMPLETED")
                .and_then(|n| n.checkpoint_sha.as_deref())
                .ok_or("All workers must complete before integration")?;
            if !self
                .git
                .is_ancestor(Path::new(&state.task.worktree_path), sha, "HEAD")
                .await
                .map_err(|e| e.to_string())?
            {
                return Err(format!("Worker checkpoint {} has not been integrated", item.plan_key));
            }
        }
        Ok(())
    }
    pub(crate) fn run_cwd(&self, state: &TaskState, node: usize) -> Result<PathBuf, WorkflowError> {
        if state.nodes[node].key.starts_with("worker_") {
            return Ok(PathBuf::from(self.worker_item(state, node)?.worktree_path));
        }
        Ok(super::snapshot::harness_cwd(&state.task))
    }
}
