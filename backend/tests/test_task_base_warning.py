"""Task creation surfaces a base branch that couldn't be refreshed from origin."""
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from agentflow.services.git_workspace import CreatedWorktree
from agentflow.workflow.models import CreateTaskRequest
from test_workflow_safety import engine  # noqa: F401 (fixture)

WARNING = ("Couldn't fetch main from origin, so this task branched from the last fetched copy of main, "
           "which may be missing newer commits.")


async def create(engine, tmp_path, monkeypatch, created):
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root=str(tmp_path), path=str(tmp_path)))
    engine.git.create_worktree = AsyncMock(return_value=created)
    engine.zellij.ensure_session = AsyncMock(return_value=False)
    monkeypatch.setattr("agentflow.workflow.engine.build_jira_service", lambda _config: None)
    return await engine.create_task(CreateTaskRequest(jiraKey="NEW-1", repositoryPath=str(tmp_path), baseBranch="main"))


def warnings(snapshot):
    return [event["message"] for event in snapshot["events"] if event["level"] == "warning"]


@pytest.mark.asyncio
async def test_a_base_that_could_not_be_fetched_is_flagged_on_the_new_task(engine, tmp_path, monkeypatch):
    snapshot = await create(engine, tmp_path, monkeypatch, CreatedWorktree("agent/NEW-1", WARNING))
    assert snapshot["statusMessage"] == WARNING
    assert WARNING in warnings(snapshot)


@pytest.mark.asyncio
async def test_a_fetched_base_leaves_the_new_task_without_a_warning(engine, tmp_path, monkeypatch):
    snapshot = await create(engine, tmp_path, monkeypatch, CreatedWorktree("agent/NEW-1"))
    assert snapshot["statusMessage"] is None
    assert WARNING not in warnings(snapshot)
