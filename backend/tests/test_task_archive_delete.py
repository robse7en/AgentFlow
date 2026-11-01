"""Archive, delete and cancel-from-post-ready."""
import shutil
import subprocess
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace as Item
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import func, select

from agentflow.api.security import digest
from agentflow.config import CostControls
from agentflow.database.database import Database
from agentflow.database.models import (AgentAccess, AgentQuestion, AgentSession, ChatThread, DeletedRunSpend,
                                       ImplementationWorkItem, NodeRun, ProcessLease, RequirementsApproval,
                                       ReviewFinding, RunEvidence, Task, TaskArtifact, WorkflowEvent, WorkflowNode)
from agentflow.services.git_workspace import GitError, GitService
from agentflow.workflow import default_workflow as wf
from agentflow.workflow.budgets import _daily
from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.models import CreateTaskRequest
from agentflow.workflow.usage import UsageReport
from agentflow.workflow.states import TaskStatus
from test_workflow_safety import SHA, add_run, client, engine  # noqa: F401 - fixtures


def paired(client):
    headers = {"Origin": "http://127.0.0.1"}
    client.post("/api/auth/pair", json={"code": client.app.state.human.pairing_code}, headers=headers).raise_for_status()
    return headers


def set_status(engine, status):
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = status


# cancel from Ready for PR / PR Created ------------------------------------------------------

@pytest.mark.asyncio
@pytest.mark.parametrize("status", [TaskStatus.READY_FOR_PR, TaskStatus.PR_CREATED])
async def test_cancel_retires_a_task_parked_before_merge(engine, status):
    set_status(engine, status)
    snapshot = await engine.cancel_task("TEST-1")
    assert snapshot["status"] == "CANCELLED"


@pytest.mark.asyncio
async def test_a_merged_task_cannot_be_cancelled(engine):
    set_status(engine, TaskStatus.MERGED)
    with pytest.raises(WorkflowError, match="already reached MERGED"):
        await engine.cancel_task("TEST-1")


# archive -----------------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_archive_requires_a_finished_task(engine):
    with pytest.raises(WorkflowError, match="merged or cancelled"):
        await engine.archive_task("TEST-1")
    set_status(engine, TaskStatus.READY_FOR_PR)
    with pytest.raises(WorkflowError, match="merged or cancelled"):
        await engine.archive_task("TEST-1")


@pytest.mark.asyncio
async def test_archived_tasks_leave_the_default_list_and_come_back(engine):
    set_status(engine, TaskStatus.MERGED)
    snapshot = await engine.archive_task("TEST-1")
    assert snapshot["archivedAt"]
    assert await engine.list_tasks() == []
    assert [t["jiraKey"] for t in await engine.list_tasks("only")] == ["TEST-1"]
    assert [t["jiraKey"] for t in await engine.list_tasks("include")] == ["TEST-1"]

    snapshot = await engine.unarchive_task("TEST-1")
    assert snapshot["archivedAt"] is None
    assert [t["jiraKey"] for t in await engine.list_tasks()] == ["TEST-1"]
    assert await engine.list_tasks("only") == []


@pytest.mark.asyncio
async def test_creating_an_archived_ticket_again_says_how_to_proceed(engine):
    set_status(engine, TaskStatus.CANCELLED)
    await engine.archive_task("TEST-1")
    request = CreateTaskRequest(jiraKey="TEST-1", repositoryPath=".", baseBranch="main")
    with pytest.raises(WorkflowError, match="archived. Unarchive or delete"):
        await engine.create_task(request)


def test_archive_routes_and_list_filter(client, engine):
    set_status(engine, TaskStatus.CANCELLED)
    headers = paired(client)
    assert client.post("/api/tasks/TEST-1/archive", headers=headers).json()["archivedAt"]
    assert client.get("/api/tasks", headers=headers).json() == []
    assert [t["jiraKey"] for t in client.get("/api/tasks?archived=only", headers=headers).json()] == ["TEST-1"]
    assert client.get("/api/tasks?archived=bogus", headers=headers).status_code == 422
    assert client.post("/api/tasks/TEST-1/unarchive", headers=headers).json()["archivedAt"] is None


def test_upgrade_adds_the_archive_column(tmp_path):
    db = Database(tmp_path / "old.db")
    db.create_all()
    with db.engine.begin() as connection:
        connection.exec_driver_sql("ALTER TABLE tasks DROP COLUMN archived_at")
    db.create_all()
    with db.engine.begin() as connection:
        columns = {row[1] for row in connection.exec_driver_sql("PRAGMA table_info(tasks)")}
    assert "archived_at" in columns
    db.engine.dispose()


# spend ledger --------------------------------------------------------------------------------

def test_deleted_runs_still_count_toward_today():
    now = datetime.now(timezone.utc)
    deleted = [
        Item(charged_at=now, billing_mode="metered", cost_usd=2.5),
        Item(charged_at=now, billing_mode="unknown", cost_usd=None),
        Item(charged_at=now, billing_mode="subscription", cost_usd=None),
        Item(charged_at=now - timedelta(days=2), billing_mode="metered", cost_usd=100.0),
    ]
    daily = _daily([], {}, CostControls(dailyBudgetUsd=10), deleted)
    assert daily["knownUsd"] == 2.5
    assert daily["unknownRuns"] == 1
    assert daily["limitUsd"] == 10


# delete --------------------------------------------------------------------------------------

@pytest.fixture
def finished(engine):
    """A cancelled task with a row in every table that references it."""
    run_id = add_run(engine, wf.IMPLEMENTER, "COMPLETED")
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.status = TaskStatus.CANCELLED
        run = session.get(NodeRun, run_id)
        run.completed_at = datetime.now(timezone.utc)
        usage = UsageReport(inputTokens=10, outputTokens=10, model="m", source="fixture", costUsd=1.25)
        session.add(RunEvidence(run_id=run_id, billing_mode="metered", usage=usage.record("fixture")))
        agent = AgentSession(task_id=task.id, role="implementer", harness="codex", zellij_tab_name="Implementer",
                             status="stopped", current_run_id=run_id)
        session.add(agent)
        session.flush()
        session.add(ProcessLease(session_id=agent.id, launch_run_id=run_id))
        session.add(AgentAccess(task_id=task.id, run_id=run_id, role="implementer", token_hash=digest("t")))
        node = engine._node(task, wf.IMPLEMENTER)
        session.add(ImplementationWorkItem(
            task_id=task.id, node_id=node.id, plan_key="a", brief="b", owned_paths=[], acceptance_criteria=[],
            dependencies=[], worktree_path=str(engine.config.worktree_dir / "TEST-1-worker-a"),
            branch_name="agentflow-worker/a", base_sha=SHA))
        session.add(ChatThread(task_id=task.id, platform="slack", notified={}))
        session.add(ReviewFinding(task_id=task.id, finding_id="CODE-001", type="code-review", iteration=1,
                                  severity="low", title="t"))
        session.add(AgentQuestion(task_id=task.id, role="implementer", message="q?"))
        session.add(TaskArtifact(task_id=task.id, kind="k", path="p"))
        task.worktree_path = str(engine.config.worktree_dir / "TEST-1")
    engine.git.branch_sha = AsyncMock(return_value=SHA)
    engine.git.remove_task_worktree = AsyncMock(return_value=[" M changed.py"])
    engine.git.discard_worktree = AsyncMock(return_value=[])
    engine.git.delete_branch = AsyncMock(return_value=True)
    engine.git.branch_report = AsyncMock(return_value={"name": "agent/TEST-1", "exists": True, "sha": SHA,
                                                        "unmergedCommits": 2, "upstream": None, "unpushedCommits": None})
    return engine


def counts(engine):
    with engine.db.session() as session:
        return {model.__tablename__: session.scalar(select(func.count()).select_from(model))
                for model in (Task, WorkflowNode, NodeRun, AgentSession, ProcessLease, AgentAccess, RunEvidence,
                              RequirementsApproval, ImplementationWorkItem, ChatThread, ReviewFinding,
                              AgentQuestion, TaskArtifact, WorkflowEvent)}


async def delete(engine, **flags):
    options = {"remove_worktree": False, "remove_branch": False, "remove_task_folder": False, "expected_sha": None}
    return await engine.delete_task("TEST-1", **{**options, **flags})


@pytest.mark.asyncio
async def test_delete_removes_every_row_and_keeps_the_spend(finished):
    messages = []
    finished.broadcaster.subscribe(messages.append)
    folder = Path(finished.config.task_dir / "TEST-1")
    result = await delete(finished)
    assert result["deleted"] is True
    assert set(counts(finished).values()) == {0}
    with finished.db.session() as session:
        (spend,) = session.scalars(select(DeletedRunSpend)).all()
    assert (spend.jira_key, spend.billing_mode, spend.cost_usd) == ("TEST-1", "metered", 1.25)
    finished.zellij.kill_session.assert_awaited_once_with("agentflow-TEST-1")
    finished.git.remove_task_worktree.assert_not_awaited()
    finished.git.delete_branch.assert_not_awaited()
    # A kept folder is moved aside so the ticket can start again from an empty one.
    assert not folder.exists()
    assert Path(result["removed"]["taskFolderMovedTo"]).is_dir()
    assert {"type": "task.deleted", "jiraKey": "TEST-1"} in messages
    with pytest.raises(WorkflowError):
        finished.snapshot("TEST-1")


@pytest.mark.asyncio
async def test_deleted_spend_keeps_the_daily_budget_whole(finished):
    finished.config.workflow.cost_controls.daily_budget_usd = 100
    before = finished.snapshot("TEST-1")["budget"]["daily"]["knownUsd"]
    await delete(finished)
    with finished.db.session() as session:
        from agentflow.workflow.budgets import daily_spend
        assert daily_spend(session, finished.config.workflow.cost_controls)["knownUsd"] == before == 1.25


@pytest.mark.asyncio
async def test_delete_everything(finished):
    result = await delete(finished, remove_worktree=True, remove_branch=True, remove_task_folder=True, expected_sha=SHA)
    finished.git.remove_task_worktree.assert_awaited_once()
    finished.git.discard_worktree.assert_awaited_once()
    assert [c.args[1] for c in finished.git.delete_branch.await_args_list] == ["agent/TEST-1", "agentflow-worker/a"]
    assert result["removed"]["discardedPaths"] == ["changed.py"]
    assert not (finished.config.task_dir / "TEST-1").exists()
    assert not list(finished.config.task_dir.glob("TEST-1.deleted-*"))


@pytest.mark.asyncio
async def test_delete_refuses_an_unfinished_task(finished):
    set_status(finished, TaskStatus.PR_CREATED)
    with pytest.raises(WorkflowError, match="merged or cancelled"):
        await delete(finished)
    with pytest.raises(WorkflowError, match="merged or cancelled"):
        await finished.delete_preview("TEST-1")
    assert counts(finished)["tasks"] == 1


@pytest.mark.asyncio
async def test_delete_refuses_a_moved_branch(finished):
    with pytest.raises(WorkflowError, match="branch changed since the preview"):
        await delete(finished, remove_worktree=True, expected_sha="b" * 40)
    finished.git.remove_task_worktree.assert_not_awaited()
    finished.zellij.kill_session.assert_not_awaited()
    assert counts(finished)["tasks"] == 1


@pytest.mark.asyncio
async def test_branch_removal_needs_the_worktree_removed(finished):
    Path(finished.config.worktree_dir / "TEST-1").mkdir(parents=True)
    with pytest.raises(WorkflowError, match="remove the worktrees"):
        await delete(finished, remove_branch=True, expected_sha=SHA)
    finished.git.delete_branch.assert_not_awaited()


@pytest.mark.asyncio
async def test_branch_removal_needs_worker_worktrees_removed_too(finished):
    Path(finished.config.worktree_dir / "TEST-1-worker-a").mkdir(parents=True)
    with pytest.raises(WorkflowError, match="remove the worktrees"):
        await delete(finished, remove_branch=True, expected_sha=SHA)
    finished.git.delete_branch.assert_not_awaited()


@pytest.mark.asyncio
async def test_a_failed_disk_step_leaves_the_task_to_retry(finished):
    finished.git.remove_task_worktree.side_effect = GitError("Permission denied")
    with pytest.raises(WorkflowError, match="Permission denied"):
        await delete(finished, remove_worktree=True, remove_task_folder=True, expected_sha=SHA)
    assert finished.git.remove_task_worktree.await_count == 2  # retried once
    assert counts(finished)["tasks"] == 1
    assert (finished.config.task_dir / "TEST-1").exists()

    finished.git.remove_task_worktree.side_effect = None
    await delete(finished, remove_worktree=True, remove_task_folder=True, expected_sha=SHA)
    assert counts(finished)["tasks"] == 0


@pytest.mark.asyncio
async def test_unknown_cost_blocks_delete_only_under_a_daily_limit(finished):
    with finished.db.session() as session:
        session.scalar(select(RunEvidence)).billing_mode = "unknown"
    finished.config.workflow.cost_controls.daily_budget_usd = 50
    assert (await finished.delete_preview("TEST-1"))["unknownCostRunsToday"] == 1
    with pytest.raises(WorkflowError, match="Import the missing run costs"):
        await delete(finished)
    finished.config.workflow.cost_controls.daily_budget_usd = None
    assert (await finished.delete_preview("TEST-1"))["unknownCostRunsToday"] == 0
    await delete(finished)
    assert counts(finished)["tasks"] == 0


@pytest.mark.asyncio
async def test_delete_preview_reports_what_would_go(finished):
    preview = await finished.delete_preview("TEST-1")
    assert preview["branch"]["unmergedCommits"] == 2
    assert preview["branch"]["sha"] == SHA
    assert preview["worktree"] == {"path": str(finished.config.worktree_dir / "TEST-1"), "exists": False,
                                   "dirtyPaths": [], "error": None}
    assert preview["workerBranches"] == ["agentflow-worker/a"]
    assert preview["taskFolder"]["exists"] is True
    assert preview["sessionWillBeClosed"] is True


def test_delete_routes(client, finished):
    headers = paired(client)
    assert client.get("/api/tasks/TEST-1/delete-preview", headers=headers).json()["branch"]["sha"] == SHA
    response = client.post("/api/tasks/TEST-1/delete", headers=headers, json={
        "removeWorktree": False, "removeBranch": False, "removeTaskFolder": True})
    assert response.status_code == 200, response.text
    assert client.get("/api/tasks/TEST-1", headers=headers).status_code == 404


# real git ------------------------------------------------------------------------------------

class TaskDeleteGitTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.workspace = Path(__file__).resolve().parents[2]
        self.folder = Path(tempfile.mkdtemp(prefix=".git-verification-", dir=self.workspace))
        self.repo = self.folder / "repo"
        self.repo.mkdir()
        self.trees = self.folder / "trees"
        self.git("init", "-q", "-b", "main")
        ignore = self.folder / "ignore"
        ignore.touch()
        for key, value in {"core.excludesFile": str(ignore), "core.hooksPath": str(self.folder / "hooks"),
                           "commit.gpgSign": "false", "user.name": "Test", "user.email": "test@localhost"}.items():
            self.git("config", key, value)
        (self.repo / "file.txt").write_text("base\n")
        self.git("add", "file.txt")
        self.git("commit", "-qm", "base")
        self.service = GitService()

    def tearDown(self):
        resolved = self.folder.resolve()
        if not resolved.is_relative_to(self.workspace):
            raise AssertionError("Refusing unsafe test cleanup")

        def writable_retry(function, filename, error):
            Path(filename).chmod(0o700)
            function(filename)
        shutil.rmtree(resolved, onerror=writable_retry)

    def git(self, *args, cwd=None):
        result = subprocess.run(["git", "-C", str(cwd or self.repo), *map(str, args)], capture_output=True, text=True)
        if result.returncode:
            self.fail(result.stderr)
        return result

    async def test_unmerged_work_is_reported_then_removed(self):
        tree = self.trees / "TEST-1"
        self.git("worktree", "add", "-q", "-b", "agent/TEST-1", tree, "main")
        (tree / "work.txt").write_text("work\n")
        self.git("add", "work.txt", cwd=tree)
        self.git("commit", "-qm", "work", cwd=tree)
        (tree / "dirty.txt").write_text("uncommitted\n")

        report = await self.service.branch_report(str(self.repo), "agent/TEST-1", "main")
        assert report["exists"] and report["unmergedCommits"] == 1
        assert report["upstream"] is None and report["unpushedCommits"] is None

        discarded = await self.service.remove_task_worktree(str(self.repo), str(tree), self.trees)
        assert [line[3:] for line in discarded] == ["dirty.txt"]
        assert not tree.exists()
        assert await self.service.delete_branch(str(self.repo), "agent/TEST-1") is True
        assert await self.service.delete_branch(str(self.repo), "agent/TEST-1") is False
        assert (await self.service.branch_report(str(self.repo), "agent/TEST-1", "main"))["exists"] is False

    async def test_worktree_outside_the_managed_directory_is_refused(self):
        outside = self.folder / "elsewhere"
        self.git("worktree", "add", "-q", "-b", "agent/TEST-2", outside, "main")
        with self.assertRaises(GitError):
            await self.service.remove_task_worktree(str(self.repo), str(outside), self.trees)
        with self.assertRaises(GitError):
            await self.service.remove_task_worktree(str(self.repo), str(self.repo), self.trees)
        assert outside.exists()

    async def test_unregistered_leftover_directory_is_removed(self):
        leftover = self.trees / "TEST-3"
        leftover.mkdir(parents=True)
        (leftover / "junk.txt").write_text("x")
        assert await self.service.remove_task_worktree(str(self.repo), str(leftover), self.trees) == []
        assert not leftover.exists()
