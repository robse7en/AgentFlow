//! Prepares each fresh task or worker worktree from the repository's `workspace.worktreeSetup`:
//! copy ignored local files from the checkout, run the
//! setup commands, then require a clean `git status`. Setup runs in the background, never under the
//! task or launch lock, and every agent but the Architect waits for its worktree's setup.
//!
//! Status lives in a JSON file per worktree in the task folder (`worktree-setup/<name>.json`, the
//! log beside it), so the database both backends share is unchanged. Rust only: the Python backend
//! accepts the configuration but does not run it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::states::{NodeState, TaskStatus};
use super::store::TaskState;
use super::{Engine, WorkflowError, spec, verification};
use crate::config::WorktreeSetup;
use crate::db::{DbTime, TaskRow};
use crate::paths::{is_within, py_str, resolve};
use crate::vcs::Git;

/// Starts the message of a node held back by setup; when setup succeeds, automatic nodes start.
pub const WAITING: &str = "Waiting for worktree setup";
const RUNNING_MESSAGE: &str = "Waiting for worktree setup to finish";
const FAILED_MESSAGE: &str = "Waiting for worktree setup: it failed. Retry setup from the task view";
const FINISHED_MESSAGE: &str = "Worktree setup finished; start when ready";
const INTERRUPTED: &str = "Interrupted: AgentFlow stopped during setup";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupState {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    /// `task`, or `worker-<uid>` for a worker's worktree.
    pub name: String,
    pub worktree: String,
    /// Of the repository's setup configuration when this run started: a change re-runs setup.
    pub digest: String,
    pub state: SetupState,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub log_path: String,
}

/// Running setups by status file: the task (upper-case key) and the sender that cancels them.
pub type Running = std::sync::Mutex<std::collections::HashMap<String, (String, tokio::sync::watch::Sender<bool>)>>;

fn status_dir(task: &TaskRow) -> PathBuf {
    Path::new(&task.task_dir).join("worktree-setup")
}

pub fn digest(setup: &WorktreeSetup) -> String {
    let text = serde_json::to_string(setup).unwrap_or_default();
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The setup a node waits for: the task worktree's, or its worker's own.
fn target_name(node_key: &str) -> String {
    node_key.strip_prefix("worker_").map_or_else(|| "task".into(), |uid| format!("worker-{uid}"))
}

impl Engine {
    fn setup_config(&self, task: &TaskRow) -> Option<WorktreeSetup> {
        self.config().workspace.setup_for(Path::new(&task.repository_root)).cloned()
    }

    /// A status as last written, except that one left running by an earlier backend is failed.
    fn observed(&self, path: &Path) -> Option<SetupStatus> {
        let mut status: SetupStatus = serde_json::from_value(crate::workspace::read_json(path).ok()?).ok()?;
        let running = self.setups.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&py_str(path));
        if status.state == SetupState::Running && !running {
            status.state = SetupState::Failed;
            status.error = Some(INTERRUPTED.into());
        }
        Some(status)
    }

    /// Every setup of this task, the task worktree's first; None when its repository has none.
    pub(crate) fn setup_statuses(&self, task: &TaskRow) -> Option<Vec<SetupStatus>> {
        self.setup_config(task)?;
        let mut statuses: Vec<SetupStatus> = std::fs::read_dir(status_dir(task))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .filter_map(|p| self.observed(&p))
                    .collect()
            })
            .unwrap_or_default();
        statuses.sort_by_key(|s| (s.name != "task", s.started_at.clone()));
        Some(statuses)
    }

    /// Why node `index` cannot launch yet because of worktree setup, or None. Starts setup when
    /// it has not run for the current configuration. Called before the launch lock is taken; a
    /// worker's worktree is created (and its dependencies merged) here, so setup sees them.
    pub(crate) async fn setup_blocker(
        &self,
        state: &mut TaskState,
        index: usize,
    ) -> Result<Option<String>, WorkflowError> {
        let node_key = state.nodes[index].key.clone();
        let Some(setup) = self.setup_config(&state.task).filter(|_| node_key != spec::ARCHITECT) else {
            return Ok(None);
        };
        let worktree = if node_key.starts_with("worker_") {
            self.prepare_worker(state, index).await?;
            PathBuf::from(self.worker_item(state, index)?.worktree_path)
        } else {
            PathBuf::from(&state.task.worktree_path)
        };
        let name = target_name(&node_key);
        let digest = digest(&setup);
        match self.observed(&status_dir(&state.task).join(format!("{name}.json"))) {
            Some(s) if s.state == SetupState::Succeeded && s.digest == digest => return Ok(None),
            // A run of an older configuration finishes first; the next launch re-runs it.
            Some(s) if s.state == SetupState::Running => {}
            Some(s) if s.state == SetupState::Failed && s.digest == digest => return Ok(Some(FAILED_MESSAGE.into())),
            _ => {
                if self.start_setup(&state.task, &name, &worktree, setup) {
                    state.log(format!("Worktree setup started: {}", py_str(&worktree)), Some(&node_key), "info", None);
                }
            }
        }
        Ok(Some(RUNNING_MESSAGE.into()))
    }

    /// Start the task worktree's setup in the background, if its repository has one (task creation).
    pub(crate) fn start_task_setup(&self, state: &mut TaskState) {
        if let Some(setup) = self.setup_config(&state.task) {
            let worktree = PathBuf::from(&state.task.worktree_path);
            if self.start_setup(&state.task, "task", &worktree, setup) {
                state.info("Worktree setup started");
            }
        }
    }

    /// Run setup `name` for `worktree` in the background, unless it is already running.
    fn start_setup(&self, task: &TaskRow, name: &str, worktree: &Path, setup: WorktreeSetup) -> bool {
        let dir = status_dir(task);
        let path = dir.join(format!("{name}.json"));
        let id = py_str(&path);
        let (cancel, cancelled) = tokio::sync::watch::channel(false);
        {
            let mut running = self.setups.lock().unwrap_or_else(|p| p.into_inner());
            if running.contains_key(&id) {
                return false;
            }
            running.insert(id.clone(), (task.jira_key.to_uppercase(), cancel));
        }
        let log = dir.join(format!("{name}.log"));
        let mut status = SetupStatus {
            name: name.into(),
            worktree: py_str(worktree),
            digest: digest(&setup),
            state: SetupState::Running,
            started_at: DbTime::now().isoformat(),
            completed_at: None,
            error: None,
            log_path: py_str(&log),
        };
        let written = std::fs::create_dir_all(&dir)
            .and_then(|_| crate::workspace::write_json(&path, &json!(status)).map(drop))
            .map_err(|e| format!("Could not record worktree setup: {e}"));
        let (engine, git, key) = (self.me.clone(), self.git.clone(), task.jira_key.clone());
        let (root, worktree) = (PathBuf::from(&task.repository_root), worktree.to_path_buf());
        tokio::spawn(async move {
            let outcome = match written {
                Ok(()) => run_setup(git.as_ref(), &root, &worktree, &setup, &log, cancelled).await,
                Err(error) => Err(error),
            };
            status.completed_at = Some(DbTime::now().isoformat());
            (status.state, status.error) = match outcome {
                Ok(()) => (SetupState::Succeeded, None),
                Err(error) => (SetupState::Failed, Some(error)),
            };
            if let Some(engine) = engine.upgrade() {
                engine.finish_setup(&key, &path, status).await;
            }
        });
        true
    }

    async fn finish_setup(&self, key: &str, path: &Path, status: SetupStatus) {
        let lock = self.lock(key);
        let _held = lock.lock().await;
        // A deleted task keeps nothing, not even this status.
        let state =
            self.db.session(|tx| TaskState::load(tx, key)).ok().filter(|s| Path::new(&s.task.task_dir).is_dir());
        // Written before it stops counting as running, so it never reads as interrupted.
        if state.is_some()
            && let Err(error) = crate::workspace::write_json(path, &json!(status))
        {
            tracing::warn!(%error, "could not record worktree setup");
        }
        self.setups.lock().unwrap_or_else(|p| p.into_inner()).remove(&py_str(path));
        let Some(mut state) = state else { return };
        let succeeded = status.state == SetupState::Succeeded;
        match &status.error {
            None => state.info(format!("Worktree setup finished: {}", status.worktree)),
            Some(error) => state.log(
                format!("Worktree setup failed: {error}"),
                None,
                "error",
                Some(json!({"worktree": status.worktree, "log": status.log_path})),
            ),
        }
        let active = state.status() == Some(TaskStatus::Active);
        for index in 0..state.nodes.len() {
            let node = &state.nodes[index];
            let held = node.state == NodeState::Ready.as_str()
                && node.message.as_deref().is_some_and(|m| m.starts_with(WAITING))
                && target_name(&node.key) == status.name;
            if !held {
                continue;
            }
            if !succeeded {
                state.set_state(index, NodeState::Ready, Some(FAILED_MESSAGE), None);
            } else if active && node.execution_mode == "automatic" {
                if let Err(error) = self.on_ready(&mut state, index).await {
                    tracing::warn!(%error, "could not start a node after worktree setup");
                }
            } else {
                state.set_state(index, NodeState::Ready, Some(FINISHED_MESSAGE), None);
            }
        }
        if let Err(error) = self.db.session(|tx| state.flush(tx)) {
            tracing::warn!(%error, "could not record worktree setup");
        }
        let _ = self.publish(key);
    }

    /// Re-run every failed (or outdated) setup of a task, on the human's request.
    pub async fn retry_worktree_setup(&self, key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.status().is_some_and(|s| s == TaskStatus::Cancelled || s.is_post_ready()) {
            return Err(WorkflowError::conflict(format!("Task is {}; setup can no longer run", state.task.status)));
        }
        let setup = self
            .setup_config(&state.task)
            .ok_or_else(|| WorkflowError::conflict("No worktree setup is configured for this repository"))?;
        let digest = digest(&setup);
        let mut retried = Vec::new();
        for status in self.setup_statuses(&state.task).unwrap_or_default() {
            let stale = status.state == SetupState::Succeeded && status.digest != digest;
            if (status.state == SetupState::Failed || stale)
                && self.start_setup(&state.task, &status.name, Path::new(&status.worktree), setup.clone())
            {
                retried.push(status.name);
            }
        }
        if retried.is_empty() {
            return Err(WorkflowError::conflict("No failed worktree setup to retry"));
        }
        for index in 0..state.nodes.len() {
            let node = &state.nodes[index];
            if node.state == NodeState::Ready.as_str()
                && node.message.as_deref().is_some_and(|m| m.starts_with(WAITING))
                && retried.contains(&target_name(&node.key))
            {
                state.set_state(index, NodeState::Ready, Some(RUNNING_MESSAGE), None);
            }
        }
        state.log(format!("Human retried worktree setup ({})", retried.join(", ")), None, "info", None);
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }

    /// Signal every running setup of a task to stop (cancel and delete; before the task lock).
    pub(crate) fn cancel_worktree_setups(&self, key: &str) {
        let key = key.to_uppercase();
        for (task, cancel) in self.setups.lock().unwrap_or_else(|p| p.into_inner()).values() {
            if *task == key {
                cancel.send_replace(true);
            }
        }
    }
}

/// Copy, run and check, writing what happened to `log`. The error is the human-readable reason.
pub async fn run_setup(
    git: &dyn Git,
    root: &Path,
    worktree: &Path,
    setup: &WorktreeSetup,
    log: &Path,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let mut journal = String::new();
    let outcome = async {
        for path in &setup.copy {
            copy_ignored(git, root, worktree, path).await?;
            journal.push_str(&format!("Copied {path}\n"));
        }
        let timeout = Duration::from_secs(setup.timeout_seconds.max(1) as u64);
        for argv in &setup.commands {
            let shown = crate::process::describe(argv);
            journal.push_str(&format!("$ {}\n", argv.join(" ")));
            // A Windows .cmd shim (npm, yarn) is found through PATHEXT, as a shell would.
            let mut resolved = argv.clone();
            if let Some(found) = crate::process::resolve_executable(&argv[0]) {
                resolved[0] = py_str(&found);
            }
            let (code, output) = verification::run_command(&resolved, worktree, timeout, &mut cancel).await.map_err(
                |error| match error.as_str() {
                    verification::CANCELLED => "Cancelled".to_string(),
                    "TimeoutError" => {
                        format!("`{shown}` did not finish within {} seconds", setup.timeout_seconds)
                    }
                    _ => format!("`{shown}` could not be started"),
                },
            )?;
            journal.push_str(&output);
            if !output.ends_with('\n') {
                journal.push('\n');
            }
            if code != 0 {
                return Err(format!("`{shown}` exited with code {code}"));
            }
        }
        let changed = git.dirty_paths(worktree, false).await.map_err(|e| e.to_string())?;
        if !changed.is_empty() {
            let mut listed = changed.iter().take(5).map(|l| l.get(3..).unwrap_or(l).to_string()).collect::<Vec<_>>();
            if changed.len() > 5 {
                listed.push(format!("and {} more", changed.len() - 5));
            }
            return Err(format!(
                "Setup changed files that Git tracks or does not ignore ({}); inspect the worktree, then retry",
                listed.join(", ")
            ));
        }
        Ok(())
    }
    .await;
    if let Err(error) = &outcome {
        journal.push_str(&format!("Failed: {error}\n"));
    }
    if let Err(error) = std::fs::write(log, journal) {
        tracing::warn!(%error, "could not write the worktree setup log");
    }
    outcome
}

/// Copy one configured path from the repository checkout to the same place in the worktree. The
/// source must resolve inside the repository and the destination must be ignored by Git there, so
/// a tracked file is never replaced and drift detection never sees the copy.
async fn copy_ignored(git: &dyn Git, root: &Path, worktree: &Path, path: &str) -> Result<(), String> {
    if let Some(problem) = crate::config::copy_path_problem(path) {
        return Err(format!("{problem}: {path}"));
    }
    let parts: Vec<&str> = path.trim().split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    let relative: PathBuf = parts.iter().collect();
    let posix = parts.join("/");
    let source = root.join(&relative);
    let metadata = std::fs::metadata(&source).map_err(|_| format!("Setup copy source is missing: {posix}"))?;
    if !is_within(&resolve(&source), &resolve(root)) {
        return Err(format!("Setup copy source {posix} resolves outside the repository"));
    }
    let query = if metadata.is_dir() { format!("{posix}/") } else { posix.clone() };
    if !git.is_ignored(worktree, &query).await.map_err(|e| e.to_string())? {
        return Err(format!(
            "Setup copy destination {posix} is not ignored by Git in the worktree; only ignored paths can be copied"
        ));
    }
    let destination = worktree.join(&relative);
    if !is_within(&resolve(&destination), &resolve(worktree)) {
        return Err(format!("Setup copy destination {posix} resolves outside the worktree"));
    }
    if std::fs::symlink_metadata(&destination).is_ok_and(|m| is_link(&m)) {
        return Err(format!("Setup copy destination {posix} is a link"));
    }
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Could not copy {posix}: {e}"))?;
        }
        copy_tree(&source, &destination).map_err(|e| format!("Could not copy {posix}: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A symlink, junction or other reparse point: never followed inside a copied directory.
fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    if !std::fs::metadata(source)?.is_dir() {
        std::fs::copy(source, destination)?;
        return Ok(());
    }
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if is_link(&metadata) {
            return Err(std::io::Error::other(format!("{} is a link; links are not copied", entry.path().display())));
        }
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}
