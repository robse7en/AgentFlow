"""GitHub PR sync: move tasks to PR Created / Merged when GitHub shows their pull request.

Merged tasks that still have a worktree also get the merged PR's head commit recorded, which
lets cleanup accept commits added after final review (see GitService.cleanup_worktree).

AgentFlow binds to loopback, so GitHub webhooks cannot reach it; this polls through the `gh`
CLI instead (config `github`). Settings are re-read every cycle, so a config reload can turn
it on or off or change the interval without a restart. The manual PR buttons keep working,
and undoing a PR status pauses sync for that task (see WorkflowEngine._pause_pr_sync).
"""

from __future__ import annotations

import asyncio
import logging
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

from sqlalchemy import select

from ..config import GitHubConfig
from ..database.models import Task
from ..services.git_workspace import GitError
from ..services.github import GitHubCli, GitHubError, GitHubUnavailable, PullRequest, is_pr_url, repo_from_remote
from ..services.process import ProcessError
from .engine import NotFound, WorkflowEngine
from .states import TaskStatus

log = logging.getLogger(__name__)

_WATCHED = (TaskStatus.READY_FOR_PR, TaskStatus.PR_CREATED)


def _utc(value: datetime) -> datetime:
    return value if value.tzinfo else value.replace(tzinfo=timezone.utc)


def pick_pull_request(prs: list[PullRequest], task_created_at: datetime) -> PullRequest | None:
    """The PR a task's branch stands for: the newest open one, else the newest merged one.

    PRs opened before the task existed belong to an earlier use of the same branch name
    (e.g. the ticket was worked on before), so they are never taken as this task's PR."""
    since = _utc(task_created_at)
    current = [pr for pr in prs if _utc(pr.created_at) >= since]
    for state in ("OPEN", "MERGED"):
        matching = [pr for pr in current if pr.state == state]
        if matching:
            return max(matching, key=lambda pr: pr.created_at)
    return None


class PrSync:
    def __init__(self, engine: WorkflowEngine,
                 github: Callable[[GitHubConfig], GitHubCli] = lambda settings: GitHubCli(settings.command)) -> None:
        self.engine = engine
        self.github = github
        self._worker: asyncio.Task | None = None
        # None -> gh-wide problem; jira key -> that task's problem. Logged only when it changes.
        self._problems: dict[str | None, str] = {}

    def start(self) -> None:
        self._worker = asyncio.create_task(self._run(), name="github-pr-sync")

    async def stop(self) -> None:
        if self._worker:
            self._worker.cancel()
            try:
                await self._worker
            except (asyncio.CancelledError, Exception):  # noqa: BLE001 - shutting down
                pass

    async def _run(self) -> None:
        while True:
            settings = self.engine.config.github
            if settings.pr_sync:
                try:
                    await self.poll_once()
                except Exception:  # noqa: BLE001 - never let one bad cycle end the loop
                    log.exception("GitHub PR sync cycle failed")
            await asyncio.sleep(settings.poll_interval_seconds)

    async def poll_once(self) -> None:
        settings = self.engine.config.github
        cli = self.github(settings)
        with self.engine.db.session() as session:
            rows = session.execute(
                select(Task.jira_key, Task.status, Task.pr_url, Task.repository_root, Task.branch_name,
                       Task.created_at, Task.worktree_path)
                .where(Task.pr_sync_paused.is_(False),
                       Task.status.in_(_WATCHED)
                       | ((Task.status == TaskStatus.MERGED) & Task.merged_head_sha.is_(None) & Task.pr_url.is_not(None)))
            ).all()
        # A merged task's head commit only matters to a cleanup that hasn't happened yet.
        tasks = [row for row in rows if row.status != TaskStatus.MERGED or Path(row.worktree_path).exists()]
        for task in tasks:
            try:
                pr = await self._lookup(cli, settings, task)
            except GitHubUnavailable as exc:
                self._report(None, str(exc))
                return  # every other lookup would fail the same way
            except (GitHubError, GitError, ProcessError) as exc:
                self._report(task.jira_key, str(exc))
                continue
            self._report(task.jira_key, None)
            if pr is not None:
                try:
                    await self.engine.apply_github_pr(task.jira_key, pr)
                except NotFound:
                    pass  # deleted while gh was running

    async def _lookup(self, cli: GitHubCli, settings: GitHubConfig, task) -> PullRequest | None:
        if task.status in (TaskStatus.PR_CREATED, TaskStatus.MERGED) and task.pr_url:
            # Follow the PR already on record, even if the human typed it in; a URL on
            # another host (or not a PR link) is simply not checked.
            if not is_pr_url(task.pr_url, settings.hosts):
                return None
            pr = await cli.pull_request(task.pr_url)
        else:
            remote = await self.engine.git.remote_url(task.repository_root)
            repo = repo_from_remote(remote, settings.hosts) if remote else None
            if repo is None:
                return None  # origin is missing or not on a configured GitHub host
            pr = pick_pull_request(await cli.pull_requests_for_branch(repo, task.branch_name), task.created_at)
        self._report(None, None)  # only a real gh answer shows gh itself is working again
        return pr

    def _report(self, key: str | None, problem: str | None) -> None:
        if self._problems.get(key) == problem:
            return
        if problem is None:
            del self._problems[key]
            log.info("GitHub PR sync%s is working again", f" for {key}" if key else "")
            return
        self._problems[key] = problem
        log.warning("GitHub PR sync%s: %s", f" for {key}" if key else "", problem)
