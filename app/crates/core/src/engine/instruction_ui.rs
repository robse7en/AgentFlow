//! Human-only instruction editing and previews, serialized with existing lifecycle controls.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::lifecycle::git_error;
use super::snapshot::workspace;
use super::states::Role;
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::{ProjectRow, table};
use crate::instructions::{self, Snapshot};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSave {
    pub content: String,
    #[serde(deserialize_with = "super::projects::nullable")]
    pub expected_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyUpdate {
    pub expected_version: String,
    pub expected_source_hashes: BTreeMap<String, Option<String>>,
    pub fresh_conversations: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PreviewQuery {
    pub node_key: Option<String>,
    pub run_id: Option<i64>,
    pub preview: Option<String>,
}

impl Engine {
    fn instruction_project_root(&self, id: i64) -> Result<PathBuf, WorkflowError> {
        let project = self
            .db
            .session(|tx| table::select_one::<ProjectRow>(tx, "WHERE id = ?1", [id]))?
            .ok_or_else(|| WorkflowError::not_found("Project not found"))?;
        project
            .default_repository_path
            .map(PathBuf::from)
            .ok_or_else(|| WorkflowError::conflict("Choose a target repository in project defaults first"))
    }

    pub async fn project_instructions(&self, id: i64) -> Result<Value, WorkflowError> {
        let _held = self.projects_lock.lock().await;
        let root = self.instruction_project_root(id)?;
        let files = instructions::read_set(&root)?;
        Ok(json!({"repository": crate::paths::py_str(&root), "files": files,
            "revision": self.git.head_sha(&root).await.map_err(|e| git_error(e, 409))?,
            "dirtyPaths": self.git.dirty_paths(&root, false).await.map_err(|e| git_error(e, 409))?}))
    }

    pub async fn save_project_instruction(
        &self,
        id: i64,
        role: &str,
        request: SourceSave,
    ) -> Result<Value, WorkflowError> {
        let _held = self.projects_lock.lock().await;
        let root = self.instruction_project_root(id)?;
        Ok(json!(instructions::save_source(&root, role, &request.content, request.expected_hash.as_deref())?))
    }

    pub async fn save_task_instruction(
        &self,
        key: &str,
        role: &str,
        request: SourceSave,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let _held = lock.lock().await;
        let _projects = self.projects_lock.lock().await;
        let state = self.db.session(|tx| TaskState::load(tx, key))?;
        Ok(json!(instructions::save_source(
            Path::new(&state.task.worktree_path),
            role,
            &request.content,
            request.expected_hash.as_deref()
        )?))
    }

    pub async fn task_instructions(&self, key: &str, query: PreviewQuery) -> Result<Value, WorkflowError> {
        if query.preview.as_deref().is_some_and(|p| !["snapshot", "source"].contains(&p)) {
            return Err(WorkflowError::new("preview must be snapshot or source", 422));
        }
        let lock = self.lock(key);
        let _held = lock.lock().await;
        let state = self.db.session(|tx| TaskState::load(tx, key))?;
        let _sources = self.projects_lock.lock().await;
        let task_workspace = workspace(&state.task);
        if let Some(id) = query.run_id {
            let run =
                state.runs.iter().find(|r| r.id == id).ok_or_else(|| WorkflowError::not_found("Run not found"))?;
            let metadata = run.run_dir.as_ref().map(|p| Path::new(p).join("instructions.json"));
            let metadata = match metadata.filter(|p| p.exists()) {
                Some(path) => crate::workspace::read_json(&path).map_err(|e| WorkflowError::conflict(e.to_string()))?,
                None => Value::Null,
            };
            let briefing = run.prompt_path.as_ref().map(fs_read).transpose()?.unwrap_or_default();
            return Ok(json!({"kind": "historical", "runId": id, "metadata": metadata, "briefing": briefing}));
        }
        let index = state.node_index(query.node_key.as_deref().unwrap_or("architect"))?;
        let role =
            state.nodes[index].role.as_deref().ok_or_else(|| WorkflowError::bad_request("Choose an agent node"))?;
        let role_enum = Role::parse(role).ok_or_else(|| WorkflowError::bad_request("Choose an agent role"))?;
        let root = Path::new(&state.task.worktree_path);
        let snapshot = instructions::active_snapshot(&task_workspace.root)?;
        let (files, source_error) = match instructions::read_set(root) {
            Ok(files) => (Some(files), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let source_preview = Snapshot {
            schema: 1,
            version: "preview".into(),
            repository: state.task.worktree_path.clone(),
            revision: "preview".into(),
            fresh_conversations: false,
            files: files.clone().unwrap_or_default(),
        };
        let effective = if query.preview.as_deref() != Some("source") { snapshot.as_ref() } else { None }
            .or_else(|| files.as_ref().map(|_| &source_preview));
        let briefing = match effective {
            Some(effective) => {
                let count = state
                    .runs
                    .iter()
                    .filter(|r| state.nodes.iter().any(|n| n.id == r.node_id && n.role.as_deref() == Some(role)))
                    .count();
                let run_dir = task_workspace.runs_dir.join(role_enum.run_dir()).join(format!("{:02}", count + 1));
                let builtin = self.build_prompt(&state, index, 0, &run_dir)?;
                Some(instructions::compose(&builtin, effective, role, 0, "")?)
            }
            None => None,
        };
        let differs = snapshot.as_ref().and_then(|s| files.as_ref().map(|f| f != &s.files));
        let runs: Vec<Value> =
            state.runs.iter().rev().map(|r| json!({"id": r.id, "nodeKey": state.run_node_key(r)})).collect();
        let dirty = if files.is_some() {
            self.git.dirty_paths(root, false).await.map_err(|e| git_error(e, 409))?
        } else {
            Vec::new()
        };
        Ok(json!({"kind": "preview", "repository": state.task.worktree_path, "files": files,
            "sourceError": source_error, "snapshot": snapshot, "briefing": briefing,
            "dirtyPaths": dirty, "differs": differs, "runs": runs}))
    }

    pub async fn apply_instructions(&self, key: &str, request: ApplyUpdate) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let _held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        let _sources = self.projects_lock.lock().await;
        let directory = workspace(&state.task).root;
        let previous = instructions::active_snapshot(&directory)?
            .filter(|s| s.version == request.expected_version)
            .ok_or_else(|| WorkflowError::conflict("Instruction version changed; reload before applying"))?;
        if !self.work_items(&state)?.is_empty() && state.node("implementer")?.state != "COMPLETED" {
            return Err(WorkflowError::conflict(
                "Finish the parallel batch and integration before updating instructions",
            ));
        }
        let root = Path::new(&state.task.worktree_path);
        let files = instructions::read_set(root)?;
        let hashes: BTreeMap<_, _> = files.iter().map(|(r, f)| (r.clone(), f.hash.clone())).collect();
        if hashes != request.expected_source_hashes {
            return Err(WorkflowError::conflict("Instruction sources changed; reload before applying"));
        }
        let revision = self.git.head_sha(root).await.map_err(|e| git_error(e, 409))?;
        let current = instructions::capture_files(&directory, root, &revision, request.fresh_conversations, files)?;
        state.log(
            "Human applied repository instruction update",
            None,
            "info",
            Some(json!({"previousVersion": previous.version, "version": current.version,
                "freshConversations": request.fresh_conversations})),
        );
        self.db.session(|tx| state.flush(tx))?;
        Ok(json!(current))
    }
}

fn fs_read(path: &String) -> Result<String, WorkflowError> {
    std::fs::read_to_string(path).map_err(|e| WorkflowError::conflict(e.to_string()))
}
