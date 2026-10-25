"""Pull-request lookups through the GitHub CLI (`gh`), for PR sync (workflow/pr_sync.py).

Uses whichever account `gh auth login` set up. The one write is `retarget`, changing a PR's base
branch, and only when the human asks for it.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from datetime import datetime
from typing import Any
from urllib.parse import quote

from .process import ProcessError, resolve_executable, run

_FIELDS = "url,state,createdAt,mergedAt,isCrossRepository,headRefOid,baseRefName"
# clean_env drops these, but they are how gh is told which account or host to use.
_PASSTHROUGH_ENV = ("GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN",
                    "GH_HOST", "GH_CONFIG_DIR")
_AUTH_REQUIRED_EXIT = 4  # gh's documented exit code for "authentication required"

# https://host/owner/repo(.git), ssh://git@host[:port]/owner/repo(.git), git@host:owner/repo(.git)
_URL_REMOTE = re.compile(r"^(?:https?|ssh|git)://(?:[^@/]+@)?(?P<host>[^/:]+)(?::\d+)?/(?P<path>[^?#]+?)(?:\.git)?/?$")
_SCP_REMOTE = re.compile(r"^(?:[^@/]+@)?(?P<host>[^/:]+):(?P<path>[^/][^?#]*?)(?:\.git)?/?$")
_PR_URL = re.compile(r"^https://(?P<host>[^/]+)/[^/]+/[^/]+/pull/\d+/?$")


class GitHubError(RuntimeError):
    """A lookup for one task failed."""


class GitHubUnavailable(GitHubError):
    """gh is missing or not logged in, so no lookup can succeed until that is fixed."""


@dataclass(frozen=True)
class PullRequest:
    url: str
    state: str  # OPEN | CLOSED | MERGED
    created_at: datetime
    merged_at: datetime | None
    head_sha: str | None = None  # the PR's head commit; for a merged PR, what GitHub merged
    base_branch: str | None = None  # the branch the PR merges into


def repo_from_remote(url: str, hosts: list[str]) -> str | None:
    """`host/owner/repo` for gh's --repo, or None when the remote is not on a configured host."""
    match = _URL_REMOTE.match(url) or _SCP_REMOTE.match(url)
    if not match or match["host"].lower() not in {h.lower() for h in hosts}:
        return None
    parts = match["path"].strip("/").split("/")
    if len(parts) != 2 or not all(parts):
        return None
    return f"{match['host']}/{parts[0]}/{parts[1]}"


def compare_url(repo: str, base: str, branch: str) -> str:
    """GitHub's new-PR form for `repo` (`host/owner/repo`, from repo_from_remote) with the base
    branch already chosen, so the PR cannot default to the repository's default branch."""
    return f"https://{repo}/compare/{quote(base, safe='/~')}...{quote(branch, safe='/~')}?expand=1"


def is_pr_url(url: str, hosts: list[str]) -> bool:
    match = _PR_URL.match(url)
    return bool(match) and match["host"].lower() in {h.lower() for h in hosts}


def _timestamp(value: str | None) -> datetime | None:
    return datetime.fromisoformat(value.replace("Z", "+00:00")) if value else None


def _pull_request(item: dict[str, Any]) -> PullRequest:
    return PullRequest(url=item["url"], state=item["state"], created_at=_timestamp(item["createdAt"]),
                       merged_at=_timestamp(item.get("mergedAt")), head_sha=item.get("headRefOid") or None,
                       base_branch=item.get("baseRefName") or None)


class GitHubCli:
    def __init__(self, command: str = "gh", timeout: float = 30.0) -> None:
        self.command = command
        self.timeout = timeout

    async def _run(self, *args: str):
        exe = resolve_executable(self.command)
        if not exe:
            raise GitHubUnavailable(f"GitHub CLI '{self.command}' was not found on PATH")
        env = {name: os.environ[name] for name in _PASSTHROUGH_ENV if os.environ.get(name)}
        env.update(GH_PROMPT_DISABLED="1", GH_NO_UPDATE_NOTIFIER="1", NO_COLOR="1")
        try:
            return await run([exe, *args], env=env, timeout=self.timeout)
        except ProcessError as exc:
            raise GitHubError(str(exc)) from exc

    async def logged_in(self, host: str) -> bool:
        """Whether gh has a working login for `host`, as PR sync would see it."""
        return (await self._run("auth", "status", "--hostname", host)).ok

    async def _gh_text(self, *args: str) -> str:
        """gh's stdout, for commands that print text rather than JSON."""
        result = await self._run(*args)
        if result.returncode == _AUTH_REQUIRED_EXIT:
            raise GitHubUnavailable("GitHub CLI is not logged in; run `gh auth login`")
        if not result.ok:
            detail = (result.stderr or result.stdout).strip().splitlines()
            raise GitHubError(f"gh {args[0]} {args[1]} failed: {detail[0] if detail else f'exit {result.returncode}'}")
        return result.stdout

    async def _gh(self, *args: str) -> Any:
        stdout = await self._gh_text(*args)
        try:
            return json.loads(stdout)
        except json.JSONDecodeError as exc:
            raise GitHubError(f"gh {args[0]} {args[1]} returned unreadable output") from exc

    async def pull_requests_for_branch(self, repo: str, branch: str) -> list[PullRequest]:
        """PRs (any state) whose head is `branch` in `repo` itself; same-named fork branches are skipped."""
        items = await self._gh("pr", "list", "--repo", repo, "--head", branch, "--state", "all",
                               "--limit", "20", "--json", _FIELDS)
        return [_pull_request(item) for item in items if not item.get("isCrossRepository")]

    async def pull_request(self, url: str) -> PullRequest:
        return _pull_request(await self._gh("pr", "view", url, "--json", _FIELDS))

    async def retarget(self, url: str, base: str) -> None:
        """Change the branch the PR at `url` merges into."""
        await self._gh_text("pr", "edit", url, "--base", base)
