import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from pydantic import ValidationError
from sqlalchemy import select
from fastapi import HTTPException
from starlette.requests import Request

from agentflow.api.security import authorize_agent
from agentflow.database.models import AgentSession, NodeRun, WorkflowNode
from agentflow.harnesses.base import LaunchSpec
from agentflow.services.git_workspace import GitService
from agentflow.workflow import default_workflow as wf, worker_prompts, workers
from agentflow.workflow.prompts import PromptContext
from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.models import AgentOverride
from agentflow.workflow.states import NodeState
from agentflow.workflow.work_plans import WorkPlan
from test_workflow_safety import SHA, add_run, engine, requirements_digest, task_state


def plan(count=4):
    return {"summary": "Implement approved scope and verify integration", "subtasks": [
        {"key": f"part-{index}", "title": f"Part {index}", "brief": f"Implement part {index}",
         "ownedPaths": [f"src/part-{index}"], "acceptanceCriteria": ["Relevant tests pass"],
         "dependencies": []} for index in range(count)
    ]}


@pytest.fixture
def parallel_engine(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        spec = engine.spec.node(wf.ORCHESTRATOR)
        task.nodes.append(WorkflowNode(task_id=task.id, key=spec.key, kind=spec.kind,
                         label=spec.label, role=spec.role, execution_mode="manual", state="PENDING"))
        engine.workspace(task).create({}, None)
        Path(task.worktree_path).mkdir(exist_ok=True)
    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: None,
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    counter = 0

    async def new_tab(*args):
        nonlocal counter
        counter += 1
        return counter, f"terminal_{counter}"

    engine.zellij.new_tab = AsyncMock(side_effect=new_tab)
    engine.zellij.shell_command = lambda: ["pwsh"]
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    engine.git.create_worker_worktree = AsyncMock()
    engine.git.verify_worker_worktree = AsyncMock()
    engine.git.merge_worker_checkpoint = AsyncMock()
    engine.git.checkpoint = AsyncMock(return_value=(SHA, True))
    engine.git.changed_paths = AsyncMock(return_value=[])
    engine.git.is_ancestor = AsyncMock(return_value=True)
    return engine


async def submit_plan(engine, value=None):
    run = add_run(engine, wf.ORCHESTRATOR)
    await engine.agent_complete("TEST-1", "orchestrator", run, value or plan())
    return [n for n in engine.snapshot("TEST-1")["nodes"] if n.get("worker")]


def completed():
    return {"status": "completed", "summary": "Done", "tests": {
        "executed": 1, "passed": 1, "failed": 0, "notes": "Focused suite"}}


async def finish(engine, node):
    return await engine.agent_complete("TEST-1", "implementer", node["lastRun"]["id"], completed())


@pytest.mark.parametrize("path", ["../escape", "/absolute", "C:/root", ".", ".GIT/config",
                                     "src/*", "src\\file", "src/../file", "src./file"])
def test_unsafe_ownership_rejected(path):
    value = plan(1)
    value["subtasks"][0]["ownedPaths"] = [path]
    with pytest.raises(ValidationError):
        WorkPlan.model_validate(value)


def test_invalid_dependencies_and_overlapping_ownership_rejected():
    value = plan(2)
    value["subtasks"][0]["dependencies"] = ["missing"]
    with pytest.raises(ValidationError, match="cycle or unknown"):
        WorkPlan.model_validate(value)
    value["subtasks"][0]["dependencies"] = ["part-1"]
    value["subtasks"][1]["dependencies"] = ["part-0"]
    with pytest.raises(ValidationError, match="cycle or unknown"):
        WorkPlan.model_validate(value)
    value = plan(2)
    value["subtasks"][1]["ownedPaths"] = ["SRC/part-0/file.py"]
    with pytest.raises(ValidationError, match="Overlapping"):
        WorkPlan.model_validate(value)


@pytest.mark.asyncio
async def test_three_fresh_workers_then_queued_work_then_integration(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine)
    assert [n["state"] for n in nodes].count("RUNNING") == 3
    assert [n["state"] for n in nodes].count("PENDING") == 1
    assert len({n["worker"]["worktreePath"] for n in nodes}) == 4
    assert task_state(engine, wf.IMPLEMENTER) == "PENDING"
    await finish(engine, nodes[0])
    nodes = [n for n in engine.snapshot("TEST-1")["nodes"] if n.get("worker")]
    assert nodes[3]["state"] == "RUNNING"
    for node in nodes[1:]:
        await finish(engine, node)
    assert task_state(engine, wf.IMPLEMENTER) == "READY"
    assert task_state(engine, wf.CODE_REVIEW) == "PENDING"
    assert engine.zellij.new_tab.await_count == 4
    snap = await engine.run_node("TEST-1", wf.IMPLEMENTER)
    integrator = next(n for n in snap["nodes"] if n["key"] == wf.IMPLEMENTER)
    assert engine.zellij.new_tab.await_count == 5
    await finish(engine, integrator)
    assert task_state(engine, wf.CODE_REVIEW) == "READY"
    assert engine.git.is_ancestor.await_count == 4


@pytest.mark.asyncio
async def test_dependencies_wait_and_context_is_focused(parallel_engine):
    engine = parallel_engine
    value = plan(2)
    value["subtasks"][1]["dependencies"] = ["part-0"]
    nodes = await submit_plan(engine, value)
    assert nodes[1]["state"] == "PENDING"
    first_prompt = Path(nodes[0]["lastRun"]["promptPath"]).read_text()
    assert "Implement part 0" in first_prompt
    assert "Implement part 1" not in first_prompt
    assert "implementation-plan.md" not in first_prompt
    await finish(engine, nodes[0])
    assert engine.git.merge_worker_checkpoint.await_count == 1
    nodes = [n for n in engine.snapshot("TEST-1")["nodes"] if n.get("worker")]
    assert nodes[1]["state"] == "RUNNING"
    assert '"summary": "Done"' in Path(nodes[1]["lastRun"]["promptPath"]).read_text()


@pytest.mark.asyncio
async def test_terminals_questions_and_retry_are_worker_specific(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine)
    first, second = nodes[:2]
    a = await engine.terminal_target("TEST-1", first["key"])
    b = await engine.terminal_target("TEST-1", second["key"])
    assert a[1] != b[1]
    await engine.agent_question("TEST-1", "implementer", second["lastRun"]["id"], "Which behavior?")
    question = engine.snapshot("TEST-1")["questions"][0]
    await engine.answer_question("TEST-1", question["uid"], "Approved behavior")
    assert engine.zellij.write_line.call_args.args[1] == b[1]
    await engine.agent_fail("TEST-1", "implementer", first["lastRun"]["id"], "Retry me")
    await engine.run_node("TEST-1", first["key"])
    assert await engine.terminal_target("TEST-1", second["key"]) == b
    with pytest.raises(WorkflowError, match="Stale"):
        await finish(engine, first)


@pytest.mark.asyncio
async def test_worker_retry_briefing_carries_the_failed_attempt(parallel_engine):
    engine = parallel_engine
    first = (await submit_plan(engine))[0]
    run_id = first["lastRun"]["id"]
    await engine.agent_question("TEST-1", "implementer", run_id, "Keep the old route?")
    question = engine.snapshot("TEST-1")["questions"][0]
    await engine.answer_question("TEST-1", question["uid"], "Keep it")
    await engine.agent_fail("TEST-1", "implementer", run_id, "Out of context")
    engine.git.head_sha = AsyncMock(return_value="d" * 40)
    engine.git.changed_paths = AsyncMock(return_value=["src/part-0/api.py"])
    engine.git.dirty_paths = AsyncMock(return_value=["?? src/part-0/draft.py"])
    snap = await engine.run_node("TEST-1", first["key"])
    retry = next(n for n in snap["nodes"] if n["key"] == first["key"])
    prompt = Path(retry["lastRun"]["promptPath"]).read_text()
    section = prompt.split("## Previous attempt at this stage")[1]
    assert "Out of context" in section and "Keep the old route?" in section and "Keep it" in section
    assert f"HEAD moved from `{SHA}`" in section
    assert "src/part-0/api.py" in section and "?? src/part-0/draft.py" in section
    worktree = first["worker"]["worktreePath"]
    engine.git.changed_paths.assert_awaited_once_with(str(Path(worktree)), SHA, "d" * 40)


@pytest.mark.asyncio
async def test_worker_harness_exit_is_retried_automatically_in_its_own_slot(parallel_engine):
    engine = parallel_engine
    engine.config.workflow.max_automatic_retries = 1
    engine.automatic_retry_delay = 0
    nodes = await submit_plan(engine)
    first = nodes[0]
    run_id = first["lastRun"]["id"]
    await engine.agent_exited("TEST-1", "implementer", run_id, 1)
    await asyncio.gather(*engine._automatic_retries.values())
    snap = engine.snapshot("TEST-1")
    retried = next(n for n in snap["nodes"] if n["key"] == first["key"])
    assert retried["state"] == "RUNNING" and retried["lastRun"]["id"] != run_id
    assert "## Previous attempt at this stage" in Path(retried["lastRun"]["promptPath"]).read_text()
    # The other workers keep their slots; the fourth is still queued.
    states = [n["state"] for n in snap["nodes"] if n["key"].startswith("worker_")]
    assert states.count("RUNNING") == 3 and states.count("PENDING") == 1


@pytest.mark.asyncio
async def test_ownership_failure_does_not_start_reviews(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine, plan(1))
    engine.git.changed_paths.return_value = ["src/unassigned/file.py"]
    result = await finish(engine, nodes[0])
    assert result["state"] == "FAILED"
    assert task_state(engine, wf.IMPLEMENTER) == "PENDING"
    assert task_state(engine, wf.CODE_REVIEW) == "PENDING"


@pytest.mark.asyncio
async def test_integration_requires_every_checkpoint(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine, plan(1))
    await finish(engine, nodes[0])
    snap = await engine.run_node("TEST-1", wf.IMPLEMENTER)
    integrator = next(n for n in snap["nodes"] if n["key"] == wf.IMPLEMENTER)
    engine.git.is_ancestor.return_value = False
    result = await finish(engine, integrator)
    assert result["state"] == "FAILED"
    assert task_state(engine, wf.CODE_REVIEW) == "PENDING"


@pytest.mark.asyncio
async def test_worker_settings_are_inherited_and_cannot_be_overridden(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine, plan(1))
    with pytest.raises(WorkflowError, match="parent Implementer"):
        await engine.set_node_override("TEST-1", nodes[0]["key"], AgentOverride(model="different"))
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        parent = engine._node(task, wf.IMPLEMENTER)
        parent.config_override = {"model": "shared-model"}
        child = engine._node(task, nodes[0]["key"])
        assert engine.resolve_agent_config(task, child)["model"] == "shared-model"


@pytest.mark.asyncio
async def test_restart_does_not_spawn_or_replan_and_cancel_stops_workers(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine)
    await engine.recover()
    assert engine.zellij.new_tab.await_count == 3
    for node in nodes[:3]:
        assert task_state(engine, node["key"]) == "INTERRUPTED"
    await engine.cancel_task("TEST-1")
    with engine.db.session() as session:
        assert not session.scalars(select(AgentSession).where(AgentSession.status == "running")).all()
        assert len(workers.work_items(session, engine._load_task(session, "TEST-1"))) == 4


@pytest.mark.asyncio
async def test_approval_starts_orchestrator_for_new_tasks(parallel_engine):
    engine = parallel_engine
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        engine._node(task, wf.REQUIREMENTS_GATE).state = "WAITING_FOR_APPROVAL"
        from agentflow.database.models import RequirementsApproval
        session.delete(session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id)))
    await engine.approve("TEST-1", wf.REQUIREMENTS_GATE, expected_requirements_digest=requirements_digest(engine))
    assert task_state(engine, wf.ORCHESTRATOR) == "READY"
    assert task_state(engine, wf.IMPLEMENTER) == "PENDING"


@pytest.mark.asyncio
async def test_architect_planning_resumes_the_discovery_conversation(parallel_engine):
    """New Parallel tasks plan with the Architect: the planning stage joins the Architect's
    conversation, and the flowctl it reports with adds a status the plan itself never has."""
    engine = parallel_engine
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        planner = engine._node(task, wf.ORCHESTRATOR)
        planner.role, planner.label, planner.state = "architect", wf.ARCHITECT_PLANNING_LABEL, "READY"
        discovery = engine._node(task, wf.ARCHITECT)
        discovery_run = NodeRun(task_id=task.id, node_id=discovery.id, run_number=1, state="COMPLETED",
                                iteration=task.review_iteration)
        session.add(discovery_run)
        session.flush()
        session.add(AgentSession(task_id=task.id, role="architect", harness=engine.resolve_agent_config(task, planner)["harness"],
                                 zellij_tab_name="Architect", harness_session_id="discovery-1", status="stopped",
                                 current_run_id=discovery_run.id))
    resumed, fresh = [], []
    adapter = engine.harnesses.get("claude")
    adapter.build_launch = lambda **kwargs: fresh.append(kwargs) or LaunchSpec(["stub-agent"])
    adapter.build_resume = lambda handle, **kwargs: resumed.append(handle) or LaunchSpec(["stub-agent"])

    snap = await engine.run_node("TEST-1", wf.ORCHESTRATOR)
    assert [handle.harness_session_id for handle in resumed] == ["discovery-1"] and not fresh
    run = next(n for n in snap["nodes"] if n["key"] == wf.ORCHESTRATOR)["lastRun"]
    prompt = Path(run["promptPath"]).read_text(encoding="utf-8")
    assert "# Subtask planning for TEST-1" in prompt and "instead of exploring it again" in prompt
    assert Path(run["promptPath"]).parent.parent.name == "architect"
    assert engine.zellij.new_tab.await_args.args[1] == "Architect"

    await engine.agent_complete("TEST-1", "architect", run["id"], {"status": "completed", **plan(2)})
    assert task_state(engine, wf.ORCHESTRATOR) == "COMPLETED"
    assert len([n for n in engine.snapshot("TEST-1")["nodes"] if n.get("worker")]) == 2
    assert len(fresh) == 2 and len(resumed) == 1  # workers still start fresh


def test_orchestrator_briefing_is_unchanged_for_a_separate_planner(parallel_engine, tmp_path):
    ctx = PromptContext(jira_key="TEST-1", worktree_root=tmp_path, project_dir=None, task_dir=tmp_path / "task",
                        run_dir=tmp_path / "run", run_id=7, role="orchestrator", base_branch="main",
                        branch_name="agent/TEST-1", additional_context="", iteration=1, max_iterations=5,
                        flowctl_path="flowctl")
    briefing = worker_prompts.orchestrator(ctx)
    assert briefing.startswith("# Orchestrator for TEST-1\nRead the approved documents in ")
    assert "Subtask planning" not in briefing


@pytest.mark.asyncio
async def test_worker_credential_cannot_report_for_another_worker(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine, plan(2))
    credential = json.loads((Path(nodes[0]["lastRun"]["runDir"]) / "credential.json").read_text())

    async def receive():
        return {"type": "http.request", "body": json.dumps({
            "role": "implementer", "runId": nodes[1]["lastRun"]["id"],
        }).encode()}

    app = SimpleNamespace(state=SimpleNamespace(agentflow=SimpleNamespace(engine=engine, db=engine.db)))
    request = Request({"type": "http", "method": "POST", "app": app,
                       "headers": [(b"authorization", f"Bearer {credential['token']}".encode())]}, receive)
    with pytest.raises(HTTPException) as error:
        await authorize_agent(request, "TEST-1")
    assert error.value.status_code == 403


@pytest.mark.asyncio
async def test_repeated_completion_does_not_dispatch_twice(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine)
    await finish(engine, nodes[0])
    await finish(engine, nodes[0])
    assert engine.zellij.new_tab.await_count == 4
    assert engine.git.checkpoint.await_count == 1


@pytest.mark.asyncio
async def test_restart_exposes_pending_work_but_preserves_capacity(parallel_engine):
    engine = parallel_engine
    nodes = await submit_plan(engine)
    await engine.recover()
    assert task_state(engine, nodes[3]["key"]) == "READY"
    with pytest.raises(WorkflowError, match="three worker slots"):
        await engine.run_node("TEST-1", nodes[3]["key"])
    assert engine.zellij.new_tab.await_count == 3


@pytest.mark.asyncio
async def test_real_git_workers_dependencies_and_integration(parallel_engine, tmp_path):
    engine = parallel_engine
    git = GitService()
    engine.git = git
    repo = tmp_path / "local-repo"
    repo.mkdir()
    assert (await git._git("init", "-b", "main", cwd=repo)).ok
    (repo / "README.md").write_text("Local test repository\n")
    base, _ = await git.checkpoint(repo, "Initial commit")
    engine.config.worktree_dir.mkdir(parents=True, exist_ok=True)
    main_tree = engine.config.worktree_dir / "TEST-1"
    await git.create_worker_worktree(str(repo), main_tree, "task", base)
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.repository_root = str(repo)
        task.repository_path = str(repo)
        task.worktree_path = str(main_tree)
        task.branch_name = "task"
    value = plan(2)
    value["subtasks"][1]["dependencies"] = ["part-0"]
    nodes = await submit_plan(engine, value)
    first_tree = Path(nodes[0]["worker"]["worktreePath"])
    first_file = first_tree / "src" / "part-0" / "one.txt"
    first_file.parent.mkdir(parents=True)
    first_file.write_text("First worker\n")
    await finish(engine, nodes[0])
    nodes = [n for n in engine.snapshot("TEST-1")["nodes"] if n.get("worker")]
    second_tree = Path(nodes[1]["worker"]["worktreePath"])
    assert (second_tree / "src" / "part-0" / "one.txt").read_text() == "First worker\n"
    second_file = second_tree / "src" / "part-1" / "two.txt"
    second_file.parent.mkdir(parents=True)
    second_file.write_text("Second worker\n")
    await finish(engine, nodes[1])
    nodes = [n for n in engine.snapshot("TEST-1")["nodes"] if n.get("worker")]
    for node in nodes:
        await git.merge_worker_checkpoint(main_tree, node["checkpointSha"])
    snap = await engine.run_node("TEST-1", wf.IMPLEMENTER)
    integrator = next(n for n in snap["nodes"] if n["key"] == wf.IMPLEMENTER)
    result = await finish(engine, integrator)
    assert result["state"] == "COMPLETED"
    assert (main_tree / "src" / "part-1" / "two.txt").read_text() == "Second worker\n"
    assert not (repo / "src").exists()
    assert len(await git.list_worktrees(str(repo))) == 4


@pytest.mark.asyncio
async def test_launch_failure_is_durable_and_recovery_finds_guid_tab(parallel_engine):
    from agentflow.services.zellij import ZellijError

    engine = parallel_engine
    engine.zellij.new_tab.side_effect = ZellijError("Interrupted while recording pane")
    nodes = await submit_plan(engine, plan(1))
    assert nodes[0]["state"] == "FAILED"
    with engine.db.session() as session:
        tracked = session.scalar(select(AgentSession))
        assert tracked.current_run_id == nodes[0]["lastRun"]["id"]
        name = tracked.zellij_tab_name
        node = engine._node(engine._load_task(session, "TEST-1"), nodes[0]["key"])
        node.state = "INTERRUPTED"
        session.get(NodeRun, tracked.current_run_id).state = "INTERRUPTED"
    engine.zellij.list_panes.return_value = [SimpleNamespace(
        tab_name=name, tab_id=19, pane_id="terminal_19", exited=False,
    )]
    await engine.recover_node("TEST-1", nodes[0]["key"], retry=False)
    assert await engine.terminal_target("TEST-1", nodes[0]["key"]) == ("agentflow-TEST-1", "terminal_19")
    assert engine.zellij.new_tab.await_count == 1
