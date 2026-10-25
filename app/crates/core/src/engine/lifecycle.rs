//! A task's life outside its agent runs, ported from the engine: repository validation, creation,
//! cancelling, archiving, deletion and worktree cleanup, and stopping a task's agent sessions.
//!
//! Each operation holds the task's lock, works on a [`TaskState`] and flushes it where the Python
//! engine committed. Messages and status codes are Python's (the UI shows them).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::snapshot::harness_cwd;
use super::spec;
use super::states::{NodeState, Role, TaskStatus};
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::table::{self, Tracked};
use crate::db::{
    AgentSessionRow, DbTime, DeletedRunSpendRow, ImplementationWorkItemRow, Json, NodeRunRow, ProjectRow,
    RunEvidenceRow, TaskRow, WorkflowNodeRow,
};
use crate::git::GitError;
use crate::paths::{py_str, resolve, same_path};
use crate::terminal::{PaneInfo, TerminalError};
use crate::workspace::TaskWorkspace;

/// Seconds a just-killed Windows process can keep holding a directory before a retry.
const RETRY_PAUSE: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentOverride {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

impl AgentOverride {
    /// `model_dump(exclude_none=True)`.
    pub fn without_nones(&self) -> Map<String, Value> {
        let mut map = Map::new();
        for (key, value) in [("harness", &self.harness), ("model", &self.model), ("reasoning", &self.reasoning)] {
            if let Some(value) = value {
                map.insert(key.into(), json!(value));
            }
        }
        map
    }
}

/// POST /api/tasks (workflow/models.py CreateTaskRequest).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskRequest {
    #[serde(rename = "implementationMode")]
    pub implementation_mode: Option<String>,
    #[serde(rename = "taskSource", default = "default_task_source")]
    pub task_source: String,
    pub prompt: Option<String>,
    #[serde(rename = "jiraKey", default)]
    pub jira_key: String,
    #[serde(rename = "jiraTitle")]
    pub jira_title: Option<String>,
    #[serde(rename = "repositoryPath")]
    pub repository_path: String,
    #[serde(rename = "projectRelativePath")]
    pub project_relative_path: Option<String>,
    #[serde(rename = "baseBranch")]
    pub base_branch: String,
    #[serde(rename = "additionalContext")]
    pub additional_context: Option<String>,
    #[serde(rename = "agentOverrides", default)]
    pub agent_overrides: indexmap::IndexMap<String, AgentOverride>,
    #[serde(rename = "projectId", default)]
    pub project_id: Option<i64>,
}

fn default_task_source() -> String {
    "jira".into()
}

/// Local task identifiers occupy a reserved UUID namespace.
pub fn is_local_task(key: &str) -> bool {
    key.strip_prefix("LOCAL-").is_some_and(|uid| uuid::Uuid::parse_str(uid).is_ok())
}

impl CreateTaskRequest {
    /// Pydantic's field constraints and validators: the key is trimmed and upper-cased and may hold
    /// only letters, digits, '-' and '_'; a blank title is none.
    pub fn validated(mut self) -> Result<Self, String> {
        if self.task_source != "jira" && self.task_source != "local" {
            return Err("taskSource: must be 'jira' or 'local'".into());
        }
        if self.prompt.as_ref().is_some_and(|p| p.chars().count() > 100_000) {
            return Err("prompt: at most 100000 characters".into());
        }
        if self.task_source == "local" {
            let prompt =
                self.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty()).ok_or("prompt: must not be empty")?;
            self.jira_key = format!("LOCAL-{}", uuid::Uuid::new_v4()).to_uppercase();
            if self.jira_title.as_deref().is_none_or(|s| s.trim().is_empty()) {
                self.jira_title = Some(prompt.lines().next().unwrap_or(prompt).chars().take(120).collect());
            }
            self.additional_context = Some(match self.additional_context.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(context) => format!("{prompt}\n\n{context}"),
                None => prompt.to_owned(),
            });
        } else if is_local_task(&self.jira_key.trim().to_uppercase()) {
            return Err("jiraKey: reserved for local tasks".into());
        }
        if self.jira_key.is_empty() || self.jira_key.chars().count() > 64 {
            return Err("jiraKey: must be 1 to 64 characters".into());
        }
        let key = self.jira_key.trim().to_uppercase();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err("jiraKey: Jira key may only contain letters, digits, '-' and '_'".into());
        }
        self.jira_key = key;
        if self.jira_title.as_ref().is_some_and(|t| t.chars().count() > 500) {
            return Err("jiraTitle: at most 500 characters".into());
        }
        self.jira_title = self.jira_title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        if self.base_branch.is_empty() {
            return Err("baseBranch: must not be empty".into());
        }
        if let Some(mode) = &self.implementation_mode
            && mode != "direct"
            && mode != "parallel"
        {
            return Err("implementationMode: must be 'direct' or 'parallel'".into());
        }
        Ok(self)
    }
}

/// `relative` as a POSIX path inside the repository at `root`; None for the root itself.
pub(crate) fn working_dir(root: &Path, relative: Option<&str>) -> Result<Option<String>, WorkflowError> {
    let Some(relative) = relative.filter(|p| !p.is_empty()) else { return Ok(None) };
    let candidate = resolve(&root.join(relative));
    if !candidate.is_dir() {
        return Err(WorkflowError::bad_request(format!("Working directory does not exist: {}", py_str(&candidate))));
    }
    let relative = candidate
        .strip_prefix(resolve(root))
        .map_err(|_| WorkflowError::bad_request("Working directory must be inside the repository"))?;
    let relative = crate::harness::as_posix(relative);
    Ok((relative != ".").then_some(relative))
}

pub(crate) fn git_error(error: GitError, status: u16) -> WorkflowError {
    let code = matches!(error, GitError::Unverified(_)).then_some(WorkflowError::BRANCH_UNVERIFIED);
    WorkflowError { code, ..WorkflowError::new(error.to_string(), status) }
}

/// Whether `path` is a git checkout of its own. A leftover directory whose `.git` is gone isn't:
/// asking git there would answer for whatever repository encloses it (the data directory can sit
/// inside one), and the previews would list that repository's files as ones about to be discarded.
fn is_checkout(path: &Path) -> bool {
    path.join(".git").exists()
}

fn stop_error(error: impl std::fmt::Display) -> WorkflowError {
    WorkflowError::conflict(format!("Could not confirm agent stopped: {error}. Retry cancellation."))
}

impl Engine {
    // ------------------------------------------------------------------------------ creation

    pub async fn validate_repository(
        &self,
        path: &str,
        base_branch: Option<&str>,
        project_relative_path: Option<&str>,
    ) -> Result<Value, GitError> {
        let info = self.git.inspect_repository(path).await?;
        let mut checks = vec![
            json!({"ok": true, "message": "Git repository detected"}),
            json!({"ok": true, "message": format!("Root: {}", info.root)}),
        ];
        let mut ok = true;
        if let Some(base) = base_branch.filter(|b| !b.is_empty()) {
            // Runs on every keystroke pause in the New Task form: only a name unknown locally (new on
            // the remote, or a typo) is worth a fetch. Creation resolves the ref again anyway.
            if info.branches.iter().any(|b| b == base) {
                checks.push(json!({"ok": true, "message": format!("Base branch: {base}")}));
            } else {
                match self.git.resolve_base_ref(Path::new(&info.root), base).await {
                    Ok(_) => checks.push(json!({"ok": true, "message": format!("Base branch: {base} (on origin)")})),
                    Err(error) => {
                        ok = false;
                        checks.push(json!({"ok": false, "message": error.to_string()}));
                    }
                }
            }
        }
        if let Some(project) = project_relative_path.filter(|p| !p.is_empty()) {
            let root = resolve(Path::new(&info.root));
            let candidate = resolve(&root.join(project));
            if !candidate.is_dir() || !crate::paths::is_within(&candidate, &root) {
                ok = false;
                checks.push(
                    json!({"ok": false, "message": format!("Working directory not inside repository: {project}")}),
                );
            } else {
                checks.push(json!({"ok": true, "message": format!("Working directory: {project}")}));
            }
        }
        Ok(json!({"ok": ok, "root": info.root, "currentBranch": info.current_branch, "branches": info.branches,
                  "checks": checks}))
    }

    pub async fn create_task(&self, request: CreateTaskRequest) -> Result<Value, WorkflowError> {
        let key = request.jira_key.clone();
        let lock = self.lock(&key);
        let _held = lock.lock().await;
        let config = self.config();
        let existing: Option<TaskRow> = self.db.session(|tx| table::select_one(tx, "WHERE jira_key = ?1", [&key]))?;
        if let Some(existing) = existing {
            return Err(WorkflowError::conflict(if existing.archived_at.is_some() {
                format!("A task for {key} exists but is archived. Unarchive or delete it first.")
            } else {
                format!("A task for {key} already exists. Open it from the task list.")
            }));
        }
        if let Some(project_id) = request.project_id {
            self.db.session(|tx| self.assignable_project(tx, project_id))?;
        }
        self.step(&key, "Checking the repository");
        let info = self.git.inspect_repository(&request.repository_path).await.map_err(|e| git_error(e, 400))?;
        let root = PathBuf::from(&info.root);
        let project_rel = working_dir(&root, request.project_relative_path.as_deref())?;

        let at = &self.locations;
        let branch = format!("{}{key}", config.workspace.branch_prefix);
        let worktree_path = config.worktree_dir(at).join(&key);
        // create_worktree fetches origin and resolves the base ref itself (never a local base that
        // is behind origin).
        self.step(&key, &format!("Fetching {} and creating the worktree", request.base_branch));
        let created = self
            .git
            .create_worktree(&root, &worktree_path, &branch, &request.base_branch)
            .await
            .map_err(|e| git_error(e, 409))?;
        let branch = created.branch;
        let zellij_session = format!("{}{key}", config.zellij.session_prefix);
        let workspace = TaskWorkspace::new(&config.task_dir(at), &key);
        let overrides: Map<String, Value> = request
            .agent_overrides
            .iter()
            .map(|(role, o)| (role.clone(), o.without_nones()))
            .filter(|(_, o)| !o.is_empty())
            .map(|(role, o)| (role, Value::Object(o)))
            .collect();
        let planner = spec::planner_role(&config, &overrides);
        let now = DbTime::now();
        let meta = json!({
            "jiraKey": key, "repositoryPath": info.path, "repositoryRoot": info.root,
            "projectRelativePath": project_rel, "baseBranch": request.base_branch, "branchName": branch,
            "worktreePath": py_str(&worktree_path), "zellijSession": zellij_session, "agentOverrides": overrides,
            "createdAt": now.isoformat(),
        });
        workspace
            .create(&meta, request.additional_context.as_deref())
            .map_err(|e| WorkflowError::new(format!("Could not create the task folder: {e}"), 500))?;

        let jira_sync = if is_local_task(&key) {
            None
        } else {
            match self.jira_service() {
                Ok(Some(service)) => {
                    self.step(&key, "Syncing the Jira ticket");
                    Some(service.sync_ticket(&key, &workspace.context_dir, true).await)
                }
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        };
        let jira_title = request
            .jira_title
            .clone()
            .or_else(|| jira_sync.as_ref().and_then(|r| r.as_ref().ok()).and_then(|r| r.title.clone()));
        // Checked again because the worktree already exists: a project deleted meanwhile leaves
        // the task without one rather than without a task row. Held until the row is written.
        let projects_held = self.projects_lock.lock().await;
        let project_id = match request.project_id {
            Some(id) => self.db.session(|tx| table::select_one::<ProjectRow>(tx, "WHERE id = ?1", [id]))?.map(|p| p.id),
            None => None,
        };
        let mut state = TaskState::new(TaskRow {
            id: 0,
            jira_key: key.clone(),
            api_budget_usd: None,
            jira_title,
            repository_path: info.path.clone(),
            repository_root: info.root.clone(),
            project_relative_path: project_rel,
            base_branch: request.base_branch.clone(),
            branch_name: branch.clone(),
            worktree_path: py_str(&worktree_path),
            zellij_session_name: zellij_session.clone(),
            task_dir: py_str(&workspace.root),
            additional_context: request.additional_context.clone(),
            status: TaskStatus::Active.as_str().into(),
            // Shown on the task until the first agent run replaces it; the event below stays.
            status_message: created.base_warning.clone(),
            review_iteration: 1,
            max_review_iterations: config.workflow.max_review_iterations,
            max_requirements_revisions: config.workflow.max_requirements_revisions,
            current_sha: None,
            pr_url: None,
            pr_created_at: None,
            merged_at: None,
            pr_sync_paused: false,
            merged_head_sha: None,
            pr_base_branch: None,
            archived_at: None,
            project_id,
            agent_overrides: Json(Value::Object(overrides)),
            created_at: now,
            updated_at: now,
        });
        let direct = request
            .implementation_mode
            .as_deref()
            .map_or(config.workflow.implementation_mode == crate::config::ImplementationMode::Direct, |mode| {
                mode == "direct"
            });
        for node in spec::nodes(&config) {
            if node.key == spec::ORCHESTRATOR && direct {
                continue;
            }
            let (label, role) = if node.key == spec::ORCHESTRATOR && planner == Role::Architect {
                (spec::ARCHITECT_PLANNING_LABEL, Some(Role::Architect))
            } else {
                (node.label, node.role)
            };
            state.add_node(WorkflowNodeRow {
                id: 0,
                task_id: 0,
                key: node.key.into(),
                kind: node.kind.as_str().into(),
                label: label.into(),
                role: role.map(|r| r.as_str().into()),
                state: if node.key == spec::ARCHITECT { NodeState::Ready } else { NodeState::Pending }.as_str().into(),
                execution_mode: spec::mode_str(node.execution_mode),
                iteration: 0,
                checkpoint_sha: None,
                message: None,
                error: None,
                config_override: Json(json!({})),
                updated_at: now,
            });
        }
        state.log(
            "Task created",
            None,
            "info",
            Some(json!({"repository": info.root, "baseBranch": request.base_branch})),
        );
        state.info(format!("Worktree created: {} on branch {branch}", py_str(&worktree_path)));
        if project_id != request.project_id {
            state.log(
                "The chosen project was deleted while the task was being created, so the task has no project",
                None,
                "warning",
                None,
            );
        }
        if let Some(warning) = &created.base_warning {
            state.log(warning.clone(), None, "warning", Some(json!({"baseBranch": request.base_branch})));
        }
        if let Some(result) = jira_sync {
            match result {
                Ok(result) => self.log_jira_sync(&mut state, &result),
                Err(error) => state.log(format!("Could not sync Jira ticket context: {error}"), None, "warning", None),
            }
        }
        self.db.session(|tx| state.flush(tx))?;
        drop(projects_held);

        self.step(&key, "Opening the terminal session");
        match self.open_task_session(&state.task).await {
            Ok(true) => state.info(format!("Zellij session created: {zellij_session}")),
            Ok(false) => state.log(format!("Zellij session already existed: {zellij_session}"), None, "warning", None),
            // Task creation still succeeds; the session is retried on the first agent launch.
            Err(error) => state.log(format!("Zellij session could not be created yet: {error}"), None, "error", None),
        }
        // In the background, while the Architect works; later agents wait for it.
        self.start_task_setup(&mut state);
        self.db.session(|tx| state.flush(tx))?;
        drop(_held);
        self.publish(&key)
    }

    /// Report a long request's current step to the toast that started it (see `Broadcaster::progress`).
    fn step(&self, jira_key: &str, message: &str) {
        self.broadcaster.progress(Some(jira_key), message);
    }

    /// The task's terminal session, with its Tests and Shell tabs when newly created.
    async fn open_task_session(&self, task: &TaskRow) -> Result<bool, TerminalError> {
        let created = self.terminals.ensure_session(&task.zellij_session_name).await?;
        if created {
            let cwd = harness_cwd(task);
            for name in ["Tests", "Shell"] {
                self.terminals.new_tab(&task.zellij_session_name, name, &cwd, None).await?;
            }
        }
        Ok(created)
    }

    // ------------------------------------------------------------------------------ cancel and archive

    pub async fn cancel_task(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        self.cancel_worktree_setups(jira_key);
        if let Some(cancel) = self.verifications.lock().unwrap_or_else(|p| p.into_inner()).get(&jira_key.to_uppercase())
        {
            cancel.send_replace(true);
        }
        let lock = self.lock(jira_key);
        // Keep signalling while awaiting the task lock: completion may register verification
        // just after cancellation first looked it up. Do not wait five minutes in that race.
        let held = loop {
            if let Some(cancel) =
                self.verifications.lock().unwrap_or_else(|p| p.into_inner()).get(&jira_key.to_uppercase())
            {
                cancel.send_replace(true);
            }
            tokio::select! {
                held = lock.lock() => break held,
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
        };
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        // Ready for PR / PR Created stay cancellable so an abandoned PR can be retired (then
        // archived or deleted); only a merged task is past cancelling.
        if state.status() == Some(TaskStatus::Merged) {
            return Err(WorkflowError::conflict(format!("Task already reached {}", state.task.status)));
        }
        self.stop_sessions(&mut state, None, None, true).await?;
        state.set_status(TaskStatus::Cancelled);
        state.task.status_message = Some("Cancelled by user".into());
        let stoppable = [
            NodeState::Pending,
            NodeState::Ready,
            NodeState::Running,
            NodeState::WaitingForUser,
            NodeState::WaitingForApproval,
        ];
        for index in 0..state.nodes.len() {
            if state.node_state(index).is_some_and(|s| stoppable.contains(&s)) {
                state.set_state(index, NodeState::Cancelled, None, None);
            }
        }
        for run in &mut state.runs {
            if NodeState::parse(&run.state).is_some_and(NodeState::is_active) {
                TaskState::finish_run(run, NodeState::Cancelled, None);
            }
        }
        state.log(
            "Task cancelled; agent tabs stopped. Worktree, branch and artifacts preserved.",
            None,
            "warning",
            None,
        );
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    pub async fn archive_task(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        if !state.status().is_some_and(TaskStatus::is_finished) {
            return Err(WorkflowError::conflict(format!(
                "Only a merged or cancelled task can be archived (currently {})",
                state.task.status
            )));
        }
        if state.task.archived_at.is_none() {
            state.task.archived_at = Some(DbTime::now());
            state.info("Task archived");
        }
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    pub async fn unarchive_task(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        if state.task.archived_at.is_some() {
            state.task.archived_at = None;
            state.info("Task unarchived");
        }
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    // ------------------------------------------------------------------------------ stopping agents

    /// Close the matching running agents' tabs (`_stop_sessions`). An agent whose tab is gone is
    /// marked stopped; one whose tab identity is unknown is matched by tab name. With `graceful`,
    /// idle agents whose CLI writes its final usage only on a normal exit are asked to exit first,
    /// and that usage is collected.
    pub async fn stop_sessions(
        &self,
        state: &mut TaskState,
        role: Option<&str>,
        node_ids: Option<&[i64]>,
        graceful: bool,
    ) -> Result<(), WorkflowError> {
        let session = state.task.zellij_session_name.clone();
        let mut open: Vec<usize> = Vec::new();
        for index in 0..state.agents.len() {
            let agent = &state.agents[index];
            if agent.status != "running" || role.is_some_and(|r| agent.role != r) {
                continue;
            }
            if let Some(node_ids) = node_ids {
                let run = agent.current_run_id.and_then(|id| state.runs.iter().find(|r| r.id == id));
                if !run.is_some_and(|r| node_ids.contains(&r.node_id)) {
                    continue;
                }
            }
            if self.terminals.session_exists(&session).await.map_err(stop_error)? {
                let panes = self.terminals.list_panes(&session).await.map_err(stop_error)?;
                if state.agents[index].zellij_tab_id.is_none_or(|id| id < 0) {
                    match untracked_tab(&state.agents, index, &panes).map_err(stop_error)? {
                        None => {
                            self.mark_stopped(&mut state.agents[index]);
                            continue;
                        }
                        Some(pane) => {
                            state.agents[index].zellij_tab_id = Some(pane.tab_id);
                            state.agents[index].zellij_pane_id = Some(pane.pane_id.clone());
                        }
                    }
                }
                if panes.iter().any(|p| Some(p.tab_id) == state.agents[index].zellij_tab_id) {
                    open.push(index);
                    continue;
                }
            }
            self.mark_stopped(&mut state.agents[index]);
        }
        if graceful && self.exit_for_usage(state, &open).await? {
            let panes = self.terminals.list_panes(&session).await.map_err(stop_error)?;
            open.retain(|&index| panes.iter().any(|p| Some(p.tab_id) == state.agents[index].zellij_tab_id));
        }
        for index in open {
            let tab = state.agents[index].zellij_tab_id.unwrap_or(-1);
            self.terminals.close_tab(&session, tab).await.map_err(stop_error)?;
            self.mark_stopped(&mut state.agents[index]);
        }
        Ok(())
    }

    pub(crate) fn mark_stopped(&self, agent: &mut Tracked<AgentSessionRow>) {
        agent.status = "stopped".into();
        agent.ended_at = Some(DbTime::now());
        self.screen_trackers.cancel(agent.id);
    }

    /// Align agent rows with the panes positively identified by id before cleanup
    /// (`_reconcile_agent_pane_status`), so cleanup neither removes a worktree out from under a
    /// live process nor blocks on a dead one.
    async fn reconcile_agent_pane_status(&self, state: &mut TaskState) -> Result<(), WorkflowError> {
        if state.agents.is_empty() {
            return Ok(());
        }
        let session = state.task.zellij_session_name.clone();
        let verify = |e: TerminalError| WorkflowError::conflict(format!("Cannot verify agents have stopped: {e}"));
        let live = if self.terminals.session_exists(&session).await.map_err(verify)? {
            self.terminals.list_panes(&session).await.map_err(verify)?
        } else {
            Vec::new()
        };
        for agent in &mut state.agents {
            let Some(pane_id) = agent.zellij_pane_id.clone() else { continue };
            match live.iter().find(|p| !p.exited && p.pane_id == pane_id) {
                Some(pane) => {
                    agent.status = "running".into();
                    agent.zellij_tab_id = Some(pane.tab_id);
                }
                None if agent.status == "running" => {
                    agent.status = "stopped".into();
                    agent.ended_at = Some(DbTime::now());
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Refuse cleanup while a run is genuinely in flight; otherwise force-close the task's agent
    /// tabs still open (`_check_cleanup_agents`).
    async fn check_cleanup_agents(&self, state: &mut TaskState) -> Result<(), WorkflowError> {
        let busy = [NodeState::Running, NodeState::WaitingForUser, NodeState::Interrupted];
        let in_busy = |s: &str| NodeState::parse(s).is_some_and(|s| busy.contains(&s));
        if state.nodes.iter().any(|n| in_busy(&n.state)) || state.runs.iter().any(|r| in_busy(&r.state)) {
            return Err(WorkflowError::conflict("An agent run is still active or awaiting recovery"));
        }
        self.reconcile_agent_pane_status(state).await?;
        self.stop_sessions(state, None, None, true).await
    }

    // ------------------------------------------------------------------------------ worktree cleanup

    /// Read-only preview of what a cleanup would discard or close, for the confirm dialog.
    pub async fn worktree_cleanup_preview(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let _held = lock.lock().await;
        let (state, items) = self.load_with_items(jira_key)?;
        if state.status() != Some(TaskStatus::Merged) {
            return Err(WorkflowError::conflict("Cleanup requires the task to be marked Merged"));
        }
        let target = PathBuf::from(&state.task.worktree_path);
        // Modified/untracked only; ignored files can number in the tens of thousands, so cleanup
        // discards them silently instead.
        let dirty = if is_checkout(&target) {
            self.git.dirty_paths(&target, false).await.map_err(|e| git_error(e, 409))?
        } else {
            Vec::new()
        };
        let agents: Vec<&AgentSessionRow> = state.agents.iter().map(|a| &a.row).collect();
        let running =
            || agents.iter().filter(|a| a.status == "running").map(|a| a.zellij_tab_name.clone()).collect::<Vec<_>>();
        // Cleanup kills the task's whole terminal session, so its other tabs are reported too.
        let session = &state.task.zellij_session_name;
        let (session_will_be_closed, open_panes, other_open_tabs) = match self.terminals.session_exists(session).await {
            Ok(true) => match self.terminals.list_panes(session).await {
                Ok(live) => {
                    let open_live: Vec<&PaneInfo> = live.iter().filter(|p| !p.exited).collect();
                    let open_ids: Vec<&str> = open_live.iter().map(|p| p.pane_id.as_str()).collect();
                    let open_panes: Vec<String> = agents
                        .iter()
                        .filter(|a| match &a.zellij_pane_id {
                            Some(id) => open_ids.contains(&id.as_str()),
                            None => a.status == "running",
                        })
                        .map(|a| a.zellij_tab_name.clone())
                        .collect();
                    let tracked: Vec<&str> = agents.iter().filter_map(|a| a.zellij_pane_id.as_deref()).collect();
                    let mut others: Vec<String> = open_live
                        .iter()
                        .filter(|p| !tracked.contains(&p.pane_id.as_str()))
                        .map(|p| p.tab_name.clone())
                        .collect();
                    others.sort();
                    others.dedup();
                    (true, open_panes, others)
                }
                Err(_) => (false, running(), Vec::new()),
            },
            Ok(false) => (false, if agents.is_empty() { Vec::new() } else { running() }, Vec::new()),
            Err(_) => (false, running(), Vec::new()),
        };
        let worker_worktrees: Vec<&String> =
            items.iter().map(|i| &i.worktree_path).filter(|p| Path::new(p).exists()).collect();
        Ok(json!({
            "dirtyPaths": dirty.iter().map(|p| p.get(3..).unwrap_or_default()).collect::<Vec<_>>(),
            "openAgentPanes": open_panes,
            "workerWorktrees": worker_worktrees,
            "sessionWillBeClosed": session_will_be_closed,
            "otherOpenTabs": other_open_tabs,
        }))
    }

    /// Close the task's terminal session whole, then the Zellij session it left from a run before
    /// agents moved to embedded terminals (if any). Only the first is required: the second isn't
    /// where agents run now, and once exited it holds no directory open.
    async fn close_task_sessions(&self, state: &mut TaskState) -> Result<(), WorkflowError> {
        let session = state.task.zellij_session_name.clone();
        self.terminals
            .kill_session(&session)
            .await
            .map_err(|e| WorkflowError::conflict(format!("Could not close the task's Zellij session: {e}")))?;
        if let Some(zellij) = &self.leftover_zellij
            && let Err(error) = zellij.kill_session(&session).await
        {
            tracing::warn!("[{}] Could not close the leftover Zellij session: {error}", state.key());
            state.log(format!("Could not close the task's leftover Zellij session: {error}"), None, "warning", None);
        }
        Ok(())
    }

    /// `force` skips the check that the task branch is the reviewed checkpoint or in a merged pull
    /// request, for a task merged somewhere AgentFlow can't see (a local branch). The branch is kept.
    pub async fn cleanup_worktree(
        &self,
        jira_key: &str,
        expected_sha: &str,
        force: bool,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let config = self.config();
        let (mut state, items) = self.load_with_items(jira_key)?;
        if state.status() != Some(TaskStatus::Merged) {
            return Err(WorkflowError::conflict("Cleanup requires the task to be marked Merged"));
        }
        if expected_sha.is_empty() || state.task.current_sha.as_deref() != Some(expected_sha) {
            return Err(WorkflowError::conflict("Final checkpoint changed; refresh before cleaning up"));
        }
        let target = PathBuf::from(&state.task.worktree_path);
        let worktree_dir = config.worktree_dir(&self.locations);
        let expected = worktree_dir.join(&state.task.jira_key);
        if !same_path(&target, &expected) || target.is_symlink() {
            return Err(WorkflowError::conflict("Worktree is outside the configured task location"));
        }
        if !resolve(&target).parent().is_some_and(|p| same_path(p, &worktree_dir)) {
            return Err(WorkflowError::conflict("Worktree must be directly inside the managed worktree directory"));
        }
        self.step(jira_key, "Stopping agent tabs");
        if let Err(error) = self.check_cleanup_agents(&mut state).await {
            return Err(self.cleanup_failed(&mut state, error, "stopping agent tabs", force, json!({})));
        }
        // The tabs just closed can't be reopened: keep their "stopped" rows even if a git step fails.
        self.db.session(|tx| state.flush(tx))?;
        // The task's terminal session can still hold panes cleanup never tracks (the Tests/Shell
        // tabs, cwd inside the worktree), which lock the directory on Windows: close it whole.
        self.step(jira_key, "Closing the terminal session");
        if let Err(error) = self.close_task_sessions(&mut state).await {
            return Err(self.cleanup_failed(&mut state, error, "closing the terminal session", force, json!({})));
        }
        let root = PathBuf::from(&state.task.repository_root);
        // Recorded before removal so the log names the unverified tip the human vouched for.
        let forced_tip =
            if force { self.git.branch_sha(&root, &state.task.branch_name).await.ok().flatten() } else { None };
        let mut discarded = Vec::new();
        let mut failure = None;
        self.step(jira_key, "Removing the worktree");
        for attempt in 0..2 {
            let result = self
                .git
                .cleanup_worktree(
                    &root,
                    &target,
                    &state.task.branch_name,
                    expected_sha,
                    Some(&worktree_dir),
                    state.task.merged_head_sha.as_deref(),
                    force,
                )
                .await;
            match result {
                Ok(paths) => {
                    discarded = paths;
                    failure = None;
                    break;
                }
                // Waiting can't make the branch reviewed or merged.
                Err(error @ GitError::Unverified(_)) => {
                    failure = Some(error);
                    break;
                }
                Err(error) => {
                    if attempt == 0 {
                        if matches!(error, GitError::Busy(_)) {
                            self.step(jira_key, "Worktree is in use; closing processes still running in it");
                            self.stop_worktree_processes(&mut state).await?;
                        }
                        tokio::time::sleep(RETRY_PAUSE).await;
                        self.step(jira_key, "Retrying the worktree removal");
                    }
                    failure = Some(error);
                }
            }
        }
        if let Some(error) = failure {
            // A directory that can't be deleted is usually held open by something AgentFlow won't
            // close for the human (an editor with the folder open): name it.
            let holders = if matches!(error, GitError::Busy(_)) {
                self.worktree_killer.holders(&target).await.unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut error = git_error(error, 409);
            let data =
                json!({"holders": holders.iter().map(|h| json!({"pid": h.pid, "name": h.name})).collect::<Vec<_>>()});
            if !holders.is_empty() {
                let names: Vec<String> = holders.iter().map(|h| format!("{} ({})", h.name, h.pid)).collect();
                let them = if holders.len() == 1 { "it" } else { "them" };
                error.message =
                    format!("{} (still open in {}: close {them} and try again)", error.message, names.join(", "));
            }
            return Err(self.cleanup_failed(&mut state, error, "removing the worktree", force, data));
        }
        // Worker worktrees are disposable by Merged; a failure here doesn't undo the task's cleanup.
        let (mut removed, mut failed) = (Vec::new(), Vec::new());
        if !items.is_empty() {
            self.step(jira_key, &format!("Removing {} worker worktree(s)", items.len()));
        }
        for item in &items {
            match self.git.discard_worktree(&root, Path::new(&item.worktree_path)).await {
                Ok(_) => removed.push(item.worktree_path.clone()),
                Err(error) => failed.push(format!("{}: {error}", item.worktree_path)),
            }
        }
        let names: Vec<&str> = discarded.iter().map(|p| p.get(3..).unwrap_or_default()).collect();
        let mut first = "Worktree cleaned up".to_string();
        if !discarded.is_empty() {
            let more = if discarded.len() > 5 { format!(", +{} more", discarded.len() - 5) } else { String::new() };
            first += &format!(
                "; discarded {} uncommitted file(s) ({}{more})",
                discarded.len(),
                names[..names.len().min(5)].join(", ")
            );
        }
        first.push('.');
        let mut parts = vec![first];
        parts.push(format!(
            "Ignored build/dependency output, if any, was cleared too{}",
            if discarded.is_empty() { "." } else { " (not listed)." }
        ));
        if !removed.is_empty() {
            parts.push(format!("Also removed {} worker worktree(s).", removed.len()));
        }
        if !failed.is_empty() {
            let more = if failed.len() > 3 { format!(", +{} more", failed.len() - 3) } else { String::new() };
            parts.push(format!(
                "Could not remove {} worker worktree(s): {}{more}.",
                failed.len(),
                failed[..failed.len().min(3)].join("; ")
            ));
        }
        parts.push("Branch and task artifacts preserved.".into());
        let message = parts.join(" ");
        state.task.status_message = Some(message.clone());
        if force {
            state.log(
                "Worktree cleanup forced past the reviewed/merged branch check",
                None,
                "warning",
                Some(
                    json!({"sha": expected_sha, "branchSha": forced_tip, "mergedHeadSha": state.task.merged_head_sha}),
                ),
            );
        }
        let level = if !discarded.is_empty() || !failed.is_empty() { "warning" } else { "info" };
        state.log(
            message,
            None,
            level,
            Some(json!({"sha": expected_sha, "discardedPaths": names, "workerWorktreesRemoved": removed,
                        "workerWorktreeFailures": failed})),
        );
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    /// Record why a cleanup stopped on the task itself: the status message (the task page shows it
    /// until a cleanup succeeds) and a warning event. Committed at once, since the error returns
    /// past the flush that would otherwise lose it. The error comes back unchanged.
    fn cleanup_failed(
        &self,
        state: &mut TaskState,
        error: WorkflowError,
        step: &str,
        force: bool,
        mut data: Value,
    ) -> WorkflowError {
        let message = format!("Worktree cleanup failed while {step}: {}", error.message);
        if let Some(object) = data.as_object_mut() {
            object.insert("step".into(), json!(step));
            object.insert("force".into(), json!(force));
            object.insert("code".into(), json!(error.code));
        }
        crate::diagnostics::event(
            "worktree.cleanup.failed",
            json!({"path": state.task.worktree_path, "step": step, "force": force, "code": error.code}),
        );
        state.task.status_message = Some(message.clone());
        state.log(message, None, "warning", Some(data));
        if let Err(flush) = self.db.session(|tx| state.flush(tx)) {
            tracing::warn!("[{}] Could not record the failed cleanup: {flush}", state.key());
        }
        error
    }

    /// Kill whatever still works inside the worktree once cleanup couldn't delete the directory
    /// (e.g. a shell the human started there). Committed at once: the kills can't be undone.
    async fn stop_worktree_processes(&self, state: &mut TaskState) -> Result<(), WorkflowError> {
        let stopped = match self.worktree_killer.stop_processes_in(Path::new(&state.task.worktree_path)).await {
            Ok(stopped) => stopped,
            Err(error) => {
                tracing::warn!("[{}] Could not look for processes in the worktree: {error}", state.key());
                return Ok(());
            }
        };
        if stopped.is_empty() {
            return Ok(());
        }
        let names: Vec<String> = stopped.iter().map(|p| format!("{} ({})", p.name, p.pid)).collect();
        let processes: Vec<Value> =
            stopped.iter().map(|p| json!({"pid": p.pid, "name": p.name, "cwd": p.cwd})).collect();
        state.log(
            format!("Closed {} process(es) still running in the worktree: {}", stopped.len(), names.join(", ")),
            None,
            "warning",
            Some(json!({"processes": processes})),
        );
        self.db.session(|tx| state.flush(tx))?;
        Ok(())
    }

    // ------------------------------------------------------------------------------ deletion

    fn load_with_items(&self, jira_key: &str) -> Result<(TaskState, Vec<ImplementationWorkItemRow>), WorkflowError> {
        self.db.session(|tx| {
            let state = TaskState::load(tx, jira_key)?;
            let items = table::select(tx, "WHERE task_id = ?1", [state.task.id])?;
            Ok((state, items))
        })
    }

    /// Today's runs of this task whose cost is unknown, when a daily limit would miss them.
    fn unknown_cost_blocker(&self, state: &TaskState) -> Result<i64, WorkflowError> {
        let config = self.config();
        let controls = &config.workflow.cost_controls;
        if controls.daily_budget_usd.is_none() {
            return Ok(0);
        }
        let evidence: Vec<RunEvidenceRow> = self.db.session(|tx| {
            table::select(tx, "WHERE run_id IN (SELECT id FROM node_runs WHERE task_id = ?1)", [state.task.id])
        })?;
        let evidence = evidence.iter().map(|e| (e.run_id, e)).collect();
        let runs: Vec<&NodeRunRow> = state.runs.iter().map(|r| &r.row).collect();
        Ok(super::rules::unknown_cost_runs_today(&runs, &evidence, controls))
    }

    fn deletable(&self, state: &TaskState) -> Result<(), WorkflowError> {
        if !state.status().is_some_and(TaskStatus::is_finished) {
            return Err(WorkflowError::conflict(format!(
                "Only a merged or cancelled task can be deleted (currently {})",
                state.task.status
            )));
        }
        if self.unknown_cost_blocker(state)? != 0 {
            return Err(WorkflowError::conflict(
                "Import the missing run costs before deleting: today's API spend for this task is incomplete and the \
                 daily budget needs it",
            ));
        }
        Ok(())
    }

    fn task_folder(&self, task: &TaskRow) -> Result<PathBuf, WorkflowError> {
        let folder = PathBuf::from(&task.task_dir);
        let task_dir = self.config().task_dir(&self.locations);
        let inside = resolve(&folder).parent().is_some_and(|p| same_path(p, &task_dir));
        if folder.is_symlink() || folder.file_name().is_none_or(|n| n != task.jira_key.as_str()) || !inside {
            return Err(WorkflowError::conflict("Task folder is outside the configured task directory"));
        }
        Ok(folder)
    }

    /// Read-only: everything a delete could remove, for the confirm dialog.
    pub async fn delete_preview(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let (state, items) = self.load_with_items(jira_key)?;
        if !state.status().is_some_and(TaskStatus::is_finished) {
            return Err(WorkflowError::conflict(format!(
                "Only a merged or cancelled task can be deleted (currently {})",
                state.task.status
            )));
        }
        let unknown = self.unknown_cost_blocker(&state)?;
        drop(held);
        let task = &state.task;
        let root = PathBuf::from(&task.repository_root);
        let worktree = PathBuf::from(&task.worktree_path);
        let (mut dirty, mut worktree_error) = (Vec::new(), Value::Null);
        if is_checkout(&worktree) {
            match self.git.dirty_paths(&worktree, false).await {
                Ok(paths) => dirty = paths.iter().map(|p| p.get(3..).unwrap_or_default().to_string()).collect(),
                Err(error) => worktree_error = json!(error.to_string()),
            }
        }
        let (branch, worker_branches) = match self.git.branch_report(&root, &task.branch_name, &task.base_branch).await
        {
            Ok(report) => {
                let mut names = Vec::new();
                for item in &items {
                    if matches!(self.git.branch_sha(&root, &item.branch_name).await, Ok(Some(_))) {
                        names.push(item.branch_name.clone());
                    }
                }
                (report, names)
            }
            Err(error) => {
                (json!({"name": task.branch_name, "exists": null, "sha": null, "error": error.to_string()}), Vec::new())
            }
        };
        let session_open = self.terminals.session_exists(&task.zellij_session_name).await.unwrap_or(false);
        let folder = PathBuf::from(&task.task_dir);
        Ok(json!({
            "worktree": {"path": py_str(&worktree), "exists": worktree.exists(), "dirtyPaths": dirty, "error": worktree_error},
            "branch": branch,
            "workerWorktrees": items.iter().map(|i| &i.worktree_path).filter(|p| Path::new(p).exists()).collect::<Vec<_>>(),
            "workerBranches": worker_branches,
            "taskFolder": {"path": py_str(&folder), "exists": folder.exists()},
            "sessionWillBeClosed": session_open,
            "unknownCostRunsToday": unknown,
        }))
    }

    /// Permanently delete a finished task. Disk steps run first and are idempotent, so a failure
    /// part-way leaves the task in place to retry; the database goes last, in one transaction; its
    /// runs' spend is kept (DeletedRunSpend) so the daily budget stays whole.
    pub async fn delete_task(
        &self,
        jira_key: &str,
        remove_worktree: bool,
        remove_branch: bool,
        remove_task_folder: bool,
        expected_sha: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        let key = jira_key.to_uppercase();
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let config = self.config();
        let (mut state, items) = self.load_with_items(jira_key)?;
        self.deletable(&state)?;
        self.cancel_worktree_setups(&key);
        let checked_out =
            std::iter::once(state.task.worktree_path.clone()).chain(items.iter().map(|i| i.worktree_path.clone()));
        if remove_branch && !remove_worktree && checked_out.into_iter().any(|p| Path::new(&p).exists()) {
            return Err(WorkflowError::conflict(
                "The branches are checked out in worktrees; remove the worktrees to delete them",
            ));
        }
        self.step(&key, "Stopping agent tabs");
        self.check_cleanup_agents(&mut state).await?;
        // Tabs just closed can't be reopened: keep their "stopped" rows even if a disk step fails.
        self.db.session(|tx| state.flush(tx))?;
        let folder = if Path::new(&state.task.task_dir).exists() { Some(self.task_folder(&state.task)?) } else { None };
        let root = PathBuf::from(&state.task.repository_root);
        let (branch, worktree) = (state.task.branch_name.clone(), PathBuf::from(&state.task.worktree_path));
        let worktree_dir = config.worktree_dir(&self.locations);
        if remove_worktree && !same_path(&worktree, worktree_dir.join(&state.task.jira_key)) {
            return Err(WorkflowError::conflict("Worktree is outside the configured task location"));
        }
        if remove_worktree || remove_branch {
            let current = self.git.branch_sha(&root, &branch).await.map_err(|e| git_error(e, 409))?;
            if current.as_deref() != expected_sha.filter(|s| !s.is_empty()) {
                return Err(WorkflowError::conflict(
                    "The task branch changed since the preview; review it again before deleting",
                ));
            }
        }
        // The session's tabs have their cwd in the worktree and would lock it on Windows.
        self.step(&key, "Closing the terminal session");
        self.close_task_sessions(&mut state).await?;

        let mut discarded = Vec::new();
        let mut deleted_branches = Vec::new();
        let disk = async {
            if remove_worktree {
                self.step(&key, "Removing the worktree");
                for attempt in 0..2 {
                    // A just-killed Windows process can hold the directory briefly.
                    match self.git.remove_task_worktree(&root, &worktree, &worktree_dir).await {
                        Ok(paths) => {
                            discarded = paths;
                            break;
                        }
                        Err(error) if attempt == 1 => return Err(error),
                        Err(_) => {
                            tokio::time::sleep(RETRY_PAUSE).await;
                            self.step(&key, "Retrying the worktree removal");
                        }
                    }
                }
                if !items.is_empty() {
                    self.step(&key, &format!("Removing {} worker worktree(s)", items.len()));
                }
                for item in &items {
                    self.git.discard_worktree(&root, Path::new(&item.worktree_path)).await?;
                }
            }
            if remove_branch {
                self.step(&key, "Deleting branches");
                for name in std::iter::once(&branch).chain(items.iter().map(|i| &i.branch_name)) {
                    if self.git.delete_branch(&root, name).await? {
                        deleted_branches.push(name.clone());
                    }
                }
            }
            Ok(())
        };
        disk.await.map_err(|e| git_error(e, 409))?;

        let mut folder_note = None;
        if let Some(folder) = &folder {
            let step = if remove_task_folder { "Deleting the task folder" } else { "Moving the task folder aside" };
            self.step(&key, step);
            let moved = if remove_task_folder {
                std::fs::remove_dir_all(folder)
            } else {
                // A re-created task must start from an empty folder, so move this one aside.
                let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
                let kept = folder.with_file_name(format!("{}.deleted-{stamp}", state.task.jira_key));
                folder_note = Some(py_str(&kept));
                std::fs::rename(folder, &kept)
            };
            moved.map_err(|e| {
                let action = if remove_task_folder { "remove" } else { "move aside" };
                WorkflowError::conflict(format!("Could not {action} the task folder: {e}"))
            })?;
        }

        self.step(&key, "Deleting the task record");
        {
            let _launches = self.launch_lock.lock().await;
            self.db.session(|tx| -> Result<(), WorkflowError> {
                let state = TaskState::load(tx, jira_key)?;
                purge_task(tx, &state)?;
                Ok(())
            })?;
        }
        drop(held);
        let removed = json!({
            "worktree": remove_worktree,
            "discardedPaths": discarded.iter().map(|p| p.get(3..).unwrap_or_default()).collect::<Vec<_>>(),
            "branches": deleted_branches,
            "taskFolder": remove_task_folder && folder.is_some(),
            "taskFolderMovedTo": folder_note,
        });
        let mut details = json!({"jiraKey": key});
        for (k, v) in removed.as_object().unwrap() {
            details[k] = v.clone();
        }
        crate::diagnostics::event("task.deleted", details);
        self.broadcaster.broadcast(&json!({"type": "task.deleted", "jiraKey": key}));
        Ok(json!({"jiraKey": key, "deleted": true, "removed": removed}))
    }
}

/// The live pane in a tab named like the agent's that no other running session tracks (for a
/// launch that recorded its session but never learned the tab identity). One match is that
/// launch's tab; none proves nothing is running.
pub(crate) fn untracked_tab<'a>(
    agents: &[Tracked<AgentSessionRow>],
    index: usize,
    panes: &'a [PaneInfo],
) -> Result<Option<&'a PaneInfo>, TerminalError> {
    let agent = &agents[index];
    let tracked: Vec<Option<i64>> = agents
        .iter()
        .enumerate()
        .filter(|(i, a)| *i != index && a.status == "running")
        .map(|(_, a)| a.zellij_tab_id)
        .collect();
    let matches: Vec<&PaneInfo> = panes
        .iter()
        .filter(|p| p.tab_name == agent.zellij_tab_name && !p.exited && !tracked.contains(&Some(p.tab_id)))
        .collect();
    if matches.len() > 1 {
        return Err(TerminalError(format!(
            "{} tab identity is ambiguous; inspect Zellij before retrying",
            agent.zellij_tab_name
        )));
    }
    Ok(matches.first().copied())
}

/// Delete the task row and everything under it, keeping its runs' spend (`_purge_task`). Foreign
/// keys are enforced, so children go before parents.
fn purge_task(tx: &rusqlite::Transaction, state: &TaskState) -> rusqlite::Result<()> {
    let id = state.task.id;
    let evidence: Vec<RunEvidenceRow> =
        table::select(tx, "WHERE run_id IN (SELECT id FROM node_runs WHERE task_id = ?1)", [id])?;
    for run in &state.runs {
        let (mode, cost) = super::rules::run_charge(evidence.iter().find(|e| e.run_id == run.id));
        let mut spend = DeletedRunSpendRow {
            id: 0,
            jira_key: state.task.jira_key.clone(),
            run_id: run.id,
            charged_at: super::rules::charged_at(run),
            billing_mode: mode,
            cost_usd: cost,
        };
        table::insert(tx, &mut spend)?;
    }
    let runs = "SELECT id FROM node_runs WHERE task_id = ?1";
    for statement in [
        format!(
            "DELETE FROM process_leases WHERE session_id IN (SELECT id FROM agent_sessions WHERE task_id = ?1) \
             OR launch_run_id IN ({runs})"
        ),
        format!("DELETE FROM agent_access WHERE task_id = ?1 OR run_id IN ({runs})"),
        format!("DELETE FROM run_evidence WHERE run_id IN ({runs})"),
        "DELETE FROM requirements_approvals WHERE task_id = ?1".into(),
        "DELETE FROM implementation_work_items WHERE task_id = ?1".into(),
        "DELETE FROM chat_threads WHERE task_id = ?1".into(),
        "DELETE FROM agent_questions WHERE task_id = ?1".into(),
        "DELETE FROM review_findings WHERE task_id = ?1".into(),
        "DELETE FROM task_artifacts WHERE task_id = ?1".into(),
        "DELETE FROM workflow_events WHERE task_id = ?1".into(),
        "DELETE FROM agent_sessions WHERE task_id = ?1".into(),
        "DELETE FROM node_runs WHERE task_id = ?1".into(),
        "DELETE FROM workflow_nodes WHERE task_id = ?1".into(),
        "DELETE FROM tasks WHERE id = ?1".into(),
    ] {
        tx.execute(&statement, [id])?;
    }
    Ok(())
}
