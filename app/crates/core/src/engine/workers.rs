//! Durable worker scheduling, called under the task lock.
use super::states::NodeState;
use super::store::TaskState;
use super::work_plans::WorkPlan;
use super::{Engine, WorkflowError, spec};
use crate::db::table::{self, Tracked};
use crate::db::{DbTime, ImplementationWorkItemRow, Json, WorkflowNodeRow, new_uid};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

impl Engine {
    pub(crate) fn work_items(&self, state: &TaskState) -> Result<Vec<ImplementationWorkItemRow>, WorkflowError> {
        Ok(self.db.session(|tx| table::select(tx, "WHERE task_id = ?1 ORDER BY rowid", [state.task.id]))?)
    }
    pub(crate) fn worker_item(
        &self,
        state: &TaskState,
        node: usize,
    ) -> Result<ImplementationWorkItemRow, WorkflowError> {
        self.work_items(state)?
            .into_iter()
            .find(|i| i.node_id == state.nodes[node].id)
            .ok_or_else(|| WorkflowError::conflict("Worker plan is missing"))
    }
    pub(crate) fn worker_capacity(&self, state: &TaskState, node: usize) -> Result<(), WorkflowError> {
        let occupied = state
            .nodes
            .iter()
            .filter(|n| {
                n.key.starts_with("worker_")
                    && n.id != state.nodes[node].id
                    && (NodeState::parse(&n.state).is_some_and(|s| s.is_active() || s == NodeState::Interrupted)
                        // Held for its worktree setup (running or failed): it keeps its slot.
                        || (n.state == NodeState::Ready.as_str()
                            && n.message.as_deref().is_some_and(|m| m.starts_with(super::worktree_setup::WAITING)))
                        || state.agents.iter().any(|a| {
                            a.status == "running"
                                && a.current_run_id
                                    .is_some_and(|id| state.runs.iter().any(|r| r.id == id && r.node_id == n.id))
                        }))
            })
            .count();
        if occupied >= 3 {
            return Err(WorkflowError::conflict("All three worker slots are occupied; wait for a worker to finish"));
        }
        Ok(())
    }
    pub(crate) async fn accept_plan(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), String> {
        // flowctl adds "status": "completed" for every role but the Orchestrator, and the Architect
        // plans new tasks itself; the plan has no status of its own.
        let mut raw = raw.clone();
        if raw.get("status").and_then(Value::as_str) == Some("completed") {
            raw.remove("status");
        }
        let plan: WorkPlan = serde_json::from_value(json!(raw)).map_err(|e| e.to_string())?;
        let plan = plan.validated()?;
        if !self.work_items(state).map_err(|e| e.to_string())?.is_empty() {
            return Err("A plan already exists for this task".into());
        }
        let root = Path::new(&state.task.worktree_path);
        if self.git.is_dirty(root).await.map_err(|e| e.to_string())? {
            return Err("Planning changed the task worktree; inspect it before retrying".into());
        }
        let base = self.git.head_sha(root).await.map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        for entry in &plan.subtasks {
            let uid = new_uid();
            state.add_node(WorkflowNodeRow {
                id: 0,
                task_id: state.task.id,
                key: format!("worker_{uid}"),
                kind: "agent".into(),
                label: entry.title.clone(),
                role: Some("implementer".into()),
                state: "PENDING".into(),
                execution_mode: "automatic".into(),
                iteration: 1,
                checkpoint_sha: None,
                message: None,
                error: None,
                config_override: Json(json!({})),
                updated_at: DbTime::now(),
            });
            items.push(ImplementationWorkItemRow {
                uid: uid.clone(),
                task_id: state.task.id,
                node_id: 0,
                plan_key: entry.key.clone(),
                brief: entry.brief.clone(),
                owned_paths: Json(json!(entry.owned_paths)),
                acceptance_criteria: Json(json!(entry.acceptance_criteria)),
                dependencies: Json(json!(entry.dependencies)),
                worktree_path: crate::paths::py_str(
                    &self.config().worktree_dir(&self.locations).join(format!("{}-worker-{uid}", state.key())),
                ),
                branch_name: format!("agentflow-worker/{uid}"),
                base_sha: base.clone(),
                prepared: false,
            });
        }
        state.runs[run].result = Json(json!(plan));
        TaskState::finish_run(&mut state.runs[run], NodeState::Completed, None);
        state.set_state(node, NodeState::Completed, Some(&plan.summary), None);
        self.db
            .session(|tx| {
                state.flush(tx)?;
                for item in &mut items {
                    item.node_id = state.nodes.iter().find(|n| n.key == format!("worker_{}", item.uid)).unwrap().id;
                    table::insert(tx, item)?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .map_err(|e| e.to_string())?;
        self.dispatch_workers(state).await.map_err(|e| e.to_string())
    }
    pub(crate) async fn prepare_worker(&self, state: &mut TaskState, node: usize) -> Result<(), WorkflowError> {
        self.worker_capacity(state, node)?;
        let items = self.work_items(state)?;
        let mut item = Tracked::loaded(self.worker_item(state, node)?);
        let deps = items
            .iter()
            .filter(|i| item.dependencies.0.as_array().is_some_and(|d| d.contains(&json!(i.plan_key))))
            .collect::<Vec<_>>();
        if deps.iter().any(|i| state.nodes.iter().find(|n| n.id == i.node_id).is_none_or(|n| n.state != "COMPLETED")) {
            return Err(WorkflowError::conflict("Worker dependencies have not completed"));
        }
        let path = PathBuf::from(&item.worktree_path);
        let root = Path::new(&state.task.repository_root);
        let git_error = |e: crate::git::GitError| WorkflowError::conflict(e.to_string());
        if path.exists() {
            self.git.verify_worker_worktree(root, &path, &item.branch_name).await.map_err(git_error)?;
            if item.prepared {
                return Ok(());
            }
            if self.git.is_dirty(&path).await.map_err(git_error)? {
                return Err(WorkflowError::conflict(
                    "Incomplete dependency preparation has local changes; inspect before retrying",
                ));
            }
        } else {
            self.git.create_worker_worktree(root, &path, &item.branch_name, &item.base_sha).await.map_err(git_error)?;
        }
        for dep in deps {
            let sha = state
                .nodes
                .iter()
                .find(|n| n.id == dep.node_id)
                .and_then(|n| n.checkpoint_sha.as_deref())
                .ok_or_else(|| WorkflowError::conflict("Worker dependency has no checkpoint"))?;
            self.git.merge_worker_checkpoint(&path, sha).await.map_err(git_error)?;
        }
        item.base_sha = self.git.head_sha(&path).await.map_err(git_error)?;
        item.prepared = true;
        self.db.session(|tx| item.flush(tx))?;
        Ok(())
    }
    pub(crate) async fn dispatch_workers(&self, state: &mut TaskState) -> Result<(), WorkflowError> {
        let items = self.work_items(state)?;
        if items.is_empty() {
            return Ok(());
        }
        for item in &items {
            let node = state
                .nodes
                .iter()
                .position(|n| n.id == item.node_id)
                .ok_or_else(|| WorkflowError::conflict("Stored worker node missing"))?;
            if state.nodes[node].state != "PENDING" || !self.dependencies_done(state, &items, item) {
                continue;
            }
            if self.worker_capacity(state, node).is_err() {
                break;
            }
            state.set_state(node, NodeState::Ready, None, None);
            self.on_ready(state, node).await?;
            self.db.session(|tx| state.flush(tx))?;
        }
        if items.iter().all(|i| state.nodes.iter().any(|n| n.id == i.node_id && n.state == "COMPLETED")) {
            let integration = state.node_index(spec::IMPLEMENTER)?;
            if state.nodes[integration].state == "PENDING" {
                state.set_state(
                    integration,
                    NodeState::Ready,
                    Some("All workers completed; integrate checkpoints and run combined tests"),
                    None,
                );
                self.on_ready(state, integration).await?;
            }
        }
        Ok(())
    }
    fn dependencies_done(
        &self,
        state: &TaskState,
        items: &[ImplementationWorkItemRow],
        item: &ImplementationWorkItemRow,
    ) -> bool {
        item.dependencies.0.as_array().is_some_and(|d| {
            d.iter().all(|key| {
                items
                    .iter()
                    .find(|i| json!(i.plan_key) == *key)
                    .is_some_and(|i| state.nodes.iter().any(|n| n.id == i.node_id && n.state == "COMPLETED"))
            })
        })
    }
    pub(crate) fn recover_workers(&self, state: &mut TaskState) -> Result<(), WorkflowError> {
        let items = self.work_items(state)?;
        if items.is_empty() {
            return Ok(());
        }
        for item in &items {
            if self.dependencies_done(state, &items, item)
                && let Some(node) = state.nodes.iter().position(|n| n.id == item.node_id && n.state == "PENDING")
            {
                state.set_state(node, NodeState::Ready, Some("Backend restarted; start this worker explicitly"), None);
            }
        }
        let integration = state.node_index(spec::IMPLEMENTER)?;
        if state.nodes[integration].state == "PENDING"
            && items.iter().all(|i| state.nodes.iter().any(|n| n.id == i.node_id && n.state == "COMPLETED"))
        {
            state.set_state(
                integration,
                NodeState::Ready,
                Some("Workers complete; start integration explicitly"),
                None,
            );
        }
        Ok(())
    }
    pub(crate) fn worker_results(
        &self,
        state: &TaskState,
        items: &[ImplementationWorkItemRow],
    ) -> Result<Vec<Value>, WorkflowError> {
        let mut remaining = items.iter().collect::<Vec<_>>();
        let mut done = HashSet::new();
        let mut results = Vec::new();
        while !remaining.is_empty() {
            let ready = remaining
                .iter()
                .copied()
                .filter(|i| {
                    i.dependencies
                        .0
                        .as_array()
                        .is_some_and(|d| d.iter().all(|key| key.as_str().is_some_and(|s| done.contains(s))))
                })
                .collect::<Vec<_>>();
            if ready.is_empty() {
                return Err(WorkflowError::conflict("Stored worker dependencies are invalid"));
            }
            for item in ready {
                let node = state
                    .nodes
                    .iter()
                    .find(|n| n.id == item.node_id)
                    .ok_or_else(|| WorkflowError::conflict("Stored worker node missing"))?;
                let run = state.runs.iter().rev().find(|r| r.node_id == item.node_id);
                results.push(json!({"key":item.plan_key,"sha":node.checkpoint_sha,"summary":run.and_then(|r|r.result.0.get("summary")).unwrap_or(&json!("")),
                    "tests":run.and_then(|r|r.result.0.get("tests")),"knownIssues":run.and_then(|r|r.result.0.get("knownIssues")).unwrap_or(&json!([])),"acceptanceCriteria":item.acceptance_criteria.0}));
                done.insert(item.plan_key.as_str());
                remaining.retain(|i| i.node_id != item.node_id);
            }
        }
        Ok(results)
    }
}
