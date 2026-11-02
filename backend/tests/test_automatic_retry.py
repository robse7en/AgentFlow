"""workflow.maxAutomaticRetries: a stage whose harness exited without reporting completion is launched
again by itself, a limited number of times, through the same checks as a human's Retry."""

import asyncio
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import select

from agentflow.config import CostControls
from agentflow.database.models import NodeRun, WorkflowEvent
from agentflow.services.zellij import ZellijError
from agentflow.workflow import default_workflow as wf
from test_retry_briefing import HEADING, launching, prompt_of_latest_run  # noqa: F401 - fixture
from test_workflow_safety import add_run, engine, task_state  # noqa: F401 - fixture


@pytest.fixture
def retrying(launching):
    engine = launching
    engine.config.workflow.max_automatic_retries = 1
    engine.automatic_retry_delay = 0
    return engine


async def start(engine, key=wf.IMPLEMENTER) -> int:
    with engine.db.session() as session:
        engine._node(engine._load_task(session, "TEST-1"), key).state = "READY"
    await engine.run_node("TEST-1", key)
    return latest_run(engine, key).id


def latest_run(engine, key=wf.IMPLEMENTER) -> NodeRun:
    with engine.db.session() as session:
        node = engine._node(engine._load_task(session, "TEST-1"), key)
        run = session.scalar(select(NodeRun).where(NodeRun.node_id == node.id).order_by(NodeRun.id.desc()))
        session.expunge(run)
        return run


def run_count(engine, key=wf.IMPLEMENTER) -> int:
    with engine.db.session() as session:
        node = engine._node(engine._load_task(session, "TEST-1"), key)
        return len(session.scalars(select(NodeRun).where(NodeRun.node_id == node.id)).all())


def node_of(engine, key=wf.IMPLEMENTER):
    with engine.db.session() as session:
        node = engine._node(engine._load_task(session, "TEST-1"), key)
        session.expunge(node)
        return node


def logs(engine) -> list[str]:
    with engine.db.session() as session:
        return [entry.message for entry in session.scalars(select(WorkflowEvent).order_by(WorkflowEvent.id))]


async def settle(engine) -> None:
    """Wait for every pending automatic retry."""
    await asyncio.gather(*engine._automatic_retries.values())


@pytest.mark.asyncio
async def test_off_by_default_a_harness_exit_stays_failed(launching):
    engine = launching
    run_id = await start(engine)
    await engine.agent_exited("TEST-1", "implementer", run_id, 1)
    assert not engine._automatic_retries
    assert task_state(engine, wf.IMPLEMENTER) == "FAILED"
    assert node_of(engine).message == "Harness exited (code 1)"


@pytest.mark.asyncio
async def test_a_harness_exit_is_retried_once_with_a_fresh_briefing(retrying):
    engine = retrying
    first = await start(engine)
    await engine.agent_exited("TEST-1", "implementer", first, 1)
    assert node_of(engine).message == "Harness exited (code 1); automatic retry 1 of 1 starts shortly"
    assert first in engine._automatic_retries
    await settle(engine)

    second = latest_run(engine)
    assert second.id != first and second.state == "RUNNING" and task_state(engine, wf.IMPLEMENTER) == "RUNNING"
    assert HEADING in prompt_of_latest_run(engine)  # the retry is told what the failed attempt did
    assert "Implementer: automatic retry 1 of 1 after the harness exited" in logs(engine)

    await engine.agent_exited("TEST-1", "implementer", second.id, 1)
    assert not engine._automatic_retries
    assert task_state(engine, wf.IMPLEMENTER) == "FAILED" and run_count(engine) == 2
    assert node_of(engine).message == "Harness exited (code 1)"
    assert any("not retried automatically: it has failed 2 times in a row" in line for line in logs(engine))


@pytest.mark.asyncio
async def test_a_reported_failure_or_a_later_exit_is_not_retried(retrying):
    engine = retrying
    run_id = await start(engine)
    await engine.agent_fail("TEST-1", "implementer", run_id, "Cannot build the project")
    # The launcher still reports its exit afterwards; the run is no longer active.
    await engine.agent_exited("TEST-1", "implementer", run_id, 0)
    assert not engine._automatic_retries
    assert task_state(engine, wf.IMPLEMENTER) == "FAILED" and run_count(engine) == 1


@pytest.mark.asyncio
async def test_a_human_retry_or_cancel_during_the_delay_wins(retrying):
    engine = retrying
    engine.automatic_retry_delay = 3600
    first = await start(engine)
    await engine.agent_exited("TEST-1", "implementer", first, 1)
    pending = engine._automatic_retries[first]
    await engine.run_node("TEST-1", wf.IMPLEMENTER)  # the human clicked Retry first
    pending.cancel()
    engine.automatic_retry_delay = 0
    await engine._automatic_retry("TEST-1", wf.IMPLEMENTER, first, 1)
    assert run_count(engine) == 2

    human = latest_run(engine).id
    engine.automatic_retry_delay = 3600
    engine.config.workflow.max_automatic_retries = 3
    await engine.agent_exited("TEST-1", "implementer", human, 1)
    engine._automatic_retries[human].cancel()
    await engine.cancel_task("TEST-1")
    engine.automatic_retry_delay = 0
    await engine._automatic_retry("TEST-1", wf.IMPLEMENTER, human, 2)
    assert run_count(engine) == 2  # a cancelled task keeps its failed stage, but nothing relaunches


@pytest.mark.asyncio
async def test_no_retry_once_the_task_needs_human_review(retrying):
    engine = retrying
    run_id = await start(engine)
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = "NEEDS_HUMAN_REVIEW"
    await engine.agent_exited("TEST-1", "implementer", run_id, 1)
    assert not engine._automatic_retries


@pytest.mark.asyncio
async def test_metered_approval_keeps_the_stage_failed(retrying):
    engine = retrying
    first = await start(engine)
    engine.config.workflow.cost_controls = CostControls(requireMeteredApproval=True)
    engine.config.harnesses["codex"].billing_mode = "metered"
    await engine.agent_exited("TEST-1", "implementer", first, 1)
    await settle(engine)
    node = node_of(engine)
    assert node.state == "FAILED" and run_count(engine) == 1
    assert node.message.startswith("Not retried automatically: API launch needs approval")
    assert node.error == "Harness exited with code 1 before reporting completion"


@pytest.mark.asyncio
async def test_global_capacity_keeps_the_stage_failed(retrying):
    engine = retrying
    first = await start(engine)
    engine.config.workflow.max_concurrent_agents = 1
    add_run(engine, wf.CODE_REVIEW, "RUNNING")  # another stage holds the only slot
    await engine.agent_exited("TEST-1", "implementer", first, 1)
    await settle(engine)
    node = node_of(engine)
    assert node.state == "FAILED" and run_count(engine) == 1
    assert node.message == "Not retried automatically: Global agent capacity reached; start again when a slot is free"


@pytest.mark.asyncio
async def test_a_failed_automatic_launch_is_not_retried_again(retrying):
    engine = retrying
    first = await start(engine)
    engine.zellij.new_tab = AsyncMock(side_effect=ZellijError("zellij is gone"))
    engine.config.workflow.max_automatic_retries = 3
    await engine.agent_exited("TEST-1", "implementer", first, 1)
    await settle(engine)
    node = node_of(engine)
    assert node.state == "FAILED" and node.message == "Automatic retry failed"
    assert "zellij is gone" in node.error
    assert not engine._automatic_retries and run_count(engine) == 2


@pytest.mark.asyncio
async def test_shutdown_cancels_a_pending_retry(retrying):
    engine = retrying
    engine.automatic_retry_delay = 3600
    first = await start(engine)
    await engine.agent_exited("TEST-1", "implementer", first, 1)
    pending = engine._automatic_retries[first]
    await engine.shutdown()
    with pytest.raises(asyncio.CancelledError):
        await pending
    assert run_count(engine) == 1
