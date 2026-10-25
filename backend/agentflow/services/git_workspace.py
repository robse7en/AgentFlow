"""Infrastructure-level Git operations owned by AgentFlow.

Agents use their own Git MCP for development work; this module only creates worktrees,
branches and checkpoints.
"""

from __future__ import annotations

import asyncio
import shutil
from dataclasses import dataclass
from pathlib import Path
from ..diagnostics import event

from .process import ProcessError, resolve_executable, run


@dataclass
class RepositoryInfo:
    path: str
    root: str
    current_branch: str | None
    branches: list[str]


@dataclass
class CreatedWorktree:
    """A new task worktree: the branch actually used (suffixed -N on collision) and, when origin
    couldn't be fetched, a warning that the base may be out of date."""
    branch: str
    base_warning: str | None = None


class GitError(RuntimeError):
    pass


class WorktreeBusy(GitError):
    """The worktree directory could not be deleted, typically because a process still has it
    (or something inside it) open. On Windows that includes being a working directory."""


class BranchUnverified(GitError):
    """Cleanup refused: the task branch has commits that are neither the reviewed checkpoint
    nor known to be in a merged pull request (e.g. it was merged into a local branch)."""

    def __init__(self, message: str | None = None) -> None:
        super().__init__(message or "Task branch changed since final review and doesn't match a merged pull "
                                    "request; worktree cleanup stopped")


class GitService:
    def __init__(self, command: str = "git") -> None:
        self.command = command

    def executable(self) -> str | None:
        return resolve_executable(self.command)

    async def _git(self, *args: str, cwd: str | Path | None = None, timeout: float = 120.0):
        exe = self.executable()
        if not exe:
            raise GitError("Git executable not found on PATH")
        # core.longpaths is a no-op outside Windows; on Windows it's required for git to
        # touch (and, critically, delete) files under a checkout deeper than MAX_PATH (260
        # chars) - easy to hit once ignored build output (bin/, obj/, node_modules/) is in
        # scope, as cleanup_worktree's forced discard now puts it.
        result = await run([exe, "-c", "core.longpaths=true", *args], cwd=cwd, timeout=timeout)
        if not result.ok:
            # Arguments and command output may contain commit text or credentials.
            event('git.command.nonzero', operation=args[0] if args else None,
                  exitCode=result.returncode)
        return result

    async def version(self) -> str | None:
        try:
            return (await self._git("--version")).stdout.strip()
        except (GitError, ProcessError):
            return None

    async def remote_url(self, root: str | Path, name: str = "origin") -> str | None:
        result = await self._git("remote", "get-url", name, cwd=root, timeout=30.0)
        if not result.ok:
            return None
        return result.stdout.strip() or None

    async def inspect_repository(self, path: str) -> RepositoryInfo:
        directory = Path(path).expanduser()
        if not directory.is_dir():
            raise GitError(f"Directory does not exist: {directory}")
        result = await self._git("rev-parse", "--show-toplevel", cwd=directory)
        if not result.ok:
            raise GitError(f"Not a Git repository: {directory}")
        root = str(Path(result.stdout.strip()).resolve())
        head = await self._git("rev-parse", "--abbrev-ref", "HEAD", cwd=root)
        branches_result = await self._git("branch", "--format=%(refname:short)", "--list", cwd=root)
        remote_result = await self._git("branch", "-r", "--format=%(refname:short)", "--list", cwd=root)
        branches = [b.strip() for b in branches_result.stdout.splitlines() if b.strip()]
        for remote in remote_result.stdout.splitlines():
            remote = remote.strip()
            if "/" in remote and not remote.endswith("/HEAD"):
                short = remote.split("/", 1)[1]
                if short not in branches:
                    branches.append(short)
        return RepositoryInfo(
            path=str(directory.resolve()),
            root=root,
            current_branch=head.stdout.strip() if head.ok else None,
            branches=sorted(set(branches)),
        )

    async def directories(self, root: str) -> list[str]:
        """Every directory committed at HEAD, relative to `root` with "/" separators."""
        result = await self._git("ls-tree", "-r", "-d", "-z", "--name-only", "HEAD", cwd=root, timeout=30.0)
        if not result.ok:
            raise GitError(f"Cannot list the directories of {root}")
        return [name for name in result.stdout.split("\0") if name]

    async def branch_exists(self, root: str, branch: str) -> bool:
        local = await self._git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=root)
        if local.returncode not in (0, 1):
            raise GitError(f"Cannot inspect branch '{branch}': {local.stderr.strip()}")
        return local.ok

    async def _remote_branch_exists(self, root: str, branch: str) -> bool:
        return (await self._git("rev-parse", "--verify", "--quiet", f"refs/remotes/origin/{branch}", cwd=root)).ok

    async def fetch_origin(self, root: str, branch: str) -> str | None:
        """Best-effort update of refs/remotes/origin/<branch>; a failed (or timed-out) fetch never
        fails resolution. It only matters when origin is known to have the branch: then the
        returned warning says the base may be out of date. (No origin remote, or a branch origin
        doesn't have, leaves nothing to be stale against.)"""
        # An explicit refspec updates the tracking ref even in a single-branch or narrowed clone,
        # where `fetch origin <branch>` would only write FETCH_HEAD.
        refspec = f"+refs/heads/{branch}:refs/remotes/origin/{branch}"
        try:
            result = await self._git("fetch", "origin", refspec, cwd=root)
            if result.ok:
                return None
            exit_code = result.returncode
        except (GitError, ProcessError):
            exit_code = None
        event('git.fetch.failed', repository=root, branch=branch, exitCode=exit_code)
        if not await self._remote_branch_exists(root, branch):
            return None
        return (f"Couldn't fetch {branch} from origin, so this task branched from the last fetched copy of "
                f"{branch}, which may be missing newer commits.")

    async def _resolve_base(self, root: str, base_branch: str) -> tuple[str, str | None]:
        """The ref to branch from, after fetching origin: origin/<branch>, unless the local branch
        already contains it (so unpushed local commits are kept, but a local copy that is behind
        origin is never used); whichever exists when only one does. Also returns fetch_origin's
        warning."""
        warning = await self.fetch_origin(root, base_branch)
        local = await self.branch_exists(root, base_branch)
        remote = await self._remote_branch_exists(root, base_branch)
        if local and remote:
            contains = await self._git("merge-base", "--is-ancestor", f"refs/remotes/origin/{base_branch}",
                                       f"refs/heads/{base_branch}", cwd=root)
            if contains.returncode not in (0, 1):
                raise GitError(f"Cannot compare {base_branch} with origin/{base_branch}")
            reference = base_branch if contains.ok else f"origin/{base_branch}"
        elif local:
            reference = base_branch
        elif remote:
            reference = f"origin/{base_branch}"
        else:
            raise GitError(f"Base branch '{base_branch}' not found locally or on origin")
        event('worktree.base.resolved', baseBranch=base_branch, ref=reference)
        return reference, warning

    async def resolve_base_ref(self, root: str, base_branch: str) -> str:
        """A ref for the base branch: see _resolve_base."""
        return (await self._resolve_base(root, base_branch))[0]

    async def list_worktrees(self, root: str) -> list[dict[str, str]]:
        result = await self._git("worktree", "list", "--porcelain", "-z", cwd=root)
        if not result.ok:
            raise GitError(f"Cannot inspect registered worktrees: {result.stderr.strip()}")
        worktrees: list[dict[str, str]] = []
        current: dict[str, str] = {}
        for line in result.stdout.split("\0"):
            if not line:
                if current:
                    worktrees.append(current)
                    current = {}
                continue
            key, _, value = line.partition(" ")
            current[key] = value
        if current:
            worktrees.append(current)
        return worktrees

    async def create_worktree(self, root: str, worktree_path: Path, branch: str, base_branch: str) -> CreatedWorktree:
        """Create a worktree on a new branch from `base_branch` (see _resolve_base), recovering
        only a missing, unlocked registration at this path."""
        event('worktree.create.requested', repository=root, path=str(worktree_path),
              branch=branch, baseBranch=base_branch)
        if worktree_path.exists() or worktree_path.is_symlink() or worktree_path.resolve() != worktree_path.absolute():
            raise GitError(
                f"Worktree directory already exists: {worktree_path}. "
                "Remove it (git worktree remove) or choose a different Jira key."
            )
        worktrees = await self.list_worktrees(root)
        stale = next((wt for wt in worktrees if self._same_path(wt["worktree"], worktree_path)), None)
        if stale:
            event('worktree.stale.detected', path=str(worktree_path), requestedBranch=branch,
                  registeredBranch=stale.get('branch'), head=stale.get('HEAD'),
                  locked='locked' in stale, prunable='prunable' in stale)
        if stale and ("locked" in stale or "prunable" not in stale):
            raise GitError("The registered worktree is locked or not safely prunable")
        base_ref, base_warning = await self._resolve_base(root, base_branch)
        if stale:
            stale_branch = stale.get("branch")
            if stale_branch:
                # A branch-backed worktree keeps its work reachable through that branch ref.
                retained_head = await self._git("rev-parse", "--verify", stale_branch, cwd=root)
                if not retained_head.ok or retained_head.stdout.strip() != stale.get("HEAD"):
                    event('worktree.recovery.blocked', reason='stale_branch_commit_not_retained',
                          path=str(worktree_path), registeredBranch=stale_branch,
                          requestedBranch=branch)
                    raise GitError(
                        "The stale worktree commit is not retained by its registered branch; "
                        "inspect it before recovery"
                    )
            else:
                base_head = await self._git("rev-parse", "--verify", base_ref, cwd=root)
                if not base_head.ok or stale.get("HEAD") != base_head.stdout.strip():
                    raise GitError("Missing worktree has a different commit from the selected base; inspect it before recovery")
        if stale:
            # Recheck immediately before removing metadata; never force-remove a live directory.
            if worktree_path.exists() or worktree_path.is_symlink():
                raise GitError("Worktree path appeared during recovery; retry after inspecting it")
            if stale.get("branch") not in (None, f"refs/heads/{branch}"):
                event('worktree.recovery.different_branch', path=str(worktree_path),
                      requestedBranch=branch, registeredBranch=stale.get('branch'),
                      oldBranchPreserved=True)
            await self.remove_worktree(root, str(worktree_path))
            event('worktree.stale.registration_removed', path=str(worktree_path),
                  branchPreserved=bool(stale.get('branch')))
        requested_branch = branch
        suffix = 0
        while await self.branch_exists(root, branch):
            suffix += 1
            branch = f"{requested_branch}-{suffix}"
            if suffix > 10000:
                raise GitError(f"Could not find an available branch name based on '{requested_branch}'")
        if branch != requested_branch:
            event('worktree.branch.collision', requestedBranch=requested_branch, selectedBranch=branch)
        worktree_path.parent.mkdir(parents=True, exist_ok=True)
        # --no-track: a branch made from origin/<base> must not take it as its upstream, or a pull
        # in the worktree would merge the base and a push could target it.
        args = ["--no-track", "-b", branch, str(worktree_path), base_ref]
        # _git sets core.longpaths for the checkout; some repos (e.g. generated WCF service
        # references) have paths long enough to trip Windows' 260-char MAX_PATH without it.
        result = await self._git("worktree", "add", *args, cwd=root, timeout=600)
        if not result.ok:
            event('worktree.create.failed', path=str(worktree_path), exitCode=result.returncode)
            raise GitError(f"git worktree add failed: {(result.stderr or result.stdout).strip()}")
        event('worktree.create.completed', path=str(worktree_path), branch=branch)
        return CreatedWorktree(branch, base_warning)

    @staticmethod
    def _same_path(left: str | Path, right: str | Path) -> bool:
        return Path(left).resolve() == Path(right).resolve()

    async def head_sha(self, cwd: str | Path) -> str:
        result = await self._git("rev-parse", "HEAD", cwd=cwd)
        if not result.ok:
            raise GitError(f"Cannot read HEAD in {cwd}: {result.stderr.strip()}")
        return result.stdout.strip()

    async def verify_worker_worktree(self, root: str, path: Path, branch: str) -> None:
        if path.is_symlink() or path.resolve() != path.absolute():
            raise GitError("Worker worktree must not be a linked directory")
        entries = await self.list_worktrees(root)
        entry = next((wt for wt in entries if self._same_path(wt["worktree"], path)), None)
        if entry is None or entry.get("branch") != f"refs/heads/{branch}" or "locked" in entry:
            raise GitError("Worker worktree registration or branch changed; inspect before retrying")

    async def create_worker_worktree(self, root: str, path: Path, branch: str, sha: str) -> None:
        """Branch from a local checkpoint only; never fetch or reuse an existing directory."""
        if path.exists() or path.is_symlink() or path.resolve() != path.absolute():
            raise GitError("Worker worktree path already exists or is linked")
        result = await self._git("worktree", "add", "-b", branch, str(path), sha, cwd=root)
        if not result.ok:
            # `worktree add -b` creates the branch before attempting checkout, so a checkout
            # failure (e.g. Windows MAX_PATH) leaves it behind and blocks every retry with
            # "branch already exists". Best-effort: the worktree itself was never registered.
            await self._git("branch", "-D", branch, cwd=root)
            raise GitError(f"Cannot create worker worktree: {result.stderr.strip()}")

    async def merge_worker_checkpoint(self, path: Path, sha: str) -> None:
        result = await self._git("-c", "user.name=AgentFlow", "-c", "user.email=agentflow@local",
                                 "merge", "--no-edit", sha, cwd=path)
        if not result.ok:
            raise GitError("Dependency merge needs inspection in the worker worktree: " + result.stderr.strip())

    async def changed_paths(self, path: str, base: str, sha: str) -> list[str]:
        result = await self._git("diff", "--name-only", "--no-renames", "-z", base, sha, "--", cwd=path)
        if not result.ok:
            raise GitError("Cannot verify worker file ownership")
        return [name for name in result.stdout.split("\0") if name]

    async def is_ancestor(self, path: str, ancestor: str, sha: str) -> bool:
        result = await self._git("merge-base", "--is-ancestor", ancestor, sha, cwd=path)
        if result.returncode not in (0, 1):
            raise GitError("Cannot verify integrated worker checkpoint")
        return result.ok

    async def is_dirty(self, cwd: str | Path) -> bool:
        result = await self._git("status", "--porcelain", cwd=cwd)
        if not result.ok:
            raise GitError(f"Cannot inspect worktree: {result.stderr.strip()}")
        return bool(result.stdout.strip())

    async def dirty_paths(self, cwd: str | Path, *, ignored: bool = False) -> list[str]:
        """Porcelain status lines (e.g. ' M path', '?? path'), untracked files included."""
        args = ["status", "--porcelain", "--untracked-files=all"]
        if ignored:
            args.append("--ignored")
        result = await self._git(*args, cwd=cwd)
        if not result.ok:
            raise GitError(f"Cannot inspect worktree: {result.stderr.strip()}")
        return [line for line in result.stdout.splitlines() if line.strip()]

    async def reset_hard(self, cwd: str | Path, sha: str) -> None:
        result = await self._git("reset", "--hard", sha, cwd=cwd)
        if not result.ok:
            raise GitError(f"git reset --hard failed: {(result.stderr or result.stdout).strip()}")

    async def checkpoint(self, cwd: str | Path, message: str, commit: bool = True) -> tuple[str, bool]:
        """Record a stable checkpoint. Returns (sha, committed)."""
        committed = False
        if not commit and await self.is_dirty(cwd):
            raise GitError("Commit the worktree changes before requesting a review checkpoint")
        if commit and await self.is_dirty(cwd):
            (await self._git("add", "-A", cwd=cwd)).check("git add")
            result = await self._git("-c", "user.name=AgentFlow", "-c", "user.email=agentflow@local",
                                     "commit", "-q", "-m", message, cwd=cwd)
            if not result.ok:
                raise GitError(f"git commit failed: {(result.stderr or result.stdout).strip()}")
            committed = True
        return await self.head_sha(cwd), committed

    async def remove_worktree(self, root: str, worktree_path: str, *, force: bool = False) -> None:
        args = ["worktree", "remove"]
        if force:
            args.append("--force")
        args.append(worktree_path)
        result = await self._git(*args, cwd=root)
        if not result.ok:
            raise WorktreeBusy(f"git worktree remove failed: {(result.stderr or result.stdout).strip()}")

    async def cleanup_worktree(self, root: str, path: str, branch: str, expected_sha: str,
                               worktree_dir: str | Path | None = None,
                               merged_head_sha: str | None = None, *, force: bool = False) -> list[str]:
        """Remove the task worktree, discarding any modified/untracked/ignored files in it.

        Returns the porcelain status lines for modified/untracked files only - the ones
        worth a human's attention - so callers can tell them what was thrown away. Ignored
        files (build output, dependencies) are discarded too via `--force` below, but are
        deliberately not enumerated: a repo can have tens of thousands of them, and listing
        each one is both slow (a full tree walk) and pure noise for something the project
        itself already marked as regenerable.

        `worktree_dir`, when given, is the configured directory task worktrees live directly
        inside of; it gates the leftover-directory recovery below to that managed location.

        The branch tip must be the reviewed checkpoint `expected_sha`, or `merged_head_sha`
        (the head commit GitHub reports it merged) or one of its ancestors. The latter lets
        commits added after final review (PR feedback) be cleaned up once they are known to
        be in the merged PR, even when the PR also picked up commits made outside this
        worktree (a GitHub web edit, a push from another clone). `force` skips that check: the
        human vouches for the branch (merged somewhere AgentFlow can't see, like a local branch).
        The branch itself is kept either way.
        """
        event('worktree.cleanup.requested', path=path, branch=branch, expectedSha=expected_sha, force=force)
        target = Path(path)
        if self._same_path(root, target) or target.is_symlink() or target.resolve() != target.absolute():
            raise GitError("Refusing to remove the repository root or a linked directory")
        entries = await self.list_worktrees(root)
        entry = next((wt for wt in entries if self._same_path(wt["worktree"], target)), None)
        if entry is None:
            if not target.exists():
                return []  # A previous cleanup already completed.
            return await self._cleanup_leftover_directory(root, target, branch, expected_sha, worktree_dir,
                                                          merged_head_sha, force=force)
        if "locked" in entry:
            raise GitError("Worktree is locked; unlock it (`git worktree unlock`) and retry")
        if entry.get("branch") == f"refs/heads/{branch}":
            branch_head = await self._git("rev-parse", "--verify", f"refs/heads/{branch}", cwd=root)
            head = branch_head.stdout.strip() if branch_head.ok else None
            if not force and (entry.get("HEAD") != head
                              or not await self._is_cleanable_head(root, head, expected_sha, merged_head_sha)):
                raise BranchUnverified()
        elif not await self._is_cleanable_head(root, entry.get("HEAD"), expected_sha, merged_head_sha):
            # Switched or renamed since the task made it (the human pushing `<branch>-rc`, say): fine
            # once every commit it has is reviewed or merged. Never forced: a named branch isn't the
            # one the human vouched for, and a detached HEAD can hold commits no branch has.
            current = entry.get("branch")
            where = f"branch {current.removeprefix('refs/heads/')}" if current else "a detached HEAD"
            raise GitError(f"Worktree is on {where}, not the task branch, and its commits aren't the final "
                           "review or a merged pull request, so it no longer belongs to the task")
        discarded: list[str] = []
        if target.exists():
            discarded = await self.dirty_paths(target, ignored=False)
            if discarded:
                event('worktree.cleanup.discarding', path=path, count=len(discarded))
        await self.remove_worktree(root, path, force=True)
        if target.exists() or any(self._same_path(wt["worktree"], target) for wt in await self.list_worktrees(root)):
            raise WorktreeBusy("Worktree cleanup could not be verified")
        event('worktree.cleanup.completed', path=path, branch=branch, discarded=len(discarded))
        return discarded

    async def _is_cleanable_head(self, root: str, head: str | None, expected_sha: str,
                                 merged_head_sha: str | None) -> bool:
        """Whether every commit on the branch is known to be reviewed or merged."""
        if not head:
            return False
        if head in (expected_sha, merged_head_sha):
            return True
        if not merged_head_sha:
            return False
        # Exit 1 means not an ancestor; anything else (e.g. the merged commit was never
        # fetched) can't vouch for the branch either, so both refuse.
        result = await self._git("merge-base", "--is-ancestor", head, merged_head_sha, cwd=root)
        return result.returncode == 0

    async def _cleanup_leftover_directory(self, root: str, target: Path, branch: str, expected_sha: str,
                                          worktree_dir: str | Path | None,
                                          merged_head_sha: str | None = None, *, force: bool = False) -> list[str]:
        """Recover from a worktree removal that partially failed: `git worktree remove`
        deregisters the worktree before it attempts to delete the directory, so a delete
        that fails afterwards (e.g. a Windows process still holding the directory as its
        cwd) leaves the directory on disk but unregistered - which used to dead-end every
        retry with "not a registered task worktree" (the only way out was a manual delete).
        Only ever treats this as recoverable debris, never as license to delete arbitrary
        directories: it must sit directly inside the configured worktree directory, must not
        be a symlink (checked by the caller), and the task branch must still point at exactly
        the reviewed checkpoint, or at the merged PR's head commit or one of its ancestors
        (unless `force`: the human vouched for the branch).
        """
        refused = GitError("Directory is not a registered task worktree")
        if worktree_dir is None or target.resolve().parent != Path(worktree_dir).resolve():
            raise refused
        if not force:
            branch_head = await self._git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=root)
            if not branch_head.ok:
                # Deleted after the merge: nothing left to check the directory against, so the
                # human decides (force), the same as for a branch merged out of sight.
                raise BranchUnverified("The task branch no longer exists, so AgentFlow can't confirm the "
                                       "leftover directory holds nothing unreviewed; worktree cleanup stopped")
            if not await self._is_cleanable_head(root, branch_head.stdout.strip(), expected_sha, merged_head_sha):
                raise BranchUnverified()
        event('worktree.cleanup.leftover_detected', path=str(target), branch=branch)
        await self._remove_leftover_directory(root, target)
        event('worktree.cleanup.leftover_removed', path=str(target), branch=branch)
        return []

    async def _remove_leftover_directory(self, root: str, target: Path) -> None:
        """Delete an unregistered worktree directory the caller has already vetted, then prune."""
        # A just-closed Windows process can hold its old cwd's directory handle open for a
        # little while after the process itself is confirmed gone (observed >10s in one
        # case), so this backs off rather than giving up after a couple of quick retries.
        backoff = (0.5, 1.0, 1.5, 2.0, 2.5, 3.0)
        for attempt, delay in enumerate(backoff):
            try:
                shutil.rmtree(target)
                break
            except OSError as exc:
                if attempt == len(backoff) - 1:
                    # The OS error says why (32: in use by another process, 5: access denied).
                    raise WorktreeBusy(f"Could not remove leftover worktree directory: {target} ({exc})") from exc
                await asyncio.sleep(delay)
        prune = await self._git("worktree", "prune", cwd=root)
        if not prune.ok:
            raise GitError(f"git worktree prune failed: {(prune.stderr or prune.stdout).strip()}")
        if target.exists():
            raise WorktreeBusy(f"Could not verify removal of leftover worktree directory: {target}")

    async def remove_task_worktree(self, root: str, path: str, worktree_dir: str | Path) -> list[str]:
        """Force-remove a task's worktree at whatever commit it is on (task deletion).

        Unlike `cleanup_worktree` there is no reviewed checkpoint to insist on: a cancelled
        task never reached one, and the human has seen the preview of what is discarded. The
        containment rules stay: only a directory directly inside the managed worktree
        directory, never a symlink or the repository root. Returns the discarded
        modified/untracked porcelain lines."""
        target = Path(path)
        if (self._same_path(root, target) or target.is_symlink() or target.resolve() != target.absolute()
                or target.resolve().parent != Path(worktree_dir).resolve()):
            raise GitError("Refusing to remove a worktree outside the managed worktree directory")
        event('worktree.delete.requested', path=path)
        entries = await self.list_worktrees(root)
        entry = next((wt for wt in entries if self._same_path(wt["worktree"], target)), None)
        if entry is None:
            if target.exists():
                await self._remove_leftover_directory(root, target)
            return []
        if "locked" in entry:
            raise GitError("Worktree is locked")
        discarded = await self.dirty_paths(target, ignored=False) if target.exists() else []
        await self.remove_worktree(root, path, force=True)
        if target.exists() or any(self._same_path(wt["worktree"], target) for wt in await self.list_worktrees(root)):
            raise GitError(f"Could not verify removal of {path}")
        event('worktree.delete.completed', path=path, discarded=len(discarded))
        return discarded

    async def branch_sha(self, root: str, branch: str) -> str | None:
        result = await self._git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=root)
        if result.returncode not in (0, 1):
            raise GitError(f"Cannot inspect branch '{branch}': {result.stderr.strip()}")
        return result.stdout.strip() if result.ok else None

    async def push_branch(self, cwd: str, branch: str) -> None:
        """Push `branch` to origin under the same name and track it there. Run from the task
        worktree, so any pre-push hook sees that checkout; hooks are why the timeout is long."""
        result = await self._git("push", "--set-upstream", "origin", f"refs/heads/{branch}:refs/heads/{branch}",
                                 cwd=cwd, timeout=600.0)
        if not result.ok:
            # git's output can include the remote URL and its credentials; say only what failed.
            raise GitError(f"Pushing {branch} to origin failed (git exit {result.returncode}). "
                           "Push it from the worktree to see why.")

    async def _count(self, root: str, revisions: str) -> int | None:
        result = await self._git("rev-list", "--count", revisions, cwd=root)
        return int(result.stdout.strip()) if result.ok and result.stdout.strip().isdigit() else None

    async def branch_report(self, root: str, branch: str, base_branch: str) -> dict:
        """What deleting `branch` would lose: commits not on the base branch, and commits not
        on its remote (its upstream, else origin/<branch>). Read-only; never fetches."""
        sha = await self.branch_sha(root, branch)
        report: dict = {"name": branch, "exists": sha is not None, "sha": sha, "unmergedCommits": None,
                        "upstream": None, "unpushedCommits": None}
        if sha is None:
            return report
        for base in (f"refs/heads/{base_branch}", f"refs/remotes/origin/{base_branch}"):
            if (await self._git("rev-parse", "--verify", "--quiet", base, cwd=root)).ok:
                report["unmergedCommits"] = await self._count(root, f"{base}..{sha}")
                break
        upstream = await self._git("rev-parse", "--abbrev-ref", "--symbolic-full-name", f"{branch}@{{upstream}}", cwd=root)
        if upstream.ok and upstream.stdout.strip():
            report["upstream"] = upstream.stdout.strip()
        elif (await self._git("rev-parse", "--verify", "--quiet", f"refs/remotes/origin/{branch}", cwd=root)).ok:
            report["upstream"] = f"origin/{branch}"
        if report["upstream"]:
            report["unpushedCommits"] = await self._count(root, f"{report['upstream']}..{sha}")
        return report

    async def delete_branch(self, root: str, branch: str) -> bool:
        """Force-delete a local branch. False when it was already gone."""
        if await self.branch_sha(root, branch) is None:
            return False
        result = await self._git("branch", "-D", branch, cwd=root)
        if not result.ok:
            raise GitError(f"git branch -D {branch} failed: {(result.stderr or result.stdout).strip()}")
        event('branch.delete.completed', branch=branch)
        return True

    async def discard_worktree(self, root: str, path: str) -> list[str]:
        """Force-remove a disposable worktree (e.g. a worker subtask's), discarding any
        local changes. Unlike `cleanup_worktree`, there's no branch-tip/checkpoint to
        verify against: by the time a task's own cleanup runs, worker checkpoints are
        already folded into the task branch, so their scratch worktrees are safe to
        discard outright.
        """
        target = Path(path)
        if self._same_path(root, target) or target.is_symlink() or target.resolve() != target.absolute():
            raise GitError("Refusing to remove the repository root or a linked directory")
        entries = await self.list_worktrees(root)
        entry = next((wt for wt in entries if self._same_path(wt["worktree"], target)), None)
        if entry is None:
            return []  # Already gone.
        if "locked" in entry:
            raise GitError("Worktree is locked")
        discarded: list[str] = []
        if target.exists():
            discarded = await self.dirty_paths(target, ignored=False)
        await self.remove_worktree(root, path, force=True)
        if target.exists() or any(self._same_path(wt["worktree"], target) for wt in await self.list_worktrees(root)):
            raise GitError(f"Could not verify removal of {path}")
        return discarded
