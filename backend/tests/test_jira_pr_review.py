"""Moving the Jira ticket to In Code Review once a task's pull request is created."""
import json
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest

from agentflow.config import JiraConfig
from agentflow.services.github import PullRequest
from agentflow.services.jira_client import JiraError, JiraService, TransitionOutcome
from agentflow.workflow.states import TaskStatus
from test_workflow_safety import SHA, engine

URL = "https://github.com/acme/repo/pull/7"
REVIEW = [{"id": "41", "name": "Request review", "to": {"name": "In Code Review"}}]


def _service(status_name, transitions, posted):
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/rest/api/3/issue/DE-1" and request.method == "GET":
            return httpx.Response(200, json={"fields": {"status": {"name": status_name}}})
        if request.url.path == "/rest/api/3/issue/DE-1/transitions" and request.method == "GET":
            return httpx.Response(200, json={"transitions": transitions})
        if request.url.path == "/rest/api/3/issue/DE-1/transitions" and request.method == "POST":
            posted.append(json.loads(request.content))
            return httpx.Response(204)
        raise AssertionError(f"unexpected {request.method} {request.url}")
    return JiraService(JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
                       transport=httpx.MockTransport(handler))


def test_defaults_move_to_in_code_review_from_any_status():
    config = JiraConfig()
    assert config.pr_created_transition_status == "In Code Review"
    assert config.pr_created_transition_from_statuses == ["*"]
    assert JiraConfig.model_validate({"prCreatedTransitionStatus": None}).pr_created_transition_status is None


@pytest.mark.asyncio
async def test_star_allows_any_status():
    posted = []
    outcome = await _service("Blocked", REVIEW, posted).transition("DE-1", "In Code Review", ["*"])
    assert outcome == TransitionOutcome("moved", "Blocked")
    assert posted == [{"transition": {"id": "41"}}]


@pytest.mark.asyncio
@pytest.mark.parametrize("status_name, transitions, from_statuses, expected", [
    ("in code review", REVIEW, ["*"], "already"),       # already there: no self-transition
    ("Blocked", REVIEW, ["In Progress"], "not-allowed"),
    ("In Progress", [], ["*"], "unavailable"),          # the workflow has no such status
])
async def test_transition_says_why_it_did_not_move(status_name, transitions, from_statuses, expected):
    posted = []
    service = _service(status_name, transitions, posted)
    outcome = await service.transition("DE-1", "In Code Review", from_statuses)
    assert outcome == TransitionOutcome(expected, status_name)
    assert await service.transition_to("DE-1", "In Code Review", from_statuses) is False
    assert posted == []


# engine -----------------------------------------------------------------------------------
@pytest.fixture
def ready(engine, monkeypatch):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.status = TaskStatus.READY_FOR_PR
        task.current_sha = SHA
    lock_held = []

    async def transition(*args):
        lock_held.append(engine._lock("TEST-1").locked())
        return fake.outcome

    fake = SimpleNamespace(transition=AsyncMock(side_effect=transition), outcome=TransitionOutcome("moved", "In Progress"),
                           lock_held=lock_held)
    monkeypatch.setattr("agentflow.workflow.engine.build_jira_service", lambda cfg: fake)
    engine.fake_jira = fake
    return engine


def _messages(snap):
    return [(e["level"], e["message"]) for e in snap["events"]]


@pytest.mark.asyncio
async def test_marking_the_pr_created_moves_the_ticket_outside_the_task_lock(ready):
    snap = await ready.mark_pr_created("TEST-1", URL)
    ready.fake_jira.transition.assert_awaited_once_with("TEST-1", "In Code Review", ["*"])
    assert ready.fake_jira.lock_held == [False]
    assert snap["status"] == "PR_CREATED"
    assert ("info", "Jira ticket moved from In Progress to In Code Review") in _messages(snap)


@pytest.mark.asyncio
async def test_a_jira_failure_never_undoes_the_pr(ready):
    ready.fake_jira.transition.side_effect = JiraError("Jira is down")
    snap = await ready.mark_pr_created("TEST-1", URL)
    assert snap["status"] == "PR_CREATED"
    assert ("warning", "Could not move the Jira ticket to In Code Review: Jira is down") in _messages(snap)


@pytest.mark.asyncio
async def test_a_missing_review_status_is_logged_as_a_warning(ready):
    ready.fake_jira.outcome = TransitionOutcome("unavailable", "In Progress")
    snap = await ready.mark_pr_created("TEST-1", URL)
    assert any(level == "warning" and "has no transition to In Code Review" in message
               for level, message in _messages(snap))


@pytest.mark.asyncio
async def test_already_in_review_logs_nothing(ready):
    ready.fake_jira.outcome = TransitionOutcome("already", "In Code Review")
    snap = await ready.mark_pr_created("TEST-1", URL)
    assert not any("Jira" in message for _, message in _messages(snap))


@pytest.mark.asyncio
async def test_custom_status_and_from_list_are_used_and_null_turns_it_off(ready):
    ready.config.jira.pr_created_transition_status = "Review"
    ready.config.jira.pr_created_transition_from_statuses = ["In Progress"]
    await ready.mark_pr_created("TEST-1", URL)
    ready.fake_jira.transition.assert_awaited_once_with("TEST-1", "Review", ["In Progress"])

    await ready.revert_pr_created("TEST-1")
    ready.config.jira.pr_created_transition_status = None
    await ready.mark_pr_created("TEST-1", URL)
    assert ready.fake_jira.transition.await_count == 1


def _pr(state):
    created = datetime.now(timezone.utc) + timedelta(hours=1)
    return PullRequest(url=URL, state=state, created_at=created,
                       merged_at=created + timedelta(hours=1) if state == "MERGED" else None, head_sha="d" * 40)


@pytest.mark.asyncio
async def test_github_sync_moves_the_ticket_once_when_it_finds_the_pr(ready):
    ready.config.github.pr_sync = True
    await ready.apply_github_pr("TEST-1", _pr("OPEN"))
    await ready.apply_github_pr("TEST-1", _pr("OPEN"))  # unchanged
    await ready.apply_github_pr("TEST-1", _pr("MERGED"))
    ready.fake_jira.transition.assert_awaited_once()


@pytest.mark.asyncio
async def test_a_pr_first_seen_merged_does_not_move_the_ticket_to_review(ready):
    ready.config.github.pr_sync = True
    snap = await ready.apply_github_pr("TEST-1", _pr("MERGED"))
    assert snap["status"] == "MERGED"
    ready.fake_jira.transition.assert_not_awaited()


@pytest.mark.asyncio
async def test_undoing_merged_does_not_move_the_ticket_again(ready):
    await ready.mark_pr_created("TEST-1", URL)
    await ready.mark_pr_merged("TEST-1")
    await ready.revert_pr_merged("TEST-1")
    ready.fake_jira.transition.assert_awaited_once()
