"""Local prompts retain normal workflow controls without invoking Jira."""
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock
from uuid import UUID

import pytest
from pydantic import ValidationError

from agentflow.services.git_workspace import CreatedWorktree
from agentflow.workflow import prompts
from agentflow.workflow.models import CreateTaskRequest, is_local_task
from test_prompts import make_ctx
from test_workflow_safety import engine

LOCAL_KEY = "LOCAL-71C009F9-E419-4C6C-878F-D4E90467C9DF"

@pytest.mark.parametrize("payload", [
    {"taskSource": "local", "prompt": "  "},
    {"taskSource": "jira"},
    {"jiraKey": LOCAL_KEY},
    {"taskSource": "other", "prompt": "hello"},
])
def test_invalid_task_source_request_is_rejected(payload):
    with pytest.raises(ValidationError):
        CreateTaskRequest(repositoryPath="repo", baseBranch="main", **payload)

@pytest.mark.asyncio
async def test_local_creation_preserves_prompt_and_never_builds_jira_service(engine, tmp_path, monkeypatch):
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root=str(tmp_path), path=str(tmp_path)))
    engine.git.create_worktree = AsyncMock(return_value=CreatedWorktree("agent/local"))
    engine.zellij.ensure_session = AsyncMock(return_value=False)
    jira = Mock(side_effect=AssertionError("Local tasks must not access Jira"))
    monkeypatch.setattr("agentflow.workflow.engine.build_jira_service", jira)
    request = CreateTaskRequest(taskSource="local", prompt="  Build a dashboard\nwith search  ",
                                repositoryPath=str(tmp_path), baseBranch="main", additionalContext="Use existing styles")
    snapshot = await engine.create_task(request)
    assert snapshot["taskSource"] == "local"
    UUID(snapshot["jiraKey"][6:])
    assert snapshot["jiraTitle"] == "Build a dashboard"
    with engine.db.session() as session:
        task = engine._load_task(session, snapshot["jiraKey"])
        assert task.additional_context == "Build a dashboard\nwith search\n\nUse existing styles"
        await engine._sync_jira_context(session, task)
        engine.config.jira.auto_transition_status = "In progress"
        await engine._auto_transition_jira(session, task, task.jira_key)
    assert engine.jira_issue(snapshot["jiraKey"]) is None
    assert not jira.called
    assert any(node["state"] == "READY" for node in snapshot["nodes"])


def test_local_architect_discovery_uses_prompt_without_jira_instructions():
    text = prompts.architect_discovery(make_ctx(jira_key=LOCAL_KEY, additional_context="Build a dashboard"))
    assert "Build a dashboard" in text
    assert "Local task discovery" in text
    assert "Jira" not in text
    assert "MCP" not in text
    assert "flowctl" in text


def test_local_task_ignores_a_client_jira_key():
    request = CreateTaskRequest(taskSource="local", prompt="Fix it", jiraKey="not a key!",
                                repositoryPath="repo", baseBranch="main")
    assert request.jira_key is None


def test_prompt_is_limited_to_100000_characters():
    CreateTaskRequest(taskSource="local", prompt="x" * 100000, repositoryPath="repo", baseBranch="main")
    with pytest.raises(ValidationError):
        CreateTaskRequest(taskSource="local", prompt="x" * 100001, repositoryPath="repo", baseBranch="main")


@pytest.mark.asyncio
async def test_local_title_is_the_first_line_cut_to_120_characters(engine, tmp_path):
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root=str(tmp_path), path=str(tmp_path)))
    engine.git.create_worktree = AsyncMock(return_value=CreatedWorktree("agent/local"))
    engine.zellij.ensure_session = AsyncMock(return_value=False)
    prompt = "é" * 130 + "\nmore"
    snapshot = await engine.create_task(CreateTaskRequest(
        taskSource="local", prompt=prompt, repositoryPath=str(tmp_path), baseBranch="main", additionalContext="  \n "))
    assert snapshot["jiraTitle"] == "é" * 120
    assert snapshot["additionalContext"] == prompt


def test_jira_project_named_local_is_not_a_local_task():
    assert not is_local_task("LOCAL-123")
    assert CreateTaskRequest(jiraKey="local-123", repositoryPath="repo", baseBranch="main").jira_key == "LOCAL-123"
