//! Real Git recovery and removal checks. Port of backend/tests/test_git_worktrees.py: same test
//! names, same scenarios, same assertions. The one mocked Python test
//! (test_listing_failure_is_not_treated_as_empty) runs git for real against a non-repository.
//!
//! Each repository is isolated from the user's global git configuration the way the Python
//! fixture does it: no hooks, no signing, an empty excludes file and a fixed identity.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use agentflow_core::git::{GitError, GitService};
use agentflow_core::paths::same_path;

struct Fixture {
    _dir: tempfile::TempDir,
    folder: PathBuf,
    repo: PathBuf,
    path: PathBuf,
    sha: String,
    service: GitService,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let folder = agentflow_core::paths::resolve(dir.path());
        let repo = folder.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let mut f = Fixture {
            path: folder.join("AF-14449"),
            folder,
            repo,
            sha: String::new(),
            service: GitService::default(),
            _dir: dir,
        };
        f.git(&["init", "-q", "-b", "main"]);
        let ignore = f.folder.join("ignore");
        std::fs::write(&ignore, "").unwrap();
        let hooks = f.folder.join("hooks");
        for (key, value) in [
            ("core.excludesFile", ignore.to_string_lossy().into_owned()),
            ("core.hooksPath", hooks.to_string_lossy().into_owned()),
            ("commit.gpgSign", "false".into()),
            ("user.name", "Test".into()),
            ("user.email", "test@localhost".into()),
        ] {
            f.git(&["config", key, &value]);
        }
        std::fs::write(f.repo.join("file.txt"), "preserved\n").unwrap();
        f.git(&["add", "file.txt"]);
        f.git(&["commit", "-qm", "fixture"]);
        f.sha = f.out(&["rev-parse", "HEAD"]);
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new("git").arg("-C").arg(&self.repo).args(args).output().unwrap()
    }

    fn git(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        output
    }

    fn out(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.git(args).stdout).trim().to_string()
    }

    fn p(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn stale_at(&self, target: &Path) {
        self.git(&["worktree", "add", "--detach", &Self::p(target), "HEAD"]);
        remove_directory(target);
    }

    fn stale(&self) {
        self.stale_at(&self.path.clone());
    }

    async fn create(&self) -> Result<String, GitError> {
        Ok(self.service.create_worktree(&self.repo, &self.path, "AF-14449", "main").await?.branch)
    }

    /// A bare `origin` holding `main` as it is now.
    fn add_origin(&self) -> PathBuf {
        let remote = self.folder.join("origin.git");
        assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
        self.git(&["remote", "add", "origin", &Self::p(&remote)]);
        self.git(&["push", "-q", "origin", "main"]);
        remote
    }

    /// Commit on top of `main` straight to origin, leaving local `main` behind it; the fetch
    /// before branching is what brings it in.
    fn advance_origin(&self, message: &str) -> String {
        let tree = self.out(&["rev-parse", "main^{tree}"]);
        let sha = self.out(&["commit-tree", &tree, "-p", "main", "-m", message]);
        self.git(&["push", "-q", "origin", &format!("{sha}:refs/heads/main")]);
        self.git(&["update-ref", "refs/remotes/origin/main", "main"]);
        sha
    }

    fn head_of(&self, path: &Path) -> String {
        self.out(&["-C", &Self::p(path), "rev-parse", "HEAD"])
    }

    async fn cleanup(&self) -> Result<Vec<String>, GitError> {
        self.service.cleanup_worktree(&self.repo, &self.path, "AF-14449", &self.sha, None, None, false).await
    }

    async fn cleanup_forced(&self, worktree_dir: Option<&Path>) -> Result<Vec<String>, GitError> {
        self.service.cleanup_worktree(&self.repo, &self.path, "AF-14449", &self.sha, worktree_dir, None, true).await
    }

    async fn cleanup_with(
        &self,
        expected: &str,
        worktree_dir: Option<&Path>,
        merged: Option<&str>,
    ) -> Result<Vec<String>, GitError> {
        self.service.cleanup_worktree(&self.repo, &self.path, "AF-14449", expected, worktree_dir, merged, false).await
    }

    async fn registered(&self, path: &Path) -> bool {
        self.service.list_worktrees(&self.repo).await.unwrap().iter().any(|wt| same_path(&wt["worktree"], path))
    }

    fn branch_in(&self, path: &Path) -> String {
        self.out(&["-C", &Self::p(path), "branch", "--show-current"])
    }

    fn commit_in_worktree(&self, message: &str) -> String {
        let path = Self::p(&self.path);
        self.git(&["-C", &path, "commit", "--allow-empty", "-qm", message]);
        self.out(&["-C", &path, "rev-parse", "HEAD"])
    }

    /// A commit the PR got without it passing through the worktree (a GitHub web edit).
    fn commit_outside_worktree(&self, parent: &str, message: &str) -> String {
        let tree = self.out(&["rev-parse", &format!("{parent}^{{tree}}")]);
        self.out(&["commit-tree", &tree, "-p", parent, "-m", message])
    }

    fn admin_dir(&self) -> PathBuf {
        self.repo.join(".git").join("worktrees").join(self.path.file_name().unwrap())
    }

    fn porcelain_mentions(&self, path: &Path) -> bool {
        let listing = String::from_utf8_lossy(&self.git(&["worktree", "list", "--porcelain"]).stdout).to_lowercase();
        listing.contains(&Self::p(path).replace('\\', "/").to_lowercase())
    }
}

/// Git marks object files read-only, which a plain recursive delete cannot remove on Windows.
fn remove_directory(path: &Path) {
    fn writable(path: &Path) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Ok(metadata) = std::fs::symlink_metadata(&p) {
                    let mut permissions = metadata.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    let _ = std::fs::set_permissions(&p, permissions);
                    if metadata.is_dir() {
                        writable(&p);
                    }
                }
            }
        }
    }
    writable(path);
    std::fs::remove_dir_all(path).unwrap();
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.folder.exists() {
            remove_directory(&self.folder);
        }
    }
}

fn assert_error(result: Result<impl std::fmt::Debug, GitError>, needle: &str) {
    match result {
        Err(error) => assert!(error.to_string().contains(needle), "expected {needle:?} in {error}"),
        Ok(value) => panic!("expected an error containing {needle:?}, got Ok({value:?})"),
    }
}

/// Task creation relies on create_worktree alone to fetch origin and resolve the base ref.
#[tokio::test]
async fn test_create_worktree_fetches_a_base_branch_that_only_exists_on_origin() {
    let f = Fixture::new();
    let remote = f.folder.join("origin.git");
    assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
    f.git(&["remote", "add", "origin", &Fixture::p(&remote)]);
    f.git(&["push", "-q", "origin", "main:release"]);
    // push records origin/release locally; drop it so the branch exists on origin only.
    f.git(&["update-ref", "-d", "refs/remotes/origin/release"]);
    assert!(!f.service.branch_exists(&f.repo, "release").await.unwrap());
    assert!(!f.run(&["rev-parse", "--verify", "--quiet", "refs/remotes/origin/release"]).status.success());

    let created = f.service.create_worktree(&f.repo, &f.path, "AF-14449", "release").await.unwrap();

    assert_eq!(created.branch, "AF-14449");
    assert_eq!(created.base_warning, None);
    assert_eq!(f.out(&["-C", &Fixture::p(&f.path), "rev-parse", "HEAD"]), f.sha);
    assert_eq!(f.out(&["rev-parse", "refs/remotes/origin/release"]), f.sha);
}

#[tokio::test]
async fn test_new_worktree_branches_from_origin_when_the_local_base_is_behind() {
    let f = Fixture::new();
    f.add_origin();
    let newer = f.advance_origin("newer on origin");

    let created = f.service.create_worktree(&f.repo, &f.path, "AF-14449", "main").await.unwrap();

    assert_eq!(created.base_warning, None);
    assert_eq!(f.head_of(&f.path), newer);
    // The local base is left alone.
    assert_eq!(f.out(&["rev-parse", "main"]), f.sha);
    // Branched with --no-track: origin/main is not the task branch's upstream.
    assert!(!f.run(&["rev-parse", "--abbrev-ref", "AF-14449@{upstream}"]).status.success());
}

#[tokio::test]
async fn test_new_worktree_keeps_unpushed_commits_on_the_local_base() {
    let f = Fixture::new();
    f.add_origin();
    f.git(&["commit", "--allow-empty", "-qm", "local only"]);
    let local = f.out(&["rev-parse", "main"]);

    f.service.create_worktree(&f.repo, &f.path, "AF-14449", "main").await.unwrap();

    assert_eq!(f.head_of(&f.path), local);
}

#[tokio::test]
async fn test_new_worktree_branches_from_origin_when_the_local_base_has_diverged() {
    let f = Fixture::new();
    f.add_origin();
    let newer = f.advance_origin("newer on origin");
    f.git(&["commit", "--allow-empty", "-qm", "local only"]);

    f.service.create_worktree(&f.repo, &f.path, "AF-14449", "main").await.unwrap();

    assert_eq!(f.head_of(&f.path), newer);
}

#[tokio::test]
async fn test_failed_fetch_of_a_base_known_on_origin_warns_that_it_may_be_out_of_date() {
    let f = Fixture::new();
    f.add_origin();
    f.git(&["remote", "set-url", "origin", &Fixture::p(&f.folder.join("missing.git"))]);

    let created = f.service.create_worktree(&f.repo, &f.path, "AF-14449", "main").await.unwrap();

    assert_eq!(f.head_of(&f.path), f.sha);
    let warning = created.base_warning.expect("a warning");
    assert!(warning.contains("Couldn't fetch main from origin"), "{warning}");
}

#[tokio::test]
async fn test_repository_without_origin_branches_from_the_local_base_without_a_warning() {
    let f = Fixture::new();

    let created = f.service.create_worktree(&f.repo, &f.path, "AF-14449", "main").await.unwrap();

    assert_eq!(created.base_warning, None);
    assert_eq!(f.head_of(&f.path), f.sha);
}

#[tokio::test]
async fn test_recovers_failed_add_and_preserves_other_registration_and_commit() {
    let f = Fixture::new();
    f.stale();
    let other = f.folder.join("other");
    f.stale_at(&other);
    let failed = f.run(&["worktree", "add", "-b", "AF-14449", &Fixture::p(&f.path), "HEAD"]);
    assert!(String::from_utf8_lossy(&failed.stderr).contains("missing but already registered"));
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), f.sha);
    let actual = f.create().await.unwrap();
    assert_eq!(agentflow_core::pyfile::read_text(&f.path.join("file.txt")).unwrap(), "preserved\n");
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), f.sha);
    assert_eq!(actual, "AF-14449-1");
    assert_eq!(f.branch_in(&f.path), actual);
    let entries = f.service.list_worktrees(&f.repo).await.unwrap();
    assert!(entries.iter().any(|wt| same_path(&wt["worktree"], &other)), "{other:?} not in {entries:?}");
}

#[tokio::test]
async fn test_recovers_stale_path_before_creating_new_branch() {
    let f = Fixture::new();
    f.stale();
    f.create().await.unwrap();
    assert!(f.path.join("file.txt").is_file());
}

#[tokio::test]
async fn test_recovery_handles_spaces_in_registered_path() {
    let mut f = Fixture::new();
    f.path = f.folder.join("task with spaces");
    f.stale();
    f.create().await.unwrap();
    f.cleanup().await.unwrap();
    assert!(!f.path.exists());
}

#[tokio::test]
async fn test_recovers_different_branch_stale_registration_and_preserves_branch() {
    let f = Fixture::new();
    f.git(&["worktree", "add", "-b", "other", &Fixture::p(&f.path), "HEAD"]);
    remove_directory(&f.path);
    f.git(&["commit", "--allow-empty", "-qm", "base advanced"]);
    let current_base = f.out(&["rev-parse", "HEAD"]);
    f.create().await.unwrap();
    assert_eq!(f.out(&["rev-parse", "other"]), f.sha);
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), current_base);
    assert!(f.path.is_dir());
    assert_eq!(f.branch_in(&f.path), "AF-14449");
    let entries = f.service.list_worktrees(&f.repo).await.unwrap();
    assert!(
        !entries.iter().any(|wt| same_path(&wt["worktree"], &f.path)
            && wt.get("branch").map(String::as_str) == Some("refs/heads/other"))
    );
}

#[tokio::test]
async fn test_does_not_remove_different_branch_registration_if_branch_was_deleted() {
    let f = Fixture::new();
    f.git(&["worktree", "add", "-b", "other", &Fixture::p(&f.path), "HEAD"]);
    remove_directory(&f.path);
    f.git(&["update-ref", "-d", "refs/heads/other"]);
    assert_error(f.create().await, "not retained");
    assert!(f.registered(&f.path).await);
    assert!(!f.path.exists());
}

#[tokio::test]
async fn test_locked_missing_registration_is_preserved() {
    let f = Fixture::new();
    f.git(&["worktree", "add", "--detach", &Fixture::p(&f.path), "HEAD"]);
    f.git(&["worktree", "lock", &Fixture::p(&f.path)]);
    remove_directory(&f.path);
    assert_error(f.create().await, "locked");
    assert!(String::from_utf8_lossy(&f.git(&["worktree", "list", "--porcelain"]).stdout).contains("locked"));
}

#[tokio::test]
async fn test_existing_directory_is_preserved() {
    let f = Fixture::new();
    std::fs::create_dir(&f.path).unwrap();
    std::fs::write(f.path.join("owned.txt"), "keep").unwrap();
    assert!(f.create().await.is_err());
    assert_eq!(agentflow_core::pyfile::read_text(&f.path.join("owned.txt")).unwrap(), "keep");
}

#[tokio::test]
async fn test_advanced_existing_branch_is_not_reused() {
    let f = Fixture::new();
    f.stale();
    f.git(&["branch", "AF-14449"]);
    f.git(&["commit", "--allow-empty", "-qm", "new base"]);
    assert_error(f.create().await, "different commit");
    assert!(f.porcelain_mentions(&f.path));
}

#[tokio::test]
async fn test_branch_checked_out_elsewhere_uses_next_suffix() {
    let f = Fixture::new();
    f.stale();
    let active = f.folder.join("active");
    f.git(&["worktree", "add", "-b", "AF-14449", &Fixture::p(&active), "HEAD"]);
    let actual = f.create().await.unwrap();
    assert_eq!(actual, "AF-14449-1");
    assert_eq!(f.branch_in(&f.path), actual);
    assert_eq!(f.branch_in(&active), "AF-14449");
}

#[tokio::test]
async fn test_branch_collision_uses_first_available_numbered_suffix() {
    let f = Fixture::new();
    f.git(&["branch", "AF-14449"]);
    f.git(&["branch", "AF-14449-1"]);
    let actual = f.create().await.unwrap();
    assert_eq!(actual, "AF-14449-2");
    assert_eq!(f.branch_in(&f.path), actual);
}

#[tokio::test]
async fn test_existing_branch_with_new_commits_is_preserved_and_suffixed() {
    let f = Fixture::new();
    f.stale();
    f.git(&["branch", "AF-14449"]);
    f.git(&["checkout", "-q", "AF-14449"]);
    f.git(&["commit", "--allow-empty", "-qm", "user work"]);
    let ahead = f.out(&["rev-parse", "HEAD"]);
    f.git(&["checkout", "-q", "main"]);
    let actual = f.create().await.unwrap();
    assert_eq!(actual, "AF-14449-1");
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), ahead);
    assert_eq!(f.branch_in(&f.path), actual);
    assert_eq!(f.out(&["rev-parse", &actual]), f.sha);
}

#[tokio::test]
async fn test_detached_stale_commit_is_not_discarded_when_base_changed() {
    let f = Fixture::new();
    f.stale();
    f.git(&["commit", "--allow-empty", "-qm", "new base"]);
    assert_error(f.create().await, "different commit");
    assert!(f.porcelain_mentions(&f.path));
}

#[tokio::test]
async fn test_cleanup_is_idempotent_and_preserves_branch() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.cleanup().await.unwrap();
    f.cleanup().await.unwrap();
    assert!(!f.path.exists());
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), f.sha);
}

/// Ignored files are still wiped, but the reported list names only real changes.
#[tokio::test]
async fn test_cleanup_discards_ignored_files_without_enumerating_them() {
    let f = Fixture::new();
    f.create().await.unwrap();
    std::fs::write(f.path.join("notes.txt"), "keep").unwrap();
    f.git(&["config", "core.excludesFile", &Fixture::p(&f.folder.join("ignore"))]);
    std::fs::write(f.folder.join("ignore"), "built/\n").unwrap();
    std::fs::create_dir(f.path.join("built")).unwrap();
    std::fs::write(f.path.join("built").join("artifact.bin"), "generated").unwrap();
    let discarded = f.cleanup().await.unwrap();
    assert!(!f.path.exists());
    let names: Vec<&str> = discarded.iter().map(|line| &line[3..]).collect();
    assert!(names.contains(&"notes.txt"), "{names:?}");
    assert!(!names.contains(&"built/artifact.bin"), "{names:?}");
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), f.sha);
}

#[tokio::test]
async fn test_cleanup_discards_modified_files() {
    let f = Fixture::new();
    f.create().await.unwrap();
    std::fs::write(f.path.join("file.txt"), "changed").unwrap();
    let discarded = f.cleanup().await.unwrap();
    assert!(!f.path.exists());
    assert!(discarded.iter().any(|line| line.ends_with("file.txt")));
    assert_eq!(agentflow_core::pyfile::read_text(&f.repo.join("file.txt")).unwrap(), "preserved\n");
}

#[tokio::test]
async fn test_cleanup_still_rejects_locked_worktree_even_if_dirty() {
    let f = Fixture::new();
    f.create().await.unwrap();
    std::fs::write(f.path.join("file.txt"), "changed").unwrap();
    f.git(&["worktree", "lock", &Fixture::p(&f.path)]);
    assert_error(f.cleanup().await, "locked");
    assert!(f.path.exists());
    f.git(&["worktree", "unlock", &Fixture::p(&f.path)]);
}

#[tokio::test]
async fn test_cleanup_rejects_changed_branch_tip() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.commit_in_worktree("new work");
    assert_error(f.cleanup().await, "changed");
    assert!(f.path.exists());
}

/// Merged into a local branch: no PR vouches for the new commits, the human does. The branch,
/// and so every commit on it, is kept.
#[tokio::test]
async fn test_forced_cleanup_removes_a_branch_merged_elsewhere() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let tip = f.commit_in_worktree("merged locally");
    assert!(matches!(f.cleanup().await, Err(GitError::Unverified(_))));
    f.cleanup_forced(None).await.unwrap();
    assert!(!f.path.exists());
    assert!(!f.registered(&f.path).await);
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), tip);
}

#[tokio::test]
async fn test_forced_cleanup_still_rejects_locked_or_foreign_worktree() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.commit_in_worktree("merged locally");
    f.git(&["worktree", "lock", &Fixture::p(&f.path)]);
    assert_error(f.cleanup_forced(None).await, "locked");
    f.git(&["worktree", "unlock", &Fixture::p(&f.path)]);
    f.git(&["-C", &Fixture::p(&f.path), "checkout", "-q", "-b", "someone-else"]);
    assert_error(f.cleanup_forced(None).await, "no longer belongs");
    assert!(f.path.exists());
    assert_error(
        f.service.cleanup_worktree(&f.repo, &f.repo, "main", &f.sha, None, None, true).await,
        "repository root",
    );
}

#[tokio::test]
async fn test_forced_cleanup_recovers_leftover_directory() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.commit_in_worktree("merged locally");
    remove_directory(&f.admin_dir());
    assert!(matches!(f.cleanup_with(&f.sha, Some(&f.folder), None).await, Err(GitError::Unverified(_))));
    assert_error(f.cleanup_forced(None).await, "not a registered");
    assert!(f.path.exists());
    f.cleanup_forced(Some(&f.folder)).await.unwrap();
    assert!(!f.path.exists());
}

/// The human pushed `<branch>-rc` from the worktree, so it is no longer on the task branch; its
/// tip is the head GitHub merged, which vouches for it. Both branches are kept.
#[tokio::test]
async fn test_cleanup_accepts_a_worktree_switched_to_a_branch_that_is_the_merged_head() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.git(&["-C", &Fixture::p(&f.path), "checkout", "-q", "-b", "AF-14449-rc"]);
    let merged_head = f.commit_in_worktree("address PR review");
    assert_eq!(f.branch_in(&f.path), "AF-14449-rc");
    f.cleanup_with(&f.sha, None, Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
    assert!(!f.registered(&f.path).await);
    assert_eq!(f.out(&["rev-parse", "AF-14449-rc"]), merged_head);
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), f.sha);
}

/// A branch behind the merged head is as safe as one at it.
#[tokio::test]
async fn test_cleanup_accepts_a_switched_worktree_behind_the_merged_pr() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.git(&["-C", &Fixture::p(&f.path), "checkout", "-q", "-b", "AF-14449-rc"]);
    let local = f.commit_in_worktree("address PR review");
    let merged_head = f.commit_outside_worktree(&local, "edited on GitHub");
    f.cleanup_with(&f.sha, None, Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
}

#[tokio::test]
async fn test_cleanup_refuses_a_switched_worktree_with_commits_nothing_vouches_for() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.git(&["-C", &Fixture::p(&f.path), "checkout", "-q", "-b", "AF-14449-rc"]);
    let merged_head = f.commit_in_worktree("address PR review");
    f.commit_in_worktree("unmerged follow-up");
    let error = f.cleanup_with(&f.sha, None, Some(&merged_head)).await.unwrap_err();
    assert!(error.to_string().contains("branch AF-14449-rc"), "{error}");
    assert!(error.to_string().contains("no longer belongs"), "{error}");
    assert!(!matches!(error, GitError::Unverified(_)), "force must not be offered for another branch");
    assert!(f.path.exists());
    assert_error(f.cleanup_forced(None).await, "no longer belongs");
}

#[tokio::test]
async fn test_cleanup_accepts_a_detached_worktree_only_when_its_commits_are_vouched_for() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.git(&["-C", &Fixture::p(&f.path), "checkout", "-q", "--detach"]);
    let merged_head = f.commit_in_worktree("address PR review");
    f.commit_in_worktree("unmerged follow-up");
    assert_error(f.cleanup_with(&f.sha, None, Some(&merged_head)).await, "detached HEAD");
    assert!(f.path.exists());
    f.git(&["-C", &Fixture::p(&f.path), "checkout", "-q", "--detach", &merged_head]);
    f.cleanup_with(&f.sha, None, Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
}

/// The branch was deleted after the merge, leaving an unregistered directory: nothing to check it
/// against, so it is the human's call (force) rather than a dead end.
#[tokio::test]
async fn test_cleanup_leftover_directory_of_a_deleted_branch_can_be_forced() {
    let f = Fixture::new();
    f.create().await.unwrap();
    remove_directory(&f.admin_dir());
    f.git(&["branch", "-D", "AF-14449"]);
    let error = f.cleanup_with(&f.sha, Some(&f.folder), None).await.unwrap_err();
    assert!(matches!(error, GitError::Unverified(_)), "{error}");
    assert!(error.to_string().contains("no longer exists"), "{error}");
    assert!(f.path.exists());
    f.cleanup_forced(Some(&f.folder)).await.unwrap();
    assert!(!f.path.exists());
}

/// The OS error says why the delete failed (here a file held open without sharing), where the
/// message used to say only that it could not be removed.
#[cfg(windows)]
#[tokio::test]
async fn test_a_leftover_directory_that_cannot_be_deleted_reports_the_os_error() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    f.create().await.unwrap();
    remove_directory(&f.admin_dir());
    let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(f.path.join("file.txt")).unwrap();
    let error = f.cleanup_with(&f.sha, Some(&f.folder), None).await.unwrap_err();
    assert!(matches!(error, GitError::Busy(_)), "{error}");
    assert!(error.to_string().contains("(os error"), "{error}");
    assert!(f.path.exists());
}

/// PR feedback commits land after final review; GitHub's merged head vouches for them.
#[tokio::test]
async fn test_cleanup_accepts_commits_after_review_that_match_the_merged_pr() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let merged_head = f.commit_in_worktree("address PR review");
    f.cleanup_with(&f.sha, None, Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), merged_head);
}

/// Every local commit is in the merged PR, which also has one made elsewhere.
#[tokio::test]
async fn test_cleanup_accepts_a_branch_behind_the_merged_pr() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let local = f.commit_in_worktree("address PR review");
    let merged_head = f.commit_outside_worktree(&local, "edited on GitHub");
    f.cleanup_with(&f.sha, None, Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
    assert_eq!(f.out(&["rev-parse", "AF-14449"]), local);
}

#[tokio::test]
async fn test_cleanup_rejects_a_branch_that_diverged_from_the_merged_pr() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let merged_head = f.commit_outside_worktree(&f.sha, "edited on GitHub");
    f.commit_in_worktree("unmerged local work");
    assert_error(f.cleanup_with(&f.sha, None, Some(&merged_head)).await, "changed");
    assert!(f.path.exists());
}

#[tokio::test]
async fn test_cleanup_rejects_a_merged_head_missing_locally() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.commit_in_worktree("address PR review");
    assert_error(f.cleanup_with(&f.sha, None, Some(&"f".repeat(40))).await, "changed");
    assert!(f.path.exists());
}

#[tokio::test]
async fn test_cleanup_leftover_directory_accepts_a_branch_behind_the_merged_pr() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let local = f.commit_in_worktree("address PR review");
    let merged_head = f.commit_outside_worktree(&local, "edited on GitHub");
    remove_directory(&f.admin_dir());
    f.cleanup_with(&f.sha, Some(&f.folder), Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
}

#[tokio::test]
async fn test_cleanup_rejects_commits_beyond_the_merged_pr() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let merged_head = f.commit_in_worktree("address PR review");
    f.commit_in_worktree("unmerged follow-up");
    assert_error(f.cleanup_with(&f.sha, None, Some(&merged_head)).await, "changed");
    assert!(f.path.exists());
}

#[tokio::test]
async fn test_cleanup_leftover_directory_accepts_the_merged_pr_head() {
    let f = Fixture::new();
    f.create().await.unwrap();
    let merged_head = f.commit_in_worktree("address PR review");
    remove_directory(&f.admin_dir());
    assert!(matches!(f.cleanup_with(&f.sha, Some(&f.folder), None).await, Err(GitError::Unverified(_))));
    f.cleanup_with(&f.sha, Some(&f.folder), Some(&merged_head)).await.unwrap();
    assert!(!f.path.exists());
}

#[tokio::test]
async fn test_cleanup_rejects_repository_root_and_unregistered_directory() {
    let f = Fixture::new();
    assert_error(
        f.service.cleanup_worktree(&f.repo, &f.repo, "main", &f.sha, None, None, false).await,
        "repository root",
    );
    std::fs::create_dir(&f.path).unwrap();
    assert_error(f.cleanup().await, "not a registered");
    assert!(f.path.exists());
}

/// `git worktree remove` deregisters before deleting, so a failed delete leaves an unregistered
/// directory; a retry must finish the job.
#[tokio::test]
async fn test_cleanup_recovers_leftover_directory_after_partial_removal() {
    let f = Fixture::new();
    f.create().await.unwrap();
    assert!(f.admin_dir().is_dir());
    remove_directory(&f.admin_dir());
    assert!(f.path.is_dir());
    assert!(!f.registered(&f.path).await);
    let discarded = f.cleanup_with(&f.sha, Some(&f.folder), None).await.unwrap();
    assert!(discarded.is_empty());
    assert!(!f.path.exists());
}

#[tokio::test]
async fn test_cleanup_leftover_directory_refuses_if_checkpoint_changed() {
    let f = Fixture::new();
    f.create().await.unwrap();
    remove_directory(&f.admin_dir());
    assert!(matches!(f.cleanup_with(&"b".repeat(40), Some(&f.folder), None).await, Err(GitError::Unverified(_))));
    assert!(f.path.exists());
}

#[tokio::test]
async fn test_cleanup_leftover_directory_refuses_outside_managed_worktree_dir() {
    let f = Fixture::new();
    f.create().await.unwrap();
    remove_directory(&f.admin_dir());
    let other_dir = f.folder.join("not-the-worktree-dir");
    std::fs::create_dir(&other_dir).unwrap();
    assert_error(f.cleanup_with(&f.sha, Some(&other_dir), None).await, "not a registered");
    assert!(f.path.exists());
}

/// Python mocks a failing `git worktree list`; here git really fails on a non-repository.
#[tokio::test]
async fn test_listing_failure_is_not_treated_as_empty() {
    let f = Fixture::new();
    let not_a_repo = f.folder.join("plain");
    std::fs::create_dir(&not_a_repo).unwrap();
    assert_error(f.service.list_worktrees(&not_a_repo).await, "Cannot inspect");
}

// ------------------------------------------------------------------------------------------
// Beyond the Python file: behaviour the engine relies on, and the deliberate path difference.
// ------------------------------------------------------------------------------------------

#[tokio::test]
async fn checkpoint_commits_changes_as_agentflow_and_reports_the_sha() {
    let f = Fixture::new();
    f.create().await.unwrap();
    std::fs::write(f.path.join("new.txt"), "x").unwrap();
    assert_error(f.service.checkpoint(&f.path, "AgentFlow checkpoint", false).await, "Commit the worktree changes");
    let (sha, committed) = f.service.checkpoint(&f.path, "AgentFlow checkpoint", true).await.unwrap();
    assert!(committed);
    assert_eq!(sha, f.out(&["-C", &Fixture::p(&f.path), "rev-parse", "HEAD"]));
    assert_eq!(
        f.out(&["-C", &Fixture::p(&f.path), "log", "-1", "--format=%an <%ae>|%s"]),
        "AgentFlow <agentflow@local>|AgentFlow checkpoint"
    );
    let (same, committed) = f.service.checkpoint(&f.path, "nothing to do", true).await.unwrap();
    assert_eq!((same, committed), (sha, false));
}

#[tokio::test]
async fn inspect_repository_lists_local_and_remote_branches() {
    let f = Fixture::new();
    f.git(&["branch", "feature"]);
    let info = f.service.inspect_repository(&Fixture::p(&f.repo.join("."))).await.unwrap();
    assert!(same_path(&info.root, &f.repo));
    assert_eq!(info.current_branch.as_deref(), Some("main"));
    assert_eq!(info.branches, ["feature", "main"]);
    assert_error(
        f.service.inspect_repository(&Fixture::p(&f.folder.join("missing"))).await,
        "Directory does not exist",
    );
}

#[tokio::test]
async fn branch_report_counts_unmerged_commits() {
    let f = Fixture::new();
    f.create().await.unwrap();
    f.commit_in_worktree("one");
    f.commit_in_worktree("two");
    let report = f.service.branch_report(&f.repo, "AF-14449", "main").await.unwrap();
    assert_eq!(report["exists"], true);
    assert_eq!(report["unmergedCommits"], 2);
    assert_eq!(report["upstream"], serde_json::Value::Null);
    let missing = f.service.branch_report(&f.repo, "nope", "main").await.unwrap();
    assert_eq!(missing["exists"], false);
}

#[tokio::test]
async fn remove_task_worktree_stays_inside_the_managed_directory() {
    let f = Fixture::new();
    f.create().await.unwrap();
    std::fs::write(f.path.join("file.txt"), "changed").unwrap();
    let elsewhere = f.folder.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    assert_error(f.service.remove_task_worktree(&f.repo, &f.path, &elsewhere).await, "outside the managed");
    let discarded = f.service.remove_task_worktree(&f.repo, &f.path, &f.folder).await.unwrap();
    assert!(discarded.iter().any(|l| l.ends_with("file.txt")));
    assert!(!f.path.exists());
    assert!(f.service.delete_branch(&f.repo, "AF-14449").await.unwrap());
    assert!(!f.service.delete_branch(&f.repo, "AF-14449").await.unwrap());
}

/// Python rejected this (resolve() != absolute()); the port accepts a worktree directory reached
/// through an 8.3 short name, which is what %TEMP% often is on Windows.
#[cfg(windows)]
#[tokio::test]
async fn creates_a_worktree_through_an_8_3_short_path() {
    let mut f = Fixture::new();
    let long = f.folder.join("long worktree parent");
    std::fs::create_dir(&long).unwrap();
    let output = Command::new("cmd")
        .args(["/c", "for", "%I", "in"])
        .arg(format!("(\"{}\")", long.display()))
        .args(["do", "@echo", "%~sI"])
        .output()
        .unwrap();
    let short = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if short.is_empty() || short.eq_ignore_ascii_case(&long.to_string_lossy()) {
        eprintln!("no 8.3 name on this volume; skipped");
        return;
    }
    f.path = PathBuf::from(short).join("AF-14449");
    f.create().await.unwrap();
    assert!(long.join("AF-14449").join("file.txt").is_file());
    f.cleanup().await.unwrap();
    assert!(!long.join("AF-14449").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn refuses_a_worktree_path_through_a_junction() {
    let mut f = Fixture::new();
    let target = f.folder.join("real");
    let junction = f.folder.join("junction");
    std::fs::create_dir(&target).unwrap();
    assert!(
        Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&junction).arg(&target).output().unwrap().status.success()
    );
    f.path = junction.join("AF-14449");
    assert_error(f.create().await, "already exists");
    assert!(!target.join("AF-14449").exists());
    std::fs::remove_dir(&junction).unwrap();
}

// Ported from backend/tests/test_task_archive_delete.py (TaskDeleteGitTests) ------------------------

#[tokio::test]
async fn test_unmerged_work_is_reported_then_removed() {
    let f = Fixture::new();
    let trees = f.folder.join("trees");
    let tree = trees.join("TEST-1");
    f.git(&["worktree", "add", "-q", "-b", "agent/TEST-1", &Fixture::p(&tree), "main"]);
    std::fs::write(tree.join("work.txt"), "work\n").unwrap();
    f.git(&["-C", &Fixture::p(&tree), "add", "work.txt"]);
    f.git(&["-C", &Fixture::p(&tree), "commit", "-qm", "work"]);
    std::fs::write(tree.join("dirty.txt"), "uncommitted\n").unwrap();

    let report = f.service.branch_report(&f.repo, "agent/TEST-1", "main").await.unwrap();
    assert!(report["exists"] == true && report["unmergedCommits"] == 1, "{report}");
    assert!(report["upstream"].is_null() && report["unpushedCommits"].is_null());

    let discarded = f.service.remove_task_worktree(&f.repo, &tree, &trees).await.unwrap();
    assert_eq!(discarded.iter().map(|line| &line[3..]).collect::<Vec<_>>(), ["dirty.txt"]);
    assert!(!tree.exists());
    assert!(f.service.delete_branch(&f.repo, "agent/TEST-1").await.unwrap());
    assert!(!f.service.delete_branch(&f.repo, "agent/TEST-1").await.unwrap());
    assert_eq!(f.service.branch_report(&f.repo, "agent/TEST-1", "main").await.unwrap()["exists"], false);
}

#[tokio::test]
async fn test_worktree_outside_the_managed_directory_is_refused() {
    let f = Fixture::new();
    let trees = f.folder.join("trees");
    let outside = f.folder.join("elsewhere");
    f.git(&["worktree", "add", "-q", "-b", "agent/TEST-2", &Fixture::p(&outside), "main"]);
    assert!(f.service.remove_task_worktree(&f.repo, &outside, &trees).await.is_err());
    assert!(f.service.remove_task_worktree(&f.repo, &f.repo, &trees).await.is_err());
    assert!(outside.exists());
}

#[tokio::test]
async fn test_unregistered_leftover_directory_is_removed() {
    let f = Fixture::new();
    let trees = f.folder.join("trees");
    let leftover = trees.join("TEST-3");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("junk.txt"), "x").unwrap();
    assert!(f.service.remove_task_worktree(&f.repo, &leftover, &trees).await.unwrap().is_empty());
    assert!(!leftover.exists());
}

#[tokio::test]
async fn test_push_branch_publishes_the_task_branch_and_tracks_it() {
    let f = Fixture::new();
    let remote = f.add_origin();
    f.create().await.unwrap();
    let tip = f.commit_in_worktree("reviewed change");

    f.service.push_branch(&f.path, "AF-14449").await.unwrap();

    let on_origin = Command::new("git").arg("--git-dir").arg(&remote).args(["rev-parse", "AF-14449"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&on_origin.stdout).trim(), tip);
    assert_eq!(f.out(&["rev-parse", "--abbrev-ref", "AF-14449@{upstream}"]), "origin/AF-14449");
    // Pushing again with nothing new is fine.
    f.service.push_branch(&f.path, "AF-14449").await.unwrap();
}

#[tokio::test]
async fn test_failed_push_names_the_branch_but_not_the_remote() {
    let f = Fixture::new();
    f.add_origin();
    f.create().await.unwrap();
    let secret = "https://user:hunter2@127.0.0.1:9/acme/repo.git";
    f.git(&["remote", "set-url", "origin", secret]);

    let error = f.service.push_branch(&f.path, "AF-14449").await.unwrap_err().to_string();

    assert!(error.contains("Pushing AF-14449 to origin failed"), "{error}");
    assert!(!error.contains("hunter2") && !error.contains("127.0.0.1"), "{error}");
}

// ---------------------------------------------------------------------- finding code (test_finding_code.py)

impl Fixture {
    /// Commit `text` as file.txt and return the new HEAD.
    fn commit(&self, text: &str, message: &str) -> String {
        std::fs::write(self.repo.join("file.txt"), text).unwrap();
        self.git(&["commit", "-qam", message]);
        self.out(&["rev-parse", "HEAD"])
    }
}

#[tokio::test]
async fn test_file_at_reads_a_commit_and_misses_other_paths() {
    let f = Fixture::new();
    let text = f.service.file_at(&f.repo, &f.sha, "file.txt", 100).await.unwrap();
    assert_eq!(text.as_deref().map(str::trim_end), Some("preserved"));
    assert_eq!(f.service.file_at(&f.repo, &f.sha, "missing.txt", 100).await.unwrap(), None);
    // A pathspec is literal: a glob matches nothing.
    assert_eq!(f.service.file_at(&f.repo, &f.sha, "*.txt", 100).await.unwrap(), None);
    // Too large is refused before the file is read.
    let error = f.service.file_at(&f.repo, &f.sha, "file.txt", 4).await.unwrap_err().to_string();
    assert_eq!(error, "file.txt is too large to show");
    // A directory is not a file.
    std::fs::create_dir(f.repo.join("dir")).unwrap();
    std::fs::write(f.repo.join("dir").join("a"), "a\n").unwrap();
    f.git(&["add", "-A"]);
    let head = f.commit("preserved\n", "dir");
    assert_eq!(f.service.file_at(&f.repo, &head, "dir", 100).await.unwrap(), None);
}

#[tokio::test]
async fn test_fork_point_finds_where_the_branch_left_its_base() {
    let f = Fixture::new();
    f.git(&["switch", "-qc", "task"]);
    let reviewed = f.commit("preserved\nchanged\n", "task change");
    let fixed = f.commit("preserved\nfixed\n", "fix");
    assert_eq!(f.service.fork_point(&f.repo, &reviewed, "main").await.unwrap(), Some(f.sha.clone()));
    assert_eq!(f.service.fork_point(&f.repo, &reviewed, "nope").await.unwrap(), None);
    let diff = f.service.file_diff(&f.repo, &f.sha, &reviewed, "file.txt").await.unwrap();
    assert!(diff.contains("@@ -1 +1,2 @@") && diff.contains("+changed"), "{diff}");

    // Merged with a merge commit: the fork is still found through the merge's first parent.
    f.git(&["switch", "-q", "main"]);
    std::fs::write(f.repo.join("other.txt"), "main moves on\n").unwrap();
    f.git(&["add", "other.txt"]);
    f.git(&["commit", "-qm", "main moves on"]);
    f.git(&["merge", "-q", "--no-ff", "-m", "merge task", "task"]);
    assert_eq!(f.service.fork_point(&f.repo, &reviewed, "main").await.unwrap(), Some(f.sha.clone()));

    // Fast-forwarded: nothing tells where the branch began, so the reviewed commit is its own base.
    f.git(&["reset", "-q", "--hard", &f.sha]);
    f.git(&["merge", "-q", "--ff-only", &fixed]);
    assert_eq!(f.service.fork_point(&f.repo, &reviewed, "main").await.unwrap(), Some(reviewed));
}
