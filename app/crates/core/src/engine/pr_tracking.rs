//! Human PR bookkeeping and forward-only reconciliation with GitHub.
use super::lifecycle::git_error;
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::db::{DbTime, TaskRow};
use crate::github::{PullRequest, compare_url, is_pr_url, repo_from_remote};
use serde_json::{Value, json};
use std::path::Path;

pub fn clean_pr_url(value: &str) -> Result<String, String> {
    let value = super::prompts::py_strip(value);
    if !value.starts_with("http://") && !value.starts_with("https://") {
        return Err("PR URL must start with http:// or https://".into());
    }
    if !(8..=2000).contains(&value.chars().count()) || !value.chars().all(super::agents::py_isprintable) {
        return Err("PR URL must be one line of 8 to 2000 characters".into());
    }
    Ok(value.into())
}
impl Engine {
    pub async fn mark_pr_created(&self, key: &str, url: &str) -> Result<Value, WorkflowError> {
        let url = clean_pr_url(url).map_err(WorkflowError::bad_request)?;
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.task.status != "READY_FOR_PR" {
            return Err(WorkflowError::conflict(format!(
                "Task must be Ready for PR to mark a PR created (currently {})",
                state.task.status
            )));
        }
        state.task.status = "PR_CREATED".into();
        state.task.pr_url = Some(url.clone());
        state.task.pr_created_at = Some(DbTime::now());
        state.task.pr_base_branch = None;
        state.task.pr_sync_paused = false;
        state.task.status_message = Some(format!("PR created: {url}. Mark it Merged once it lands."));
        state.log("Human marked the pull request as created", None, "info", Some(json!({"prUrl":url})));
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.transition_jira_for_pr(key).await;
        self.publish(key)
    }
    pub async fn revert_pr_created(&self, key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.task.status != "PR_CREATED" {
            return Err(WorkflowError::conflict(format!(
                "Task is not marked PR Created (currently {})",
                state.task.status
            )));
        }
        state.task.status = "READY_FOR_PR".into();
        state.task.pr_url = None;
        state.task.pr_created_at = None;
        state.task.pr_base_branch = None;
        let pause = self.pause_pr_sync(&mut state.task);
        state.task.status_message =
            Some(format!("Ready for PR. Push branch {} and open the pull request.{pause}", state.task.branch_name));
        state.log("Human reverted the PR-created status", None, "warning", None);
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }
    pub async fn mark_pr_merged(&self, key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.task.status != "PR_CREATED" {
            return Err(WorkflowError::conflict(format!(
                "Task must have a PR created before it can be marked Merged (currently {})",
                state.task.status
            )));
        }
        state.task.status = "MERGED".into();
        state.task.merged_at = Some(DbTime::now());
        state.task.pr_sync_paused = false;
        state.task.status_message = Some("Merged. Clean up the worktree when you're ready.".into());
        state.info("Human marked the pull request as merged");
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }
    pub async fn revert_pr_merged(&self, key: &str) -> Result<Value, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.task.status != "MERGED" {
            return Err(WorkflowError::conflict(format!(
                "Task is not marked Merged (currently {})",
                state.task.status
            )));
        }
        state.task.status = "PR_CREATED".into();
        state.task.merged_at = None;
        state.task.merged_head_sha = None;
        let mut note = state.task.pr_url.as_ref().map_or("PR created. Mark it Merged once it lands.".into(), |u| {
            format!("PR created: {u}. Mark it Merged once it lands.")
        });
        note += &self.pause_pr_sync(&mut state.task);
        state.task.status_message = Some(note);
        state.log("Human reverted the Merged status", None, "warning", None);
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        self.publish(key)
    }
    fn pause_pr_sync(&self, task: &mut TaskRow) -> String {
        if !self.config().github.pr_sync {
            return String::new();
        }
        task.pr_sync_paused = true;
        " GitHub PR sync is paused for this task until you mark the PR yourself.".into()
    }
    pub async fn apply_github_pr(&self, key: &str, pr: &PullRequest) -> Result<Option<Value>, WorkflowError> {
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if state.task.pr_sync_paused {
            return Ok(None);
        }
        let tracked = |t: &TaskRow| {
            (t.status.clone(), t.status_message.clone(), t.merged_head_sha.clone(), t.pr_base_branch.clone())
        };
        let before = tracked(&state.task);
        let open = format!("PR created: {}. It is marked Merged when GitHub shows it merged.", pr.url);
        let closed = format!(
            "PR closed on GitHub without merging: {}. Reopen it, or undo PR Created and mark the replacement PR.",
            pr.url
        );
        if state.task.status == "READY_FOR_PR" && ["OPEN", "MERGED"].contains(&pr.state.as_str()) {
            state.task.status = "PR_CREATED".into();
            state.task.pr_url = Some(pr.url.clone());
            state.task.pr_created_at = Some(pr.created_at);
            state.task.status_message = Some(open.clone());
            state.log("Detected the pull request on GitHub", None, "info", Some(json!({"prUrl":pr.url})));
        }
        let recorded = state
            .task
            .pr_url
            .as_deref()
            .unwrap_or(&pr.url)
            .trim_end_matches('/')
            .eq_ignore_ascii_case(pr.url.trim_end_matches('/'));
        if state.task.status == "PR_CREATED" && recorded {
            if pr.state == "MERGED" {
                state.task.status = "MERGED".into();
                state.task.merged_at = Some(pr.merged_at.unwrap_or_else(DbTime::now));
                state.task.status_message = Some("Merged on GitHub. Clean up the worktree when you're ready.".into());
                state.log("Detected the pull request merge on GitHub", None, "info", Some(json!({"prUrl":pr.url})));
            } else if pr.state == "CLOSED" && state.task.status_message.as_ref() != Some(&closed) {
                state.task.status_message = Some(closed.clone());
                state.log(
                    "The pull request was closed on GitHub without merging",
                    None,
                    "warning",
                    Some(json!({"prUrl":pr.url})),
                );
            } else if pr.state == "OPEN" && state.task.status_message.as_ref() == Some(&closed) {
                state.task.status_message = Some(open);
                state.log("The pull request was reopened on GitHub", None, "info", Some(json!({"prUrl":pr.url})));
            }
        }
        if state.task.status == "MERGED"
            && recorded
            && pr.state == "MERGED"
            && pr.head_sha.is_some()
            && state.task.merged_head_sha != pr.head_sha
        {
            state.task.merged_head_sha = pr.head_sha.clone();
            if before.0 == "MERGED" {
                state.log(
                    "Recorded the merged pull request's head commit from GitHub",
                    None,
                    "info",
                    Some(json!({"prUrl":pr.url,"sha":pr.head_sha})),
                );
            }
        }
        // Where the PR merges into: the human may have opened it against the wrong branch.
        if ["PR_CREATED", "MERGED"].contains(&state.task.status.as_str())
            && recorded
            && pr.base_branch.is_some()
            && state.task.pr_base_branch != pr.base_branch
        {
            state.task.pr_base_branch = pr.base_branch.clone();
            let target = pr.base_branch.as_deref().unwrap_or_default();
            if target != state.task.base_branch {
                let message =
                    format!("The pull request targets {target}, but the task started from {}", state.task.base_branch);
                state.log(message, None, "warning", Some(json!({"prUrl":pr.url,"prBaseBranch":target})));
            }
        }
        if tracked(&state.task) == before {
            return Ok(None);
        }
        // Not when the PR was found already merged: that ticket is past code review.
        let opened = before.0 == "READY_FOR_PR" && state.task.status == "PR_CREATED";
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        if opened {
            self.transition_jira_for_pr(key).await;
        }
        Ok(Some(self.publish(key)?))
    }
    /// Push the reviewed branch and return GitHub's new-PR form with the task's base branch
    /// already chosen as the target. No lock is held over the push, which can take a while.
    pub async fn open_pr(&self, key: &str) -> Result<Value, WorkflowError> {
        let task = self.db.session(|tx| TaskState::load(tx, key))?.task;
        if task.status != "READY_FOR_PR" {
            return Err(WorkflowError::conflict(format!(
                "Task must be Ready for PR to open a pull request (currently {})",
                task.status
            )));
        }
        let root = Path::new(&task.repository_root);
        let remote = self.git.remote_url(root, "origin").await.map_err(|e| git_error(e, 409))?;
        let Some(repo) = remote.and_then(|r| repo_from_remote(&r, &self.config().github.hosts)) else {
            return Err(WorkflowError::conflict(format!(
                "origin is not on a GitHub host AgentFlow knows (github.hosts). Push {} and open the PR into {} yourself.",
                task.branch_name, task.base_branch
            )));
        };
        // Only what final review saw goes out; newer commits are the human's to push.
        let tip = self.git.branch_sha(root, &task.branch_name).await.map_err(|e| git_error(e, 409))?;
        if tip.is_none() || tip != task.current_sha {
            return Err(WorkflowError::conflict(format!(
                "{} is not at the reviewed checkpoint. Push it yourself if you mean to include newer commits.",
                task.branch_name
            )));
        }
        let worktree = Path::new(&task.worktree_path);
        let cwd = if worktree.is_dir() { worktree } else { root };
        self.git.push_branch(cwd, &task.branch_name).await.map_err(|e| git_error(e, 502))?;
        let url = compare_url(&repo, &task.base_branch, &task.branch_name);
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        state.log(
            format!("Pushed {} to origin to open a pull request into {}", task.branch_name, task.base_branch),
            None,
            "info",
            Some(json!({"compareUrl":url})),
        );
        self.db.session(|tx| state.flush(tx))?;
        drop(held);
        Ok(json!({"url": url, "task": self.publish(key)?}))
    }
    /// Point the recorded PR at the branch the task started from (`gh pr edit --base`).
    pub async fn retarget_pr(&self, key: &str) -> Result<Value, WorkflowError> {
        let task = self.db.session(|tx| TaskState::load(tx, key))?.task;
        if task.status != "PR_CREATED" {
            return Err(WorkflowError::conflict(format!(
                "Task must have a PR created to retarget it (currently {})",
                task.status
            )));
        }
        let settings = self.config().github.clone();
        let Some(url) = task.pr_url.clone().filter(|u| is_pr_url(u, &settings.hosts)) else {
            return Err(WorkflowError::conflict(
                "The recorded PR link is not a pull request on a GitHub host AgentFlow knows (github.hosts)",
            ));
        };
        let source = self.github_source.read().unwrap_or_else(|p| p.into_inner()).clone();
        source
            .retarget(&settings, &url, &task.base_branch)
            .await
            .map_err(|e| WorkflowError::new(format!("Could not retarget the PR: {e}"), 502))?;
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        // The human may have undone PR Created while gh ran; record only against the same PR.
        if state.task.pr_url.as_deref() == Some(url.as_str()) {
            state.task.pr_base_branch = Some(task.base_branch.clone());
            state.log(
                format!("Human retargeted the pull request to {}", task.base_branch),
                None,
                "info",
                Some(json!({"prUrl":url})),
            );
            self.db.session(|tx| state.flush(tx))?;
        }
        drop(held);
        self.publish(key)
    }
}
