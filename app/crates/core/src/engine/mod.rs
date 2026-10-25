//! The workflow engine, ported from `backend/agentflow/workflow/engine.py`: the single source of
//! truth for task and node state. Everything that changes state goes through it; the UI routes
//! and flowctl only call into it.
//!
//! Ported in slices (docs/desktop-app-investigation.md §7, "Engine port plan").

pub mod agents;
pub mod approvals;
pub mod assessment;
pub mod completion;
mod consolidate;
pub mod controls;
pub mod decisions;
pub mod heartbeats;
pub mod instruction_ui;
mod jira_context;
pub mod launch;
pub mod lifecycle;
pub mod pr_sync;
pub mod pr_tracking;
pub mod projects;
pub mod prompts;
pub mod recovery;
pub mod result_models;
pub mod review_loop;
mod review_support;
mod reviews;
pub mod rules;
pub mod screen;
pub mod snapshot;
pub mod spec;
pub mod states;
pub mod store;
pub mod usage;
pub mod verification;
pub mod work_plans;
mod worker_completion;
mod workers;
pub mod worktree_setup;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};

use crate::config::{Config, Locations};
use crate::db::Database;
use crate::events::Broadcaster;
use crate::harness::HarnessRegistry;
use crate::terminal::Terminals;
use crate::vcs::Git;

/// A user-facing, actionable error, sent as `{"detail": message, "code": code}` with `status`.
/// `code` is a stable tag for the few errors the UI reacts to with extra controls, so it never has
/// to match message text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkflowError {
    pub message: String,
    pub status: u16,
    pub code: Option<&'static str>,
}

impl WorkflowError {
    pub const WORKTREE_DRIFT: &'static str = "worktree_drift";
    /// Worktree cleanup refused because the branch isn't reviewed or merged; the UI offers force.
    pub const BRANCH_UNVERIFIED: &'static str = "branch_unverified";

    pub fn new(message: impl Into<String>, status: u16) -> Self {
        WorkflowError { message: message.into(), status, code: None }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(message, 400)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(message, 404)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(message, 409)
    }
}

impl From<rusqlite::Error> for WorkflowError {
    fn from(error: rusqlite::Error) -> Self {
        // Database failures are not user errors; the message stays out of the response.
        tracing::error!(%error, "database error");
        WorkflowError::new("Database error", 500)
    }
}

pub struct Engine {
    pub locations: Locations,
    config: RwLock<Arc<Config>>,
    harnesses: RwLock<Arc<HarnessRegistry>>,
    pub db: Arc<Database>,
    pub git: Arc<dyn Git>,
    pub terminals: Arc<dyn Terminals>,
    /// Zellij, when agents run embedded and it is installed: a task that ran there under an
    /// earlier setting left its session behind, and cleanup and delete close that one too.
    pub leftover_zellij: Option<Arc<dyn Terminals>>,
    /// Frees a worktree directory from processes working in it, for cleanup.
    pub worktree_killer: Arc<dyn crate::process::worktree::WorktreeKiller>,
    pub broadcaster: Arc<Broadcaster>,
    pub api_base_url: String,
    /// Per task (by upper-case key): everything that changes one task is serialized.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Serializes agent launches and launch-policy changes across all tasks.
    pub launch_lock: tokio::sync::Mutex<()>,
    /// Every project change and task assignment; taken after a task's lock, never before it.
    projects_lock: tokio::sync::Mutex<()>,
    pub bin_dir: PathBuf,
    /// The flowctl executable (launchers report a harness's exit through it).
    pub flowctl: PathBuf,
    pub screen_trackers: screen::ScreenTrackers,
    /// Background collections of usage a provider had not finished writing, by run id.
    usage_retries: agents::UsageRetries,
    pub(crate) verifications: Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>,
    /// Worktree setups running in the background.
    setups: worktree_setup::Running,
    github_source: RwLock<Arc<dyn crate::github::GitHubSource>>,
    pr_sync_worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pr_sync_problems: Mutex<HashMap<Option<String>, String>>,
    jira_transport: RwLock<Arc<dyn crate::jira::JiraTransport>>,
    jira_titles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    usage_source: RwLock<Arc<dyn usage::UsageSource>>,
    /// Pauses before each background usage retry.
    usage_retry_delays: RwLock<Vec<std::time::Duration>>,
    /// This engine, for the background work it starts (screen trackers, usage retries).
    me: Weak<Engine>,
}

pub struct EngineSetup {
    pub locations: Locations,
    pub config: Arc<Config>,
    pub harnesses: Arc<HarnessRegistry>,
    pub db: Arc<Database>,
    pub git: Arc<dyn Git>,
    pub terminals: Arc<dyn Terminals>,
    /// Zellij, when agents run embedded and it is installed: a task that ran there under an
    /// earlier setting left its session behind, and cleanup and delete close that one too.
    pub leftover_zellij: Option<Arc<dyn Terminals>>,
    pub worktree_killer: Arc<dyn crate::process::worktree::WorktreeKiller>,
    pub broadcaster: Arc<Broadcaster>,
    pub api_base_url: String,
    /// The flowctl executable agents call (the `bin/flowctl` shims point at it).
    pub flowctl: PathBuf,
}

impl Engine {
    pub fn new(setup: EngineSetup) -> std::io::Result<Arc<Engine>> {
        let bin_dir = setup.config.data_dir(&setup.locations).join("bin");
        // cmd.exe cannot run a `\\?\` path, which is how the desktop's resource directory arrives.
        let flowctl = crate::paths::strip_verbatim(setup.flowctl);
        write_flowctl_shims(&bin_dir, &flowctl)?;
        Ok(Arc::new_cyclic(|me| Engine {
            locations: setup.locations,
            config: RwLock::new(setup.config),
            harnesses: RwLock::new(setup.harnesses),
            db: setup.db,
            git: setup.git,
            terminals: setup.terminals,
            leftover_zellij: setup.leftover_zellij,
            worktree_killer: setup.worktree_killer,
            broadcaster: setup.broadcaster,
            api_base_url: setup.api_base_url,
            locks: Mutex::new(HashMap::new()),
            launch_lock: tokio::sync::Mutex::new(()),
            projects_lock: tokio::sync::Mutex::new(()),
            bin_dir,
            flowctl,
            screen_trackers: screen::ScreenTrackers::default(),
            usage_retries: Mutex::new(HashMap::new()),
            verifications: Mutex::new(HashMap::new()),
            setups: Mutex::new(HashMap::new()),
            github_source: RwLock::new(Arc::new(crate::github::GitHubCli)),
            pr_sync_worker: Mutex::new(None),
            pr_sync_problems: Mutex::new(HashMap::new()),
            jira_transport: RwLock::new(Arc::new(crate::jira::UreqTransport)),
            jira_titles: Mutex::new(HashMap::new()),
            usage_source: RwLock::new(Arc::new(usage::CommandUsage)),
            usage_retry_delays: RwLock::new(
                agents::USAGE_RETRY_DELAYS.iter().map(|s| std::time::Duration::from_secs(*s)).collect(),
            ),
            me: me.clone(),
        }))
    }

    pub fn usage_source(&self) -> Arc<dyn usage::UsageSource> {
        self.usage_source.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Replace where usage reports come from (tests).
    pub fn set_usage_source(&self, source: Arc<dyn usage::UsageSource>) {
        *self.usage_source.write().unwrap_or_else(|p| p.into_inner()) = source;
    }

    pub fn usage_retry_delays(&self) -> Vec<std::time::Duration> {
        self.usage_retry_delays.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Replace the pauses before background usage retries (tests).
    pub fn set_usage_retry_delays(&self, delays: Vec<std::time::Duration>) {
        *self.usage_retry_delays.write().unwrap_or_else(|p| p.into_inner()) = delays;
    }

    /// Wait for a run's background usage retry, if one is scheduled.
    pub async fn join_usage_retry(&self, run_id: i64) {
        let retry = self.usage_retries.lock().unwrap_or_else(|e| e.into_inner()).remove(&run_id);
        if let Some(retry) = retry {
            let _ = retry.await;
        }
    }

    /// Whether any background usage retry is still scheduled.
    pub fn usage_retries_pending(&self) -> bool {
        self.usage_retries.lock().unwrap_or_else(|e| e.into_inner()).values().any(|r| !r.is_finished())
    }

    /// The flowctl executable launchers call.
    pub fn flowctl_executable(&self) -> PathBuf {
        self.flowctl.clone()
    }

    /// Start (or confirm) live screen classification for a session already verified as the one
    /// to keep using (`_ensure_screen_tracker`).
    pub(crate) fn ensure_screen_tracker(&self, state: &store::TaskState, agent_index: usize) {
        let agent = &state.agents[agent_index];
        let Some(pane_id) = agent.zellij_pane_id.clone().filter(|p| !p.is_empty()) else { return };
        let Ok(adapter) = self.harnesses().get(&agent.harness).cloned() else { return };
        self.screen_trackers.start(
            self.me.clone(),
            state.task.jira_key.clone(),
            agent.id,
            state.task.zellij_session_name.clone(),
            pane_id,
            adapter,
        );
    }

    /// Tasks with live sessions or running work. Hold launch_lock while confirming shutdown.
    pub fn active_work_labels(&self) -> Result<Vec<String>, WorkflowError> {
        let mut labels = self.db.session(|tx| -> Result<Vec<String>, WorkflowError> {
            let mut query = tx.prepare("select distinct t.jira_key from tasks t where exists (select 1 from agent_sessions a where a.task_id = t.id and a.status = 'running') or exists (select 1 from node_runs r where r.task_id = t.id and r.state in ('RUNNING', 'WAITING_FOR_USER')) order by t.jira_key")?;
            Ok(query.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
        })?;
        for key in self.verifications.lock().unwrap_or_else(|p| p.into_inner()).keys() {
            if !labels.contains(key) {
                labels.push(key.clone());
            }
        }
        labels.sort();
        Ok(labels)
    }

    /// Stop background work (usage retries, screen trackers) before the process exits.
    pub fn shutdown(&self) {
        for (_, worker) in self.jira_titles.lock().unwrap_or_else(|p| p.into_inner()).drain() {
            worker.abort();
        }
        if let Some(worker) = self.pr_sync_worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            worker.abort();
        }
        for cancel in self.verifications.lock().unwrap_or_else(|p| p.into_inner()).values() {
            cancel.send_replace(true);
        }
        for (_, retry) in self.usage_retries.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            retry.abort();
        }
        self.screen_trackers.stop_all();
    }

    /// Cancel background work and wait for verification processes to finish stopping.
    /// False means callers must keep the application open and allow another attempt.
    pub async fn shutdown_and_wait(&self, timeout: std::time::Duration) -> bool {
        self.shutdown();
        tokio::time::timeout(timeout, async {
            loop {
                if self.verifications.lock().unwrap_or_else(|p| p.into_inner()).is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .is_ok()
    }

    pub fn config(&self) -> Arc<Config> {
        self.config.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Swap in a reloaded configuration; requests already running keep the one they started with.
    pub fn set_config(&self, config: Arc<Config>) {
        *self.config.write().unwrap_or_else(|p| p.into_inner()) = config;
    }

    pub fn harnesses(&self) -> Arc<HarnessRegistry> {
        self.harnesses.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn set_harnesses(&self, harnesses: Arc<HarnessRegistry>) {
        *self.harnesses.write().unwrap_or_else(|p| p.into_inner()) = harnesses;
    }

    /// The lock for one task, by key (case-insensitive).
    pub fn lock(&self, jira_key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        locks.entry(jira_key.to_uppercase()).or_default().clone()
    }
}

/// Make `flowctl` resolvable inside agent terminals however AgentFlow was installed:
/// `<data>/bin/flowctl.cmd` (Windows) and `<data>/bin/flowctl` (sh) run the given executable.
fn write_flowctl_shims(bin_dir: &Path, flowctl: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(bin_dir)?;
    std::fs::write(bin_dir.join("flowctl.cmd"), format!("@echo off\r\n\"{}\" %*\r\n", flowctl.display()))?;
    let posix = bin_dir.join("flowctl");
    let target = flowctl.to_string_lossy().replace('\\', "/");
    std::fs::write(&posix, format!("#!/bin/sh\nexec \"{target}\" \"$@\"\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&posix, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}
