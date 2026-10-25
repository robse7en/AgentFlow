import pytest

from sqlalchemy import select

from agentflow.database.models import NodeRun, RunHeartbeat
from agentflow.workflow import default_workflow as wf
from agentflow.workflow.engine import WorkflowError
from test_workflow_safety import add_run, engine, task_state


@pytest.mark.asyncio
async def test_heartbeat_is_coalesced_and_does_not_resume_waiting_work(engine):
    run = add_run(engine, wf.IMPLEMENTER, 'WAITING_FOR_USER')
    before = engine.snapshot('TEST-1')
    for phase in ('implementing', 'testing'):
        await engine.agent_heartbeat('TEST-1', 'implementer', run, phase)
    after = engine.snapshot('TEST-1')
    node = next(n for n in after['nodes'] if n['key'] == wf.IMPLEMENTER)
    assert node['lastRun']['heartbeat']['phase'] == 'testing'
    assert node['lastRun']['heartbeat']['receivedAt']
    assert node['lastRun']['heartbeat']['warningSeconds'] == 600
    assert task_state(engine, wf.IMPLEMENTER) == 'WAITING_FOR_USER'
    assert after['events'] == before['events']
    assert after['currentSha'] == before['currentSha']
    with engine.db.session() as session:
        reports = session.scalars(select(RunHeartbeat).where(RunHeartbeat.run_id == run)).all()
        assert len(reports) == 1
        session.delete(session.get(NodeRun, run))
    with engine.db.session() as session:
        assert session.scalar(select(RunHeartbeat).where(RunHeartbeat.run_id == run)) is None


@pytest.mark.asyncio
async def test_heartbeat_rejects_stale_wrong_role_and_inactive_runs(engine):
    old = add_run(engine, wf.IMPLEMENTER, 'FAILED')
    run = add_run(engine, wf.IMPLEMENTER)
    for role, identity in [('implementer', old), ('architect', run)]:
        with pytest.raises(WorkflowError):
            await engine.agent_heartbeat('TEST-1', role, identity, 'working')
    for state in ('COMPLETED', 'FAILED', 'INTERRUPTED', 'CANCELLED'):
        with engine.db.session() as session:
            session.get(NodeRun, run).state = state
        with pytest.raises(WorkflowError):
            await engine.agent_heartbeat('TEST-1', 'implementer', run, 'working')


@pytest.mark.asyncio
@pytest.mark.parametrize('phase', ['invented', '<script>', 'x' * 1000])
async def test_heartbeat_phase_is_bounded(engine, phase):
    run = add_run(engine, wf.IMPLEMENTER)
    with pytest.raises(WorkflowError):
        await engine.agent_heartbeat('TEST-1', 'implementer', run, phase)
