from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from agentflow.database.models import AgentSession, ImplementationWorkItem, Task
from agentflow.services.git_workspace import BranchUnverified, GitError, WorktreeBusy
from agentflow.services.worktree_processes import WorktreeHolder, WorktreeProcess
from agentflow.services.zellij import ZellijError
from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.states import TaskStatus
from test_workflow_safety import SHA, client, engine


@pytest.fixture
def ready(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.status = TaskStatus.MERGED
        task.worktree_path = str(engine.config.worktree_dir / "TEST-1")
    engine.git.cleanup_worktree = AsyncMock(return_value=[])
    engine.git.discard_worktree = AsyncMock(return_value=[])
    return engine


def add_worker_item(engine, worktree_path):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = task.nodes[0]
        session.add(ImplementationWorkItem(
            task_id=task.id, node_id=node.id, plan_key="worker-a", brief="do the thing",
            owned_paths=[], acceptance_criteria=[], dependencies=[],
            worktree_path=worktree_path, branch_name="agentflow-worker/a", base_sha=SHA,
        ))


@pytest.mark.asyncio
async def test_cleanup_requires_merged_status_and_current_checkpoint(ready):
    with pytest.raises(WorkflowError, match="checkpoint changed"):
        await ready.cleanup_worktree("TEST-1", "b" * 40)
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        task.status = TaskStatus.READY_FOR_PR
    with pytest.raises(WorkflowError, match="marked Merged"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_cleanup_refuses_active_task_or_unmanaged_path(ready):
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        task.status = TaskStatus.ACTIVE
    with pytest.raises(WorkflowError):
        await ready.cleanup_worktree("TEST-1", SHA)
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        task.status = TaskStatus.MERGED
        task.worktree_path = str(ready.config.task_dir / "TEST-1")
    with pytest.raises(WorkflowError, match="configured task location"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_cleanup_force_closes_live_agent_pane(ready):
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=7,
                                zellij_pane_id="terminal_7", status="running"))
    ready.zellij.list_panes.return_value = [SimpleNamespace(pane_id="terminal_7", tab_id=7, exited=False)]
    await ready.cleanup_worktree("TEST-1", SHA)
    ready.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)
    ready.git.cleanup_worktree.assert_awaited_once()
    with ready.db.session() as session:
        agent = ready._load_task(session, "TEST-1").sessions[0]
        assert agent.status == "stopped"


@pytest.mark.asyncio
async def test_cleanup_closes_pane_even_when_db_status_is_stale(ready):
    """DB status can drift from Zellij reality by the time a task reaches Merged
    (e.g. a crash recovery that never confirmed exit). A pane that's actually still
    live must be force-closed, not silently ignored because the row says 'stopped'."""
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=None,
                                zellij_pane_id="terminal_7", status="stopped"))
    ready.zellij.list_panes.return_value = [SimpleNamespace(pane_id="terminal_7", tab_id=7, exited=False)]
    await ready.cleanup_worktree("TEST-1", SHA)
    ready.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)
    ready.git.cleanup_worktree.assert_awaited_once()


@pytest.mark.asyncio
async def test_cleanup_does_not_block_on_a_running_row_whose_pane_already_exited(ready):
    """The inverse drift: DB still says 'running' but the pane is actually gone.
    This used to be handled gracefully (marked exited, cleanup proceeds); it must
    still not demand a tab id `_stop_sessions` has no way to supply."""
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=None,
                                zellij_pane_id="terminal_7", status="running"))
    ready.zellij.list_panes.return_value = []  # pane is gone
    await ready.cleanup_worktree("TEST-1", SHA)
    ready.zellij.close_tab.assert_not_awaited()
    ready.git.cleanup_worktree.assert_awaited_once()
    with ready.db.session() as session:
        agent = ready._load_task(session, "TEST-1").sessions[0]
        assert agent.status == "stopped"


@pytest.mark.asyncio
async def test_cleanup_fails_when_pane_cannot_be_confirmed_stopped(ready):
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=7,
                                zellij_pane_id="terminal_7", status="running"))
    ready.zellij.list_panes.return_value = [SimpleNamespace(pane_id="terminal_7", tab_id=7, exited=False)]
    ready.zellij.close_tab.side_effect = RuntimeError("stop failed")
    with pytest.raises(WorkflowError, match="Could not confirm"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_cleanup_rejects_unfinished_node_even_on_completed_task(ready):
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        task.nodes[0].state = "RUNNING"
    with pytest.raises(WorkflowError, match="still active"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_cleanup_fails_closed_when_zellij_unavailable(ready):
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=7,
                                zellij_pane_id="terminal_7", status="running"))
    ready.zellij.session_exists.side_effect = OSError("unavailable")
    with pytest.raises(WorkflowError, match="Cannot verify agents"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_git_refusal_is_actionable_and_does_not_record_success(ready):
    ready.git.cleanup_worktree.side_effect = GitError("dirty worktree")
    with pytest.raises(WorkflowError, match="dirty worktree"):
        await ready.cleanup_worktree("TEST-1", SHA)
    assert not any("Worktree cleaned up" in event["message"] for event in ready.snapshot("TEST-1")["events"])


@pytest.fixture
def stop_processes(monkeypatch):
    stop = AsyncMock(return_value=[WorktreeProcess(27500, "claude.exe", "D:\worktrees\TEST-1\Admin\\")])
    monkeypatch.setattr("agentflow.workflow.engine.stop_processes_in", stop)
    return stop


@pytest.mark.asyncio
async def test_cleanup_kills_processes_holding_the_worktree_then_retries(ready, stop_processes):
    """A terminal the human opened in the worktree (outside the task's zellij session) keeps
    Windows from deleting it; cleanup kills it and tries again, and says what it killed."""
    ready.git.cleanup_worktree.side_effect = [WorktreeBusy("Could not remove leftover worktree directory"), []]
    await ready.cleanup_worktree("TEST-1", SHA)
    stop_processes.assert_awaited_once_with(str(ready.config.worktree_dir / "TEST-1"))
    assert ready.git.cleanup_worktree.await_count == 2
    messages = [event["message"] for event in ready.snapshot("TEST-1")["events"]]
    assert any("Closed 1 process(es)" in message and "claude.exe (27500)" in message for message in messages)


@pytest.mark.asyncio
async def test_cleanup_records_killed_processes_even_if_the_retry_fails(ready, stop_processes):
    ready.git.cleanup_worktree.side_effect = WorktreeBusy("Could not remove leftover worktree directory")
    with pytest.raises(WorkflowError, match="Could not remove"):
        await ready.cleanup_worktree("TEST-1", SHA)
    assert any("claude.exe (27500)" in event["message"] for event in ready.snapshot("TEST-1")["events"])


@pytest.mark.asyncio
async def test_cleanup_kills_nothing_when_git_refuses_the_branch(ready, stop_processes):
    """Processes are only killed once the branch has passed cleanup's checks."""
    ready.git.cleanup_worktree.side_effect = GitError("Task branch changed since final review")
    with pytest.raises(WorkflowError, match="changed"):
        await ready.cleanup_worktree("TEST-1", SHA)
    stop_processes.assert_not_awaited()


@pytest.mark.asyncio
async def test_cleanup_tags_an_unverified_branch_and_does_not_retry(ready, stop_processes):
    """Merged into a local branch: the refusal carries a code the UI offers force for, and isn't
    retried (waiting can't make the branch reviewed or merged)."""
    ready.git.cleanup_worktree.side_effect = BranchUnverified()
    with pytest.raises(WorkflowError) as raised:
        await ready.cleanup_worktree("TEST-1", SHA)
    assert (raised.value.status_code, raised.value.code) == (409, WorkflowError.BRANCH_UNVERIFIED)
    ready.git.cleanup_worktree.assert_awaited_once()
    stop_processes.assert_not_awaited()


def failure_logs(engine):
    return [e for e in engine.snapshot("TEST-1")["events"] if e["message"].startswith("Worktree cleanup failed")]


@pytest.mark.asyncio
async def test_a_failed_cleanup_is_logged_and_becomes_the_status_message(ready):
    """A refused cleanup used to leave nothing but a toast: it is now on the task, as its status
    message (which the task page shows) and a warning event with the same text."""
    ready.git.cleanup_worktree.side_effect = GitError("dirty worktree")
    with pytest.raises(WorkflowError, match="dirty worktree"):
        await ready.cleanup_worktree("TEST-1", SHA)
    snapshot = ready.snapshot("TEST-1")
    expected = "Worktree cleanup failed while removing the worktree: dirty worktree"
    assert snapshot["statusMessage"] == expected
    [logged] = failure_logs(ready)
    assert (logged["message"], logged["level"]) == (expected, "warning")
    assert logged["data"]["step"] == "removing the worktree"
    assert logged["data"]["force"] is False


@pytest.mark.asyncio
async def test_each_failed_attempt_is_logged_and_a_success_replaces_the_message(ready):
    ready.git.cleanup_worktree.side_effect = GitError("dirty worktree")
    for _ in range(2):
        with pytest.raises(WorkflowError):
            await ready.cleanup_worktree("TEST-1", SHA)
    assert len(failure_logs(ready)) == 2
    ready.git.cleanup_worktree.side_effect = None
    ready.git.cleanup_worktree.return_value = []
    result = await ready.cleanup_worktree("TEST-1", SHA)
    assert result["statusMessage"].startswith("Worktree cleaned up")


@pytest.mark.asyncio
async def test_failures_before_the_removal_are_logged_too(ready):
    with ready.db.session() as session:
        ready._load_task(session, "TEST-1").nodes[0].state = "RUNNING"
    with pytest.raises(WorkflowError, match="still active"):
        await ready.cleanup_worktree("TEST-1", SHA)
    [logged] = failure_logs(ready)
    assert logged["data"]["step"] == "stopping agent tabs"
    with ready.db.session() as session:
        ready._load_task(session, "TEST-1").nodes[0].state = "COMPLETED"
    ready.zellij.kill_session.side_effect = ZellijError("session stuck")
    with pytest.raises(WorkflowError, match="Zellij session"):
        await ready.cleanup_worktree("TEST-1", SHA)
    assert failure_logs(ready)[-1]["data"]["step"] == "closing the terminal session"


@pytest.mark.asyncio
async def test_a_rejected_request_is_not_logged_as_a_failure(ready):
    """Requests that never started a cleanup (stale checkpoint) are refusals, not failures."""
    with pytest.raises(WorkflowError, match="checkpoint changed"):
        await ready.cleanup_worktree("TEST-1", "b" * 40)
    assert failure_logs(ready) == []


@pytest.mark.asyncio
async def test_a_busy_directory_names_what_still_holds_it(ready, monkeypatch):
    """An editor with the folder open holds a directory without working in it, so killing by
    working directory can't free it: the error names who to close instead."""
    monkeypatch.setattr("agentflow.workflow.engine.stop_processes_in", AsyncMock(return_value=[]))
    holders = AsyncMock(return_value=[WorktreeHolder(31944, "Code.exe")])
    monkeypatch.setattr("agentflow.workflow.engine.find_holders", holders)
    ready.git.cleanup_worktree.side_effect = WorktreeBusy("Could not remove leftover worktree directory")
    with pytest.raises(WorkflowError) as raised:
        await ready.cleanup_worktree("TEST-1", SHA)
    assert "still open in Code.exe (31944): close it and try again" in str(raised.value)
    holders.assert_awaited_once_with(str(ready.config.worktree_dir / "TEST-1"))
    [logged] = failure_logs(ready)
    assert "Code.exe (31944)" in logged["message"]
    assert logged["data"]["holders"] == [{"pid": 31944, "name": "Code.exe"}]


@pytest.mark.asyncio
async def test_holders_are_only_looked_up_for_a_busy_directory(ready, monkeypatch):
    holders = AsyncMock(return_value=[WorktreeHolder(31944, "Code.exe")])
    monkeypatch.setattr("agentflow.workflow.engine.find_holders", holders)
    ready.git.cleanup_worktree.side_effect = BranchUnverified()
    with pytest.raises(WorkflowError) as raised:
        await ready.cleanup_worktree("TEST-1", SHA)
    assert "Code.exe" not in str(raised.value)
    assert raised.value.code == WorkflowError.BRANCH_UNVERIFIED
    holders.assert_not_awaited()
    assert failure_logs(ready)[0]["data"]["code"] == WorkflowError.BRANCH_UNVERIFIED


@pytest.mark.asyncio
async def test_forced_cleanup_passes_force_to_git_and_logs_the_unverified_tip(ready):
    ready.git.branch_sha = AsyncMock(return_value="d" * 40)
    snapshot = await ready.cleanup_worktree("TEST-1", SHA, force=True)
    assert ready.git.cleanup_worktree.await_args.kwargs["force"] is True
    forced = next(e for e in snapshot["events"] if "forced" in e["message"])
    assert forced["level"] == "warning"
    assert forced["data"]["branchSha"] == "d" * 40


@pytest.mark.asyncio
async def test_cleanup_kills_the_task_zellij_session_before_removing_the_worktree(ready):
    """D2: closing tracked agent tabs alone isn't enough on Windows - the Tests/Shell
    convenience tabs created at task creation also have their cwd inside the worktree and
    keep it locked. Cleanup must kill the whole task session before calling git."""
    await ready.cleanup_worktree("TEST-1", SHA)
    ready.zellij.kill_session.assert_awaited_once_with("agentflow-TEST-1")
    ready.git.cleanup_worktree.assert_awaited_once()


@pytest.mark.asyncio
async def test_cleanup_fails_closed_when_session_cannot_be_killed(ready):
    ready.zellij.kill_session.side_effect = ZellijError("session stuck")
    with pytest.raises(WorkflowError, match="Zellij session"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_cleanup_passes_the_configured_worktree_dir_to_git(ready):
    await ready.cleanup_worktree("TEST-1", SHA)
    kwargs = ready.git.cleanup_worktree.await_args.kwargs
    assert kwargs["worktree_dir"] == ready.config.worktree_dir
    assert kwargs["merged_head_sha"] is None


@pytest.mark.asyncio
async def test_cleanup_passes_the_merged_pr_head_to_git(ready):
    with ready.db.session() as session:
        ready._load_task(session, "TEST-1").merged_head_sha = "c" * 40
    await ready.cleanup_worktree("TEST-1", SHA)
    assert ready.git.cleanup_worktree.await_args.kwargs["merged_head_sha"] == "c" * 40


@pytest.mark.asyncio
async def test_cleanup_commits_agent_stopped_status_even_if_git_then_fails(ready):
    """D4: `_check_cleanup_agents` closing a tab is a real, irreversible side effect,
    so the DB row it updates must be committed before the git step - otherwise a later
    GitError rolls the whole `with self.db.session()` block back and the row is left
    'running' with a pane id that no longer exists."""
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=7,
                                zellij_pane_id="terminal_7", status="running"))
    ready.zellij.list_panes.return_value = [SimpleNamespace(pane_id="terminal_7", tab_id=7, exited=False)]
    ready.git.cleanup_worktree.side_effect = GitError("permission denied")
    with pytest.raises(WorkflowError, match="permission denied"):
        await ready.cleanup_worktree("TEST-1", SHA)
    ready.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)
    with ready.db.session() as session:
        agent = ready._load_task(session, "TEST-1").sessions[0]
        assert agent.status == "stopped"


@pytest.mark.asyncio
async def test_cleanup_warns_about_discarded_files_instead_of_blocking(ready):
    ready.git.cleanup_worktree.return_value = [" M src/foo.py", "?? scratch.txt", "!! build/out.js"]
    result = await ready.cleanup_worktree("TEST-1", SHA)
    assert "discarded 3" in result["statusMessage"]
    assert "src/foo.py" in result["statusMessage"]
    events = ready.snapshot("TEST-1")["events"]
    assert any(e["level"] == "warning" and "discarded 3" in e["message"] for e in events)


@pytest.mark.asyncio
async def test_worktree_cleanup_preview_lists_dirt_and_open_panes(ready):
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        session.add(AgentSession(task_id=task.id, role="architect", harness="claude",
                                zellij_tab_name="Architect", zellij_tab_id=7,
                                zellij_pane_id="terminal_7", status="running"))
    ready.git.dirty_paths = AsyncMock(return_value=[" M src/foo.py", "?? scratch.txt"])
    ready.zellij.list_panes.return_value = [SimpleNamespace(pane_id="terminal_7", tab_id=7, exited=False)]
    worktree = Path(ready.config.worktree_dir / "TEST-1")
    worktree.mkdir(parents=True, exist_ok=True)
    (worktree / ".git").write_text("gitdir: elsewhere\n")  # a linked worktree's marker
    preview = await ready.worktree_cleanup_preview("TEST-1")
    assert preview["dirtyPaths"] == ["src/foo.py", "scratch.txt"]
    assert preview["openAgentPanes"] == ["Architect"]
    assert preview["workerWorktrees"] == []
    assert preview["sessionWillBeClosed"] is True
    assert preview["otherOpenTabs"] == []


@pytest.mark.asyncio
async def test_worktree_cleanup_preview_does_not_ask_git_about_a_leftover_directory(ready):
    """A directory whose `.git` is gone isn't a checkout: git would answer for whatever repository
    encloses the data directory and the preview would list that repository's files as discarded."""
    ready.git.dirty_paths = AsyncMock(return_value=[" M README.md"])
    Path(ready.config.worktree_dir / "TEST-1").mkdir(parents=True, exist_ok=True)
    preview = await ready.worktree_cleanup_preview("TEST-1")
    assert preview["dirtyPaths"] == []
    ready.git.dirty_paths.assert_not_awaited()


@pytest.mark.asyncio
async def test_worktree_cleanup_preview_reports_non_agent_tabs_that_will_close(ready):
    """D2: cleanup closes the whole task session, not just tracked agent tabs - e.g. the
    Tests/Shell convenience tabs created alongside the task. The preview must surface that
    so the confirm dialog can warn about it, not just about agent panes."""
    ready.zellij.list_panes.return_value = [
        SimpleNamespace(pane_id="terminal_1", tab_id=1, tab_name="Tests", exited=False),
        SimpleNamespace(pane_id="terminal_2", tab_id=2, tab_name="Shell", exited=False),
    ]
    preview = await ready.worktree_cleanup_preview("TEST-1")
    assert preview["sessionWillBeClosed"] is True
    assert preview["otherOpenTabs"] == ["Shell", "Tests"]
    assert preview["openAgentPanes"] == []


@pytest.mark.asyncio
async def test_worktree_cleanup_preview_no_session_reports_not_closing(ready):
    ready.zellij.session_exists.return_value = False
    preview = await ready.worktree_cleanup_preview("TEST-1")
    assert preview["sessionWillBeClosed"] is False
    assert preview["otherOpenTabs"] == []


@pytest.mark.asyncio
async def test_cleanup_also_removes_worker_worktrees(ready):
    worker_path = str(ready.config.worktree_dir / "TEST-1-worker-a")
    add_worker_item(ready, worker_path)
    result = await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.discard_worktree.assert_awaited_once()
    assert ready.git.discard_worktree.await_args.args[1] == worker_path
    assert "1 worker worktree" in result["statusMessage"]


@pytest.mark.asyncio
async def test_cleanup_reports_worker_worktree_failure_without_blocking_main_cleanup(ready):
    worker_path = str(ready.config.worktree_dir / "TEST-1-worker-a")
    add_worker_item(ready, worker_path)
    ready.git.discard_worktree.side_effect = GitError("Filename too long")
    result = await ready.cleanup_worktree("TEST-1", SHA)
    ready.git.cleanup_worktree.assert_awaited_once()  # the main worktree cleanup still ran and succeeded
    assert "Could not remove 1 worker worktree" in result["statusMessage"]
    assert "Filename too long" in result["statusMessage"]


@pytest.mark.asyncio
async def test_worktree_cleanup_preview_lists_existing_worker_worktrees(ready):
    worker_path = ready.config.worktree_dir / "TEST-1-worker-a"
    worker_path.mkdir(parents=True, exist_ok=True)
    add_worker_item(ready, str(worker_path))
    preview = await ready.worktree_cleanup_preview("TEST-1")
    assert preview["workerWorktrees"] == [str(worker_path)]


def test_snapshot_skips_worktree_existence_check_before_merged(engine):
    snapshot = engine.snapshot("TEST-1")
    assert snapshot["status"] != "MERGED"
    assert snapshot["worktreeExists"] is None
    assert snapshot["workerWorktreeCount"] is None


def test_snapshot_reports_worktree_existence_once_merged(ready):
    # ready already marks the task Merged; nothing on disk yet, so both read as empty.
    snapshot = ready.snapshot("TEST-1")
    assert snapshot["worktreeExists"] is False
    assert snapshot["workerWorktreeCount"] == 0
    Path(ready.config.worktree_dir / "TEST-1").mkdir(parents=True, exist_ok=True)
    worker_path = ready.config.worktree_dir / "TEST-1-worker-a"
    worker_path.mkdir(parents=True, exist_ok=True)
    add_worker_item(ready, str(worker_path))
    snapshot = ready.snapshot("TEST-1")
    assert snapshot["worktreeExists"] is True
    assert snapshot["workerWorktreeCount"] == 1



@pytest.mark.asyncio
async def test_task_list_reports_worktree_existence_per_merged_task(ready):
    with ready.db.session() as session:
        first = ready._load_task(session, "TEST-1")
        session.add(Task(jira_key="TEST-2", jira_title="Other", repository_path=first.repository_path,
                         repository_root=first.repository_root, base_branch="main", branch_name="agent/TEST-2",
                         worktree_path=str(ready.config.worktree_dir / "TEST-2"),
                         zellij_session_name="agentflow-TEST-2", task_dir=str(ready.config.task_dir / "TEST-2"),
                         current_sha=SHA, status=TaskStatus.MERGED))
    Path(ready.config.worktree_dir / "TEST-2").mkdir(parents=True, exist_ok=True)
    worker_path = ready.config.worktree_dir / "TEST-1-worker-a"
    worker_path.mkdir(parents=True, exist_ok=True)
    add_worker_item(ready, str(worker_path))

    rows = {row["jiraKey"]: row for row in await ready.list_tasks()}
    assert (rows["TEST-1"]["worktreeExists"], rows["TEST-1"]["workerWorktreeCount"]) == (False, 1)
    assert (rows["TEST-2"]["worktreeExists"], rows["TEST-2"]["workerWorktreeCount"]) == (True, 0)

    with ready.db.session() as session:
        ready._load_task(session, "TEST-1").status = TaskStatus.PR_CREATED
    rows = {row["jiraKey"]: row for row in await ready.list_tasks()}
    assert rows["TEST-1"]["worktreeExists"] is None
    assert rows["TEST-1"]["workerWorktreeCount"] is None

def test_cleanup_endpoint_requires_human_auth_origin_and_merged_status(client, ready):
    url = "/api/tasks/TEST-1/worktree/cleanup"
    payload = {"expectedSha": SHA}
    assert client.post(url, json=payload).status_code == 401
    assert client.post(url, json=payload, headers={"Authorization": "Bearer any"}).status_code == 403
    origin = {"Origin": "http://127.0.0.1"}
    code = client.app.state.human.pairing_code
    assert client.post("/api/auth/pair", json={"code": code}, headers=origin).status_code == 200
    assert client.post(url, json=payload, headers={"Origin": "http://evil.invalid"}).status_code == 403
    assert client.post(url, json={}, headers=origin).status_code == 422
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        task.status = TaskStatus.READY_FOR_PR
    assert client.post(url, json=payload, headers=origin).status_code == 409
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        task.status = TaskStatus.MERGED
    result = client.post(url, json=payload, headers=origin)
    assert result.status_code == 200
    assert result.json()["status"] == "MERGED"
    assert "Worktree cleaned up" in result.json()["statusMessage"]
    ready.git.cleanup_worktree.assert_awaited_once()
    assert ready.git.cleanup_worktree.await_args.kwargs["force"] is False
    ready.git.cleanup_worktree.side_effect = [BranchUnverified(), []]
    ready.git.branch_sha = AsyncMock(return_value="d" * 40)
    result = client.post(url, json=payload, headers=origin)
    assert (result.status_code, result.json().get("code")) == (409, "branch_unverified")
    result = client.post(url, json={**payload, "force": True}, headers=origin)
    assert result.status_code == 200
    assert ready.git.cleanup_worktree.await_args.kwargs["force"] is True
