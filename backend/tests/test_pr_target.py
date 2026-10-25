"""Opening a PR into the task's base branch, and catching one opened into another branch."""
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import select

from agentflow.database.models import WorkflowEvent
from agentflow.services.git_workspace import GitError
from agentflow.services.github import GitHubError, PullRequest, compare_url
from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.notifications import notices
from agentflow.workflow.states import TaskStatus
from test_workflow_safety import SHA, client, engine

URL = "https://github.com/acme/repo/pull/7"


def pr(state, base="main", url=URL):
    created = datetime.now(timezone.utc) + timedelta(hours=1)
    return PullRequest(url=url, state=state, created_at=created,
                       merged_at=created if state == "MERGED" else None, head_sha=SHA, base_branch=base)


def task_field(engine, name):
    with engine.db.session() as session:
        return getattr(engine._load_task(session, "TEST-1"), name)


def set_task(engine, **fields):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        for name, value in fields.items():
            setattr(task, name, value)


def messages(engine, level=None):
    with engine.db.session() as session:
        query = select(WorkflowEvent.message).order_by(WorkflowEvent.id)
        if level:
            query = query.where(WorkflowEvent.level == level)
        return list(session.scalars(query))


@pytest.fixture
def ready(engine):
    set_task(engine, status=TaskStatus.READY_FOR_PR, current_sha=SHA)
    return engine


class FakeGitHub:
    def __init__(self, error=None):
        self.error = error
        self.calls = []

    async def retarget(self, url, base):
        self.calls.append((url, base))
        if self.error:
            raise self.error


# recording the PR's target --------------------------------------------------------------
@pytest.mark.asyncio
async def test_detected_pr_records_its_base_and_warns_only_when_it_differs(ready):
    await ready.apply_github_pr("TEST-1", pr("OPEN"))
    assert task_field(ready, "pr_base_branch") == "main"
    assert messages(ready, "warning") == []
    # Only the base changes (retargeted on GitHub): still saved, published and warned about once.
    snap = await ready.apply_github_pr("TEST-1", pr("OPEN", base="release/candidate"))
    assert snap is not None and snap["prBaseBranch"] == "release/candidate"
    assert messages(ready, "warning") == ["The pull request targets release/candidate, but the task started from main"]
    assert await ready.apply_github_pr("TEST-1", pr("OPEN", base="release/candidate")) is None
    assert len(messages(ready, "warning")) == 1


@pytest.mark.asyncio
async def test_base_of_another_or_closed_pr_is_not_recorded(ready):
    await ready.apply_github_pr("TEST-1", pr("CLOSED", base="develop"))
    assert task_field(ready, "pr_base_branch") is None
    await ready.mark_pr_created("TEST-1", URL)
    assert await ready.apply_github_pr("TEST-1", pr("OPEN", base="develop", url=URL + "8")) is None
    assert task_field(ready, "pr_base_branch") is None


@pytest.mark.asyncio
async def test_marking_or_undoing_pr_created_forgets_the_recorded_base(ready):
    await ready.apply_github_pr("TEST-1", pr("OPEN", base="develop"))
    await ready.revert_pr_created("TEST-1")
    assert task_field(ready, "pr_base_branch") is None
    await ready.apply_github_pr("TEST-1", pr("OPEN", base="develop"))
    set_task(ready, status=TaskStatus.READY_FOR_PR)
    await ready.mark_pr_created("TEST-1", URL)
    assert task_field(ready, "pr_base_branch") is None


# retarget -------------------------------------------------------------------------------
@pytest.mark.asyncio
async def test_retarget_points_the_recorded_pr_at_the_task_base(ready):
    github = FakeGitHub()
    ready.github = lambda settings: github
    with pytest.raises(WorkflowError, match="must have a PR created") as raised:
        await ready.retarget_pr("TEST-1")
    assert raised.value.status_code == 409
    await ready.apply_github_pr("TEST-1", pr("OPEN", base="develop"))
    snap = await ready.retarget_pr("TEST-1")
    assert snap["prBaseBranch"] == "main"
    assert github.calls == [(URL, "main")]
    assert "Human retargeted the pull request to main" in messages(ready)


@pytest.mark.asyncio
async def test_retarget_refuses_links_it_cannot_edit_and_keeps_the_base_when_gh_fails(ready):
    github = FakeGitHub(GitHubError("gh pr edit failed: exit 1"))
    ready.github = lambda settings: github
    await ready.mark_pr_created("TEST-1", "https://example.test/pr/3")
    with pytest.raises(WorkflowError) as raised:
        await ready.retarget_pr("TEST-1")
    assert raised.value.status_code == 409 and github.calls == []
    set_task(ready, pr_url=URL, pr_base_branch="develop")
    with pytest.raises(WorkflowError, match="gh pr edit failed") as raised:
        await ready.retarget_pr("TEST-1")
    assert raised.value.status_code == 502
    assert task_field(ready, "pr_base_branch") == "develop"


# open: push, then GitHub's form targeting the base -----------------------------------------
@pytest.fixture
def openable(ready):
    set_task(ready, base_branch="release/candidate")
    ready.git.remote_url = AsyncMock(return_value="git@github.com:acme/repo.git")
    ready.git.branch_sha = AsyncMock(return_value=SHA)
    ready.git.push_branch = AsyncMock()
    return ready


@pytest.mark.asyncio
async def test_open_pr_pushes_the_reviewed_branch_and_targets_the_task_base(openable):
    answer = await openable.open_pr("TEST-1")
    assert answer["url"] == "https://github.com/acme/repo/compare/release/candidate...agent/TEST-1?expand=1"
    assert answer["task"]["status"] == "READY_FOR_PR"
    # No worktree directory in this fixture, so the push runs from the repository.
    root, worktree = task_field(openable, "repository_root"), task_field(openable, "worktree_path")
    assert openable.git.push_branch.await_args.args == (root, "agent/TEST-1")
    Path(worktree).mkdir(parents=True)
    await openable.open_pr("TEST-1")
    assert openable.git.push_branch.await_args.args == (worktree, "agent/TEST-1")


@pytest.mark.asyncio
async def test_open_pr_pushes_nothing_unless_it_can_target_the_base(openable):
    set_task(openable, status=TaskStatus.PR_CREATED)
    with pytest.raises(WorkflowError, match="must be Ready for PR"):
        await openable.open_pr("TEST-1")
    set_task(openable, status=TaskStatus.READY_FOR_PR)
    openable.git.remote_url.return_value = "https://gitlab.com/acme/repo.git"
    with pytest.raises(WorkflowError, match="into release/candidate yourself") as raised:
        await openable.open_pr("TEST-1")
    assert raised.value.status_code == 409
    openable.git.remote_url.return_value = None
    with pytest.raises(WorkflowError, match="not on a GitHub host"):
        await openable.open_pr("TEST-1")
    openable.git.remote_url.return_value = "https://github.com/acme/repo.git"
    openable.git.branch_sha.return_value = "b" * 40
    with pytest.raises(WorkflowError, match="not at the reviewed checkpoint"):
        await openable.open_pr("TEST-1")
    openable.git.branch_sha.return_value = None
    with pytest.raises(WorkflowError, match="not at the reviewed checkpoint"):
        await openable.open_pr("TEST-1")
    openable.git.push_branch.assert_not_awaited()


@pytest.mark.asyncio
async def test_open_pr_reports_a_failed_push(openable):
    openable.git.push_branch.side_effect = GitError("Pushing agent/TEST-1 to origin failed (git exit 1).")
    with pytest.raises(WorkflowError, match="git exit 1") as raised:
        await openable.open_pr("TEST-1")
    assert raised.value.status_code == 502


def test_open_and_retarget_endpoints(client, openable):
    origin = {"Origin": "http://127.0.0.1"}
    assert client.post("/api/tasks/TEST-1/pr/open").status_code == 401
    code = client.app.state.human.pairing_code
    assert client.post("/api/auth/pair", json={"code": code}, headers=origin).status_code == 200
    result = client.post("/api/tasks/TEST-1/pr/open", headers=origin)
    assert result.status_code == 200
    assert "/compare/release/candidate...agent/TEST-1" in result.json()["url"]
    assert client.post("/api/tasks/TEST-1/pr/retarget", headers=origin).status_code == 409
    # An agent in the worktree can neither push nor edit the PR.
    for action in ("open", "retarget"):
        refused = client.post(f"/api/tasks/TEST-1/pr/{action}", headers={"Authorization": "Bearer test-token"})
        assert refused.status_code == 403
    openable.git.push_branch.assert_awaited_once()


def test_compare_url_encodes_branch_names_but_keeps_slashes():
    assert (compare_url("github.com/acme/repo", "release/2.0", "agent/DE-1#x+y")
            == "https://github.com/acme/repo/compare/release/2.0...agent/DE-1%23x%2By?expand=1")


def test_wrong_pr_base_is_announced_with_pr_notices_until_it_matches():
    def snapshot(status, target):
        return {"status": status, "baseBranch": "release/candidate", "prBaseBranch": target}

    found = notices(snapshot("PR_CREATED", "main"), ["READY_FOR_PR"], False)
    assert list(found) == ["pr:base-mismatch"]
    assert "main" not in found["pr:base-mismatch"].summary
    for quiet in (snapshot("PR_CREATED", "release/candidate"), snapshot("PR_CREATED", None),
                  snapshot("MERGED", "main")):
        assert notices(quiet, ["READY_FOR_PR"], False) == {}
    assert notices(snapshot("PR_CREATED", "main"), [], False) == {}
