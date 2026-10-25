import logging
from datetime import datetime, timedelta, timezone
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import select

from agentflow.database.models import WorkflowEvent
from agentflow.services import github as github_module
from agentflow.services.github import (
    GitHubCli, GitHubError, GitHubUnavailable, PullRequest, is_pr_url, repo_from_remote,
)
from agentflow.services.process import CommandResult
from agentflow.workflow.pr_sync import PrSync, pick_pull_request
from agentflow.workflow.states import TaskStatus
from test_workflow_safety import SHA, engine

URL = "https://github.com/acme/repo/pull/7"
LATER = datetime.now(timezone.utc) + timedelta(hours=1)


HEAD = "d" * 40


def pr(state, url=URL, created=LATER, merged=None, head=HEAD):
    return PullRequest(url=url, state=state, created_at=created,
                       merged_at=merged or (created + timedelta(hours=1) if state == "MERGED" else None),
                       head_sha=head)


def merged_head(engine):
    with engine.db.session() as session:
        return engine._load_task(session, "TEST-1").merged_head_sha


@pytest.fixture
def synced(engine):
    engine.config.github.pr_sync = True
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.status = TaskStatus.READY_FOR_PR
        task.current_sha = SHA
    return engine


def status(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        return task.status, task.pr_url, task.status_message, task.pr_sync_paused


def event_count(engine):
    with engine.db.session() as session:
        return len(session.scalars(select(WorkflowEvent)).all())


# remote / URL parsing -------------------------------------------------------------------
@pytest.mark.parametrize("remote, expected", [
    ("https://github.com/acme/repo.git", "github.com/acme/repo"),
    ("https://github.com/acme/repo", "github.com/acme/repo"),
    ("https://user@github.com/acme/repo.git/", "github.com/acme/repo"),
    ("git@github.com:acme/repo.git", "github.com/acme/repo"),
    ("ssh://git@github.com:22/acme/repo.git", "github.com/acme/repo"),
    ("https://gitlab.com/acme/repo.git", None),
    ("https://dev.azure.com/org/project/_git/repo", None),
    ("https://github.com/acme/group/repo.git", None),
    ("/local/path/repo", None),
])
def test_repo_from_remote(remote, expected):
    assert repo_from_remote(remote, ["github.com"]) == expected


def test_enterprise_host_is_only_used_when_configured():
    remote = "git@github.acme.io:team/repo.git"
    assert repo_from_remote(remote, ["github.com"]) is None
    assert repo_from_remote(remote, ["github.com", "github.acme.io"]) == "github.acme.io/team/repo"
    assert is_pr_url("https://github.acme.io/team/repo/pull/3", ["GitHub.acme.io"])


def test_is_pr_url():
    assert is_pr_url(URL, ["github.com"])
    assert not is_pr_url(URL + "/files", ["github.com"])
    assert not is_pr_url("https://bitbucket.org/acme/repo/pull-requests/7", ["github.com"])


# choosing the task's PR -----------------------------------------------------------------
def test_pick_prefers_newest_open_then_newest_merged():
    since = datetime.now(timezone.utc)
    old_merged = pr("MERGED", url="m1", created=since + timedelta(minutes=1))
    new_merged = pr("MERGED", url="m2", created=since + timedelta(minutes=2))
    opened = pr("OPEN", url="o1", created=since + timedelta(minutes=1))
    assert pick_pull_request([old_merged, new_merged, opened], since).url == "o1"
    assert pick_pull_request([old_merged, new_merged], since).url == "m2"
    assert pick_pull_request([pr("CLOSED", created=since + timedelta(minutes=1))], since) is None


def test_pick_ignores_prs_older_than_the_task():
    """A reused branch name must not mark a fresh task Merged from an earlier PR."""
    task_created = datetime.now(timezone.utc)
    earlier = pr("MERGED", created=task_created - timedelta(days=30))
    assert pick_pull_request([earlier], task_created) is None
    # SQLite hands back naive UTC timestamps.
    assert pick_pull_request([earlier], task_created.replace(tzinfo=None)) is None


# engine transitions ---------------------------------------------------------------------
@pytest.mark.asyncio
async def test_open_pr_marks_task_pr_created(synced):
    snap = await synced.apply_github_pr("TEST-1", pr("OPEN"))
    assert snap["status"] == "PR_CREATED"
    assert snap["prUrl"] == URL
    assert snap["prCreatedAt"] == LATER.isoformat()
    assert snap["prSyncPaused"] is False


@pytest.mark.asyncio
async def test_already_merged_pr_moves_straight_to_merged(synced):
    merged_at = LATER + timedelta(hours=2)
    snap = await synced.apply_github_pr("TEST-1", pr("MERGED", merged=merged_at))
    assert snap["status"] == "MERGED"
    assert snap["prUrl"] == URL
    assert snap["mergedAt"] == merged_at.isoformat()


@pytest.mark.asyncio
async def test_unchanged_pr_writes_and_publishes_nothing(synced):
    await synced.apply_github_pr("TEST-1", pr("OPEN"))
    events = event_count(synced)
    synced.broadcaster.broadcast = AsyncMock()
    assert await synced.apply_github_pr("TEST-1", pr("OPEN")) is None
    assert event_count(synced) == events
    synced.broadcaster.broadcast.assert_not_awaited()


@pytest.mark.asyncio
async def test_closed_pr_is_noted_once_and_cleared_on_reopen(synced):
    await synced.apply_github_pr("TEST-1", pr("OPEN"))
    snap = await synced.apply_github_pr("TEST-1", pr("CLOSED"))
    assert snap["status"] == "PR_CREATED"
    assert "closed on GitHub without merging" in snap["statusMessage"]
    assert await synced.apply_github_pr("TEST-1", pr("CLOSED")) is None

    snap = await synced.apply_github_pr("TEST-1", pr("OPEN"))
    assert snap["status"] == "PR_CREATED"
    assert "closed" not in snap["statusMessage"]


@pytest.mark.asyncio
async def test_closed_pr_never_marks_a_ready_task(synced):
    assert await synced.apply_github_pr("TEST-1", pr("CLOSED")) is None
    assert status(synced)[0] == TaskStatus.READY_FOR_PR


@pytest.mark.asyncio
async def test_github_state_only_applies_to_the_recorded_pr(synced):
    """The human marked a different PR while a branch lookup was in flight."""
    await synced.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/8")
    assert await synced.apply_github_pr("TEST-1", pr("MERGED")) is None
    assert status(synced)[0] == TaskStatus.PR_CREATED
    # Same PR, URL typed with different case and a trailing slash.
    snap = await synced.apply_github_pr("TEST-1", pr("MERGED", url="https://github.com/Acme/repo/pull/8/"))
    assert snap["status"] == "MERGED"


@pytest.mark.asyncio
async def test_undo_pauses_sync_until_the_human_marks_by_hand(synced):
    await synced.apply_github_pr("TEST-1", pr("MERGED"))
    snap = await synced.revert_pr_merged("TEST-1")
    assert snap["prSyncPaused"] is True
    assert "paused" in snap["statusMessage"]
    # GitHub still says merged, but the human's undo stands.
    assert await synced.apply_github_pr("TEST-1", pr("MERGED")) is None
    assert status(synced)[0] == TaskStatus.PR_CREATED

    snap = await synced.mark_pr_merged("TEST-1")
    assert snap["prSyncPaused"] is False


@pytest.mark.asyncio
async def test_manual_pr_created_resumes_sync(synced):
    await synced.apply_github_pr("TEST-1", pr("OPEN"))
    await synced.revert_pr_created("TEST-1")
    assert status(synced)[3] is True
    await synced.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/8")
    assert status(synced)[3] is False
    snap = await synced.apply_github_pr("TEST-1", pr("MERGED", url="https://github.com/acme/repo/pull/8"))
    assert snap["status"] == "MERGED"


@pytest.mark.asyncio
async def test_undo_does_not_pause_when_sync_is_off(synced):
    synced.config.github.pr_sync = False
    await synced.mark_pr_created("TEST-1", URL)
    snap = await synced.revert_pr_created("TEST-1")
    assert snap["prSyncPaused"] is False
    assert "paused" not in snap["statusMessage"]


# poller ---------------------------------------------------------------------------------
class FakeGitHub:
    def __init__(self, branch_prs=None, by_url=None, error=None):
        self.branch_prs = branch_prs or []
        self.by_url = by_url or {}
        self.error = error
        self.calls = []

    async def pull_requests_for_branch(self, repo, branch):
        self.calls.append(("list", repo, branch))
        if self.error:
            raise self.error
        return self.branch_prs

    async def pull_request(self, url):
        self.calls.append(("view", url))
        if self.error:
            raise self.error
        return self.by_url[url]


def poller(engine, fake, remote="git@github.com:acme/repo.git"):
    engine.git.remote_url = AsyncMock(return_value=remote)
    return PrSync(engine, github=lambda settings: fake)


@pytest.mark.asyncio
async def test_poll_finds_ready_task_pr_by_branch_on_origin(synced):
    fake = FakeGitHub(branch_prs=[pr("OPEN")])
    await poller(synced, fake).poll_once()
    assert fake.calls == [("list", "github.com/acme/repo", "agent/TEST-1")]
    assert status(synced)[:2] == (TaskStatus.PR_CREATED, URL)


@pytest.mark.asyncio
async def test_poll_follows_recorded_pr_url_for_pr_created_task(synced):
    await synced.mark_pr_created("TEST-1", URL)
    fake = FakeGitHub(by_url={URL: pr("MERGED")})
    await poller(synced, fake).poll_once()
    assert fake.calls == [("view", URL)]
    assert status(synced)[0] == TaskStatus.MERGED


@pytest.mark.asyncio
async def test_poll_skips_non_github_remotes_and_pr_urls(synced):
    fake = FakeGitHub()
    await poller(synced, fake, remote="https://gitlab.com/acme/repo.git").poll_once()
    await synced.mark_pr_created("TEST-1", "https://bitbucket.org/acme/repo/pull-requests/7")
    await poller(synced, fake).poll_once()
    assert fake.calls == []


@pytest.mark.asyncio
async def test_poll_skips_paused_tasks(synced):
    await synced.mark_pr_created("TEST-1", URL)
    await synced.revert_pr_created("TEST-1")
    fake = FakeGitHub(branch_prs=[pr("OPEN")])
    await poller(synced, fake).poll_once()
    assert fake.calls == []


@pytest.mark.asyncio
async def test_poll_logs_a_gh_problem_once_and_recovery(synced, caplog):
    fake = FakeGitHub(error=GitHubUnavailable("GitHub CLI is not logged in; run `gh auth login`"))
    sync = poller(synced, fake)
    with caplog.at_level(logging.INFO, logger="agentflow.workflow.pr_sync"):
        await sync.poll_once()
        await sync.poll_once()
        fake.error = None
        await sync.poll_once()
    messages = [r.getMessage() for r in caplog.records]
    assert sum("not logged in" in m for m in messages) == 1
    assert any("working again" in m for m in messages)
    assert status(synced)[0] == TaskStatus.READY_FOR_PR


@pytest.mark.asyncio
async def test_poll_does_not_claim_recovery_without_a_gh_answer(synced, caplog):
    """A task skipped before gh runs (non-GitHub remote) must not flip the gh-wide warning."""
    fake = FakeGitHub(error=GitHubUnavailable("GitHub CLI is not logged in; run `gh auth login`"))
    sync = poller(synced, fake)
    with caplog.at_level(logging.INFO, logger="agentflow.workflow.pr_sync"):
        await sync.poll_once()
        synced.git.remote_url = AsyncMock(return_value="https://gitlab.com/acme/repo.git")
        await sync.poll_once()
    assert not any("working again" in r.getMessage() for r in caplog.records)


@pytest.mark.asyncio
async def test_poll_survives_a_failing_lookup(synced):
    fake = FakeGitHub(error=GitHubError("gh pr list failed: HTTP 404"))
    await poller(synced, fake).poll_once()
    assert status(synced)[0] == TaskStatus.READY_FOR_PR


# gh CLI wrapper -------------------------------------------------------------------------
def fake_run(monkeypatch, returncode=0, stdout="[]", stderr=""):
    seen = {}

    async def run(argv, *, env=None, timeout=60.0, **_):
        seen.update(argv=argv, env=env, timeout=timeout)
        return CommandResult(argv, returncode, stdout, stderr)
    monkeypatch.setattr(github_module, "resolve_executable", lambda command: "/bin/gh")
    monkeypatch.setattr(github_module, "run", run)
    return seen


@pytest.mark.asyncio
async def test_cli_lists_same_repo_prs_only_and_disables_prompts(monkeypatch):
    monkeypatch.setenv("GH_TOKEN", "token-from-env")
    seen = fake_run(monkeypatch, stdout='''[
        {"url": "u1", "state": "OPEN", "createdAt": "2026-09-29T01:02:03Z", "mergedAt": null, "isCrossRepository": false},
        {"url": "u2", "state": "OPEN", "createdAt": "2026-09-29T01:02:03Z", "mergedAt": null, "isCrossRepository": true}
    ]''')
    prs = await GitHubCli().pull_requests_for_branch("github.com/acme/repo", "agent/TEST-1")
    assert [p.url for p in prs] == ["u1"]
    assert prs[0].created_at == datetime(2026, 9, 29, 1, 2, 3, tzinfo=timezone.utc)
    assert seen["argv"][:8] == ["/bin/gh", "pr", "list", "--repo", "github.com/acme/repo", "--head", "agent/TEST-1", "--state"]
    assert seen["env"]["GH_PROMPT_DISABLED"] == "1"
    assert seen["env"]["GH_TOKEN"] == "token-from-env"


@pytest.mark.asyncio
async def test_cli_reports_missing_or_logged_out_gh_as_unavailable(monkeypatch):
    fake_run(monkeypatch, returncode=4, stderr="To get started with GitHub CLI, please run: gh auth login")
    with pytest.raises(GitHubUnavailable, match="not logged in"):
        await GitHubCli().pull_request(URL)
    monkeypatch.setattr(github_module, "resolve_executable", lambda command: None)
    with pytest.raises(GitHubUnavailable, match="not found"):
        await GitHubCli().pull_request(URL)


@pytest.mark.asyncio
async def test_cli_reports_other_failures_per_lookup(monkeypatch):
    fake_run(monkeypatch, returncode=1, stderr="GraphQL: Could not resolve to a PullRequest\nmore detail")
    with pytest.raises(GitHubError, match="Could not resolve") as info:
        await GitHubCli().pull_request(URL)
    assert not isinstance(info.value, GitHubUnavailable)


@pytest.mark.asyncio
async def test_cli_reads_the_pr_head_commit(monkeypatch):
    seen = fake_run(monkeypatch, stdout='{"url": "u1", "state": "MERGED", "createdAt": "2026-09-29T01:02:03Z", '
                                        '"mergedAt": "2026-09-29T02:00:00Z", "isCrossRepository": false, '
                                        f'"headRefOid": "{HEAD}"}}')
    assert (await GitHubCli().pull_request(URL)).head_sha == HEAD
    assert "headRefOid" in seen["argv"][-1]


# merged head commit (lets cleanup accept commits added after final review) ------------------
@pytest.mark.asyncio
async def test_detected_merge_records_the_merged_head_and_undo_clears_it(synced):
    await synced.apply_github_pr("TEST-1", pr("MERGED"))
    assert merged_head(synced) == HEAD
    await synced.revert_pr_merged("TEST-1")
    assert merged_head(synced) is None


@pytest.mark.asyncio
async def test_merged_head_is_backfilled_for_a_task_marked_merged_by_hand(synced):
    await synced.mark_pr_created("TEST-1", URL)
    await synced.mark_pr_merged("TEST-1")
    assert merged_head(synced) is None
    snap = await synced.apply_github_pr("TEST-1", pr("MERGED"))
    assert snap["status"] == "MERGED"
    assert merged_head(synced) == HEAD
    with synced.db.session() as session:
        messages = [e.message for e in session.scalars(select(WorkflowEvent)).all()]
    assert "Recorded the merged pull request's head commit from GitHub" in messages
    assert await synced.apply_github_pr("TEST-1", pr("MERGED")) is None


@pytest.mark.asyncio
async def test_merged_head_is_not_recorded_unless_github_shows_that_pr_merged(synced):
    await synced.mark_pr_created("TEST-1", URL)
    await synced.mark_pr_merged("TEST-1")
    assert await synced.apply_github_pr("TEST-1", pr("OPEN")) is None
    assert await synced.apply_github_pr("TEST-1", pr("MERGED", url="https://github.com/acme/repo/pull/99")) is None
    assert await synced.apply_github_pr("TEST-1", pr("MERGED", head=None)) is None
    assert merged_head(synced) is None


@pytest.mark.asyncio
async def test_poll_backfills_merged_tasks_only_while_their_worktree_exists(synced, tmp_path):
    await synced.mark_pr_created("TEST-1", URL)
    await synced.mark_pr_merged("TEST-1")
    fake = FakeGitHub(by_url={URL: pr("MERGED")})
    sync = poller(synced, fake)

    await sync.poll_once()  # the fixture's worktree path doesn't exist: already cleaned up
    assert fake.calls == []

    (tmp_path / "tree").mkdir()
    await sync.poll_once()
    assert fake.calls == [("view", URL)]
    assert merged_head(synced) == HEAD

    await sync.poll_once()  # recorded; not looked up again
    assert fake.calls == [("view", URL)]
