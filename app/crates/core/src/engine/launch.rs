//! Launching agents, ported from the engine's `run_node`, `_start_agent_run(_unlocked)`,
//! `_live_session`, `_build_prompt` and `_write_launcher`.
//!
//! A launch records its run, a scoped credential and the prompt before opening the agent's tab,
//! flushing where Python committed: the run row before the slow parts, and the session row before
//! the terminal is touched, so a crash mid-launch leaves a run the UI can see and recover.
//!
//! Worker runs use their own worktrees and inherited settings; Architect launches refresh Jira
//! context before acquiring the global launch lock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::lifecycle::{AgentOverride, untracked_tab};
use super::prompts::{self, PromptContext};
use super::rules;
use super::snapshot::{harness_cwd, resolve_agent_config, workspace};
use super::spec;
use super::states::{NodeKind, NodeState, Role, TaskStatus};
use super::store::TaskState;
use super::usage;
use super::{Engine, WorkflowError};
use crate::api::auth::{digest, token_urlsafe};
use crate::config::BillingMode;
use crate::db::table::{self, Tracked};
use crate::db::{
    AgentAccessRow, AgentQuestionRow, AgentSessionRow, DbTime, Json, NodeRunRow, ProcessLeaseRow,
    RequirementsApprovalRow, RunEvidenceRow, TaskArtifactRow, new_uid,
};
use crate::harness::{HarnessAdapter, LaunchSpec, as_posix};
use crate::jira;
use crate::paths::py_str;
use crate::process::SAFE_ENV_NAMES;
use crate::terminal::{PaneInfo, TerminalError};
use crate::workspace::{self as files, write_credential};

/// The human's go-ahead for a metered (API-billed) launch.
#[derive(Debug, Clone, Deserialize)]
pub struct MeteredApproval {
    pub harness: String,
    pub model: Option<String>,
    pub reason: String,
}

impl MeteredApproval {
    pub fn validated(self) -> Result<Self, String> {
        let length = self.reason.chars().count();
        if !(1..=1000).contains(&length) {
            return Err("meteredApproval.reason: must be 1 to 1000 characters".into());
        }
        Ok(self)
    }
}

/// Body of `POST /api/tasks/{key}/nodes/{node}/run`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunNodeRequest {
    pub r#override: Option<AgentOverride>,
    #[serde(rename = "meteredApproval")]
    pub metered_approval: Option<MeteredApproval>,
}

fn terminal_conflict(error: TerminalError) -> WorkflowError {
    WorkflowError::conflict(error.0)
}

fn io_error(error: std::io::Error) -> WorkflowError {
    WorkflowError::new(format!("Could not write the run files: {error}"), 500)
}

/// A run's timestamp as a retry briefing shows it (stored times are UTC).
fn attempt_time(value: Option<&DbTime>) -> Option<String> {
    value.map(|time| time.0.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// The HEAD an implementer run recorded at launch (its config.json `startSha`); None when it
/// recorded none (runs launched before AgentFlow recorded it) or the file is unreadable.
fn recorded_start_sha(run_dir: Option<&str>) -> Option<String> {
    let text = std::fs::read_to_string(Path::new(run_dir.filter(|d| !d.is_empty())?).join("config.json")).ok()?;
    let config: Value = serde_json::from_str(&text).ok()?;
    config.get("startSha").and_then(Value::as_str).filter(|sha| !sha.is_empty()).map(String::from)
}

/// Python's `str.strip()` truthiness for a reason.
fn has_text(text: &str) -> bool {
    !prompts::py_strip(text).is_empty()
}

/// The nodes whose agent sessions a launch of `node` may reuse: every Architect node for the
/// architect role (discovery and final verification share a conversation), else the node itself.
pub(crate) fn session_node_ids(state: &TaskState, index: usize) -> Vec<i64> {
    let node = &state.nodes[index];
    if node.role.as_deref() == Some(Role::Architect.as_str()) {
        state.nodes.iter().filter(|n| n.role.as_deref() == Some(Role::Architect.as_str())).map(|n| n.id).collect()
    } else {
        vec![node.id]
    }
}

/// The tab an agent runs in.
pub(crate) fn tab_name(key: &str, role: Option<&str>) -> String {
    match key.strip_prefix("worker_") {
        Some(worker) => format!("Worker {worker}"),
        None => role.and_then(Role::parse).map(Role::label).unwrap_or_default().to_string(),
    }
}

/// PowerShell single-quoted literal.
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Python's `repr()` of a string.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !super::agents::py_isprintable(c) => {
                let code = c as u32;
                out.push_str(&match code {
                    0..=0xff => format!("\\x{code:02x}"),
                    0x100..=0xffff => format!("\\u{code:04x}"),
                    _ => format!("\\U{code:08x}"),
                });
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

impl Engine {
    /// The task's terminal session, and the tab a node's agent runs in (when it has a role).
    fn session_and_tab(
        &self,
        jira_key: &str,
        node_key: Option<&str>,
    ) -> Result<(String, Option<String>), WorkflowError> {
        let state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let tab = match node_key {
            Some(key) => {
                let node = state.node(key)?;
                node.role.is_some().then(|| tab_name(&node.key, node.role.as_deref()))
            }
            None => None,
        };
        Ok((state.task.zellij_session_name.clone(), tab))
    }

    /// Make sure the task's session runs (on the node's tab) for the embedded read-only view.
    pub async fn open_zellij(&self, jira_key: &str, node_key: Option<&str>) -> Result<Value, WorkflowError> {
        let (session, tab) = self.session_and_tab(jira_key, node_key)?;
        let prepare = async {
            self.terminals.ensure_session(&session).await?;
            if let Some(tab) = &tab {
                self.terminals.focus_tab(&session, tab).await;
            }
            self.terminals.require_subscribe_support().await
        };
        prepare.await.map_err(terminal_conflict)?;
        Ok(json!({
            "message": "Embedded read-only terminal view is ready",
            "session": session,
            "attachCommand": self.terminals.attach_command(&session),
            "webUrl": self.terminals.web_url(&session),
        }))
    }

    /// Open an interactive terminal window on the task's session (sessions are headless on
    /// Windows).
    pub async fn open_zellij_window(&self, jira_key: &str, node_key: Option<&str>) -> Result<Value, WorkflowError> {
        let (session, mut tab) = self.session_and_tab(jira_key, node_key)?;
        match self.terminals.session_state(&session).await.map_err(terminal_conflict)? {
            None => {
                return Err(WorkflowError::conflict("This task's Zellij session is not running; start an agent first"));
            }
            // Attaching resurrects it; until then its tabs can't be focused.
            Some(crate::terminal::SessionState::Exited) => tab = None,
            Some(crate::terminal::SessionState::Running) => {}
        }
        self.terminals.open_window(&session, tab.as_deref()).await.map_err(terminal_conflict)?;
        Ok(json!({"session": session, "tab": tab}))
    }

    /// Open the task's worktree or agent cwd in the operating system's file explorer.
    pub async fn reveal_path(&self, jira_key: &str, target: &str) -> Result<Value, WorkflowError> {
        if target != "worktree" && target != "harnessCwd" {
            return Err(WorkflowError::bad_request(format!("Unknown reveal target: {target}")));
        }
        let state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let path =
            if target == "worktree" { PathBuf::from(&state.task.worktree_path) } else { harness_cwd(&state.task) };
        if !path.is_dir() {
            return Err(WorkflowError::not_found(format!("Path no longer exists: {}", py_str(&path))));
        }
        let opener = if cfg!(windows) {
            "explorer"
        } else if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let argv = vec![opener.to_string(), py_str(&path)];
        crate::process::spawn_detached(&argv, None, &std::collections::HashMap::new(), true, false)
            .map_err(|e| WorkflowError::new(format!("Could not open file explorer: {}", e.0), 500))?;
        Ok(json!({"path": py_str(&path)}))
    }

    /// A node's own harness, model and reasoning (empty values dropped), used by its next launches.
    pub async fn set_node_override(
        &self,
        jira_key: &str,
        node_key: &str,
        layer: &AgentOverride,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let index = state.node_index(node_key)?;
        if state.nodes[index].kind != NodeKind::Agent.as_str() {
            return Err(WorkflowError::conflict("Only agent nodes have harness settings"));
        }
        if node_key.starts_with("worker_") {
            return Err(WorkflowError::conflict("Configure the parent Implementer; workers share its settings"));
        }
        let mut config = Map::new();
        for (field, value) in [("harness", &layer.harness), ("model", &layer.model), ("reasoning", &layer.reasoning)] {
            if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                config.insert(field.into(), json!(value));
            }
        }
        // Python logged the dict's repr: {'model': 'x'}.
        let shown: Vec<String> = config
            .iter()
            .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr(v.as_str().unwrap_or_default())))
            .collect();
        let label = state.nodes[index].label.clone();
        state.nodes[index].config_override = Json(Value::Object(config));
        state.log(
            format!("{label} configuration override set: {{{}}}", shown.join(", ")),
            Some(node_key),
            "info",
            None,
        );
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    pub async fn run_node(
        &self,
        jira_key: &str,
        node_key: &str,
        request: RunNodeRequest,
        ready_only: bool,
    ) -> Result<Value, WorkflowError> {
        let lock = self.lock(jira_key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, jira_key))?;
        let index = state.node_index(node_key)?;
        let node_state = state.node_state(index);
        let label = state.nodes[index].label.clone();
        let status = state.status();
        if ready_only && (status != Some(TaskStatus::Active) || node_state != Some(NodeState::Ready)) {
            return Err(WorkflowError::conflict(format!("{label} is no longer ready to start")));
        }
        if status.is_some_and(|s| s.is_post_ready() || s == TaskStatus::Cancelled) {
            return Err(WorkflowError::conflict(format!("Task is {}; nodes can no longer run", state.task.status)));
        }
        if state.nodes[index].kind != NodeKind::Agent.as_str() {
            return Err(WorkflowError::conflict(format!("{label} is not an agent node")));
        }
        if !matches!(node_state, Some(NodeState::Ready | NodeState::Failed)) {
            return Err(WorkflowError::conflict(format!(
                "{label} is {}; it can only run when READY (or FAILED, to retry)",
                state.nodes[index].state
            )));
        }
        let layer = request.r#override.as_ref().map(AgentOverride::without_nones).unwrap_or_default();
        if !layer.is_empty() {
            if node_key.starts_with("worker_") {
                return Err(WorkflowError::conflict("Workers share the Implementer configuration"));
            }
            let node = &mut state.nodes[index];
            let mut merged = node.config_override.0.as_object().cloned().unwrap_or_default();
            merged.extend(layer);
            node.config_override = Json(Value::Object(merged));
        }
        if status == Some(TaskStatus::NeedsHumanReview) {
            if node_key != spec::IMPLEMENTER {
                return Err(WorkflowError::conflict(
                    "Resume the implementer or explicitly override the review blockers",
                ));
            }
            state.task.review_iteration += 1;
            state.set_status(TaskStatus::Active);
            state.task.status_message = None;
            state.log("Human resumed the stopped review loop", None, "warning", None);
        }
        self.start_agent_run(&mut state, index, request.metered_approval.as_ref(), false).await?;
        if node_key == spec::ARCHITECT {
            self.transition_jira(&mut state).await;
        }
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(jira_key)
    }

    /// Start a node's agent, unless launch policy (capacity, budget, metered approval) refuses:
    /// then the node goes back to READY with the reason. Returns the new run's id.
    ///
    /// `automatic` (an automatic retry) never carries a metered approval, and a launch it cannot
    /// make leaves the node FAILED, with its failure, instead of READY.
    pub(crate) async fn start_agent_run(
        &self,
        state: &mut TaskState,
        index: usize,
        metered: Option<&MeteredApproval>,
        automatic: bool,
    ) -> Result<Option<i64>, WorkflowError> {
        // Persist the transition before waiting for the global launch lock: a caller must not
        // hold SQLite's writer lock while another launch needs to commit.
        self.db.session(|tx| state.flush(tx))?;
        if matches!(state.nodes[index].key.as_str(), spec::ARCHITECT | spec::FINAL_ARCHITECT) {
            self.sync_jira_context(state).await;
        }
        // Setup may run for minutes: it is started here, in the background, never waited for.
        if let Some(reason) = self.setup_blocker(state, index).await? {
            if automatic {
                let error = state.nodes[index].error.clone();
                state.set_state(index, NodeState::Failed, Some(&format!("Not retried automatically: {reason}")), error);
            } else {
                state.set_state(index, NodeState::Ready, Some(&reason), None);
            }
            self.db.session(|tx| state.flush(tx))?;
            return Ok(None);
        }
        let _launching = self.launch_lock.lock().await;
        let config = self.config();
        let mut reason: Option<String> = None;
        let busy = [NodeState::Running, NodeState::WaitingForUser, NodeState::Interrupted];
        let placeholders = busy.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ");
        let active: i64 = self.db.session(|tx| {
            tx.query_row(&format!("SELECT COUNT(*) FROM node_runs WHERE state IN ({placeholders})"), [], |row| {
                row.get(0)
            })
        })?;
        if active >= config.workflow.max_concurrent_agents {
            reason = Some("Global agent capacity reached; start again when a slot is free".into());
        }
        let cfg = self.agent_config(state, index);
        let billing = cfg["billingMode"].as_str().unwrap_or_default().to_string();
        if billing != "subscription" {
            if reason.is_none() {
                reason = rules::launch_blocker(&self.budget_status(state)?);
            }
            if cfg["requiresMeteredApproval"] == json!(true) {
                let approved = metered.is_some_and(|m| {
                    has_text(&m.reason) && json!(m.harness) == cfg["harness"] && json!(m.model) == cfg["model"]
                });
                if !approved && reason.is_none() {
                    reason = Some(
                        "API launch needs approval: review the selected harness and model, and enter a reason".into(),
                    );
                }
            }
        }
        let key = state.nodes[index].key.clone();
        if let Some(reason) = reason.as_ref().filter(|_| automatic) {
            let error = state.nodes[index].error.clone();
            state.set_state(index, NodeState::Failed, Some(&format!("Not retried automatically: {reason}")), error);
            return Ok(None);
        }
        if let Some(reason) = reason {
            state.set_state(index, NodeState::Ready, Some(&reason), None);
            state.log(reason, Some(&key), "info", None);
            return Ok(None);
        }
        if let Some(metered) = metered.filter(|_| billing != "subscription") {
            state.log(
                "Human approved API launch",
                Some(&key),
                "info",
                Some(json!({"harness": cfg["harness"], "model": cfg["model"], "reason": prompts::py_strip(&metered.reason)})),
            );
        }
        let run_id = self.start_agent_run_unlocked(state, index).await?;
        self.db.session(|tx| state.flush(tx))?;
        Ok(Some(run_id))
    }

    fn agent_config(&self, state: &TaskState, index: usize) -> Map<String, Value> {
        let nodes: Vec<_> = state.nodes.iter().map(|n| &n.row).collect();
        resolve_agent_config(&self.config(), &self.harnesses(), &state.task, &nodes, &state.nodes[index], None)
    }

    fn budget_status(&self, state: &TaskState) -> Result<Value, WorkflowError> {
        let rows = self.db.session(|tx| rules::load_spending(tx, state.task.id, rules::budget_day()))?;
        Ok(rules::budget_status(&rows.spending(), Some(&state.task), &self.config().workflow.cost_controls))
    }

    async fn start_agent_run_unlocked(&self, state: &mut TaskState, index: usize) -> Result<i64, WorkflowError> {
        let node_key = state.nodes[index].key.clone();
        let task_workspace = workspace(&state.task);
        let mut approval_digest = None;
        if node_key != spec::ARCHITECT {
            let approval: Option<RequirementsApprovalRow> =
                self.db.session(|tx| table::select_one(tx, "WHERE task_id = ?1", [state.task.id]))?;
            let documents = task_workspace.requirement_files().map_err(io_error)?;
            rules::check_requirements(approval.as_ref(), &documents).map_err(WorkflowError::conflict)?;
            approval_digest = approval.map(|a| a.digest);
        }
        let instruction_snapshot = {
            let _sources = self.projects_lock.lock().await;
            match crate::instructions::active_snapshot(&task_workspace.root)? {
                Some(snapshot) => snapshot,
                None => {
                    let root = Path::new(&state.task.worktree_path);
                    let revision = self.git.head_sha(root).await.map_err(|e| super::lifecycle::git_error(e, 409))?;
                    crate::instructions::capture(&task_workspace.root, root, &revision, false)?
                }
            }
        };
        if node_key.starts_with("worker_") {
            self.prepare_worker(state, index).await?;
        }
        let role_text = state.nodes[index].role.clone().unwrap_or_default();
        let role =
            Role::parse(&role_text).ok_or_else(|| WorkflowError::conflict(format!("Unknown role {role_text}")))?;
        let cfg = self.agent_config(state, index);
        let harness_key = cfg["harness"].as_str().unwrap_or_default().to_string();
        let model = cfg["model"].as_str().map(String::from);
        let reasoning = cfg["reasoning"].as_str().map(String::from);
        let registry = self.harnesses();
        if !registry.is_usable(&harness_key) {
            let caps = registry.capabilities.get(&harness_key);
            let why = caps.and_then(|c| c.message.clone()).filter(|m| !m.is_empty()).unwrap_or("not enabled".into());
            return Err(WorkflowError::conflict(format!(
                "Harness '{harness_key}' is unavailable ({why}). Choose another harness in the node configuration."
            )));
        }
        let adapter: HarnessAdapter = registry.get(&harness_key).map_err(WorkflowError::conflict)?.clone();
        let session_name = state.task.zellij_session_name.clone();
        self.terminals.ensure_session(&session_name).await.map_err(terminal_conflict)?;

        // Settle any earlier process before recording a run, so a refusal here leaves no orphaned
        // RUNNING run behind. Resume by exact conversation identity below, never by a shared cwd.
        if self.live_session(state, role.as_str(), Some(index)).await?.is_some() {
            let ids = session_node_ids(state, index);
            self.stop_sessions(state, Some(role.as_str()), Some(&ids), true).await?;
        }

        let node_id = state.nodes[index].id;
        let run_number = state.runs.iter().filter(|r| r.node_id == node_id).count() + 1;
        // An implementer's HEAD at launch goes into the run's config.json, so a later retry can list
        // what an unfinished attempt committed. Best-effort: an unreadable worktree never blocks a launch.
        let (worktree, base_sha) = match role {
            Role::Implementer if node_key.starts_with("worker_") => {
                let item = self.worker_item(state, index)?;
                (Some(PathBuf::from(&item.worktree_path)), Some(item.base_sha))
            }
            Role::Implementer => (Some(PathBuf::from(&state.task.worktree_path)), None),
            _ => (None, None),
        };
        let head_now = match &worktree {
            Some(path) => self.git.head_sha(path).await.ok(),
            None => None,
        };
        let attempt = self
            .previous_attempt(
                state,
                node_id,
                role.as_str(),
                worktree.as_deref(),
                head_now.as_deref(),
                base_sha.as_deref(),
            )
            .await?;
        let role_node_ids: Vec<i64> =
            state.nodes.iter().filter(|n| n.role.as_deref() == Some(role.as_str())).map(|n| n.id).collect();
        let role_runs = state.runs.iter().filter(|r| role_node_ids.contains(&r.node_id)).count();
        let run_dir = task_workspace.run_dir(role.run_dir(), (role_runs + 1) as u32).map_err(io_error)?;
        let config = self.config();
        let billing_mode = config.harnesses.get(&harness_key).map_or(BillingMode::Unknown, |h| h.billing_mode);
        let expected_sha = state.nodes[index].checkpoint_sha.clone();
        state.runs.push(Tracked::new(NodeRunRow {
            id: 0,
            task_id: state.task.id,
            node_id,
            run_number: run_number as i64,
            iteration: state.task.review_iteration,
            state: NodeState::Running.as_str().into(),
            harness: Some(harness_key.clone()),
            model: model.clone(),
            reasoning: reasoning.clone(),
            run_dir: Some(py_str(&run_dir)),
            prompt_path: None,
            result_path: None,
            expected_sha: expected_sha.clone(),
            result: Json(Value::Null),
            exit_code: None,
            error: None,
            started_at: DbTime::now(),
            completed_at: None,
        }));
        let run_index = state.runs.len() - 1;
        let token = token_urlsafe(32);
        self.db.session(|tx| -> Result<(), WorkflowError> {
            state.flush(tx)?;
            let run_id = state.runs[run_index].id;
            table::insert(
                tx,
                &mut RunEvidenceRow {
                    uid: new_uid(),
                    run_id,
                    requirements_digest: approval_digest.clone(),
                    verification: Json(Value::Null),
                    usage: Json(Value::Null),
                    confirmed_model: None,
                    billing_mode: serde_json::to_value(billing_mode)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_else(|| "unknown".into()),
                },
            )?;
            table::insert(
                tx,
                &mut AgentAccessRow {
                    id: new_uid(),
                    task_id: state.task.id,
                    run_id,
                    role: role.as_str().into(),
                    token_hash: digest(&token),
                },
            )?;
            write_credential(&run_dir.join("credential.json"), &json!({"token": token, "runId": run_id}))
                .map_err(io_error)?;
            Ok(())
        })?;
        let run_id = state.runs[run_index].id;

        let mut prompt_text = self.build_prompt(state, index, run_id, &run_dir)?;
        // Composed untouched: it quotes agent-written text, which must not get flowctl run ids inserted.
        let attempt_text = attempt.as_ref().map(|a| format!("\n{}", prompts::previous_attempt(a))).unwrap_or_default();
        // Which Jira sync this Architect is shown; recorded only once it has actually launched, so
        // the next prompt's "new since your last run" is measured from what it really saw.
        let jira_generation = [spec::ARCHITECT, spec::FINAL_ARCHITECT]
            .contains(&node_key.as_str())
            .then(|| jira::manifest_generation(&task_workspace.context_dir))
            .flatten();
        prompt_text =
            crate::instructions::compose(&prompt_text, &instruction_snapshot, role.as_str(), run_id, &attempt_text)?;
        let prompt_path = run_dir.join("prompt.md");
        let persisted = (|| -> Result<(), WorkflowError> {
            if instruction_snapshot.files[role.as_str()].content.as_deref().is_some_and(|c| !c.is_empty()) {
                crate::instructions::atomic_text(&prompt_path, &prompt_text)?;
            } else {
                crate::pyfile::write_text(&prompt_path, &prompt_text).map_err(io_error)?;
            }
            crate::instructions::record_run(&run_dir, &instruction_snapshot, role.as_str())
        })();
        if let Err(error) = persisted {
            TaskState::finish_run(&mut state.runs[run_index], NodeState::Failed, Some(error.to_string()));
            state.set_state(
                index,
                NodeState::Failed,
                Some("Could not persist agent briefing"),
                Some(error.to_string()),
            );
            self.db.session(|tx| state.flush(tx))?;
            return Err(error);
        }
        state.runs[run_index].prompt_path = Some(py_str(&prompt_path));
        files::write_json(
            &run_dir.join("config.json"),
            &json!({"harness": harness_key, "model": model, "reasoning": reasoning, "node": node_key,
                    "role": role.as_str(), "iteration": state.task.review_iteration, "expectedSha": expected_sha,
                    "startSha": head_now}),
        )
        .map_err(io_error)?;
        let mut artifact = TaskArtifactRow {
            id: 0,
            task_id: state.task.id,
            node_run_id: Some(run_id),
            kind: format!("{node_key}-prompt"),
            path: py_str(&prompt_path),
            created_at: DbTime::now(),
        };

        // The run id is typed too: a resumed conversation that won't (or can't) read the new
        // briefing would otherwise keep signalling with a run id remembered from its history.
        let reference = format!(
            "{} This is AgentFlow run {run_id}: use --run {run_id} on every flowctl command. The briefing and \
             the task files it lists are outside the worktree; reading them is expected.",
            HarnessAdapter::prompt_reference(&prompt_path)
        );
        let cwd = self.run_cwd(state, index)?;
        let tab = tab_name(&node_key, Some(role.as_str()));

        // The newest earlier session of this role and harness on these nodes (or one that never
        // got a run), to resume its conversation.
        let session_nodes = session_node_ids(state, index);
        let previous_session = state
            .agents
            .iter()
            .rev()
            .find(|a| {
                a.role == role.as_str()
                    && a.harness == harness_key
                    && match a.current_run_id {
                        None => true,
                        Some(run) => state.runs.iter().any(|r| r.id == run && session_nodes.contains(&r.node_id)),
                    }
            })
            .map(|a| a.row.clone());
        // A resumed launch that itself crashed (harness exited before completing) means the CLI
        // session is broken, not just interrupted: retrying the same resume would repeat the
        // crash forever, so fall through to a fresh launch.
        let previous_run = previous_session
            .as_ref()
            .and_then(|s| s.current_run_id)
            .and_then(|id| state.runs.iter().find(|r| r.id == id))
            .map(|r| r.row.clone());
        let resume_is_broken = previous_run
            .as_ref()
            .is_some_and(|r| r.error.as_deref().unwrap_or_default().starts_with("Harness exited with code"));
        // Reviewers start every review round in a fresh conversation: an old one anchors them. A
        // retry within the same round may still resume.
        let new_review_round = matches!(role, Role::CodeReviewer | Role::SecurityReviewer)
            && previous_run.as_ref().is_none_or(|r| r.iteration != state.task.review_iteration);
        if previous_session.is_some() && new_review_round {
            state.log(
                format!("Starting {tab} in a fresh conversation for review round {}", state.task.review_iteration),
                Some(&node_key),
                "info",
                None,
            );
        }
        let mut launch: Option<LaunchSpec> = None;
        let fresh_instructions = crate::instructions::needs_fresh(
            &instruction_snapshot,
            previous_run.as_ref().and_then(|r| r.run_dir.as_deref()),
        );
        if let Some(previous) = previous_session.as_ref().filter(|_| {
            !resume_is_broken && !new_review_round && !fresh_instructions && !node_key.starts_with("worker_")
        }) {
            launch = adapter.build_resume(
                previous.harness_session_id.as_deref(),
                model.as_deref(),
                reasoning.as_deref(),
                &reference,
            );
            if launch.is_some() {
                state.log(
                    format!("Original {tab} process is gone; resuming its conversation"),
                    Some(&node_key),
                    "warning",
                    None,
                );
            }
        }
        let resumed = launch.is_some();
        let launch = match launch {
            Some(launch) => launch,
            None => {
                if resume_is_broken {
                    state.log(
                        format!("Previous {tab} resume attempt crashed; starting a fresh run instead"),
                        Some(&node_key),
                        "warning",
                        None,
                    );
                }
                adapter.build_launch(model.as_deref(), reasoning.as_deref(), &reference)
            }
        };
        let usage_command = config.harnesses.get(&harness_key).and_then(|h| h.usage_command.clone());
        let baseline = usage::prepare_baseline(
            self.usage_source().as_ref(),
            usage_command.as_deref(),
            &cwd,
            run_id,
            launch.harness_session_id.as_deref(),
            resumed,
        )
        .await;
        files::write_json(&run_dir.join("usage-baseline.json"), &baseline).map_err(io_error)?;
        adapter.prepare_workspace(&cwd, Some(Path::new(&state.task.repository_root)));
        let script = self.write_launcher(&run_dir, &launch, &cwd, state, role.as_str(), run_id).map_err(io_error)?;
        let mut command = self.terminals.shell_command();
        if cfg!(windows) {
            command.extend(["-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"].map(String::from));
        }
        command.push(py_str(&script));
        state.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: state.task.id,
            role: role.as_str().into(),
            harness: harness_key.clone(),
            model: model.clone(),
            reasoning: reasoning.clone(),
            zellij_tab_name: tab.clone(),
            zellij_tab_id: None,
            zellij_pane_id: None,
            harness_session_id: launch.harness_session_id.clone(),
            status: "running".into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: Some(run_id),
            started_at: DbTime::now(),
            ended_at: None,
        }));
        let agent_index = state.agents.len() - 1;
        // Persist intent for every role before opening an external terminal.
        state.set_state(index, NodeState::Running, Some("Launching agent"), None);
        self.db.session(|tx| -> rusqlite::Result<()> {
            table::insert(tx, &mut artifact)?;
            state.flush(tx)?;
            let session_id = state.agents[agent_index].id;
            table::insert(tx, &mut ProcessLeaseRow { id: new_uid(), session_id, launch_run_id: run_id })
        })?;
        let launch_started_at = chrono::Utc::now();
        let opened = self.terminals.new_tab(&session_name, &tab, &cwd, Some(&command)).await;
        let (tab_id, pane_id) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                TaskState::finish_run(&mut state.runs[run_index], NodeState::Failed, Some(error.0.clone()));
                state.set_state(index, NodeState::Failed, Some("Agent launch failed"), Some(error.0.clone()));
                self.db.session(|tx| state.flush(tx))?;
                return Err(WorkflowError::conflict(format!("Could not open zellij tab: {error}")));
            }
        };
        state.agents[agent_index].zellij_tab_id = Some(tab_id);
        state.agents[agent_index].zellij_pane_id = pane_id;
        if let Some(generation) = jira_generation {
            jira::mark_prompted(&task_workspace.context_dir, generation);
        }
        self.ensure_screen_tracker(state, agent_index);
        if state.agents[agent_index].harness_session_id.is_none() {
            // Harnesses like Codex assign their own session id; best-effort discover it now so a
            // later run of this role can resume.
            state.agents[agent_index].harness_session_id = adapter.capture_session_id(&cwd, launch_started_at).await;
        }
        let label = state.nodes[index].label.clone();
        state.log(
            format!("{label} started: {harness_key} in zellij tab '{tab}' (cwd {})", py_str(&cwd)),
            Some(&node_key),
            "info",
            Some(json!({"harness": harness_key, "requestedModel": model})),
        );
        if node_key != spec::ARCHITECT {
            state.nodes[index].iteration = state.task.review_iteration;
        }
        state.set_state(index, NodeState::Running, Some(&format!("Run #{run_number} ({harness_key})")), None);
        Ok(run_id)
    }

    /// The running agent session for a role (on the given node's session nodes), verified against
    /// the terminal's pane list. A session whose pane is gone is marked exited.
    pub(crate) async fn live_session(
        &self,
        state: &mut TaskState,
        role: &str,
        node_index: Option<usize>,
    ) -> Result<Option<usize>, WorkflowError> {
        let node_ids = node_index.map(|index| session_node_ids(state, index));
        let found = (0..state.agents.len()).rev().find(|&i| {
            let agent = &state.agents[i];
            agent.role == role
                && agent.status == "running"
                && node_ids.as_ref().is_none_or(|ids| {
                    agent
                        .current_run_id
                        .and_then(|run| state.runs.iter().find(|r| r.id == run))
                        .is_some_and(|r| ids.contains(&r.node_id))
                })
        });
        let Some(index) = found else { return Ok(None) };
        let session = state.task.zellij_session_name.clone();
        let pane = match self.verified_pane(state, index, &session).await {
            Ok(pane) => pane,
            Err(error) => {
                // Never mistake an unverifiable launch for a dead one.
                return Err(WorkflowError::conflict(format!(
                    "Cannot establish the existing agent's pane identity ({error}); stop it before retrying"
                )));
            }
        };
        if pane.as_ref().is_none_or(|p| p.exited) {
            let agent = &mut state.agents[index];
            agent.status = "exited".into();
            agent.ended_at = Some(DbTime::now());
            self.screen_trackers.cancel(agent.id);
            return Ok(None);
        }
        Ok(Some(index))
    }

    async fn verified_pane(
        &self,
        state: &mut TaskState,
        index: usize,
        session: &str,
    ) -> Result<Option<PaneInfo>, TerminalError> {
        if !self.terminals.session_exists(session).await? {
            return Ok(None);
        }
        if state.agents[index].zellij_tab_id.is_none_or(|id| id < 0) {
            // A launch whose new-tab call failed or timed out: the tab may still exist.
            let panes = self.terminals.list_panes(session).await?;
            if let Some(pane) = untracked_tab(&state.agents, index, &panes)?.cloned() {
                state.agents[index].zellij_tab_id = Some(pane.tab_id);
                state.agents[index].zellij_pane_id = Some(pane.pane_id);
            }
        } else if state.agents[index].zellij_pane_id.as_deref().is_none_or(str::is_empty) {
            let tab = state.agents[index].zellij_tab_id.unwrap_or(-1);
            state.agents[index].zellij_pane_id = self.terminals.pane_for_tab(session, tab).await;
            if state.agents[index].zellij_pane_id.as_deref().is_none_or(str::is_empty) {
                return Err(TerminalError("its tab has no pane".into()));
            }
        }
        match state.agents[index].zellij_pane_id.clone().filter(|p| !p.is_empty()) {
            Some(pane_id) => self.terminals.pane_state(session, &pane_id).await,
            None => Ok(None),
        }
    }

    /// The unfinished attempts at this stage in the current review round (the node's trailing
    /// FAILED runs) for a retry's briefing (`prompts::previous_attempt`); None when the latest
    /// earlier run did not fail. `worktree` is given for implementers only: their committed and
    /// uncommitted work is listed, measured from where the first unfinished attempt started
    /// (`base_sha` when it recorded nothing).
    async fn previous_attempt(
        &self,
        state: &TaskState,
        node_id: i64,
        role: &str,
        worktree: Option<&Path>,
        head: Option<&str>,
        base_sha: Option<&str>,
    ) -> Result<Option<Value>, WorkflowError> {
        let unfinished: Vec<&NodeRunRow> = state
            .runs
            .iter()
            .rev()
            .filter(|r| r.node_id == node_id)
            .take_while(|r| r.state == NodeState::Failed.as_str() && r.iteration == state.task.review_iteration)
            .map(|r| &r.row)
            .collect();
        let (Some(latest), Some(first)) = (unfinished.first(), unfinished.last()) else { return Ok(None) };
        let numbers: HashMap<i64, i64> = unfinished.iter().map(|r| (r.id, r.run_number)).collect();
        let launched = latest.exit_code.is_some()
            || state.agents.iter().any(|a| a.current_run_id == Some(latest.id) && a.zellij_tab_id.is_some());
        let (heartbeat, questions) = self.db.session(|tx| -> rusqlite::Result<_> {
            let heartbeat = tx
                .query_row("SELECT received_at, phase FROM run_heartbeats WHERE run_id = ?1", [latest.id], |row| {
                    Ok((row.get::<_, DbTime>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?;
            let questions: Vec<AgentQuestionRow> =
                table::select(tx, "WHERE task_id = ?1 AND answered = 1 ORDER BY id", [state.task.id])?;
            Ok((heartbeat, questions))
        })?;
        let questions: Vec<AgentQuestionRow> =
            questions.into_iter().filter(|q| q.node_run_id.is_some_and(|id| numbers.contains_key(&id))).collect();
        let answers: Vec<Value> = questions[questions.len().saturating_sub(10)..]
            .iter()
            .map(|q| {
                json!({"runNumber": q.node_run_id.map(|id| numbers[&id]), "question": q.message,
                       "answer": q.answer.clone().unwrap_or_default()})
            })
            .collect();
        let mut attempt = json!({
            "unfinishedAttempts": unfinished.len(), "runNumber": latest.run_number,
            "harness": latest.harness, "model": latest.model, "startedAt": attempt_time(Some(&latest.started_at)),
            "endedAt": attempt_time(latest.completed_at.as_ref()), "launched": launched,
            "error": latest.error, "exitCode": latest.exit_code,
            "heartbeat": heartbeat.map(|(at, phase)| json!({"phase": phase, "receivedAt": attempt_time(Some(&at))})),
            "role": role, "answers": answers, "worktree": null,
        });
        if let Some(path) = worktree {
            // Each listing that git cannot produce is null and reported on its own; neither blocks the launch.
            let start = recorded_start_sha(first.run_dir.as_deref()).or_else(|| base_sha.map(String::from));
            let committed = match (start.as_deref(), head) {
                (Some(start), Some(head)) if !start.is_empty() && start != head => {
                    self.git.changed_paths(path, start, head).await.ok()
                }
                _ => Some(Vec::new()),
            };
            let uncommitted = self.git.dirty_paths(path, false).await.ok();
            attempt["worktree"] = json!({"path": py_str(path), "startSha": start, "headSha": head,
                                         "committed": committed, "uncommitted": uncommitted});
        }
        Ok(Some(attempt))
    }

    pub(super) fn build_prompt(
        &self,
        state: &TaskState,
        index: usize,
        run_id: i64,
        run_dir: &Path,
    ) -> Result<String, WorkflowError> {
        let task = &state.task;
        let node = &state.nodes[index];
        let task_workspace = workspace(task);
        let root = PathBuf::from(&task.worktree_path);
        let mut ctx = PromptContext {
            jira_key: task.jira_key.clone(),
            project_dir: task.project_relative_path.as_deref().filter(|p| !p.is_empty()).map(|p| root.join(p)),
            worktree_root: root,
            task_dir: task_workspace.root.clone(),
            run_dir: run_dir.to_path_buf(),
            run_id,
            role: node.role.clone().unwrap_or_default(),
            base_branch: task.base_branch.clone(),
            branch_name: task.branch_name.clone(),
            additional_context: task.additional_context.clone().unwrap_or_default(),
            iteration: task.review_iteration,
            max_iterations: task.max_review_iterations,
            flowctl_path: as_posix(&self.bin_dir.join("flowctl")),
            previous_review_sha: None,
        };
        match node.key.as_str() {
            spec::ORCHESTRATOR => Ok(prompts::orchestrator(&ctx)),
            key if key.starts_with("worker_") => {
                let item = self.worker_item(state, index)?;
                ctx.worktree_root = PathBuf::from(&item.worktree_path);
                ctx.branch_name = item.branch_name.clone();
                ctx.project_dir = task.project_relative_path.as_deref().map(|p| ctx.worktree_root.join(p));
                let results = self
                    .worker_results(state, &self.work_items(state)?)?
                    .into_iter()
                    .filter(|r| item.dependencies.0.as_array().is_some_and(|d| d.contains(&r["key"])))
                    .collect::<Vec<_>>();
                let item = prompts::WorkerItem {
                    plan_key: &item.plan_key,
                    worktree_path: &item.worktree_path,
                    branch_name: &item.branch_name,
                    base_sha: &item.base_sha,
                    brief: &item.brief,
                    owned_paths: &item.owned_paths.0,
                    acceptance_criteria: &item.acceptance_criteria.0,
                };
                Ok(prompts::worker(&ctx, &item, &results))
            }
            spec::ARCHITECT if node.iteration > 0 => {
                let feedback_path = task_workspace.requirements_dir.join(format!("feedback-{:02}.md", node.iteration));
                let feedback = if feedback_path.exists() {
                    crate::pyfile::read_text(&feedback_path).map_err(io_error)?
                } else {
                    String::new()
                };
                Ok(prompts::architect_revision(&ctx, &feedback, node.iteration))
            }
            spec::ARCHITECT => Ok(prompts::architect_discovery(&ctx)),
            spec::IMPLEMENTER => {
                let items = self.work_items(state)?;
                if !items.is_empty() && state.task.review_iteration == 1 {
                    let summary = state
                        .runs
                        .iter()
                        .rev()
                        .find(|r| state.run_node_key(r) == Some(spec::ORCHESTRATOR))
                        .and_then(|r| r.result.0.get("summary"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    return Ok(prompts::integration(&ctx, &self.worker_results(state, &items)?, summary));
                }
                let (findings, previous_sha) = self.fix_round_findings(state)?;
                Ok(prompts::implementer(&ctx, findings.as_deref(), previous_sha.as_deref()))
            }
            spec::CODE_REVIEW | spec::SECURITY_REVIEW => {
                let sha = node.checkpoint_sha.as_deref().or(task.current_sha.as_deref()).unwrap_or("");
                let implementation = state
                    .runs
                    .iter()
                    .rev()
                    .find(|r| state.run_node_key(r) == Some(spec::IMPLEMENTER) && r.state == "COMPLETED");
                let summary =
                    implementation.and_then(|r| r.result.0.get("summary")).and_then(Value::as_str).unwrap_or("");
                let evidence: Option<RunEvidenceRow> = match implementation {
                    Some(run) => self.db.session(|tx| table::select_one(tx, "WHERE run_id = ?1", [run.id]))?,
                    None => None,
                };
                let verification =
                    evidence.as_ref().map(|e| &e.verification.0).filter(|v| v["sha"].as_str() == Some(sha));
                let (previous, rows) = self.previous_review(state, index, run_id)?;
                ctx.previous_review_sha = previous.and_then(|r| r.expected_sha);
                let findings = rows.map(|rows| rows.iter().map(super::snapshot::finding_dict).collect::<Vec<_>>());
                let review = prompts::Review {
                    sha,
                    implementation_summary: summary,
                    previous_findings: findings.as_deref(),
                    verification,
                };
                Ok(if node.key == spec::CODE_REVIEW {
                    prompts::code_review(&ctx, &review)
                } else {
                    prompts::security_review(&ctx, &review)
                })
            }
            spec::FINAL_ARCHITECT => {
                Ok(prompts::final_architect(&ctx, task.current_sha.as_deref().unwrap_or(""), &self.history(state)?))
            }
            key => Err(WorkflowError::conflict(format!("The {key} prompt is not supported by this backend yet"))),
        }
    }

    /// The script a tab runs: a clean environment holding only the run's variables, the harness,
    /// then `flowctl exited` with its exit code.
    ///
    /// Difference: Python also put its interpreter's directory on PATH; the Rust backend has none.
    fn write_launcher(
        &self,
        run_dir: &Path,
        launch: &LaunchSpec,
        cwd: &Path,
        state: &TaskState,
        role: &str,
        run_id: i64,
    ) -> std::io::Result<PathBuf> {
        let task = &state.task;
        let mut env: IndexMap<String, String> = launch.env.clone();
        for (key, value) in [
            ("AGENTFLOW_API", self.api_base_url.clone()),
            ("AGENTFLOW_TASK", task.jira_key.clone()),
            ("AGENTFLOW_AGENT", role.to_string()),
            ("AGENTFLOW_RUN", run_id.to_string()),
            ("AGENTFLOW_CREDENTIAL_FILE", py_str(&run_dir.join("credential.json"))),
            ("AGENTFLOW_TASK_DIR", task.task_dir.clone()),
            ("AGENTFLOW_WORKTREE", task.worktree_path.clone()),
            // A harness's own shell tool doesn't reliably inherit the PATH prepend below, so
            // flowctl must also be reachable by a plain variable lookup.
            ("AGENTFLOW_FLOWCTL", as_posix(&self.bin_dir.join("flowctl"))),
        ] {
            env.insert(key.to_string(), value);
        }
        let banner = format!("AgentFlow: {} / {role} / run {run_id}", task.jira_key);
        let flowctl = self.flowctl_executable();
        if cfg!(windows) {
            let mut lines = vec!["# Generated by AgentFlow - launches one agent run inside its zellij tab".to_string()];
            let mut allowed: Vec<&str> = SAFE_ENV_NAMES.to_vec();
            allowed.sort_unstable();
            let allowed = allowed.iter().map(|name| ps_quote(name)).collect::<Vec<_>>().join(", ");
            lines.push(format!(
                "Get-ChildItem Env: | Where-Object {{ $_.Name.ToUpperInvariant() -notin @({allowed}) }} | ForEach-Object {{ Remove-Item -LiteralPath ('Env:' + $_.Name) }}"
            ));
            for (key, value) in &env {
                lines.push(format!("$env:{key} = {}", ps_quote(value)));
            }
            lines.push(format!("$env:PATH = {} + ';' + $env:PATH", ps_quote(&py_str(&self.bin_dir))));
            lines.push(format!("Set-Location -LiteralPath {}", ps_quote(&py_str(cwd))));
            lines.push(format!("Write-Host '{banner}' -ForegroundColor Cyan"));
            let args = launch.argv[1..].iter().map(|a| ps_quote(a)).collect::<Vec<_>>().join(" ");
            lines.push(format!("& {} {args}", ps_quote(&launch.argv[0])).trim_end().to_string());
            lines.push("$code = $LASTEXITCODE".into());
            lines.push("if ($null -eq $code) { $code = 0 }".into());
            lines.push(format!("& {} exited --code $code | Out-Null", ps_quote(&py_str(&flowctl))));
            lines.push("exit $code".into());
            let script = run_dir.join("launch.ps1");
            // UTF-8 with a BOM: Windows PowerShell 5.1 reads a file without one as ANSI.
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend((lines.join("\r\n") + "\r\n").into_bytes());
            std::fs::write(&script, bytes)?;
            Ok(script)
        } else {
            use crate::terminal::zellij::shlex_quote;
            let mut lines = vec![
                "#!/bin/sh".to_string(),
                "# Generated by AgentFlow - launches one agent run inside its zellij tab".to_string(),
            ];
            for (key, value) in &env {
                lines.push(format!("export {key}={}", shlex_quote(value)));
            }
            lines.push(format!("export PATH={}:$PATH", shlex_quote(&py_str(&self.bin_dir))));
            lines.push(format!("cd {} || exit 1", shlex_quote(&py_str(cwd))));
            lines.push(format!("echo '{banner}'"));
            let mut names: Vec<&str> = SAFE_ENV_NAMES
                .iter()
                .copied()
                .chain(env.keys().map(String::as_str))
                .filter(|n| *n != "PROGRAMFILES(X86)")
                .collect();
            names.sort_unstable();
            names.dedup();
            let assignments = names.iter().map(|key| format!("{key}=\"${{{key}-}}\"")).collect::<Vec<_>>().join(" ");
            let argv = launch.argv.iter().map(|a| shlex_quote(a)).collect::<Vec<_>>().join(" ");
            lines.push(format!("env -i {assignments} {argv}"));
            lines.push("code=$?".into());
            lines.push(format!("{} exited --code \"$code\" >/dev/null", shlex_quote(&py_str(&flowctl))));
            lines.push("exit \"$code\"".into());
            let script = run_dir.join("launch.sh");
            std::fs::write(&script, lines.join("\n") + "\n")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(script)
        }
    }
}
