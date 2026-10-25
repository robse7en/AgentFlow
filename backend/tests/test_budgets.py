from types import SimpleNamespace as Item

from agentflow.workflow.budgets import _spend, launch_blocker
import asyncio
import pytest
from unittest.mock import AsyncMock
from agentflow.config import CostControls
from agentflow.database.models import NodeRun, RunEvidence, Task
from agentflow.database.models import WorkflowNode
from agentflow.workflow.models import MeteredApproval
from test_workflow_safety import engine, client, add_run, add_session
from agentflow.workflow import default_workflow as wf


def test_subscription_is_excluded_and_parallel_runs_reserve_capacity():
    runs = [Item(id=i, state="RUNNING") for i in range(3)]
    evidence = {0: Item(billing_mode="subscription", usage={"costUsd": 100}),
                1: Item(billing_mode="metered", usage=None)}
    assert _spend(runs, evidence, 2) == {"knownUsd": 0, "reservedUsd": 4, "unknownRuns": 0}


def test_unclassified_reported_cost_is_not_claimed_as_known_api_spend():
    result = _spend([Item(id=1, state="COMPLETED")],
                    {1: Item(billing_mode="unknown", usage={"costUsd": 100})}, 1)
    assert result == {"knownUsd": 0, "reservedUsd": 0, "unknownRuns": 1}


def test_list_price_estimate_is_not_known_api_spend():
    result = _spend([Item(id=1, state="COMPLETED")],
                    {1: Item(billing_mode="metered", usage={"costUsd": 5, "costKind": "estimated"})}, 1)
    assert result == {"knownUsd": 0, "reservedUsd": 0, "unknownRuns": 1}
    result = _spend([Item(id=1, state="COMPLETED")],
                    {1: Item(billing_mode="metered", usage={"costUsd": 5, "costKind": "billed"})}, 1)
    assert result["knownUsd"] == 5


def test_budget_blocks_next_launch_including_reservations():
    status = {"task": {"knownUsd": 2, "reservedUsd": 2, "unknownRuns": 0, "limitUsd": 5},
              "daily": {"knownUsd": 0, "reservedUsd": 0, "unknownRuns": 0, "limitUsd": None},
              "launchReserveUsd": 2}
    assert "Ticket" in launch_blocker(status)
    status["task"]["limitUsd"] = None
    assert launch_blocker(status) is None
    status["daily"].update(limitUsd=10, unknownRuns=1)
    assert "incomplete" in launch_blocker(status)


@pytest.mark.asyncio
async def test_launch_pauses_without_creating_a_run_when_budget_exhausted(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    engine.config.workflow.cost_controls = CostControls(taskBudgetUsd=1)
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.state = "COMPLETED"
        session.add(RunEvidence(run_id=run_id, billing_mode="metered", usage={"costUsd": 1}))
    engine._start_agent_run_unlocked = AsyncMock()
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, wf.IMPLEMENTER)
        assert await engine._start_agent_run(session, task, node) is None
        assert node.state == "READY"
        assert "budget" in node.message
    engine._start_agent_run_unlocked.assert_not_awaited()


@pytest.mark.asyncio
async def test_metered_approval_is_for_exact_model_and_does_not_bypass_budget(engine):
    engine.config.workflow.cost_controls = CostControls(requireMeteredApproval=True)
    engine.config.harnesses["codex"].billing_mode = "metered"
    engine._start_agent_run_unlocked = AsyncMock()
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, wf.IMPLEMENTER)
        cfg = engine.resolve_agent_config(task, node)
        await engine._start_agent_run(session, task, node)
        assert "approval" in node.message
        approval = MeteredApproval(harness="wrong", model=cfg["model"], reason="Complex change")
        await engine._start_agent_run(session, task, node, metered_approval=approval)
        engine._start_agent_run_unlocked.assert_not_awaited()
        approval.harness = cfg["harness"]
        await engine._start_agent_run(session, task, node, metered_approval=approval)
        engine._start_agent_run_unlocked.assert_awaited_once()
        engine._start_agent_run_unlocked.reset_mock()
        engine.config.workflow.cost_controls.task_budget_usd = .5
        await engine._start_agent_run(session, task, node, metered_approval=approval)
        engine._start_agent_run_unlocked.assert_not_awaited()


def test_copilot_routing_preserves_explicit_overrides_and_does_not_change_defaults(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, wf.IMPLEMENTER)
        assert engine.resolve_agent_config(task, node)["harness"] == "codex"
        engine.config.harnesses["copilot"].default_model = "copilot-test-model"
        engine.config.harnesses["copilot"].billing_mode = "subscription"
        engine.config.workflow.cost_controls.copilot_roles = ["implementer"]
        assert engine.resolve_agent_config(task, node)["harness"] == "copilot"
        copilot_model = engine.config.harnesses["copilot"].default_model
        codex_model = engine.config.harnesses["codex"].default_model
        node.config_override = {"harness": "codex"}
        assert engine.resolve_agent_config(task, node)["model"] == codex_model
        assert engine.resolve_agent_config(task, node)["model"] != copilot_model
        node.config_override = {"harness": "codex", "model": "explicit"}
        assert engine.resolve_agent_config(task, node)["model"] == "explicit"
        assert engine.resolve_agent_config(task, node)["harness"] == "codex"


def test_agent_cannot_change_budgets_billing_or_approve_metered_launch(client, engine):
    run = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run)
    headers = {"Authorization": "Bearer test-token"}
    assert client.put('/api/config/cost-controls', headers=headers,
                      json={"controls": {}, "billing": {}}).status_code == 403
    assert client.put('/api/tasks/TEST-1/budget', headers=headers, json={"amount": 100}).status_code == 403
    assert client.post('/api/tasks/TEST-1/nodes/implementer/run', headers=headers,
                       json={"meteredApproval": {"harness": "codex", "reason": "self approval"}}).status_code == 403


def test_human_can_set_ticket_budget_and_correct_billing_without_session_import(client, engine):
    headers = {"Origin": "http://127.0.0.1"}
    assert client.post('/api/auth/pair', headers=headers,
                       json={"code": client.app.state.human.pairing_code}).status_code == 200
    response = client.put('/api/tasks/TEST-1/budget', headers=headers, json={"amount": 5})
    assert response.status_code == 200
    assert response.json()["budget"]["task"]["limitUsd"] == 5
    assert client.put('/api/tasks/MISSING-1/budget', headers=headers, json={"amount": 5}).status_code == 404
    run = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    report = {"inputTokens": 100, "outputTokens": 10, "model": "measured", "source": "provider",
              "costUsd": .25, "billingMode": "metered"}
    assert client.put(f'/api/tasks/TEST-1/runs/{run}/usage', headers=headers,
                      json={**report, "scope": "session"}).status_code == 422
    response = client.put(f'/api/tasks/TEST-1/runs/{run}/usage', headers=headers, json=report)
    assert response.status_code == 200
    assert response.json()["totalUsage"]["apiCostUsd"] == .25
    assert response.json()["budget"]["task"]["knownUsd"] == .25


@pytest.mark.asyncio
async def test_concurrent_tickets_cannot_both_take_the_last_daily_reservation(engine):
    engine.config.workflow.cost_controls = CostControls(dailyBudgetUsd=1, launchReserveUsd=1)
    with engine.db.session() as session:
        first = engine._load_task(session, "TEST-1")
        second = Task(jira_key="TEST-2", repository_path=first.repository_path,
                      repository_root=first.repository_root, base_branch="main", branch_name="TEST-2",
                      worktree_path=first.worktree_path, task_dir=first.task_dir,
                      zellij_session_name="TEST-2")
        session.add(second)
        session.flush()
        session.add(WorkflowNode(task_id=second.id, key=wf.ARCHITECT, kind="agent",
                                  label="Architect", role="architect", state="READY"))

    async def launch(session, task, node):
        run = NodeRun(task_id=task.id, node_id=node.id, state="RUNNING", harness="claude")
        session.add(run)
        session.flush()
        session.add(RunEvidence(run_id=run.id, billing_mode="metered"))
        return run

    engine._start_agent_run_unlocked = AsyncMock(side_effect=launch)

    async def start(key):
        with engine.db.session() as session:
            task = engine._load_task(session, key)
            return await engine._start_agent_run(session, task, engine._node(task, wf.ARCHITECT))

    results = await asyncio.gather(start("TEST-1"), start("TEST-2"))
    assert sum(result is not None for result in results) == 1
    engine._start_agent_run_unlocked.assert_awaited_once()
