//! Read-only polling; never holds a task lock or SQLite transaction during a gh lookup.
use super::{Engine, WorkflowError};
use crate::db::{TaskRow, table};
use crate::github::{GitHubError, GitHubSource, is_pr_url, pick_pull_request, repo_from_remote};
use std::path::Path;
use std::sync::Arc;

impl Engine {
    pub fn set_github_source(&self, source: Arc<dyn GitHubSource>) {
        *self.github_source.write().unwrap_or_else(|p| p.into_inner()) = source;
    }
    pub fn start_pr_sync(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            loop {
                let Some(engine) = weak.upgrade() else { return };
                let settings = engine.config().github.clone();
                if settings.pr_sync
                    && let Err(error) = engine.poll_prs().await
                {
                    tracing::warn!(%error,"GitHub PR sync cycle failed");
                }
                drop(engine);
                tokio::time::sleep(std::time::Duration::from_secs(settings.poll_interval_seconds.max(1) as u64)).await;
            }
        });
        if let Some(previous) = self.pr_sync_worker.lock().unwrap_or_else(|p| p.into_inner()).replace(handle) {
            previous.abort();
        }
    }
    pub fn pr_sync_problems(&self) -> std::collections::HashMap<Option<String>, String> {
        self.pr_sync_problems.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
    fn report_pr_problem(&self, key: Option<&str>, problem: Option<&str>) {
        let mut problems = self.pr_sync_problems.lock().unwrap_or_else(|p| p.into_inner());
        let key = key.map(String::from);
        if problems.get(&key).map(String::as_str) == problem {
            return;
        }
        if let Some(problem) = problem {
            problems.insert(key.clone(), problem.into());
            tracing::warn!(task=?key,%problem,"GitHub PR sync");
        } else {
            problems.remove(&key);
            tracing::info!(task=?key,"GitHub PR sync is working again");
        }
    }
    pub async fn poll_prs(&self) -> Result<(), WorkflowError> {
        let settings = self.config().github.clone();
        let cli = self.github_source.read().unwrap_or_else(|p| p.into_inner()).clone();
        let rows:Vec<TaskRow> = self.db.session(|tx|table::select(tx,
            "WHERE pr_sync_paused = 0 AND (status IN ('READY_FOR_PR', 'PR_CREATED') OR (status = 'MERGED' AND merged_head_sha IS NULL AND pr_url IS NOT NULL)) ORDER BY id",[]))?;
        for task in rows {
            if task.status == "MERGED" && !Path::new(&task.worktree_path).exists() {
                continue;
            }
            let lookup = async {
                let answer = if ["PR_CREATED", "MERGED"].contains(&task.status.as_str()) && task.pr_url.is_some() {
                    let url = task.pr_url.as_deref().unwrap();
                    if !is_pr_url(url, &settings.hosts) {
                        return Ok(None);
                    }
                    Some(cli.pull_request(&settings, url).await?)
                } else {
                    let remote = self
                        .git
                        .remote_url(Path::new(&task.repository_root), "origin")
                        .await
                        .map_err(|e| GitHubError::lookup(e.to_string()))?;
                    let Some(repo) = remote.and_then(|r| repo_from_remote(&r, &settings.hosts)) else {
                        return Ok(None);
                    };
                    pick_pull_request(
                        &cli.pull_requests_for_branch(&settings, &repo, &task.branch_name).await?,
                        &task.created_at,
                    )
                };
                self.report_pr_problem(None, None);
                Ok::<_, GitHubError>(answer)
            }
            .await;
            match lookup {
                Err(error) if error.unavailable => {
                    self.report_pr_problem(None, Some(&error.message));
                    return Ok(());
                }
                Err(error) => self.report_pr_problem(Some(&task.jira_key), Some(&error.message)),
                Ok(pr) => {
                    self.report_pr_problem(Some(&task.jira_key), None);
                    if let Some(pr) = pr
                        && let Err(error) = self.apply_github_pr(&task.jira_key, &pr).await
                        && error.status != 404
                    {
                        self.report_pr_problem(Some(&task.jira_key), Some(&error.to_string()));
                    }
                }
            }
        }
        Ok(())
    }
}
