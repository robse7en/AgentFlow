"""Real Git recovery and removal checks; all disposable data stays in this workspace."""
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from agentflow.services.git_workspace import BranchUnverified, GitError, GitService, WorktreeBusy


class WorktreeTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.workspace = Path(__file__).resolve().parents[2]
        self.folder = Path(tempfile.mkdtemp(prefix=".git-verification-", dir=self.workspace))
        self.repo = self.folder / "repo"
        self.repo.mkdir()
        self.path = self.folder / "AF-14449"
        self.git("init", "-q", "-b", "main")
        ignore = self.folder / "ignore"
        ignore.touch()
        for key, value in {"core.excludesFile": str(ignore), "core.hooksPath": str(self.folder / "hooks"),
                           "commit.gpgSign": "false", "user.name": "Test", "user.email": "test@localhost"}.items():
            self.git("config", key, value)
        (self.repo / "file.txt").write_text("preserved\n")
        self.git("add", "file.txt")
        self.git("commit", "-qm", "fixture")
        self.sha = self.git("rev-parse", "HEAD").stdout.strip()
        self.service = GitService()

    def tearDown(self):
        self.remove_directory(self.folder)

    def remove_directory(self, path):
        resolved = path.resolve()
        if resolved == self.workspace or not resolved.is_relative_to(self.workspace):
            raise AssertionError("Refusing unsafe test cleanup")
        def writable_retry(function, filename, error):
            Path(filename).chmod(0o700)
            function(filename)
        shutil.rmtree(resolved, onerror=writable_retry)

    def git(self, *args, check=True):
        result = subprocess.run(["git", "-C", str(self.repo), *map(str, args)],
                                capture_output=True, text=True)
        if check and result.returncode:
            self.fail(result.stderr)
        return result

    def stale(self, path=None):
        target = path or self.path
        self.git("worktree", "add", "--detach", target, "HEAD")
        self.remove_directory(target)

    async def create(self):
        return (await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "main")).branch

    def add_origin(self):
        """A bare `origin` holding `main` as it is now."""
        remote = self.folder / "origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True, capture_output=True)
        self.git("remote", "add", "origin", remote)
        self.git("push", "-q", "origin", "main")
        return remote

    def advance_origin(self, message):
        """Commit on top of `main` straight to origin, leaving local `main` behind it; the fetch
        before branching is what brings it in."""
        tree = self.git("rev-parse", "main^{tree}").stdout.strip()
        sha = self.git("commit-tree", tree, "-p", "main", "-m", message).stdout.strip()
        self.git("push", "-q", "origin", f"{sha}:refs/heads/main")
        self.git("update-ref", "refs/remotes/origin/main", "main")
        return sha

    def head_of(self, path):
        return self.git("-C", path, "rev-parse", "HEAD").stdout.strip()

    async def cleanup(self):
        return await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha)

    async def cleanup_forced(self, worktree_dir=None):
        return await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                   worktree_dir=worktree_dir, force=True)

    async def test_create_worktree_fetches_a_base_branch_that_only_exists_on_origin(self):
        """Task creation relies on create_worktree alone to fetch origin and resolve the base
        ref (the engine no longer resolves it separately first)."""
        remote = self.folder / "origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True, capture_output=True)
        self.git("remote", "add", "origin", str(remote))
        self.git("push", "-q", "origin", "main:release")
        # push records origin/release locally; drop it so the branch exists on origin only.
        self.git("update-ref", "-d", "refs/remotes/origin/release")
        self.assertFalse(await self.service.branch_exists(str(self.repo), "release"))
        self.assertFalse(self.git("rev-parse", "--verify", "--quiet", "refs/remotes/origin/release", check=False).returncode == 0)

        created = await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "release")

        self.assertEqual(created.branch, "AF-14449")
        self.assertIsNone(created.base_warning)
        self.assertEqual(self.git("-C", self.path, "rev-parse", "HEAD").stdout.strip(), self.sha)
        self.assertEqual(self.git("rev-parse", "refs/remotes/origin/release").stdout.strip(), self.sha)

    async def test_new_worktree_branches_from_origin_when_the_local_base_is_behind(self):
        self.add_origin()
        newer = self.advance_origin("newer on origin")

        created = await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "main")

        self.assertIsNone(created.base_warning)
        self.assertEqual(self.head_of(self.path), newer)
        # The local base is left alone.
        self.assertEqual(self.git("rev-parse", "main").stdout.strip(), self.sha)
        # Branched with --no-track: origin/main is not the task branch's upstream.
        self.assertNotEqual(self.git("rev-parse", "--abbrev-ref", "AF-14449@{upstream}", check=False).returncode, 0)

    async def test_new_worktree_keeps_unpushed_commits_on_the_local_base(self):
        self.add_origin()
        self.git("commit", "--allow-empty", "-qm", "local only")
        local = self.git("rev-parse", "main").stdout.strip()

        await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "main")

        self.assertEqual(self.head_of(self.path), local)

    async def test_new_worktree_branches_from_origin_when_the_local_base_has_diverged(self):
        self.add_origin()
        newer = self.advance_origin("newer on origin")
        self.git("commit", "--allow-empty", "-qm", "local only")

        await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "main")

        self.assertEqual(self.head_of(self.path), newer)

    async def test_failed_fetch_of_a_base_known_on_origin_warns_that_it_may_be_out_of_date(self):
        self.add_origin()
        self.git("remote", "set-url", "origin", self.folder / "missing.git")

        created = await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "main")

        self.assertEqual(self.head_of(self.path), self.sha)
        self.assertIn("Couldn't fetch main from origin", created.base_warning or "")

    async def test_repository_without_origin_branches_from_the_local_base_without_a_warning(self):
        created = await self.service.create_worktree(str(self.repo), self.path, "AF-14449", "main")

        self.assertIsNone(created.base_warning)
        self.assertEqual(self.head_of(self.path), self.sha)

    async def test_recovers_failed_add_and_preserves_other_registration_and_commit(self):
        self.stale()
        other = self.folder / "other"
        self.stale(other)
        failed = self.git("worktree", "add", "-b", "AF-14449", self.path, "HEAD", check=False)
        self.assertIn("missing but already registered", failed.stderr)
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), self.sha)
        actual_branch = await self.create()
        self.assertEqual((self.path / "file.txt").read_text(), "preserved\n")
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), self.sha)
        self.assertEqual(actual_branch, "AF-14449-1")
        self.assertEqual(self.git("-C", self.path, "branch", "--show-current").stdout.strip(), actual_branch)
        entries = await self.service.list_worktrees(str(self.repo))
        self.assertTrue(any(Path(wt["worktree"]) == other for wt in entries))

    async def test_recovers_stale_path_before_creating_new_branch(self):
        self.stale()
        await self.create()
        self.assertTrue((self.path / "file.txt").is_file())

    async def test_recovery_handles_spaces_in_registered_path(self):
        self.path = self.folder / "task with spaces"
        self.stale()
        await self.create()
        await self.cleanup()
        self.assertFalse(self.path.exists())

    async def test_recovers_different_branch_stale_registration_and_preserves_branch(self):
        self.git("worktree", "add", "-b", "other", self.path, "HEAD")
        self.remove_directory(self.path)
        self.git("commit", "--allow-empty", "-qm", "base advanced")
        current_base = self.git("rev-parse", "HEAD").stdout.strip()
        await self.create()
        self.assertEqual(self.git("rev-parse", "other").stdout.strip(), self.sha)
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), current_base)
        self.assertTrue(self.path.is_dir())
        self.assertEqual(self.git("-C", self.path, "branch", "--show-current").stdout.strip(), "AF-14449")
        entries = await self.service.list_worktrees(str(self.repo))
        self.assertFalse(any(Path(wt['worktree']) == self.path and wt.get('branch') == 'refs/heads/other'
                             for wt in entries))

    async def test_does_not_remove_different_branch_registration_if_branch_was_deleted(self):
        self.git("worktree", "add", "-b", "other", self.path, "HEAD")
        self.remove_directory(self.path)
        self.git("update-ref", "-d", "refs/heads/other")
        with self.assertRaisesRegex(GitError, "not retained"):
            await self.create()
        entries = await self.service.list_worktrees(str(self.repo))
        self.assertTrue(any(Path(wt['worktree']) == self.path for wt in entries))
        self.assertFalse(self.path.exists())

    async def test_locked_missing_registration_is_preserved(self):
        self.git("worktree", "add", "--detach", self.path, "HEAD")
        self.git("worktree", "lock", self.path)
        self.remove_directory(self.path)
        with self.assertRaisesRegex(GitError, "locked"):
            await self.create()
        self.assertIn("locked", self.git("worktree", "list", "--porcelain").stdout)

    async def test_existing_directory_is_preserved(self):
        self.path.mkdir()
        (self.path / "owned.txt").write_text("keep")
        with self.assertRaises(GitError):
            await self.create()
        self.assertEqual((self.path / "owned.txt").read_text(), "keep")

    async def test_advanced_existing_branch_is_not_reused(self):
        self.stale()
        self.git("branch", "AF-14449")
        self.git("commit", "--allow-empty", "-qm", "new base")
        with self.assertRaisesRegex(GitError, "different commit"):
            await self.create()
        self.assertIn(str(self.path).replace("\\", "/"), self.git("worktree", "list", "--porcelain").stdout)

    async def test_branch_checked_out_elsewhere_uses_next_suffix(self):
        self.stale()
        self.git("worktree", "add", "-b", "AF-14449", self.folder / "active", "HEAD")
        actual_branch = await self.create()
        self.assertEqual(actual_branch, "AF-14449-1")
        self.assertEqual(self.git("-C", self.path, "branch", "--show-current").stdout.strip(), actual_branch)
        self.assertEqual(self.git("-C", self.folder / "active", "branch", "--show-current").stdout.strip(),
                         "AF-14449")

    async def test_branch_collision_uses_first_available_numbered_suffix(self):
        self.git("branch", "AF-14449")
        self.git("branch", "AF-14449-1")
        actual_branch = await self.create()
        self.assertEqual(actual_branch, "AF-14449-2")
        self.assertEqual(self.git("-C", self.path, "branch", "--show-current").stdout.strip(), actual_branch)

    async def test_existing_branch_with_new_commits_is_preserved_and_suffixed(self):
        self.stale()
        self.git("branch", "AF-14449")
        self.git("checkout", "-q", "AF-14449")
        self.git("commit", "--allow-empty", "-qm", "user work")
        ahead = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("checkout", "-q", "main")
        actual_branch = await self.create()
        self.assertEqual(actual_branch, "AF-14449-1")
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), ahead)
        self.assertEqual(self.git("-C", self.path, "branch", "--show-current").stdout.strip(), actual_branch)
        self.assertEqual(self.git("rev-parse", actual_branch).stdout.strip(), self.sha)

    async def test_detached_stale_commit_is_not_discarded_when_base_changed(self):
        self.stale()
        self.git("commit", "--allow-empty", "-qm", "new base")
        with self.assertRaisesRegex(GitError, "different commit"):
            await self.create()
        self.assertIn(str(self.path).replace("\\", "/"), self.git("worktree", "list", "--porcelain").stdout)

    async def test_cleanup_is_idempotent_and_preserves_branch(self):
        await self.create()
        await self.cleanup()
        await self.cleanup()
        self.assertFalse(self.path.exists())
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), self.sha)

    async def test_cleanup_discards_ignored_files_without_enumerating_them(self):
        """Ignored files are still physically wiped (a repo can have tens of thousands of
        them under node_modules/bin/obj), but the reported `discarded` list - meant for a
        human to sanity-check before an irreversible discard - is not made to walk and name
        every one of them; only real (untracked/modified) changes are worth their attention."""
        await self.create()
        (self.path / "notes.txt").write_text("keep")
        self.git("config", "core.excludesFile", str(self.folder / "ignore"))
        (self.folder / "ignore").write_text("built/\n")
        (self.path / "built").mkdir()
        (self.path / "built" / "artifact.bin").write_text("generated")
        discarded = await self.cleanup()
        self.assertFalse(self.path.exists())  # ignored files were still wiped with everything else
        names = [line[3:] for line in discarded]
        self.assertIn("notes.txt", names)
        self.assertNotIn("built/artifact.bin", names)
        # branch itself is untouched: it wasn't the branch content that was discarded
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), self.sha)

    async def test_cleanup_discards_modified_files(self):
        await self.create()
        (self.path / "file.txt").write_text("changed")
        discarded = await self.cleanup()
        self.assertFalse(self.path.exists())
        self.assertTrue(any(line.endswith("file.txt") for line in discarded))
        self.assertEqual((self.repo / "file.txt").read_text(), "preserved\n")

    async def test_cleanup_still_rejects_locked_worktree_even_if_dirty(self):
        await self.create()
        (self.path / "file.txt").write_text("changed")
        self.git("worktree", "lock", self.path)
        with self.assertRaisesRegex(GitError, "locked"):
            await self.cleanup()
        self.assertTrue(self.path.exists())

    async def test_cleanup_rejects_changed_branch_tip(self):
        await self.create()
        self.git("-C", self.path, "commit", "--allow-empty", "-qm", "new work")
        with self.assertRaisesRegex(GitError, "changed"):
            await self.cleanup()
        self.assertTrue(self.path.exists())

    async def test_forced_cleanup_removes_a_branch_merged_elsewhere(self):
        """Merged into a local branch: no PR vouches for the new commits, the human does. The
        branch, and so every commit on it, is kept."""
        await self.create()
        tip = self.commit_in_worktree("merged locally")
        with self.assertRaises(BranchUnverified):
            await self.cleanup()
        await self.cleanup_forced()
        self.assertFalse(self.path.exists())
        self.assertNotIn(str(self.path), self.git("worktree", "list").stdout)
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), tip)

    async def test_forced_cleanup_still_rejects_locked_or_foreign_worktree(self):
        await self.create()
        self.commit_in_worktree("merged locally")
        self.git("worktree", "lock", self.path)
        with self.assertRaisesRegex(GitError, "locked"):
            await self.cleanup_forced()
        self.git("worktree", "unlock", self.path)
        self.git("-C", self.path, "checkout", "-q", "-b", "someone-else")
        with self.assertRaisesRegex(GitError, "no longer belongs"):
            await self.cleanup_forced()
        self.assertTrue(self.path.exists())
        with self.assertRaisesRegex(GitError, "repository root"):
            await self.service.cleanup_worktree(str(self.repo), str(self.repo), "main", self.sha, force=True)

    async def test_forced_cleanup_recovers_leftover_directory(self):
        await self.create()
        self.commit_in_worktree("merged locally")
        shutil.rmtree(self.repo / ".git" / "worktrees" / self.path.name)
        with self.assertRaises(BranchUnverified):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                worktree_dir=self.folder)
        with self.assertRaisesRegex(GitError, "not a registered"):
            await self.cleanup_forced()
        self.assertTrue(self.path.exists())
        await self.cleanup_forced(worktree_dir=self.folder)
        self.assertFalse(self.path.exists())

    def commit_in_worktree(self, message):
        self.git("-C", self.path, "commit", "--allow-empty", "-qm", message)
        return self.git("-C", self.path, "rev-parse", "HEAD").stdout.strip()

    async def test_cleanup_accepts_a_worktree_switched_to_a_branch_that_is_the_merged_head(self):
        """The human pushed `<branch>-rc` from the worktree, so it is no longer on the task branch;
        its tip is the head GitHub merged, which vouches for it. Both branches are kept."""
        await self.create()
        self.git("-C", self.path, "checkout", "-q", "-b", "AF-14449-rc")
        merged_head = self.commit_in_worktree("address PR review")
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())
        self.assertNotIn(str(self.path), self.git("worktree", "list").stdout)
        self.assertEqual(self.git("rev-parse", "AF-14449-rc").stdout.strip(), merged_head)
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), self.sha)

    async def test_cleanup_accepts_a_switched_worktree_behind_the_merged_pr(self):
        await self.create()
        self.git("-C", self.path, "checkout", "-q", "-b", "AF-14449-rc")
        local = self.commit_in_worktree("address PR review")
        merged_head = self.commit_outside_worktree(local, "edited on GitHub")
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())

    async def test_cleanup_refuses_a_switched_worktree_with_commits_nothing_vouches_for(self):
        await self.create()
        self.git("-C", self.path, "checkout", "-q", "-b", "AF-14449-rc")
        merged_head = self.commit_in_worktree("address PR review")
        self.commit_in_worktree("unmerged follow-up")
        with self.assertRaises(GitError) as raised:
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                merged_head_sha=merged_head)
        self.assertIn("branch AF-14449-rc", str(raised.exception))
        self.assertIn("no longer belongs", str(raised.exception))
        self.assertNotIsInstance(raised.exception, BranchUnverified)  # force must not be offered
        self.assertTrue(self.path.exists())
        with self.assertRaisesRegex(GitError, "no longer belongs"):
            await self.cleanup_forced()

    async def test_cleanup_accepts_a_detached_worktree_only_when_its_commits_are_vouched_for(self):
        await self.create()
        self.git("-C", self.path, "checkout", "-q", "--detach")
        merged_head = self.commit_in_worktree("address PR review")
        self.commit_in_worktree("unmerged follow-up")
        with self.assertRaisesRegex(GitError, "detached HEAD"):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                merged_head_sha=merged_head)
        self.assertTrue(self.path.exists())
        self.git("-C", self.path, "checkout", "-q", "--detach", merged_head)
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())

    async def test_cleanup_leftover_directory_of_a_deleted_branch_can_be_forced(self):
        """The branch was deleted after the merge, leaving an unregistered directory: nothing to
        check it against, so it is the human's call (force) rather than a dead end."""
        await self.create()
        shutil.rmtree(self.repo / ".git" / "worktrees" / self.path.name)
        self.git("branch", "-D", "AF-14449")
        with self.assertRaisesRegex(BranchUnverified, "no longer exists"):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                worktree_dir=self.folder)
        self.assertTrue(self.path.exists())
        await self.cleanup_forced(worktree_dir=self.folder)
        self.assertFalse(self.path.exists())

    async def test_a_leftover_directory_that_cannot_be_deleted_reports_the_os_error(self):
        """The OS error says why the delete failed, where the message used to say only that it
        could not be removed."""
        await self.create()
        shutil.rmtree(self.repo / ".git" / "worktrees" / self.path.name)
        with patch("agentflow.services.git_workspace.shutil.rmtree",
                                 side_effect=PermissionError(13, "Access is denied")), \
                patch("agentflow.services.git_workspace.asyncio.sleep", new=AsyncMock()):
            with self.assertRaises(WorktreeBusy) as raised:
                await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                    worktree_dir=self.folder)
        self.assertIn("Access is denied", str(raised.exception))
        self.assertTrue(self.path.exists())

    async def test_cleanup_accepts_commits_after_review_that_match_the_merged_pr(self):
        """PR feedback commits land after final review; GitHub's merged head vouches for them."""
        await self.create()
        merged_head = self.commit_in_worktree("address PR review")
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), merged_head)

    def commit_outside_worktree(self, parent, message):
        """A commit the PR got without it passing through the worktree (a GitHub web edit)."""
        tree = self.git("rev-parse", f"{parent}^{{tree}}").stdout.strip()
        return self.git("commit-tree", tree, "-p", parent, "-m", message).stdout.strip()

    async def test_cleanup_accepts_a_branch_behind_the_merged_pr(self):
        """Every local commit is in the merged PR, which also has one made elsewhere."""
        await self.create()
        local = self.commit_in_worktree("address PR review")
        merged_head = self.commit_outside_worktree(local, "edited on GitHub")
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())
        self.assertEqual(self.git("rev-parse", "AF-14449").stdout.strip(), local)

    async def test_cleanup_rejects_a_branch_that_diverged_from_the_merged_pr(self):
        await self.create()
        merged_head = self.commit_outside_worktree(self.sha, "edited on GitHub")
        self.commit_in_worktree("unmerged local work")
        with self.assertRaisesRegex(GitError, "changed"):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                merged_head_sha=merged_head)
        self.assertTrue(self.path.exists())

    async def test_cleanup_rejects_a_merged_head_missing_locally(self):
        await self.create()
        self.commit_in_worktree("address PR review")
        with self.assertRaisesRegex(GitError, "changed"):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                merged_head_sha="f" * 40)
        self.assertTrue(self.path.exists())

    async def test_cleanup_leftover_directory_accepts_a_branch_behind_the_merged_pr(self):
        await self.create()
        local = self.commit_in_worktree("address PR review")
        merged_head = self.commit_outside_worktree(local, "edited on GitHub")
        shutil.rmtree(self.repo / ".git" / "worktrees" / self.path.name)
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            worktree_dir=self.folder, merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())

    async def test_cleanup_rejects_commits_beyond_the_merged_pr(self):
        await self.create()
        merged_head = self.commit_in_worktree("address PR review")
        self.commit_in_worktree("unmerged follow-up")
        with self.assertRaisesRegex(GitError, "changed"):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                merged_head_sha=merged_head)
        self.assertTrue(self.path.exists())

    async def test_cleanup_leftover_directory_accepts_the_merged_pr_head(self):
        await self.create()
        merged_head = self.commit_in_worktree("address PR review")
        shutil.rmtree(self.repo / ".git" / "worktrees" / self.path.name)
        with self.assertRaises(BranchUnverified):
            await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                                worktree_dir=self.folder)
        await self.service.cleanup_worktree(str(self.repo), str(self.path), "AF-14449", self.sha,
                                            worktree_dir=self.folder, merged_head_sha=merged_head)
        self.assertFalse(self.path.exists())

    async def test_cleanup_rejects_repository_root_and_unregistered_directory(self):
        with self.assertRaisesRegex(GitError, "repository root"):
            await self.service.cleanup_worktree(str(self.repo), str(self.repo), "main", self.sha)
        self.path.mkdir()
        with self.assertRaisesRegex(GitError, "not a registered"):
            await self.cleanup()
        self.assertTrue(self.path.exists())

    async def test_cleanup_recovers_leftover_directory_after_partial_removal(self):
        """D3: `git worktree remove` deregisters the worktree before it attempts to
        delete the directory, so a delete that fails afterwards (e.g. a Windows process
        still holding it as its cwd) leaves the directory on disk but unregistered -
        which used to dead-end every retry with 'not a registered task worktree'.
        Simulate that partial failure directly (removing the admin dir git itself would
        have already dropped) and confirm a retry finishes the job."""
        await self.create()
        admin_dir = self.repo / ".git" / "worktrees" / self.path.name
        self.assertTrue(admin_dir.is_dir())
        shutil.rmtree(admin_dir)
        self.assertTrue(self.path.is_dir())
        entries = await self.service.list_worktrees(str(self.repo))
        self.assertFalse(any(Path(wt["worktree"]) == self.path for wt in entries))
        discarded = await self.service.cleanup_worktree(
            str(self.repo), str(self.path), "AF-14449", self.sha, worktree_dir=self.folder)
        self.assertEqual(discarded, [])
        self.assertFalse(self.path.exists())

    async def test_cleanup_leftover_directory_refuses_if_checkpoint_changed(self):
        await self.create()
        admin_dir = self.repo / ".git" / "worktrees" / self.path.name
        shutil.rmtree(admin_dir)
        with self.assertRaises(BranchUnverified):
            await self.service.cleanup_worktree(
                str(self.repo), str(self.path), "AF-14449", "b" * 40, worktree_dir=self.folder)
        self.assertTrue(self.path.exists())

    async def test_cleanup_leftover_directory_refuses_outside_managed_worktree_dir(self):
        await self.create()
        admin_dir = self.repo / ".git" / "worktrees" / self.path.name
        shutil.rmtree(admin_dir)
        other_dir = self.folder / "not-the-worktree-dir"
        other_dir.mkdir()
        with self.assertRaisesRegex(GitError, "not a registered"):
            await self.service.cleanup_worktree(
                str(self.repo), str(self.path), "AF-14449", self.sha, worktree_dir=other_dir)
        self.assertTrue(self.path.exists())

    async def test_listing_failure_is_not_treated_as_empty(self):
        self.service._git = AsyncMock(return_value=SimpleNamespace(ok=False, stderr="denied"))
        with self.assertRaisesRegex(GitError, "Cannot inspect"):
            await self.service.list_worktrees(str(self.repo))

    async def test_push_branch_publishes_the_task_branch_and_tracks_it(self):
        remote = self.add_origin()
        await self.create()
        self.git("-C", self.path, "commit", "--allow-empty", "-qm", "reviewed change")
        tip = self.head_of(self.path)

        await self.service.push_branch(str(self.path), "AF-14449")

        on_origin = subprocess.run(["git", "--git-dir", str(remote), "rev-parse", "AF-14449"],
                                   capture_output=True, text=True)
        self.assertEqual(on_origin.stdout.strip(), tip)
        self.assertEqual(self.git("rev-parse", "--abbrev-ref", "AF-14449@{upstream}").stdout.strip(),
                         "origin/AF-14449")
        await self.service.push_branch(str(self.path), "AF-14449")  # nothing new: still fine

    async def test_failed_push_names_the_branch_but_not_the_remote(self):
        self.add_origin()
        await self.create()
        self.git("remote", "set-url", "origin", "https://user:hunter2@127.0.0.1:9/acme/repo.git")

        with self.assertRaises(GitError) as raised:
            await self.service.push_branch(str(self.path), "AF-14449")

        self.assertIn("Pushing AF-14449 to origin failed", str(raised.exception))
        self.assertNotIn("hunter2", str(raised.exception))
        self.assertNotIn("127.0.0.1", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
