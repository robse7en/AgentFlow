//! The git operations the engine performs, as a trait: [`crate::git::GitService`] in the app, and
//! [`FakeGit`] in tests, which port the Python engine tests that replaced these calls with mocks
//! (to make a removal fail once, report a moved branch, and so on).

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::git::{CreatedWorktree, GitError, GitService, RepositoryInfo, Result};

#[async_trait::async_trait]
pub trait Git: Send + Sync {
    async fn remote_url(&self, root: &Path, name: &str) -> Result<Option<String>>;
    async fn inspect_repository(&self, path: &str) -> Result<RepositoryInfo>;
    async fn directories(&self, root: &Path) -> Result<Vec<String>>;
    async fn resolve_base_ref(&self, root: &Path, base_branch: &str) -> Result<String>;
    async fn create_worktree(
        &self,
        root: &Path,
        worktree_path: &Path,
        branch: &str,
        base_branch: &str,
    ) -> Result<CreatedWorktree>;
    async fn head_sha(&self, cwd: &Path) -> Result<String>;
    async fn verify_worker_worktree(&self, root: &Path, path: &Path, branch: &str) -> Result<()>;
    async fn create_worker_worktree(&self, root: &Path, path: &Path, branch: &str, sha: &str) -> Result<()>;
    async fn merge_worker_checkpoint(&self, path: &Path, sha: &str) -> Result<()>;
    async fn changed_paths(&self, path: &Path, base: &str, sha: &str) -> Result<Vec<String>>;
    async fn is_ancestor(&self, path: &Path, ancestor: &str, sha: &str) -> Result<bool>;
    async fn is_ignored(&self, cwd: &Path, relative: &str) -> Result<bool>;
    async fn is_dirty(&self, cwd: &Path) -> Result<bool>;
    async fn dirty_paths(&self, cwd: &Path, ignored: bool) -> Result<Vec<String>>;
    async fn reset_hard(&self, cwd: &Path, sha: &str) -> Result<()>;
    async fn checkpoint(&self, cwd: &Path, message: &str, commit: bool) -> Result<(String, bool)>;
    #[allow(clippy::too_many_arguments)]
    async fn cleanup_worktree(
        &self,
        root: &Path,
        path: &Path,
        branch: &str,
        expected_sha: &str,
        worktree_dir: Option<&Path>,
        merged_head_sha: Option<&str>,
        force: bool,
    ) -> Result<Vec<String>>;
    async fn remove_task_worktree(&self, root: &Path, path: &Path, worktree_dir: &Path) -> Result<Vec<String>>;
    async fn branch_sha(&self, root: &Path, branch: &str) -> Result<Option<String>>;
    async fn push_branch(&self, cwd: &Path, branch: &str) -> Result<()>;
    async fn branch_report(&self, root: &Path, branch: &str, base_branch: &str) -> Result<Value>;
    async fn delete_branch(&self, root: &Path, branch: &str) -> Result<bool>;
    async fn discard_worktree(&self, root: &Path, path: &Path) -> Result<Vec<String>>;
    async fn file_at(&self, root: &Path, sha: &str, path: &str) -> Result<Option<String>>;
    async fn fork_point(&self, root: &Path, sha: &str, base_branch: &str) -> Result<Option<String>>;
    async fn file_diff(&self, root: &Path, from: &str, to: &str, path: &str) -> Result<String>;
}

#[async_trait::async_trait]
impl Git for GitService {
    async fn remote_url(&self, root: &Path, name: &str) -> Result<Option<String>> {
        GitService::remote_url(self, root, name).await
    }
    async fn inspect_repository(&self, path: &str) -> Result<RepositoryInfo> {
        GitService::inspect_repository(self, path).await
    }
    async fn directories(&self, root: &Path) -> Result<Vec<String>> {
        GitService::directories(self, root).await
    }
    async fn resolve_base_ref(&self, root: &Path, base_branch: &str) -> Result<String> {
        GitService::resolve_base_ref(self, root, base_branch).await
    }
    async fn create_worktree(
        &self,
        root: &Path,
        worktree_path: &Path,
        branch: &str,
        base_branch: &str,
    ) -> Result<CreatedWorktree> {
        GitService::create_worktree(self, root, worktree_path, branch, base_branch).await
    }
    async fn head_sha(&self, cwd: &Path) -> Result<String> {
        GitService::head_sha(self, cwd).await
    }
    async fn verify_worker_worktree(&self, root: &Path, path: &Path, branch: &str) -> Result<()> {
        GitService::verify_worker_worktree(self, root, path, branch).await
    }
    async fn create_worker_worktree(&self, root: &Path, path: &Path, branch: &str, sha: &str) -> Result<()> {
        GitService::create_worker_worktree(self, root, path, branch, sha).await
    }
    async fn merge_worker_checkpoint(&self, path: &Path, sha: &str) -> Result<()> {
        GitService::merge_worker_checkpoint(self, path, sha).await
    }
    async fn changed_paths(&self, path: &Path, base: &str, sha: &str) -> Result<Vec<String>> {
        GitService::changed_paths(self, path, base, sha).await
    }
    async fn is_ancestor(&self, path: &Path, ancestor: &str, sha: &str) -> Result<bool> {
        GitService::is_ancestor(self, path, ancestor, sha).await
    }
    async fn is_ignored(&self, cwd: &Path, relative: &str) -> Result<bool> {
        GitService::is_ignored(self, cwd, relative).await
    }
    async fn is_dirty(&self, cwd: &Path) -> Result<bool> {
        GitService::is_dirty(self, cwd).await
    }
    async fn dirty_paths(&self, cwd: &Path, ignored: bool) -> Result<Vec<String>> {
        GitService::dirty_paths(self, cwd, ignored).await
    }
    async fn reset_hard(&self, cwd: &Path, sha: &str) -> Result<()> {
        GitService::reset_hard(self, cwd, sha).await
    }
    async fn checkpoint(&self, cwd: &Path, message: &str, commit: bool) -> Result<(String, bool)> {
        GitService::checkpoint(self, cwd, message, commit).await
    }
    async fn cleanup_worktree(
        &self,
        root: &Path,
        path: &Path,
        branch: &str,
        expected_sha: &str,
        worktree_dir: Option<&Path>,
        merged_head_sha: Option<&str>,
        force: bool,
    ) -> Result<Vec<String>> {
        GitService::cleanup_worktree(self, root, path, branch, expected_sha, worktree_dir, merged_head_sha, force).await
    }
    async fn remove_task_worktree(&self, root: &Path, path: &Path, worktree_dir: &Path) -> Result<Vec<String>> {
        GitService::remove_task_worktree(self, root, path, worktree_dir).await
    }
    async fn branch_sha(&self, root: &Path, branch: &str) -> Result<Option<String>> {
        GitService::branch_sha(self, root, branch).await
    }
    async fn push_branch(&self, cwd: &Path, branch: &str) -> Result<()> {
        GitService::push_branch(self, cwd, branch).await
    }
    async fn branch_report(&self, root: &Path, branch: &str, base_branch: &str) -> Result<Value> {
        GitService::branch_report(self, root, branch, base_branch).await
    }
    async fn delete_branch(&self, root: &Path, branch: &str) -> Result<bool> {
        GitService::delete_branch(self, root, branch).await
    }
    async fn discard_worktree(&self, root: &Path, path: &Path) -> Result<Vec<String>> {
        GitService::discard_worktree(self, root, path).await
    }
    async fn file_at(&self, root: &Path, sha: &str, path: &str) -> Result<Option<String>> {
        GitService::file_at(self, root, sha, path).await
    }
    async fn fork_point(&self, root: &Path, sha: &str, base_branch: &str) -> Result<Option<String>> {
        GitService::fork_point(self, root, sha, base_branch).await
    }
    async fn file_diff(&self, root: &Path, from: &str, to: &str, path: &str) -> Result<String> {
        GitService::file_diff(self, root, from, to, path).await
    }
}

/// What a fake git call answers.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The call's return value, as JSON (`json!(null)` for unit).
    Ok(Value),
    Failed(String),
    /// [`GitError::Busy`]: the worktree directory could not be deleted.
    Busy(String),
    /// [`GitError::Unverified`]: the task branch isn't reviewed or merged.
    Unverified(String),
}

#[derive(Default)]
struct Answers {
    /// Answered first, one per call.
    queue: VecDeque<Outcome>,
    /// Answered once the queue is empty; unset: the return type's default.
    sticky: Option<Outcome>,
}

/// A git that records calls and answers as configured (Python's AsyncMock).
#[derive(Default)]
pub struct FakeGit {
    answers: Mutex<HashMap<String, Answers>>,
    /// (method, arguments) of every call.
    calls: Mutex<Vec<(String, Vec<String>)>>,
    /// Run when a method is called, before it answers (Python's `side_effect`).
    hooks: Mutex<HashMap<String, Box<dyn Fn() + Send + Sync>>>,
}

impl FakeGit {
    /// Every later call of `method` runs `hook` first.
    pub fn before(&self, method: &str, hook: impl Fn() + Send + Sync + 'static) {
        self.hooks.lock().unwrap().insert(method.into(), Box::new(hook));
    }

    /// Every later call of `method` answers this.
    pub fn returns(&self, method: &str, outcome: Outcome) {
        self.answers.lock().unwrap().entry(method.into()).or_default().sticky = Some(outcome);
    }

    /// The next call of `method` answers this (before any sticky answer).
    pub fn then(&self, method: &str, outcome: Outcome) {
        self.answers.lock().unwrap().entry(method.into()).or_default().queue.push_back(outcome);
    }

    /// The names of every method called so far, in order.
    pub fn methods(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(method, _)| method.clone()).collect()
    }

    /// The arguments of each call of one method.
    pub fn calls_of(&self, method: &str) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, args)| args.clone()).collect()
    }

    fn answer<T: DeserializeOwned + Default>(&self, method: &str, args: &[&str]) -> Result<T> {
        self.calls.lock().unwrap().push((method.into(), args.iter().map(|a| a.to_string()).collect()));
        if let Some(hook) = self.hooks.lock().unwrap().get(method) {
            hook();
        }
        let outcome = {
            let mut answers = self.answers.lock().unwrap();
            let entry = answers.entry(method.into()).or_default();
            entry.queue.pop_front().or_else(|| entry.sticky.clone())
        };
        match outcome {
            None => Ok(T::default()),
            Some(Outcome::Ok(value)) => serde_json::from_value(value)
                .map_err(|e| GitError::Failed(format!("FakeGit {method}: answer does not fit: {e}"))),
            Some(Outcome::Failed(message)) => Err(GitError::Failed(message)),
            Some(Outcome::Busy(message)) => Err(GitError::Busy(message)),
            Some(Outcome::Unverified(message)) => Err(GitError::Unverified(message)),
        }
    }
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[async_trait::async_trait]
impl Git for FakeGit {
    async fn remote_url(&self, root: &Path, name: &str) -> Result<Option<String>> {
        self.answer("remote_url", &[&s(root), name])
    }
    async fn inspect_repository(&self, path: &str) -> Result<RepositoryInfo> {
        self.answer("inspect_repository", &[path])
    }
    async fn directories(&self, root: &Path) -> Result<Vec<String>> {
        self.answer("directories", &[&s(root)])
    }
    async fn resolve_base_ref(&self, root: &Path, base_branch: &str) -> Result<String> {
        self.answer("resolve_base_ref", &[&s(root), base_branch])
    }
    async fn create_worktree(
        &self,
        root: &Path,
        worktree_path: &Path,
        branch: &str,
        base_branch: &str,
    ) -> Result<CreatedWorktree> {
        self.answer("create_worktree", &[&s(root), &s(worktree_path), branch, base_branch])
    }
    async fn head_sha(&self, cwd: &Path) -> Result<String> {
        self.answer("head_sha", &[&s(cwd)])
    }
    async fn verify_worker_worktree(&self, root: &Path, path: &Path, branch: &str) -> Result<()> {
        self.answer::<Option<()>>("verify_worker_worktree", &[&s(root), &s(path), branch]).map(drop)
    }
    async fn create_worker_worktree(&self, root: &Path, path: &Path, branch: &str, sha: &str) -> Result<()> {
        self.answer::<Option<()>>("create_worker_worktree", &[&s(root), &s(path), branch, sha]).map(drop)
    }
    async fn merge_worker_checkpoint(&self, path: &Path, sha: &str) -> Result<()> {
        self.answer::<Option<()>>("merge_worker_checkpoint", &[&s(path), sha]).map(drop)
    }
    async fn changed_paths(&self, path: &Path, base: &str, sha: &str) -> Result<Vec<String>> {
        self.answer("changed_paths", &[&s(path), base, sha])
    }
    async fn is_ancestor(&self, path: &Path, ancestor: &str, sha: &str) -> Result<bool> {
        self.answer("is_ancestor", &[&s(path), ancestor, sha])
    }
    async fn is_ignored(&self, cwd: &Path, relative: &str) -> Result<bool> {
        self.answer("is_ignored", &[&s(cwd), relative])
    }
    async fn is_dirty(&self, cwd: &Path) -> Result<bool> {
        self.answer("is_dirty", &[&s(cwd)])
    }
    async fn dirty_paths(&self, cwd: &Path, ignored: bool) -> Result<Vec<String>> {
        self.answer("dirty_paths", &[&s(cwd), if ignored { "ignored" } else { "" }])
    }
    async fn reset_hard(&self, cwd: &Path, sha: &str) -> Result<()> {
        self.answer::<Option<()>>("reset_hard", &[&s(cwd), sha]).map(drop)
    }
    async fn checkpoint(&self, cwd: &Path, message: &str, commit: bool) -> Result<(String, bool)> {
        self.answer("checkpoint", &[&s(cwd), message, if commit { "commit" } else { "" }])
    }
    async fn cleanup_worktree(
        &self,
        root: &Path,
        path: &Path,
        branch: &str,
        expected_sha: &str,
        worktree_dir: Option<&Path>,
        merged_head_sha: Option<&str>,
        force: bool,
    ) -> Result<Vec<String>> {
        let worktree_dir = worktree_dir.map(s).unwrap_or_default();
        let merged = merged_head_sha.unwrap_or_default();
        let force = if force { "force" } else { "" };
        self.answer("cleanup_worktree", &[&s(root), &s(path), branch, expected_sha, &worktree_dir, merged, force])
    }
    async fn remove_task_worktree(&self, root: &Path, path: &Path, worktree_dir: &Path) -> Result<Vec<String>> {
        self.answer("remove_task_worktree", &[&s(root), &s(path), &s(worktree_dir)])
    }
    async fn branch_sha(&self, root: &Path, branch: &str) -> Result<Option<String>> {
        self.answer("branch_sha", &[&s(root), branch])
    }
    async fn push_branch(&self, cwd: &Path, branch: &str) -> Result<()> {
        self.answer::<Option<()>>("push_branch", &[&s(cwd), branch]).map(drop)
    }
    async fn branch_report(&self, root: &Path, branch: &str, base_branch: &str) -> Result<Value> {
        self.answer("branch_report", &[&s(root), branch, base_branch])
    }
    async fn delete_branch(&self, root: &Path, branch: &str) -> Result<bool> {
        self.answer("delete_branch", &[&s(root), branch])
    }
    async fn discard_worktree(&self, root: &Path, path: &Path) -> Result<Vec<String>> {
        self.answer("discard_worktree", &[&s(root), &s(path)])
    }
    async fn file_at(&self, root: &Path, sha: &str, path: &str) -> Result<Option<String>> {
        self.answer("file_at", &[&s(root), sha, path])
    }
    async fn fork_point(&self, root: &Path, sha: &str, base_branch: &str) -> Result<Option<String>> {
        self.answer("fork_point", &[&s(root), sha, base_branch])
    }
    async fn file_diff(&self, root: &Path, from: &str, to: &str, path: &str) -> Result<String> {
        self.answer("file_diff", &[&s(root), from, to, path])
    }
}
