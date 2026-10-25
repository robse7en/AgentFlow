//! Infrastructure-level Git operations owned by AgentFlow, ported from
//! `backend/agentflow/services/git_workspace.py`. Agents use their own Git tooling for
//! development work; this module only creates worktrees, branches and checkpoints, and removes
//! them again under strict containment rules.
//!
//! Error messages and diagnostic event names are the Python ones: the messages reach the UI as
//! API `detail` text.
//!
//! One deliberate difference: Python refused any path where `path.resolve() != path.absolute()`,
//! meant to catch links but also rejecting 8.3 short names and paths typed in another case than
//! on disk. Here a path is refused when it has a `..` component or a symlink/junction among its
//! existing ancestors ([`paths::linked`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::diagnostics::event;
use crate::paths;
use crate::process::{CommandResult, ProcessError, resolve_executable, run};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("{0}")]
    Failed(String),
    /// The worktree directory could not be deleted, typically because a process still has it
    /// (or something inside it) open. On Windows that includes being a working directory.
    #[error("{0}")]
    Busy(String),
    /// Cleanup refused: the task branch has commits that are neither the reviewed checkpoint
    /// nor known to be in a merged pull request (e.g. it was merged into a local branch).
    #[error("{0}")]
    Unverified(String),
    #[error(transparent)]
    Process(#[from] ProcessError),
}

fn failed<T>(message: impl Into<String>) -> Result<T> {
    Err(GitError::Failed(message.into()))
}

fn unverified<T>() -> Result<T> {
    Err(GitError::Unverified(
        "Task branch changed since final review and doesn't match a merged pull request; worktree cleanup stopped"
            .into(),
    ))
}

/// Cleanup refused for a reason the human can overrule with force (the branch or checkout can't be
/// vouched for, but nothing is lost: the branch and its commits are kept).
fn unverified_because<T>(why: impl Into<String>) -> Result<T> {
    Err(GitError::Unverified(why.into()))
}

pub type Result<T> = std::result::Result<T, GitError>;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepositoryInfo {
    pub path: String,
    pub root: String,
    pub current_branch: Option<String>,
    pub branches: Vec<String>,
}

/// A new task worktree: the branch actually used (suffixed `-N` on collision) and, when origin
/// couldn't be fetched, a warning that the base may be out of date.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CreatedWorktree {
    pub branch: String,
    pub base_warning: Option<String>,
}

/// One `git worktree list --porcelain` record: `worktree`, `HEAD`, `branch`, `locked`, ...
pub type WorktreeEntry = IndexMap<String, String>;

const TIMEOUT: Duration = Duration::from_secs(120);
const SHORT: Duration = Duration::from_secs(30);
/// A push may run the repository's pre-push hooks.
const PUSH_TIMEOUT: Duration = Duration::from_secs(600);
const IDENTITY: [&str; 4] = ["-c", "user.name=AgentFlow", "-c", "user.email=agentflow@local"];

pub struct GitService {
    pub command: String,
}

impl Default for GitService {
    fn default() -> Self {
        GitService { command: "git".into() }
    }
}

/// A path that exists, or is a (possibly dangling) symlink.
fn present(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn trimmed(result: &CommandResult) -> &str {
    result.stdout.trim()
}

/// stderr, else stdout, trimmed: what a failed git command said.
fn said(result: &CommandResult) -> &str {
    if result.stderr.is_empty() { result.stdout.trim() } else { result.stderr.trim() }
}

impl GitService {
    pub fn executable(&self) -> Option<PathBuf> {
        resolve_executable(&self.command)
    }

    async fn git(&self, args: &[&str], cwd: Option<&Path>, timeout: Duration) -> Result<CommandResult> {
        let Some(exe) = self.executable() else {
            return failed("Git executable not found on PATH");
        };
        // core.longpaths: a no-op outside Windows; on Windows required to touch (and delete) files
        // deeper than MAX_PATH, easily reached once ignored build output is in scope.
        let mut argv = vec![exe.to_string_lossy().into_owned(), "-c".into(), "core.longpaths=true".into()];
        argv.extend(args.iter().map(|a| a.to_string()));
        let result = run(&argv, cwd, &HashMap::new(), timeout, None).await?;
        if !result.ok() {
            // Arguments and output may contain commit text or credentials.
            event("git.command.nonzero", json!({"operation": args.first(), "exitCode": result.returncode}));
        }
        Ok(result)
    }

    pub async fn version(&self) -> Option<String> {
        self.git(&["--version"], None, TIMEOUT).await.ok().map(|r| trimmed(&r).to_string())
    }

    pub async fn remote_url(&self, root: &Path, name: &str) -> Result<Option<String>> {
        let result = self.git(&["remote", "get-url", name], Some(root), SHORT).await?;
        Ok(if result.ok() && !trimmed(&result).is_empty() { Some(trimmed(&result).to_string()) } else { None })
    }

    pub async fn inspect_repository(&self, path: &str) -> Result<RepositoryInfo> {
        let directory = expand_user(path);
        if !directory.is_dir() {
            return failed(format!("Directory does not exist: {}", directory.display()));
        }
        let top = self.git(&["rev-parse", "--show-toplevel"], Some(&directory), TIMEOUT).await?;
        if !top.ok() {
            return failed(format!("Not a Git repository: {}", directory.display()));
        }
        let root = paths::resolve(Path::new(trimmed(&top)));
        let head = self.git(&["rev-parse", "--abbrev-ref", "HEAD"], Some(&root), TIMEOUT).await?;
        let local = self.git(&["branch", "--format=%(refname:short)", "--list"], Some(&root), TIMEOUT).await?;
        let remote = self.git(&["branch", "-r", "--format=%(refname:short)", "--list"], Some(&root), TIMEOUT).await?;
        let mut branches: Vec<String> =
            local.stdout.lines().map(str::trim).filter(|b| !b.is_empty()).map(String::from).collect();
        for remote in remote.stdout.lines().map(str::trim) {
            if let Some((_, short)) = remote.split_once('/')
                && !remote.ends_with("/HEAD")
                && !branches.iter().any(|b| b == short)
            {
                branches.push(short.to_string());
            }
        }
        branches.sort();
        branches.dedup();
        Ok(RepositoryInfo {
            path: paths::resolve(&directory).to_string_lossy().into_owned(),
            root: root.to_string_lossy().into_owned(),
            current_branch: head.ok().then(|| trimmed(&head).to_string()),
            branches,
        })
    }

    /// Every directory committed at HEAD, relative to `root` with "/" separators.
    pub async fn directories(&self, root: &Path) -> Result<Vec<String>> {
        let result = self.git(&["ls-tree", "-r", "-d", "-z", "--name-only", "HEAD"], Some(root), SHORT).await?;
        if !result.ok() {
            return failed(format!("Cannot list the directories of {}", root.display()));
        }
        Ok(result.stdout.split('\0').filter(|n| !n.is_empty()).map(String::from).collect())
    }

    pub async fn branch_exists(&self, root: &Path, branch: &str) -> Result<bool> {
        let local = self
            .git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], Some(root), TIMEOUT)
            .await?;
        if !matches!(local.returncode, 0 | 1) {
            return failed(format!("Cannot inspect branch '{branch}': {}", local.stderr.trim()));
        }
        Ok(local.ok())
    }

    async fn remote_branch_exists(&self, root: &Path, branch: &str) -> Result<bool> {
        let remote = self
            .git(&["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{branch}")], Some(root), TIMEOUT)
            .await?;
        Ok(remote.ok())
    }

    /// Best-effort update of refs/remotes/origin/<branch>; a failed (or timed-out) fetch never
    /// fails resolution. It only matters when origin is known to have the branch: then the
    /// returned warning says the base may be out of date. (No origin remote, or a branch origin
    /// doesn't have, leaves nothing to be stale against.)
    pub async fn fetch_origin(&self, root: &Path, branch: &str) -> Result<Option<String>> {
        // An explicit refspec updates the tracking ref even in a single-branch or narrowed clone,
        // where `fetch origin <branch>` would only write FETCH_HEAD.
        let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        let exit_code = match self.git(&["fetch", "origin", &refspec], Some(root), TIMEOUT).await {
            Ok(result) if result.ok() => return Ok(None),
            Ok(result) => Some(result.returncode),
            Err(_) => None,
        };
        event(
            "git.fetch.failed",
            json!({"repository": root.to_string_lossy(), "branch": branch, "exitCode": exit_code}),
        );
        if !self.remote_branch_exists(root, branch).await? {
            return Ok(None);
        }
        Ok(Some(format!(
            "Couldn't fetch {branch} from origin, so this task branched from the last fetched copy of {branch}, \
             which may be missing newer commits."
        )))
    }

    /// The ref to branch from, after fetching origin: origin/<branch>, unless the local branch
    /// already contains it (so unpushed local commits are kept, but a local copy that is behind
    /// origin is never used); whichever exists when only one does. Also returns
    /// [`Self::fetch_origin`]'s warning.
    async fn resolve_base(&self, root: &Path, base_branch: &str) -> Result<(String, Option<String>)> {
        let warning = self.fetch_origin(root, base_branch).await?;
        let local = self.branch_exists(root, base_branch).await?;
        let remote = self.remote_branch_exists(root, base_branch).await?;
        let reference = match (local, remote) {
            (true, true) => {
                let contains = self
                    .git(
                        &[
                            "merge-base",
                            "--is-ancestor",
                            &format!("refs/remotes/origin/{base_branch}"),
                            &format!("refs/heads/{base_branch}"),
                        ],
                        Some(root),
                        TIMEOUT,
                    )
                    .await?;
                match contains.returncode {
                    0 => base_branch.to_string(),
                    1 => format!("origin/{base_branch}"),
                    _ => return failed(format!("Cannot compare {base_branch} with origin/{base_branch}")),
                }
            }
            (true, false) => base_branch.to_string(),
            (false, true) => format!("origin/{base_branch}"),
            (false, false) => return failed(format!("Base branch '{base_branch}' not found locally or on origin")),
        };
        event("worktree.base.resolved", json!({"baseBranch": base_branch, "ref": reference}));
        Ok((reference, warning))
    }

    /// A ref for the base branch: see [`Self::resolve_base`].
    pub async fn resolve_base_ref(&self, root: &Path, base_branch: &str) -> Result<String> {
        Ok(self.resolve_base(root, base_branch).await?.0)
    }

    pub async fn list_worktrees(&self, root: &Path) -> Result<Vec<WorktreeEntry>> {
        let result = self.git(&["worktree", "list", "--porcelain", "-z"], Some(root), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!("Cannot inspect registered worktrees: {}", result.stderr.trim()));
        }
        let mut worktrees = Vec::new();
        let mut current = WorktreeEntry::new();
        for line in result.stdout.split('\0') {
            if line.is_empty() {
                if !current.is_empty() {
                    worktrees.push(std::mem::take(&mut current));
                }
                continue;
            }
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            current.insert(key.to_string(), value.to_string());
        }
        if !current.is_empty() {
            worktrees.push(current);
        }
        Ok(worktrees)
    }

    fn find<'a>(entries: &'a [WorktreeEntry], path: &Path) -> Option<&'a WorktreeEntry> {
        entries.iter().find(|wt| wt.get("worktree").is_some_and(|w| paths::same_path(w, path)))
    }

    /// Create a worktree on a new branch from `base_branch` (see [`Self::resolve_base`]),
    /// recovering only a missing, unlocked registration at this path.
    pub async fn create_worktree(
        &self,
        root: &Path,
        worktree_path: &Path,
        branch: &str,
        base_branch: &str,
    ) -> Result<CreatedWorktree> {
        event(
            "worktree.create.requested",
            json!({"repository": root.to_string_lossy(), "path": worktree_path.to_string_lossy(),
                   "branch": branch, "baseBranch": base_branch}),
        );
        if present(worktree_path) || paths::linked(worktree_path).is_some() {
            return failed(format!(
                "Worktree directory already exists: {}. Remove it (git worktree remove) or choose a different Jira key.",
                worktree_path.display()
            ));
        }
        let worktrees = self.list_worktrees(root).await?;
        let stale = Self::find(&worktrees, worktree_path).cloned();
        if let Some(stale) = &stale {
            event(
                "worktree.stale.detected",
                json!({"path": worktree_path.to_string_lossy(), "requestedBranch": branch,
                       "registeredBranch": stale.get("branch"), "head": stale.get("HEAD"),
                       "locked": stale.contains_key("locked"), "prunable": stale.contains_key("prunable")}),
            );
            if stale.contains_key("locked") || !stale.contains_key("prunable") {
                return failed("The registered worktree is locked or not safely prunable");
            }
        }
        let (base_ref, base_warning) = self.resolve_base(root, base_branch).await?;
        if let Some(stale) = &stale {
            match stale.get("branch") {
                Some(stale_branch) => {
                    // A branch-backed worktree keeps its work reachable through that branch ref.
                    let retained = self.git(&["rev-parse", "--verify", stale_branch], Some(root), TIMEOUT).await?;
                    if !retained.ok() || Some(trimmed(&retained)) != stale.get("HEAD").map(String::as_str) {
                        event(
                            "worktree.recovery.blocked",
                            json!({"reason": "stale_branch_commit_not_retained", "path": worktree_path.to_string_lossy(),
                                   "registeredBranch": stale_branch, "requestedBranch": branch}),
                        );
                        return failed(
                            "The stale worktree commit is not retained by its registered branch; inspect it before recovery",
                        );
                    }
                }
                None => {
                    let base_head = self.git(&["rev-parse", "--verify", &base_ref], Some(root), TIMEOUT).await?;
                    if !base_head.ok() || stale.get("HEAD").map(String::as_str) != Some(trimmed(&base_head)) {
                        return failed(
                            "Missing worktree has a different commit from the selected base; inspect it before recovery",
                        );
                    }
                }
            }
            // Recheck immediately before removing metadata; never force-remove a live directory.
            if present(worktree_path) {
                return failed("Worktree path appeared during recovery; retry after inspecting it");
            }
            let registered = stale.get("branch");
            if registered.is_some_and(|b| *b != format!("refs/heads/{branch}")) {
                event(
                    "worktree.recovery.different_branch",
                    json!({"path": worktree_path.to_string_lossy(), "requestedBranch": branch,
                           "registeredBranch": registered, "oldBranchPreserved": true}),
                );
            }
            self.remove_worktree(root, worktree_path, false).await?;
            event(
                "worktree.stale.registration_removed",
                json!({"path": worktree_path.to_string_lossy(), "branchPreserved": registered.is_some()}),
            );
        }
        let mut chosen = branch.to_string();
        let mut suffix = 0;
        while self.branch_exists(root, &chosen).await? {
            suffix += 1;
            chosen = format!("{branch}-{suffix}");
            if suffix > 10000 {
                return failed(format!("Could not find an available branch name based on '{branch}'"));
            }
        }
        if chosen != branch {
            event("worktree.branch.collision", json!({"requestedBranch": branch, "selectedBranch": chosen}));
        }
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GitError::Failed(format!("Cannot create {}: {e}", parent.display())))?;
        }
        let path = worktree_path.to_string_lossy();
        // --no-track: a branch made from origin/<base> must not take it as its upstream, or a pull
        // in the worktree would merge the base and a push could target it.
        let result = self
            .git(
                &["worktree", "add", "--no-track", "-b", &chosen, &path, &base_ref],
                Some(root),
                Duration::from_secs(600),
            )
            .await?;
        if !result.ok() {
            event("worktree.create.failed", json!({"path": path, "exitCode": result.returncode}));
            return failed(format!("git worktree add failed: {}", said(&result)));
        }
        event("worktree.create.completed", json!({"path": path, "branch": chosen}));
        Ok(CreatedWorktree { branch: chosen, base_warning })
    }

    pub async fn head_sha(&self, cwd: &Path) -> Result<String> {
        let result = self.git(&["rev-parse", "HEAD"], Some(cwd), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!("Cannot read HEAD in {}: {}", cwd.display(), result.stderr.trim()));
        }
        Ok(trimmed(&result).to_string())
    }

    pub async fn verify_worker_worktree(&self, root: &Path, path: &Path, branch: &str) -> Result<()> {
        if paths::linked(path).is_some() {
            return failed("Worker worktree must not be a linked directory");
        }
        let entries = self.list_worktrees(root).await?;
        match Self::find(&entries, path) {
            Some(entry)
                if entry.get("branch") == Some(&format!("refs/heads/{branch}")) && !entry.contains_key("locked") =>
            {
                Ok(())
            }
            _ => failed("Worker worktree registration or branch changed; inspect before retrying"),
        }
    }

    /// Branch from a local checkpoint only; never fetch or reuse an existing directory.
    pub async fn create_worker_worktree(&self, root: &Path, path: &Path, branch: &str, sha: &str) -> Result<()> {
        if present(path) || paths::linked(path).is_some() {
            return failed("Worker worktree path already exists or is linked");
        }
        let result =
            self.git(&["worktree", "add", "-b", branch, &path.to_string_lossy(), sha], Some(root), TIMEOUT).await?;
        if !result.ok() {
            // `worktree add -b` creates the branch before checking out, so a checkout failure
            // (e.g. Windows MAX_PATH) leaves it behind and blocks every retry. Best effort.
            self.git(&["branch", "-D", branch], Some(root), TIMEOUT).await?;
            return failed(format!("Cannot create worker worktree: {}", result.stderr.trim()));
        }
        Ok(())
    }

    pub async fn merge_worker_checkpoint(&self, path: &Path, sha: &str) -> Result<()> {
        let mut args = IDENTITY.to_vec();
        args.extend(["merge", "--no-edit", sha]);
        let result = self.git(&args, Some(path), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!(
                "Dependency merge needs inspection in the worker worktree: {}",
                result.stderr.trim()
            ));
        }
        Ok(())
    }

    pub async fn changed_paths(&self, path: &Path, base: &str, sha: &str) -> Result<Vec<String>> {
        let result =
            self.git(&["diff", "--name-only", "--no-renames", "-z", base, sha, "--"], Some(path), TIMEOUT).await?;
        if !result.ok() {
            return failed("Cannot verify worker file ownership");
        }
        Ok(result.stdout.split('\0').filter(|n| !n.is_empty()).map(String::from).collect())
    }

    pub async fn is_ancestor(&self, path: &Path, ancestor: &str, sha: &str) -> Result<bool> {
        let result = self.git(&["merge-base", "--is-ancestor", ancestor, sha], Some(path), TIMEOUT).await?;
        if !matches!(result.returncode, 0 | 1) {
            return failed("Cannot verify integrated worker checkpoint");
        }
        Ok(result.ok())
    }

    /// Whether Git ignores `relative` in `cwd` (a tracked file never is). A trailing `/` asks about
    /// a directory, which matters for patterns like `node_modules/` when it does not exist yet.
    pub async fn is_ignored(&self, cwd: &Path, relative: &str) -> Result<bool> {
        let result = self.git(&["check-ignore", "-q", "--", relative], Some(cwd), TIMEOUT).await?;
        if !matches!(result.returncode, 0 | 1) {
            return failed(format!("Cannot check whether {relative} is ignored: {}", result.stderr.trim()));
        }
        Ok(result.ok())
    }

    pub async fn is_dirty(&self, cwd: &Path) -> Result<bool> {
        let result = self.git(&["status", "--porcelain"], Some(cwd), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!("Cannot inspect worktree: {}", result.stderr.trim()));
        }
        Ok(!trimmed(&result).is_empty())
    }

    /// Porcelain status lines (`" M path"`, `"?? path"`), untracked files included.
    pub async fn dirty_paths(&self, cwd: &Path, ignored: bool) -> Result<Vec<String>> {
        let mut args = vec!["status", "--porcelain", "--untracked-files=all"];
        if ignored {
            args.push("--ignored");
        }
        let result = self.git(&args, Some(cwd), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!("Cannot inspect worktree: {}", result.stderr.trim()));
        }
        Ok(result.stdout.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect())
    }

    pub async fn reset_hard(&self, cwd: &Path, sha: &str) -> Result<()> {
        let result = self.git(&["reset", "--hard", sha], Some(cwd), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!("git reset --hard failed: {}", said(&result)));
        }
        Ok(())
    }

    /// Record a stable checkpoint, committing outstanding changes when `commit`. Returns
    /// (sha, whether a commit was made).
    pub async fn checkpoint(&self, cwd: &Path, message: &str, commit: bool) -> Result<(String, bool)> {
        let mut committed = false;
        if !commit && self.is_dirty(cwd).await? {
            return failed("Commit the worktree changes before requesting a review checkpoint");
        }
        if commit && self.is_dirty(cwd).await? {
            self.git(&["add", "-A"], Some(cwd), TIMEOUT).await?.check(Some("git add"))?;
            let mut args = IDENTITY.to_vec();
            args.extend(["commit", "-q", "-m", message]);
            let result = self.git(&args, Some(cwd), TIMEOUT).await?;
            if !result.ok() {
                return failed(format!("git commit failed: {}", said(&result)));
            }
            committed = true;
        }
        Ok((self.head_sha(cwd).await?, committed))
    }

    pub async fn remove_worktree(&self, root: &Path, worktree_path: &Path, force: bool) -> Result<()> {
        let path = worktree_path.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path);
        let result = self.git(&args, Some(root), TIMEOUT).await?;
        if !result.ok() {
            return Err(GitError::Busy(format!("git worktree remove failed: {}", said(&result))));
        }
        Ok(())
    }

    /// Remove the task worktree, discarding any modified/untracked/ignored files in it. Returns
    /// the porcelain lines of the modified/untracked files (ignored ones are discarded but not
    /// listed: there can be tens of thousands and the project marked them regenerable).
    ///
    /// The branch tip must be the reviewed checkpoint `expected_sha`, or `merged_head_sha` (the
    /// head GitHub reports merged) or one of its ancestors, unless `force`: the human vouches for
    /// the branch (merged somewhere AgentFlow can't see). The branch itself is kept either way.
    /// `worktree_dir`, when given, is the managed directory task worktrees live directly inside;
    /// it gates leftover recovery.
    #[allow(clippy::too_many_arguments)]
    pub async fn cleanup_worktree(
        &self,
        root: &Path,
        path: &Path,
        branch: &str,
        expected_sha: &str,
        worktree_dir: Option<&Path>,
        merged_head_sha: Option<&str>,
        force: bool,
    ) -> Result<Vec<String>> {
        event(
            "worktree.cleanup.requested",
            json!({"path": path.to_string_lossy(), "branch": branch, "expectedSha": expected_sha, "force": force}),
        );
        if paths::same_path(root, path) || paths::linked(path).is_some() {
            return failed("Refusing to remove the repository root or a linked directory");
        }
        let entries = self.list_worktrees(root).await?;
        let Some(entry) = Self::find(&entries, path).cloned() else {
            if !present(path) {
                return Ok(Vec::new()); // A previous cleanup already completed.
            }
            return self
                .cleanup_leftover_directory(root, path, branch, expected_sha, worktree_dir, merged_head_sha, force)
                .await;
        };
        if entry.contains_key("locked") {
            return failed("Worktree is locked; unlock it (`git worktree unlock`) and retry");
        }
        if entry.get("branch") == Some(&format!("refs/heads/{branch}")) {
            let branch_head =
                self.git(&["rev-parse", "--verify", &format!("refs/heads/{branch}")], Some(root), TIMEOUT).await?;
            let head = branch_head.ok().then(|| trimmed(&branch_head).to_string());
            if !force
                && (entry.get("HEAD") != head.as_ref()
                    || !self.is_cleanable_head(root, head.as_deref(), expected_sha, merged_head_sha).await?)
            {
                return unverified();
            }
        } else if !self
            .is_cleanable_head(root, entry.get("HEAD").map(String::as_str), expected_sha, merged_head_sha)
            .await?
        {
            // Switched or renamed since the task made it (the human pushing `<branch>-rc`, say): fine
            // once every commit it has is reviewed or merged. Never forced: a named branch isn't the
            // one the human vouched for, and a detached HEAD can hold commits no branch has.
            return failed(format!(
                "Worktree is on {}, not the task branch, and its commits aren't the final review or a merged \
                 pull request, so it no longer belongs to the task",
                entry.get("branch").map_or("a detached HEAD".to_string(), |b| format!(
                    "branch {}",
                    b.strip_prefix("refs/heads/").unwrap_or(b)
                ))
            ));
        }
        let mut discarded = Vec::new();
        if present(path) {
            discarded = self.dirty_paths(path, false).await?;
            if !discarded.is_empty() {
                event("worktree.cleanup.discarding", json!({"path": path.to_string_lossy(), "count": discarded.len()}));
            }
        }
        self.remove_worktree(root, path, true).await?;
        if present(path) || Self::find(&self.list_worktrees(root).await?, path).is_some() {
            return Err(GitError::Busy("Worktree cleanup could not be verified".into()));
        }
        event(
            "worktree.cleanup.completed",
            json!({"path": path.to_string_lossy(), "branch": branch, "discarded": discarded.len()}),
        );
        Ok(discarded)
    }

    /// Whether every commit on the branch is known to be reviewed or merged.
    async fn is_cleanable_head(
        &self,
        root: &Path,
        head: Option<&str>,
        expected_sha: &str,
        merged_head_sha: Option<&str>,
    ) -> Result<bool> {
        let Some(head) = head.filter(|h| !h.is_empty()) else { return Ok(false) };
        if head == expected_sha || Some(head) == merged_head_sha {
            return Ok(true);
        }
        let Some(merged) = merged_head_sha else { return Ok(false) };
        // Exit 1 means not an ancestor; anything else (the merged commit was never fetched)
        // can't vouch for the branch either.
        let result = self.git(&["merge-base", "--is-ancestor", head, merged], Some(root), TIMEOUT).await?;
        Ok(result.returncode == 0)
    }

    /// Recover from a removal that partially failed: `git worktree remove` deregisters before it
    /// deletes, so a delete that fails afterwards (a Windows process holding the directory) leaves
    /// an unregistered directory. Only ever deleted when it sits directly inside the managed
    /// worktree directory and the task branch still points at a reviewed or merged commit (or
    /// `force`: the human vouched for the branch).
    #[allow(clippy::too_many_arguments)]
    async fn cleanup_leftover_directory(
        &self,
        root: &Path,
        target: &Path,
        branch: &str,
        expected_sha: &str,
        worktree_dir: Option<&Path>,
        merged_head_sha: Option<&str>,
        force: bool,
    ) -> Result<Vec<String>> {
        let refused = || failed("Directory is not a registered task worktree");
        let Some(worktree_dir) = worktree_dir else { return refused() };
        if !directly_inside(target, worktree_dir) {
            return refused();
        }
        if !force {
            let branch_head = self
                .git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], Some(root), TIMEOUT)
                .await?;
            if !branch_head.ok() {
                // Deleted after the merge: nothing left to check the directory against, so the
                // human decides (force), the same as for a branch merged out of sight.
                return unverified_because(
                    "The task branch no longer exists, so AgentFlow can't confirm the leftover directory holds \
                     nothing unreviewed; worktree cleanup stopped",
                );
            }
            if !self.is_cleanable_head(root, Some(trimmed(&branch_head)), expected_sha, merged_head_sha).await? {
                return unverified();
            }
        }
        event("worktree.cleanup.leftover_detected", json!({"path": target.to_string_lossy(), "branch": branch}));
        self.remove_leftover_directory(root, target).await?;
        event("worktree.cleanup.leftover_removed", json!({"path": target.to_string_lossy(), "branch": branch}));
        Ok(Vec::new())
    }

    /// Delete an unregistered worktree directory the caller has already vetted, then prune.
    async fn remove_leftover_directory(&self, root: &Path, target: &Path) -> Result<()> {
        // A just-closed Windows process can hold its old cwd's handle open for a while after it is
        // gone (over 10s observed), so this backs off rather than giving up quickly.
        let backoff = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0];
        for (attempt, delay) in backoff.iter().enumerate() {
            match std::fs::remove_dir_all(target) {
                Ok(()) => break,
                Err(error) if attempt == backoff.len() - 1 => {
                    // The OS error says why (32: in use by another process, 5: access denied).
                    return Err(GitError::Busy(format!(
                        "Could not remove leftover worktree directory: {} ({error})",
                        target.display()
                    )));
                }
                Err(_) => tokio::time::sleep(Duration::from_secs_f64(*delay)).await,
            }
        }
        let prune = self.git(&["worktree", "prune"], Some(root), TIMEOUT).await?;
        if !prune.ok() {
            return failed(format!("git worktree prune failed: {}", said(&prune)));
        }
        if present(target) {
            return Err(GitError::Busy(format!(
                "Could not verify removal of leftover worktree directory: {}",
                target.display()
            )));
        }
        Ok(())
    }

    /// Force-remove a task's worktree at whatever commit it is on (task deletion): no reviewed
    /// checkpoint to insist on, but the containment rules stay. Returns the discarded lines.
    pub async fn remove_task_worktree(&self, root: &Path, path: &Path, worktree_dir: &Path) -> Result<Vec<String>> {
        if paths::same_path(root, path) || paths::linked(path).is_some() || !directly_inside(path, worktree_dir) {
            return failed("Refusing to remove a worktree outside the managed worktree directory");
        }
        event("worktree.delete.requested", json!({"path": path.to_string_lossy()}));
        let entries = self.list_worktrees(root).await?;
        let Some(entry) = Self::find(&entries, path) else {
            if present(path) {
                self.remove_leftover_directory(root, path).await?;
            }
            return Ok(Vec::new());
        };
        if entry.contains_key("locked") {
            return failed("Worktree is locked");
        }
        let discarded = if present(path) { self.dirty_paths(path, false).await? } else { Vec::new() };
        self.remove_worktree(root, path, true).await?;
        if present(path) || Self::find(&self.list_worktrees(root).await?, path).is_some() {
            return failed(format!("Could not verify removal of {}", path.display()));
        }
        event("worktree.delete.completed", json!({"path": path.to_string_lossy(), "discarded": discarded.len()}));
        Ok(discarded)
    }

    pub async fn branch_sha(&self, root: &Path, branch: &str) -> Result<Option<String>> {
        let result = self
            .git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], Some(root), TIMEOUT)
            .await?;
        if !matches!(result.returncode, 0 | 1) {
            return failed(format!("Cannot inspect branch '{branch}': {}", result.stderr.trim()));
        }
        Ok(result.ok().then(|| trimmed(&result).to_string()))
    }

    /// Push `branch` to origin under the same name and track it there. Run from the task worktree,
    /// so any pre-push hook sees that checkout.
    pub async fn push_branch(&self, cwd: &Path, branch: &str) -> Result<()> {
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let result = self.git(&["push", "--set-upstream", "origin", &refspec], Some(cwd), PUSH_TIMEOUT).await?;
        if !result.ok() {
            // git's output can include the remote URL and its credentials; say only what failed.
            return failed(format!(
                "Pushing {branch} to origin failed (git exit {}). Push it from the worktree to see why.",
                result.returncode
            ));
        }
        Ok(())
    }

    async fn count(&self, root: &Path, revisions: &str) -> Result<Option<i64>> {
        let result = self.git(&["rev-list", "--count", revisions], Some(root), TIMEOUT).await?;
        Ok(if result.ok() { trimmed(&result).parse().ok() } else { None })
    }

    async fn verifies(&self, root: &Path, reference: &str) -> Result<bool> {
        Ok(self.git(&["rev-parse", "--verify", "--quiet", reference], Some(root), TIMEOUT).await?.ok())
    }

    /// What deleting `branch` would lose: commits not on the base branch, and commits not on its
    /// remote (its upstream, else origin/<branch>). Read-only; never fetches.
    pub async fn branch_report(&self, root: &Path, branch: &str, base_branch: &str) -> Result<Value> {
        let sha = self.branch_sha(root, branch).await?;
        let mut report = json!({"name": branch, "exists": sha.is_some(), "sha": sha, "unmergedCommits": null,
                                "upstream": null, "unpushedCommits": null});
        let Some(sha) = sha else { return Ok(report) };
        for base in [format!("refs/heads/{base_branch}"), format!("refs/remotes/origin/{base_branch}")] {
            if self.verifies(root, &base).await? {
                report["unmergedCommits"] = json!(self.count(root, &format!("{base}..{sha}")).await?);
                break;
            }
        }
        let upstream = self
            .git(
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", &format!("{branch}@{{upstream}}")],
                Some(root),
                TIMEOUT,
            )
            .await?;
        if upstream.ok() && !trimmed(&upstream).is_empty() {
            report["upstream"] = json!(trimmed(&upstream));
        } else if self.verifies(root, &format!("refs/remotes/origin/{branch}")).await? {
            report["upstream"] = json!(format!("origin/{branch}"));
        }
        if let Some(upstream) = report["upstream"].as_str().map(String::from) {
            report["unpushedCommits"] = json!(self.count(root, &format!("{upstream}..{sha}")).await?);
        }
        Ok(report)
    }

    /// Force-delete a local branch. False when it was already gone.
    pub async fn delete_branch(&self, root: &Path, branch: &str) -> Result<bool> {
        if self.branch_sha(root, branch).await?.is_none() {
            return Ok(false);
        }
        let result = self.git(&["branch", "-D", branch], Some(root), TIMEOUT).await?;
        if !result.ok() {
            return failed(format!("git branch -D {branch} failed: {}", said(&result)));
        }
        event("branch.delete.completed", json!({"branch": branch}));
        Ok(true)
    }

    /// Force-remove a disposable worktree (a worker subtask's), discarding local changes: worker
    /// checkpoints are already folded into the task branch by the time the task is cleaned up.
    pub async fn discard_worktree(&self, root: &Path, path: &Path) -> Result<Vec<String>> {
        if paths::same_path(root, path) || paths::linked(path).is_some() {
            return failed("Refusing to remove the repository root or a linked directory");
        }
        let entries = self.list_worktrees(root).await?;
        let Some(entry) = Self::find(&entries, path) else { return Ok(Vec::new()) };
        if entry.contains_key("locked") {
            return failed("Worktree is locked");
        }
        let discarded = if present(path) { self.dirty_paths(path, false).await? } else { Vec::new() };
        self.remove_worktree(root, path, true).await?;
        if present(path) || Self::find(&self.list_worktrees(root).await?, path).is_some() {
            return failed(format!("Could not verify removal of {}", path.display()));
        }
        Ok(discarded)
    }
}

/// Whether `path` sits directly inside `directory` (both resolved).
fn directly_inside(path: &Path, directory: &Path) -> bool {
    paths::resolve(path).parent().is_some_and(|parent| paths::same_path(parent, directory))
}

fn expand_user(value: &str) -> PathBuf {
    match value.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => std::env::home_dir()
            .map_or_else(|| PathBuf::from(value), |home| home.join(rest.trim_start_matches(['/', '\\']))),
        _ => PathBuf::from(value),
    }
}
