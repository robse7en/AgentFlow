import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from agentflow.api.security import digest
from agentflow.config import Config
from agentflow.database.database import Database
from agentflow.database.models import AgentAccess, AgentQuestion, AgentSession, NodeRun, ReviewFinding, Task, WorkflowEvent, WorkflowNode
from agentflow.harnesses.base import LaunchSpec
from agentflow.main import create_app
from agentflow.services.events import Broadcaster
from agentflow.services.workspace import TaskWorkspace
from agentflow.workflow import default_workflow as wf
from agentflow.workflow import engine as engine_module
from agentflow.workflow.engine import WorkflowEngine, WorkflowError
from agentflow.workflow.models import AgentOverride, Finding
from agentflow.workflow.usage import UsageReport
from agentflow.workflow.states import NodeState, TaskStatus
from agentflow.workflow import assurance
from agentflow.database.models import RequirementsApproval

SHA = "a" * 40


@pytest.fixture
def engine(tmp_path):
    config = Config.model_validate({
        "app": {"dataDirectory": str(tmp_path), "databasePath": str(tmp_path / "test.db")},
        "workspace": {"taskDirectory": str(tmp_path / "tasks"), "worktreeDirectory": str(tmp_path / "trees")},
        # The `client` fixture runs the real lifespan; keep its PR sync from calling a real `gh`.
        "github": {"prSync": False},
    })
    db = Database(config.database_file)
    db.create_all()
    git = SimpleNamespace(head_sha=AsyncMock(return_value=SHA), is_dirty=AsyncMock(return_value=False),
                          dirty_paths=AsyncMock(return_value=[]), reset_hard=AsyncMock())
    zellij = SimpleNamespace(
        session_exists=AsyncMock(return_value=True), list_panes=AsyncMock(return_value=[]),
        close_tab=AsyncMock(), pane_state=AsyncMock(return_value=None), write_line=AsyncMock(),
        require_subscribe_support=AsyncMock(), kill_session=AsyncMock(),
        attach_command=lambda name: name, web_url=lambda name: None,
    )
    registry = SimpleNamespace(capabilities={}, is_usable=lambda key: True,
                               get=lambda key: SimpleNamespace(classify_screen=lambda viewport: "unknown"))
    result = WorkflowEngine(config, db, git, zellij, registry, Broadcaster(), "http://127.0.0.1:8765")
    with db.session() as session:
        task = Task(jira_key="TEST-1", repository_path=str(tmp_path), repository_root=str(tmp_path),
                    base_branch="main", branch_name="agent/TEST-1", worktree_path=str(tmp_path / "tree"),
                    zellij_session_name="agentflow-TEST-1", task_dir=str(tmp_path / "tasks" / "TEST-1"),
                    current_sha=SHA, max_review_iterations=1)
        session.add(task)
        session.flush()
        for node in result.spec.nodes:
            # This fixture represents an existing task created before worker orchestration.
            # New-task orchestration has its own fixture and lifecycle tests.
            if node.key == wf.ORCHESTRATOR:
                continue
            session.add(WorkflowNode(task_id=task.id, key=node.key, kind=node.kind, label=node.label,
                role=node.role, execution_mode="manual", state="PENDING", iteration=1, checkpoint_sha=SHA))
        workspace = result.workspace(task)
        workspace.create({}, None)
        for name in ("requirements.md", "acceptance-criteria.md", "implementation-plan.md"):
            (workspace.requirements_dir / name).write_text("Approved scope with required behavior and tests")
        assurance.approve_requirements(session, task, workspace)
    yield result
    db.engine.dispose()


def add_run(engine, key, state="RUNNING", result=None):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, key)
        node.state = state
        count = len(session.scalars(select(NodeRun).where(NodeRun.node_id == node.id)).all()) + 1
        run = NodeRun(task_id=task.id, node_id=node.id, run_number=count, state=state,
                      expected_sha=SHA, iteration=task.review_iteration, result=result)
        session.add(run)
        session.flush()
        return run.id


def task_state(engine, key):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        return engine._node(task, key).state


def requirements_digest(engine):
    """The digest of the requirements as the UI would display them just before approval."""
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        return assurance.documents_digest(engine.workspace(task).requirement_files())


@pytest.mark.asyncio
async def test_stale_completion_and_failure_cannot_change_retry(engine):
    old = add_run(engine, wf.IMPLEMENTER, "FAILED")
    add_run(engine, wf.IMPLEMENTER)
    with pytest.raises(WorkflowError, match="Stale"):
        await engine.agent_complete("TEST-1", "implementer", old, {"status": "completed"})
    with pytest.raises(WorkflowError, match="Stale"):
        await engine.agent_fail("TEST-1", "implementer", old, "late failure")
    assert task_state(engine, wf.IMPLEMENTER) == "RUNNING"
    assert engine._lock("test-1") is engine._lock("TEST-1")


@pytest.mark.asyncio
async def test_verification_does_not_hold_the_database_writer_lock(engine, monkeypatch):
    """Independent verification can run for minutes. Another task's flowctl signal (or a UI
    action) landing in that window must not stall behind this task's pending SQLite writes."""
    run = add_run(engine, wf.IMPLEMENTER)
    with engine.db.session() as session:
        session.get(NodeRun, run).harness = "claude"
    engine.config.harnesses["claude"].usage_command = ["usage-collector"]
    engine.git.checkpoint = AsyncMock(return_value=(SHA, True))
    concurrent_writes: list[str] = []

    def another_request_writes(message):
        with engine.db.session() as session:  # a separate connection, as any other request uses
            task = engine._load_task(session, "TEST-1")
            session.add(WorkflowEvent(task_id=task.id, message=message))
        concurrent_writes.append(message)

    async def collect_usage_while_another_writer_arrives(argv, cwd, run_id, session_id):
        another_request_writes("unrelated write during usage collection")
        return UsageReport(inputTokens=10, outputTokens=5, model="claude-x", source="test")

    async def verify_while_another_writer_arrives(commands, cwd, sha):
        another_request_writes("unrelated write during verification")
        return {"sha": sha, "commands": [], "passed": True}

    monkeypatch.setattr(engine_module, "collect_usage", collect_usage_while_another_writer_arrives)
    monkeypatch.setattr(assurance, "verify_commands", verify_while_another_writer_arrives)
    response = await engine.agent_complete("TEST-1", "implementer", run,
        {"status": "completed", "tests": {"executed": 1, "passed": 1, "failed": 0}})
    assert response["ok"], response
    assert concurrent_writes == ["unrelated write during usage collection", "unrelated write during verification"]
    snap = engine.snapshot("TEST-1")
    assert task_state(engine, wf.IMPLEMENTER) == "COMPLETED"
    messages = [e["message"] for e in snap["events"]]
    assert all(m in messages for m in concurrent_writes)
    evidence = next(e for e in snap["executionEvidence"] if e["runId"] == run)
    assert evidence["verification"]["passed"] is True
    assert evidence["usage"]["totalTokens"] == 15


@pytest.mark.asyncio
async def test_mutations_build_the_snapshot_once(engine, monkeypatch):
    """A snapshot reads requirement files, hashes them and runs several queries; the broadcast
    and the HTTP response must share one build rather than each computing their own."""
    builds = 0
    original = engine.snapshot

    def counting_snapshot(jira_key):
        nonlocal builds
        builds += 1
        return original(jira_key)

    monkeypatch.setattr(engine, "snapshot", counting_snapshot)
    broadcast = AsyncMock()
    monkeypatch.setattr(engine.broadcaster, "broadcast", broadcast)
    returned = await engine.set_node_override("TEST-1", wf.IMPLEMENTER, AgentOverride(model="x"))
    assert builds == 1
    assert broadcast.await_args.args[0]["task"] is returned


@pytest.mark.asyncio
async def test_wrong_role_and_terminal_failure_rejected(engine):
    run = add_run(engine, wf.IMPLEMENTER, "COMPLETED")
    with pytest.raises(WorkflowError):
        await engine.agent_fail("TEST-1", "architect", run, "bad")
    with pytest.raises(WorkflowError):
        await engine.agent_fail("TEST-1", "implementer", run, "bad")


@pytest.mark.asyncio
async def test_question_answer_uses_only_its_active_tracked_pane(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.agent_question("TEST-1", "implementer", run_id, "Choose a rollout", ["Gradual", "All at once"])
    question = engine.snapshot("TEST-1")["questions"][0]

    snapshot = await engine.answer_question("TEST-1", question["uid"], "Gradual")
    assert snapshot["questions"][0]["answer"] == "Gradual"
    assert snapshot["questions"][0]["answered"] is True
    assert task_state(engine, wf.IMPLEMENTER) == NodeState.RUNNING
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "Gradual")

    await engine.answer_question("TEST-1", question["uid"], "Gradual")
    engine.zellij.write_line.assert_awaited_once()
    with pytest.raises(WorkflowError, match="already been answered"):
        await engine.answer_question("TEST-1", question["uid"], "All at once")


@pytest.mark.asyncio
async def test_engine_validates_answers_for_callers_that_bypass_http(engine):
    """The answer is typed into the agent's pane and followed by Enter, so the engine itself
    refuses anything but one printable line, whoever the caller is."""
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.agent_question("TEST-1", "implementer", run_id, "Choose a rollout", ["Gradual"])
    question = engine.snapshot("TEST-1")["questions"][0]

    for bad in ("Gradual\nrm -rf .", "Gradual\r", "Gradual\x1b[A", "   ", "x" * 4001):
        with pytest.raises(WorkflowError) as raised:
            await engine.answer_question("TEST-1", question["uid"], bad)
        assert raised.value.status_code == 422
    engine.zellij.write_line.assert_not_awaited()
    assert task_state(engine, wf.IMPLEMENTER) == NodeState.WAITING_FOR_USER

    snapshot = await engine.answer_question("TEST-1", question["uid"], "  Gradual  ")
    assert snapshot["questions"][0]["answer"] == "Gradual"
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "Gradual")


@pytest.mark.asyncio
async def test_batched_questions_only_resume_once_every_reply_is_in(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.agent_question("TEST-1", "implementer", run_id, "Choose a rollout", ["Gradual", "All at once"])
    await engine.agent_question("TEST-1", "implementer", run_id, "Which region first?")
    first, second = engine.snapshot("TEST-1")["questions"]

    await engine.answer_question("TEST-1", first["uid"], "Gradual")
    assert task_state(engine, wf.IMPLEMENTER) == NodeState.WAITING_FOR_USER

    snapshot = await engine.answer_question("TEST-1", second["uid"], "APAC")
    assert task_state(engine, wf.IMPLEMENTER) == NodeState.RUNNING
    with engine.db.session() as session:
        assert session.get(NodeRun, run_id).state == NodeState.RUNNING

    def event_count():
        with engine.db.session() as session:
            return len(session.scalars(select(WorkflowEvent)).all())

    before = event_count()
    result = await engine.agent_running("TEST-1", "implementer", run_id)
    assert result["state"] == NodeState.RUNNING
    assert event_count() == before  # the agent's own confirmation is a no-op once already resumed


@pytest.mark.asyncio
async def test_question_from_a_superseded_run_is_not_actionable(engine):
    run_id = add_run(engine, wf.IMPLEMENTER, "WAITING_FOR_USER")
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.agent_question("TEST-1", "implementer", run_id, "Choose a rollout", ["Gradual", "All at once"])

    # Backend restarted; the human retried the interrupted node instead of answering,
    # leaving the question's run superseded without ever being answered.
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.state = NodeState.FAILED
    add_run(engine, wf.IMPLEMENTER, "RUNNING")

    question = engine.snapshot("TEST-1")["questions"][0]
    assert question["answered"] is False
    assert question["actionable"] is False

    with pytest.raises(WorkflowError, match="Stale run"):
        await engine.answer_question("TEST-1", question["uid"], "Gradual")


@pytest.mark.asyncio
async def test_terminal_view_resolves_only_the_current_active_agent_pane(engine):
    run_id = add_run(engine, wf.IMPLEMENTER, "WAITING_FOR_USER")
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)

    assert await engine.terminal_target("TEST-1", "implementer") == ("agentflow-TEST-1", "terminal_7")
    with pytest.raises(WorkflowError, match="active pane"):
        await engine.terminal_target("TEST-1", "code_reviewer")


def test_human_question_reply_requires_pairing_and_targets_server_tracked_pane(client, engine):
    from agentflow.workflow import default_workflow as workflow

    run_id = add_run(engine, workflow.IMPLEMENTER, "WAITING_FOR_USER")
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        question = AgentQuestion(task_id=task.id, node_run_id=run_id, role="implementer", message="Ship it?")
        session.add(question)
        session.flush()
        question_uid = question.uid

    path = f"/api/tasks/TEST-1/questions/{question_uid}/answer"
    origin = {"Origin": "http://127.0.0.1"}
    assert client.post(path, json={"answer": "Yes"}, headers=origin).status_code == 401
    client.post("/api/auth/pair", json={"code": client.app.state.human.pairing_code}, headers=origin).raise_for_status()
    response = client.post(path, json={"answer": "Yes"}, headers=origin)
    assert response.status_code == 200
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "Yes")


@pytest.mark.asyncio
async def test_changes_required_without_blocking_findings_passes(engine):
    code = add_run(engine, wf.CODE_REVIEW)
    security = add_run(engine, wf.SECURITY_REVIEW)
    await engine.agent_complete("TEST-1", "code_reviewer", code,
        {"reviewedSha": SHA, "verdict": "changes_required",
         "findings": [{"id": "LOW", "title": "Naming", "severity": "low", "blocking": False}]})
    await engine.agent_complete("TEST-1", "security_reviewer", security,
                                {"reviewedSha": SHA, "verdict": "approved"})
    snapshot = engine.snapshot("TEST-1")
    assert snapshot["status"] != "NEEDS_HUMAN_REVIEW"
    assert [f["blocking"] for f in snapshot["findings"]] == [False]
    assert task_state(engine, wf.CONSOLIDATE) == "COMPLETED"


@pytest.mark.asyncio
async def test_prefix_sha_rejected(engine):
    run = add_run(engine, wf.CODE_REVIEW)
    await engine.agent_complete("TEST-1", "code_reviewer", run, {"reviewedSha": "a"})
    assert task_state(engine, wf.CODE_REVIEW) == "FAILED"


@pytest.mark.asyncio
async def test_incremental_findings_not_lost(engine, tmp_path):
    run_id = add_run(engine, wf.CODE_REVIEW)
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.run_dir = str(tmp_path / "review")
    await engine.agent_finding("TEST-1", "code_reviewer", run_id,
                               Finding(id="BLOCK", title="Security flaw", severity="high"))
    await engine.agent_complete("TEST-1", "code_reviewer", run_id,
        {"reviewedSha": SHA, "findings": [{"id": "LOW", "title": "Naming", "severity": "low"}]})
    assert len(engine.snapshot("TEST-1")["findings"]) == 2


@pytest.mark.asyncio
async def test_invalid_result_failure_is_persisted(engine):
    run = add_run(engine, wf.CODE_REVIEW)
    response = await engine.agent_complete("TEST-1", "code_reviewer", run, {"verdict": "nonsense"})
    assert task_state(engine, wf.CODE_REVIEW) == "FAILED"
    assert response["ok"] is False
    assert response["error"]


@pytest.mark.asyncio
async def test_unverified_assessment_requires_reason_and_records_evidence(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run,
        {"status": "completed", "requirementsVerified": False, "overallRisk": "HIGH"})
    assert task_state(engine, wf.READY_FOR_PR) == "WAITING_FOR_APPROVAL"
    with pytest.raises(WorkflowError, match="Override reason"):
        await engine.approve("TEST-1", wf.READY_FOR_PR, expected_sha=SHA)
    await engine.approve("TEST-1", wf.READY_FOR_PR, reason="Reviewed manually; accept these risks", expected_sha=SHA)
    snap = engine.snapshot("TEST-1")
    assert snap["status"] == "READY_FOR_PR"
    assert snap["assessment"]["requirementsVerified"] is False
    event = next(e for e in snap["events"] if e["message"] == "Human overrode final blockers")
    assert event["data"]["sha"] == SHA
    assert "Overall risk is high" in event["data"]["blockers"]


@pytest.mark.asyncio
async def test_dirty_worktree_requires_reason_then_can_be_overridden(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {})
    engine.git.dirty_paths.return_value = [" M some/generated/file.g.cs"]
    with pytest.raises(WorkflowError, match="Worktree changed"):
        await engine.approve("TEST-1", wf.READY_FOR_PR, expected_sha=SHA)
    await engine.approve("TEST-1", wf.READY_FOR_PR, "accept", SHA)
    snap = engine.snapshot("TEST-1")
    assert snap["status"] == "READY_FOR_PR"
    event = next(e for e in snap["events"] if e["message"] == "Human overrode final blockers")
    assert any("Worktree changed after review" in b for b in event["data"]["blockers"])


@pytest.mark.asyncio
async def test_request_requirements_changes_sends_architect_back_for_revision(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.max_requirements_revisions = 3
        engine._node(task, wf.REQUIREMENTS_GATE).state = NodeState.WAITING_FOR_APPROVAL
        engine._node(task, wf.ARCHITECT).iteration = 0

    snap = await engine.request_requirements_changes("TEST-1", "  Please cover the mobile case too  ")

    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        gate = engine._node(task, wf.REQUIREMENTS_GATE)
        architect = engine._node(task, wf.ARCHITECT)
        assert gate.state == NodeState.PENDING
        assert architect.state == NodeState.READY
        assert architect.iteration == 1
        feedback_path = engine.workspace(task).requirements_dir / "feedback-01.md"
        assert feedback_path.read_text(encoding="utf-8").strip() == "Please cover the mobile case too"
    gate_dto = next(n for n in snap["nodes"] if n["key"] == wf.REQUIREMENTS_GATE)
    assert gate_dto["state"] == "PENDING"
    assert snap["maxRequirementsRevisions"] == 3
    assert snap["requirementsFeedback"] == [
        {"iteration": 1, "feedback": "Please cover the mobile case too", "at": snap["requirementsFeedback"][0]["at"]}
    ]


@pytest.mark.asyncio
async def test_request_requirements_changes_requires_nonblank_feedback(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        engine._node(task, wf.REQUIREMENTS_GATE).state = NodeState.WAITING_FOR_APPROVAL
    with pytest.raises(WorkflowError, match="Feedback is required"):
        await engine.request_requirements_changes("TEST-1", "   ")


@pytest.mark.asyncio
async def test_request_requirements_changes_enforces_revision_cap(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.max_requirements_revisions = 1
        engine._node(task, wf.REQUIREMENTS_GATE).state = NodeState.WAITING_FOR_APPROVAL
        engine._node(task, wf.ARCHITECT).iteration = 1
    with pytest.raises(WorkflowError, match="Maximum requirement revisions reached"):
        await engine.request_requirements_changes("TEST-1", "one more pass please")


def test_drift_rejection_carries_a_machine_readable_code(client, engine):
    """The UI offers "Discard drift" on this error; it keys off `code`, not the message text."""
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        engine._node(task, wf.READY_FOR_PR).state = NodeState.WAITING_FOR_APPROVAL
    headers = {"Origin": "http://127.0.0.1"}
    client.post("/api/auth/pair", json={"code": client.app.state.human.pairing_code}, headers=headers).raise_for_status()

    engine.git.dirty_paths.return_value = [" M some/generated/file.g.cs"]
    response = client.post("/api/tasks/TEST-1/nodes/ready_for_pr/approve", json={"expectedSha": SHA}, headers=headers)
    assert response.status_code == 409
    body = response.json()
    assert body["code"] == "worktree_drift"
    assert "Worktree changed after review" in body["detail"]

    # Other blockers alone (no drift) get no code, so the discard offer can't appear wrongly.
    engine.git.dirty_paths.return_value = []
    response = client.post("/api/tasks/TEST-1/nodes/ready_for_pr/approve", json={"expectedSha": SHA}, headers=headers)
    assert response.status_code == 409
    assert "Override reason required" in response.json()["detail"]
    assert "code" not in response.json()


@pytest.mark.asyncio
async def test_new_commits_since_checkpoint_can_be_overridden_with_reason(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {})
    engine.git.head_sha.return_value = "c" * 40
    with pytest.raises(WorkflowError, match="Override reason required"):
        await engine.approve("TEST-1", wf.READY_FOR_PR, expected_sha=SHA)
    await engine.approve("TEST-1", wf.READY_FOR_PR, "accept", SHA)
    snap = engine.snapshot("TEST-1")
    assert snap["status"] == "READY_FOR_PR"
    event = next(e for e in snap["events"] if e["message"] == "Human overrode final blockers")
    assert any("new commit" in b for b in event["data"]["blockers"])


@pytest.mark.asyncio
async def test_stale_checkpoint_reference_cannot_be_overridden(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {})
    with pytest.raises(WorkflowError, match="Checkpoint changed"):
        await engine.approve("TEST-1", wf.READY_FOR_PR, "accept", "b" * 40)


@pytest.mark.asyncio
async def test_discard_worktree_drift_resets_tracked_changes_only(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {})
    engine.git.dirty_paths.return_value = [" M some/generated/file.g.cs"]
    await engine.discard_worktree_drift("TEST-1", SHA)
    engine.git.reset_hard.assert_awaited_once()
    assert engine.git.reset_hard.await_args.args[1] == SHA


@pytest.mark.asyncio
async def test_discard_worktree_drift_refuses_untracked_files(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {})
    engine.git.dirty_paths.return_value = ["?? some/new/file.txt"]
    with pytest.raises(WorkflowError, match="Untracked"):
        await engine.discard_worktree_drift("TEST-1", SHA)
    engine.git.reset_hard.assert_not_awaited()


@pytest.mark.asyncio
async def test_discard_worktree_drift_refuses_when_head_diverged(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {})
    engine.git.head_sha.return_value = "c" * 40
    with pytest.raises(WorkflowError, match="new commits"):
        await engine.discard_worktree_drift("TEST-1", SHA)
    engine.git.reset_hard.assert_not_awaited()


def add_session(engine, run_id):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        agent = AgentSession(task_id=task.id, role="implementer", harness="codex",
                             zellij_tab_name="Implementer", zellij_tab_id=7, zellij_pane_id="terminal_7",
                             current_run_id=run_id)
        session.add(agent)
        session.add(AgentAccess(task_id=task.id, run_id=run_id, role="implementer", token_hash=digest("test-token")))


@pytest.mark.asyncio
async def test_cancel_stops_tab_before_marking_cancelled(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.list_panes.return_value = [SimpleNamespace(tab_id=7)]
    await engine.cancel_task("TEST-1")
    engine.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)
    assert engine.snapshot("TEST-1")["status"] == "CANCELLED"
    with pytest.raises(WorkflowError):
        await engine.agent_fail("TEST-1", "implementer", run_id, "late")


@pytest.mark.asyncio
async def test_failed_stop_does_not_claim_cancellation(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.list_panes.return_value = [SimpleNamespace(tab_id=7)]
    engine.zellij.close_tab.side_effect = RuntimeError("stop failed")
    with pytest.raises(WorkflowError, match="Could not confirm"):
        await engine.cancel_task("TEST-1")
    assert engine.snapshot("TEST-1")["status"] == "ACTIVE"


@pytest.mark.asyncio
async def test_restart_requires_explicit_resume_without_new_run(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.recover()
    assert task_state(engine, wf.IMPLEMENTER) == "INTERRUPTED"
    with pytest.raises(WorkflowError):
        await engine.agent_complete("TEST-1", "implementer", run_id, {})
    await engine.recover_node("TEST-1", wf.IMPLEMENTER, retry=False)
    assert task_state(engine, wf.IMPLEMENTER) == "RUNNING"
    assert next(n for n in engine.snapshot("TEST-1")["nodes"] if n["key"] == wf.IMPLEMENTER)["lastRun"]["id"] == run_id


@pytest.mark.asyncio
async def test_recover_surfaces_a_completion_saved_to_disk_before_restart(engine, tmp_path):
    """flowctl persists the result next to credential.json before POSTing; if the backend
    never confirmed receipt, recover() must not silently apply it (that could run a git
    checkpoint/verification pass during startup) but should let a human Resume to submit it."""
    run_id = add_run(engine, wf.FINAL_ARCHITECT)
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "pending-complete.json").write_text(
        json.dumps({"role": "architect", "runId": run_id, "result": {"status": "completed"}}), encoding="utf-8")
    with engine.db.session() as session:
        session.get(NodeRun, run_id).run_dir = str(run_dir)

    await engine.recover()

    assert task_state(engine, wf.FINAL_ARCHITECT) == "INTERRUPTED"
    message = next(n for n in engine.snapshot("TEST-1")["nodes"] if n["key"] == wf.FINAL_ARCHITECT)["message"]
    assert "saved to disk" in message
    engine.zellij.list_panes.assert_not_awaited()  # no live-session probing needed once a result is on disk

    await engine.recover_node("TEST-1", wf.FINAL_ARCHITECT, retry=False)

    assert task_state(engine, wf.FINAL_ARCHITECT) == "COMPLETED"
    assert not (run_dir / "pending-complete.json").exists()


@pytest.mark.asyncio
async def test_human_resume_increments_task_iteration(engine):
    add_run(engine, wf.IMPLEMENTER, "READY")
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = TaskStatus.NEEDS_HUMAN_REVIEW
    engine._start_agent_run = AsyncMock()
    await engine.run_node("TEST-1", wf.IMPLEMENTER)
    assert engine.snapshot("TEST-1")["reviewIteration"] == 2


@pytest.fixture
def client(engine, monkeypatch):
    import agentflow.main as main
    monkeypatch.setattr(main, "build_state", AsyncMock(return_value=SimpleNamespace(db=engine.db, engine=engine)))
    monkeypatch.setattr(engine, "recover", AsyncMock())
    app = create_app(engine.config, config_file=engine.config.database_file.parent / 'config.json')
    with TestClient(app, base_url="http://127.0.0.1") as client:
        yield client


def test_agent_cannot_approve_or_read_other_task(client, engine):
    run = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run)
    headers = {"Authorization": "Bearer test-token"}
    assert client.post("/api/tasks/TEST-1/nodes/requirements_gate/approve", headers=headers).status_code == 403
    assert client.get("/api/tasks/TEST-2", headers=headers).status_code == 403
    assert client.post("/api/tasks/TEST-1/agent/running", headers=headers,
                       json={"role": "architect", "runId": run}).status_code == 403
    assert client.post("/api/tasks/TEST-1/agent/running", headers=headers,
                       json={"role": "implementer", "runId": run}).status_code == 200


def test_pairing_is_required_and_one_time(client):
    assert client.get("/api/tasks").status_code == 401
    code = client.app.state.human.pairing_code
    assert client.post("/api/auth/pair", json={"code": code}, headers={"Origin": "http://evil.invalid"}).status_code == 403
    headers = {"Origin": "http://127.0.0.1"}
    assert client.post("/api/auth/pair", json={"code": code}, headers=headers).status_code == 200
    assert client.get("/api/tasks").status_code == 200
    assert client.post("/api/auth/pair", json={"code": code}, headers=headers).status_code == 401
    assert client.post("/api/tasks/TEST-1/cancel", headers={"Origin": "http://evil.invalid"}).status_code == 403


def test_paired_session_survives_restart_and_stores_only_a_hash(client, engine, capsys):
    headers = {"Origin": "http://127.0.0.1"}
    code = client.app.state.human.pairing_code
    response = client.post("/api/auth/pair", json={"code": code}, headers=headers)
    token = response.cookies["agentflow_human"]
    assert "max-age" in response.headers["set-cookie"].lower()
    stored = (engine.config.data_dir / "browser-session").read_text()
    assert stored == digest(token) and token not in stored

    restarted = create_app(engine.config, config_file=engine.config.database_file.parent / "config.json")
    with TestClient(restarted, base_url="http://127.0.0.1") as fresh:
        assert f"#pair={restarted.state.human.pairing_code}" in capsys.readouterr().out
        fresh.cookies.set("agentflow_human", token)
        assert fresh.get("/api/tasks").status_code == 200
        # Pairing again replaces the session, signing out the old cookie.
        fresh.cookies.clear()
        fresh.post("/api/auth/pair", json={"code": restarted.state.human.pairing_code}, headers=headers).raise_for_status()
        assert fresh.get("/api/tasks").status_code == 200
        fresh.cookies.set("agentflow_human", token)
        assert fresh.get("/api/tasks").status_code == 401


def test_file_traversal_is_rejected(client):
    assert client.get("/%2e%2e%5c%2e%2e%5cREADME.md").status_code == 404
    assert client.get("/%2e%2e%2f%2e%2e%2fREADME.md").status_code == 404


@pytest.mark.asyncio
async def test_full_workflow_with_retry_and_persistent_sessions(engine):
    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: None,
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(7, "terminal_7"))
    engine.zellij.shell_command = lambda: ["pwsh"]
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    engine.git.checkpoint = AsyncMock(return_value=(SHA, True))
    engine.config.workflow.verification_commands = [[__import__('sys').executable, "-c", "print('verified')"]]
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        workspace = engine.workspace(task)
        workspace.create({}, None)
        Path(task.worktree_path).mkdir(parents=True, exist_ok=True)
        session.delete(session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id)))
        for name in ("requirements.md", "acceptance-criteria.md", "implementation-plan.md"):
            (workspace.requirements_dir / name).write_text("Approved scope")
        engine._node(task, wf.ARCHITECT).state = "READY"

    async def start(key):
        snap = await engine.run_node("TEST-1", key)
        return next(node for node in snap["nodes"] if node["key"] == key)["lastRun"]["id"]

    discovery = await start(wf.ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", discovery, {"status": "completed"})
    assert task_state(engine, wf.IMPLEMENTER) == "PENDING"
    await engine.approve("TEST-1", wf.REQUIREMENTS_GATE, expected_requirements_digest=requirements_digest(engine))
    for iteration in (1, 2):
        implementation = await start(wf.IMPLEMENTER)
        responses = [{"id": fid, "action": "fixed", "rationale": "Guarded in src/app.py"} for fid in ("CODE-001", "SEC-001")]
        await engine.agent_complete("TEST-1", "implementer", implementation,
            {"status": "completed", "tests": {"executed": 1, "passed": 1, "failed": 0},
             **({"findingResponses": responses} if iteration == 2 else {})})
        for key, role, fid in ((wf.CODE_REVIEW, "code_reviewer", "CODE-001"), (wf.SECURITY_REVIEW, "security_reviewer", "SEC-001")):
            run = await start(key)
            await engine.agent_complete("TEST-1", role, run, {
                "reviewedSha": SHA, "verdict": "changes_required" if iteration == 1 else "approved",
                "findings": [{"title": "Bug", "severity": "high", "blocking": True}] if iteration == 1 else [],
                **({"priorFindings": [{"id": fid, "status": "fixed", "evidence": "src/app.py:12 guard"}]} if iteration == 2 else {}),
            })
        if iteration == 1:
            assert engine.snapshot("TEST-1")["status"] == "NEEDS_HUMAN_REVIEW"
    final = await start(wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", final, {
        "status": "completed", "requirementsVerified": True, "overallRisk": "LOW",
        "taskSummary": "Implemented approved scope", "testsPerformed": "One test passed",
    })
    assert engine.snapshot("TEST-1")["blockers"] == []
    await engine.approve("TEST-1", wf.READY_FOR_PR, expected_sha=SHA)
    snap = engine.snapshot("TEST-1")
    assert snap["status"] == "READY_FOR_PR"
    assert snap["reviewIteration"] == 2
    assert [(f["id"], f["status"], f["resolution"], f["response"]) for f in snap["findings"]] == [
        ("CODE-001", "resolved", "src/app.py:12 guard", "fixed"), ("SEC-001", "resolved", "src/app.py:12 guard", "fixed")]
    assert len(snap["sessions"]) == 8  # Fresh scoped process for each run; exact-ID resume when supported.
    with engine.db.session() as session:
        assert len(session.scalars(select(AgentAccess)).all()) == 8
    final_prompt = (workspace.runs_dir / "architect" / "02" / "prompt.md").read_text()
    assert f"flowctl complete --run {final}" in final_prompt


@pytest.mark.asyncio
async def test_resume_is_attempted_for_any_role_not_just_final_architect(engine):
    (engine.config.database_file.parent / "tree").mkdir(exist_ok=True)
    """A dead session's harness_session_id should be offered to build_resume() regardless
    of which node/role is restarting, not only the Final Architect step."""
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, wf.IMPLEMENTER)
        node.state = "READY"
        harness_key = engine.resolve_agent_config(task, node)["harness"]
        session.add(AgentSession(
            task_id=task.id, role="implementer", harness=harness_key,
            zellij_tab_name="Implementer", zellij_tab_id=3, zellij_pane_id="terminal_3",
            harness_session_id="dead-session-42", status="stopped",
        ))

    resume_calls = []

    def build_resume(handle, **kwargs):
        resume_calls.append(handle)
        return LaunchSpec(["stub-agent", "--resume", handle.harness_session_id])

    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: (_ for _ in ()).throw(AssertionError("should have resumed instead")),
        build_resume=build_resume,
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(7, "terminal_7"))
    engine.zellij.shell_command = lambda: ["pwsh"]

    await engine.run_node("TEST-1", wf.IMPLEMENTER)

    assert len(resume_calls) == 1
    assert resume_calls[0].harness_session_id == "dead-session-42"


@pytest.mark.asyncio
async def test_broken_resume_falls_back_to_a_fresh_launch(engine):
    (engine.config.database_file.parent / "tree").mkdir(exist_ok=True)
    """If the CLI's own resumed session is unusable (the harness crashed right after a
    resumed launch - a corrupt/incomplete transcript, say), retrying it again would just
    repeat the same crash forever. The next attempt should fall back to build_launch."""
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, wf.IMPLEMENTER)
        node.state = "READY"
        harness_key = engine.resolve_agent_config(task, node)["harness"]
        failed_run = NodeRun(task_id=task.id, node_id=node.id, run_number=1, state="FAILED",
                              expected_sha=SHA, iteration=task.review_iteration,
                              error="Harness exited with code 1 before reporting completion")
        session.add(failed_run)
        session.flush()
        session.add(AgentSession(
            task_id=task.id, role="implementer", harness=harness_key,
            zellij_tab_name="Implementer", zellij_tab_id=3, zellij_pane_id=None,
            harness_session_id="broken-session-1", status="exited", current_run_id=failed_run.id,
        ))

    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("should not retry a broken resume")),
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(7, "terminal_7"))
    engine.zellij.shell_command = lambda: ["pwsh"]

    await engine.run_node("TEST-1", wf.IMPLEMENTER)

    assert task_state(engine, wf.IMPLEMENTER) == "RUNNING"


def test_launcher_exposes_flowctl_independent_of_path(engine, tmp_path):
    """Confirmed failure mode: a harness's own shell tool (e.g. Claude Code's Bash tool on
    Windows) can start with a PATH that never saw this launcher's PATH prepend at all, leaving
    a bare `flowctl` unreachable even though every other AGENTFLOW_* env var came through fine.
    AGENTFLOW_FLOWCTL must resolve it regardless of what happened to PATH."""
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        script = engine._write_launcher(run_dir, LaunchSpec(["stub-agent"]), tmp_path, task, "implementer", 1)
    text = script.read_text(encoding="utf-8-sig" if script.suffix == ".ps1" else "utf-8")
    expected = (engine.bin_dir / "flowctl").as_posix()
    assert "AGENTFLOW_FLOWCTL" in text
    assert expected in text


@pytest.mark.asyncio
async def test_fresh_launch_prepares_the_harness_workspace_before_starting(engine):
    (engine.config.database_file.parent / "tree").mkdir(exist_ok=True)
    """Claude's workspace-trust dialog (and any harness's equivalent) would stall an
    unattended run forever if the harness ever sees an un-prepared directory, so
    prepare_workspace must run before the process is actually launched."""
    prepared = []
    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: None,
        prepare_workspace=lambda cwd, **kwargs: prepared.append(cwd),
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(7, "terminal_7"))
    engine.zellij.shell_command = lambda: ["pwsh"]
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        engine._node(task, wf.IMPLEMENTER).state = "READY"

    await engine.run_node("TEST-1", wf.IMPLEMENTER)

    assert len(prepared) == 1


@pytest.mark.asyncio
async def test_exit_report_cannot_mark_replacement_session_exited(engine):
    from agentflow.database.models import ProcessLease
    old = add_run(engine, wf.IMPLEMENTER, "FAILED")
    new = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, new)
    # The old launch does not own the new session and must not fail its current run.
    await engine.agent_exited("TEST-1", "implementer", old, 1)
    assert task_state(engine, wf.IMPLEMENTER) == "RUNNING"
    assert engine.snapshot("TEST-1")["sessions"][0]["status"] == "running"


def test_session_activity_reflects_current_run_not_pane_liveness(engine):
    """A persistent session's pane stays alive after its run finishes, so `status` alone
    reads as 'running' forever. `activity` must report the run outcome instead."""
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    session_dto = engine.snapshot("TEST-1")["sessions"][0]
    assert (session_dto["status"], session_dto["activity"]) == ("running", "running")

    with engine.db.session() as session:
        session.get(NodeRun, run_id).state = NodeState.COMPLETED
    session_dto = engine.snapshot("TEST-1")["sessions"][0]
    assert (session_dto["status"], session_dto["activity"]) == ("running", "completed")

    with engine.db.session() as session:
        session.get(NodeRun, run_id).state = NodeState.FAILED
    assert engine.snapshot("TEST-1")["sessions"][0]["activity"] == "failed"

    # Blocked on a human answer is not "working" either.
    with engine.db.session() as session:
        session.get(NodeRun, run_id).state = NodeState.WAITING_FOR_USER
    assert engine.snapshot("TEST-1")["sessions"][0]["activity"] == "waiting_for_user"

    with engine.db.session() as session:
        session.scalar(select(AgentSession)).status = "exited"
    session_dto = engine.snapshot("TEST-1")["sessions"][0]
    assert (session_dto["status"], session_dto["activity"]) == ("exited", "exited")


def test_pairing_payload_and_websocket_origin_validation(client):
    from starlette.websockets import WebSocketDisconnect
    headers = {"Origin": "http://127.0.0.1"}
    assert client.post("/api/auth/pair", json=[], headers=headers).status_code == 422
    with pytest.raises(WebSocketDisconnect):
        with client.websocket_connect("/api/ws", headers=headers):
            pass
    code = client.app.state.human.pairing_code
    client.post("/api/auth/pair", json={"code": code}, headers=headers).raise_for_status()
    with pytest.raises(WebSocketDisconnect):
        with client.websocket_connect("/api/ws", headers={"Origin": "http://evil.invalid"}):
            pass


@pytest.mark.asyncio
async def test_failed_tests_block_even_if_final_assessment_says_verified(engine):
    add_run(engine, wf.IMPLEMENTER, "COMPLETED", {"tests": {"failed": 1, "passed": 0, "executed": 1}})
    final = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", final, {
        "requirementsVerified": True, "taskSummary": "Done", "testsPerformed": "One test failed",
    })
    assert "Tests failed" in engine.snapshot("TEST-1")["blockers"]


@pytest.mark.asyncio
async def test_recovery_from_reopened_database_preserves_run_identity(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    reopened = Database(engine.config.database_file)
    reopened.create_all()
    restarted = WorkflowEngine(engine.config, reopened, engine.git, engine.zellij,
                               engine.harnesses, Broadcaster(), engine.api_base_url)
    try:
        await restarted.recover()
        assert task_state(restarted, wf.IMPLEMENTER) == "INTERRUPTED"
        await restarted.recover_node("TEST-1", wf.IMPLEMENTER, False)
        with reopened.session() as session:
            assert session.get(NodeRun, run_id).state == "RUNNING"
            assert len(session.scalars(select(NodeRun)).all()) == 1
    finally:
        reopened.engine.dispose()


@pytest.mark.asyncio
async def test_failed_session_inspection_is_not_treated_as_stopped():
    from agentflow.config import ZellijConfig
    from agentflow.services.zellij import ZellijError, ZellijService
    service = ZellijService(ZellijConfig())
    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=False, stderr="permission denied", stdout=""))
    with pytest.raises(ZellijError, match="Could not inspect"):
        await service.session_exists("agentflow-TEST-1")


@pytest.mark.asyncio
async def test_opening_a_window_on_an_exited_session_attaches_without_focusing_a_tab(engine):
    """Attaching resurrects an exited session; focusing a tab first would hang on it."""
    engine.zellij.open_window = AsyncMock()
    engine.zellij.session_state = AsyncMock(return_value="exited")
    result = await engine.open_zellij_window("TEST-1", wf.ARCHITECT)
    engine.zellij.open_window.assert_awaited_once_with("agentflow-TEST-1", None)
    assert result["tab"] is None

    engine.zellij.session_state = AsyncMock(return_value="running")
    await engine.open_zellij_window("TEST-1", wf.ARCHITECT)
    assert engine.zellij.open_window.await_args.args[1] is not None

    engine.zellij.session_state = AsyncMock(return_value=None)
    with pytest.raises(WorkflowError, match="not running"):
        await engine.open_zellij_window("TEST-1", wf.ARCHITECT)


def test_test_counts_cannot_be_negative_or_inconsistent():
    from pydantic import ValidationError
    from agentflow.workflow.models import TestSummary
    with pytest.raises(ValidationError):
        TestSummary(executed=1, passed=1, failed=-1)
    with pytest.raises(ValidationError):
        TestSummary(executed=1, passed=1, failed=1)


@pytest.mark.asyncio
async def test_high_security_risk_blocks_despite_low_overall_risk(engine):
    run = add_run(engine, wf.FINAL_ARCHITECT)
    await engine.agent_complete("TEST-1", "architect", run, {
        "overallRisk": "LOW", "securityRisk": "High: authentication impact",
        "requirementsVerified": True, "taskSummary": "Done", "testsPerformed": "Passed",
    })
    assert "Security risk is high" in engine.snapshot("TEST-1")["blockers"]


@pytest.fixture
def launchable(engine):
    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: None,
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.shell_command = lambda: ["pwsh"]
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        Path(task.worktree_path).mkdir(parents=True, exist_ok=True)
        engine._node(task, wf.ARCHITECT).state = "READY"
    return engine


async def fail_architect_launch(engine):
    from agentflow.services.zellij import ZellijError
    engine.zellij.new_tab = AsyncMock(side_effect=ZellijError("zellij action list-clients timed out after 30.0s"))
    with pytest.raises(WorkflowError, match="Could not open zellij tab"):
        await engine.run_node("TEST-1", wf.ARCHITECT)
    assert task_state(engine, wf.ARCHITECT) == "FAILED"
    engine.zellij.new_tab = AsyncMock(return_value=(7, "terminal_7"))


def architect_history(engine):
    with engine.db.session() as session:
        runs = session.scalars(select(NodeRun).join(WorkflowNode).where(WorkflowNode.key == wf.ARCHITECT)
                               .order_by(NodeRun.id)).all()
        sessions = session.scalars(select(AgentSession).order_by(AgentSession.id)).all()
        return [r.state for r in runs], [s.status for s in sessions]


@pytest.mark.asyncio
async def test_retry_after_failed_launch_releases_a_session_with_no_tab(launchable):
    engine = launchable
    await fail_architect_launch(engine)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.run_node("TEST-1", wf.ARCHITECT)
    assert task_state(engine, wf.ARCHITECT) == "RUNNING"
    assert architect_history(engine) == (["FAILED", "RUNNING"], ["exited", "running"])
    engine.zellij.close_tab.assert_not_awaited()


@pytest.mark.asyncio
async def test_retry_after_failed_launch_stops_the_tab_it_actually_opened(launchable):
    engine = launchable
    await fail_architect_launch(engine)
    engine.zellij.list_panes.return_value = [
        SimpleNamespace(tab_name="Architect", tab_id=4, pane_id="terminal_4", exited=False)]
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    await engine.run_node("TEST-1", wf.ARCHITECT)
    engine.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 4)
    assert architect_history(engine) == (["FAILED", "RUNNING"], ["stopped", "running"])


@pytest.mark.asyncio
async def test_retry_refused_without_orphan_run_while_zellij_is_unreachable(launchable):
    from agentflow.services.zellij import ZellijError
    engine = launchable
    await fail_architect_launch(engine)
    engine.zellij.list_panes.side_effect = ZellijError("list-panes failed")
    with pytest.raises(WorkflowError, match="Cannot establish the existing agent's pane identity"):
        await engine.run_node("TEST-1", wf.ARCHITECT)
    assert architect_history(engine) == (["FAILED"], ["running"])
    engine.zellij.new_tab.assert_not_awaited()


def test_process_errors_name_the_subcommand_but_not_its_arguments():
    from agentflow.services.process import _describe
    assert _describe(["zellij", "action", "new-tab", "--name", "Architect"]) == "zellij action new-tab"
    assert _describe(["zellij", "list-sessions", "--no-formatting"]) == "zellij list-sessions"
    assert _describe(["git", "-C", "D:/repo", "status"]) == "git"
