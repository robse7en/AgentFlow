"""Budget queries load the task's history and what can count today, not every run ever
(AF-AUDIT-005), with the accounting unchanged: compared against loading everything."""

from datetime import datetime, timedelta, timezone

import pytest
from sqlalchemy import event, select

from agentflow.config import CostControls
from agentflow.database.models import DeletedRunSpend, NodeRun, RunEvidence, Task, WorkflowNode
from agentflow.workflow import budgets
from agentflow.workflow.budgets import budget_status, daily_spend, launch_blocker
from test_workflow_safety import engine  # noqa: F401 - fixture

MIDNIGHT = datetime(2026, 10, 3, tzinfo=timezone.utc)  # "today" starts here, in UTC
NOW = MIDNIGHT + timedelta(hours=9)
CONTROLS = CostControls(taskBudgetUsd=1000, dailyBudgetUsd=1000, launchReserveUsd=0.5)


def freeze(monkeypatch, now):
    class Clock(datetime):
        @classmethod
        def now(cls, tz=None):
            return now if tz else now.replace(tzinfo=None)

    monkeypatch.setattr(budgets, "datetime", Clock)


def load_everything(session, task_id):
    """The loader before AF-AUDIT-005: the reference for what the budgets must still report."""
    runs = session.scalars(select(NodeRun)).all()
    evidence = {e.run_id: e for e in session.scalars(select(RunEvidence)).all()}
    return runs, evidence, session.scalars(select(DeletedRunSpend)).all()


def add_task(session, key, tmp_path):
    task = Task(jira_key=key, repository_path=str(tmp_path), repository_root=str(tmp_path), base_branch="main",
                branch_name=f"agent/{key}", worktree_path=str(tmp_path / key), task_dir=str(tmp_path / "tasks" / key),
                zellij_session_name=f"agentflow-{key}")
    session.add(task)
    session.flush()
    node = WorkflowNode(task_id=task.id, key="implementer", kind="agent", label="Implementer",
                        role="implementer", state="PENDING")
    session.add(node)
    session.flush()
    return task


def add_run(session, task, state, started, completed=None, mode="metered", usage=None, evidence=True):
    node = session.scalar(select(WorkflowNode).where(WorkflowNode.task_id == task.id))
    run = NodeRun(task_id=task.id, node_id=node.id, state=state, started_at=started, completed_at=completed)
    session.add(run)
    session.flush()
    if evidence:
        session.add(RunEvidence(run_id=run.id, billing_mode=mode, usage=usage))
    return run.id


def billed(cost):
    return {"costUsd": cost, "costKind": "billed"}


def seed(engine, tmp_path):  # noqa: F811
    """Every accounting case: lifetime task totals, UTC day boundaries, reservations across
    midnight, subscription and estimated costs, missing evidence and deleted-task spend."""
    day = timedelta(days=1)
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        other = add_task(session, "TEST-2", tmp_path)
        # This task: counted over its whole lifetime, and today only where charged today.
        add_run(session, task, "COMPLETED", MIDNIGHT - 30 * day, MIDNIGHT - 30 * day, usage=billed(1))
        add_run(session, task, "COMPLETED", MIDNIGHT - day, MIDNIGHT - timedelta(microseconds=1), usage=billed(2))
        add_run(session, task, "COMPLETED", MIDNIGHT - timedelta(hours=1), MIDNIGHT, usage=billed(4))  # completion day
        add_run(session, task, "COMPLETED", MIDNIGHT, MIDNIGHT, mode="subscription", usage=billed(100))
        add_run(session, task, "COMPLETED", MIDNIGHT, NOW, evidence=False)  # unknown
        add_run(session, task, "COMPLETED", MIDNIGHT, NOW, usage={"costUsd": 50, "costKind": "estimated"})  # unknown
        add_run(session, task, "RUNNING", MIDNIGHT - timedelta(hours=1))  # reserves across midnight
        add_run(session, task, "INTERRUPTED", MIDNIGHT - 2 * day)  # still reserves
        # Another task: only what counts today.
        add_run(session, other, "COMPLETED", MIDNIGHT + timedelta(hours=1), NOW, usage=billed(8))
        add_run(session, other, "COMPLETED", MIDNIGHT - 30 * day, MIDNIGHT - 30 * day, usage=billed(16))
        add_run(session, other, "FAILED", MIDNIGHT - 2 * day, MIDNIGHT - 2 * day, mode="unknown")
        add_run(session, other, "WAITING_FOR_USER", MIDNIGHT - 3 * day)
        add_run(session, other, "INTERRUPTED", MIDNIGHT - 3 * day, mode="subscription")
        # Deleted tasks' spend, by charged day.
        for charged, mode, cost in ((MIDNIGHT, "metered", 32), (NOW, "metered", None), (NOW, "subscription", 128),
                                    (MIDNIGHT - timedelta(microseconds=1), "metered", 64),
                                    (MIDNIGHT - 2 * day, "metered", None)):
            session.add(DeletedRunSpend(jira_key="GONE-1", run_id=0, charged_at=charged, billing_mode=mode,
                                        cost_usd=cost))


def statuses(engine):  # noqa: F811
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        return budget_status(session, task, CONTROLS), daily_spend(session, CONTROLS)


def test_budgets_match_loading_the_whole_history(engine, tmp_path, monkeypatch):  # noqa: F811
    freeze(monkeypatch, NOW)
    seed(engine, tmp_path)
    status, daily = statuses(engine)
    assert {key: status["task"][key] for key in ("knownUsd", "reservedUsd", "unknownRuns")} == {
        "knownUsd": 7, "reservedUsd": 1.0, "unknownRuns": 2}
    assert {key: status["daily"][key] for key in ("knownUsd", "reservedUsd", "unknownRuns")} == {
        "knownUsd": 4 + 8 + 32, "reservedUsd": 1.5, "unknownRuns": 3}
    assert status["dayUtc"] == "2026-10-03" and daily == status["daily"]
    monkeypatch.setattr(budgets, "_load", load_everything)
    assert statuses(engine) == (status, daily)


@pytest.mark.parametrize("now", [MIDNIGHT - timedelta(microseconds=1), MIDNIGHT, NOW, MIDNIGHT + timedelta(days=1)])
def test_budgets_match_loading_the_whole_history_on_either_side_of_utc_midnight(engine, tmp_path, monkeypatch, now):  # noqa: F811
    freeze(monkeypatch, now)
    seed(engine, tmp_path)
    scoped = statuses(engine)
    monkeypatch.setattr(budgets, "_load", load_everything)
    assert statuses(engine) == scoped


def test_a_run_counts_on_its_utc_completion_day(engine, tmp_path, monkeypatch):  # noqa: F811
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        add_run(session, task, "COMPLETED", MIDNIGHT - timedelta(hours=2), MIDNIGHT - timedelta(microseconds=1),
                usage=billed(2))
        add_run(session, task, "COMPLETED", MIDNIGHT - timedelta(hours=2), MIDNIGHT, usage=billed(4))
    freeze(monkeypatch, MIDNIGHT - timedelta(microseconds=1))
    assert statuses(engine)[1]["knownUsd"] == 2
    freeze(monkeypatch, MIDNIGHT)
    assert statuses(engine)[1]["knownUsd"] == 4
    assert statuses(engine)[0]["task"]["knownUsd"] == 6


def test_interrupted_and_active_runs_of_other_tasks_keep_their_daily_reservation(engine, tmp_path, monkeypatch):  # noqa: F811
    freeze(monkeypatch, NOW)
    with engine.db.session() as session:
        other = add_task(session, "TEST-2", tmp_path)
        add_run(session, other, "INTERRUPTED", MIDNIGHT - timedelta(days=5))
        add_run(session, other, "RUNNING", MIDNIGHT - timedelta(days=1))
    controls = CostControls(dailyBudgetUsd=2, launchReserveUsd=1)
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        status = budget_status(session, task, controls)
    assert status["daily"]["reservedUsd"] == 2 and status["task"]["reservedUsd"] == 0
    assert "Daily API budget reached" in launch_blocker(status)


def test_deleted_task_spend_counts_on_its_charged_day_only(engine, monkeypatch):  # noqa: F811
    freeze(monkeypatch, NOW)
    with engine.db.session() as session:
        session.add(DeletedRunSpend(jira_key="GONE-1", run_id=1, charged_at=MIDNIGHT - timedelta(microseconds=1),
                                    billing_mode="metered", cost_usd=None))
        session.add(DeletedRunSpend(jira_key="GONE-1", run_id=2, charged_at=MIDNIGHT - timedelta(days=1),
                                    billing_mode="metered", cost_usd=40))
    status = statuses(engine)[0]
    assert (status["daily"]["knownUsd"], status["daily"]["unknownRuns"]) == (0, 0)
    assert launch_blocker(status) is None
    with engine.db.session() as session:
        session.add(DeletedRunSpend(jira_key="GONE-1", run_id=3, charged_at=MIDNIGHT, billing_mode="metered",
                                    cost_usd=None))
        session.add(DeletedRunSpend(jira_key="GONE-1", run_id=4, charged_at=NOW, billing_mode="metered", cost_usd=3))
    status = statuses(engine)[0]
    assert (status["daily"]["knownUsd"], status["daily"]["unknownRuns"]) == (3, 1)
    assert "incomplete" in launch_blocker(status)


def test_archived_task_keeps_its_whole_history_in_the_task_total(engine, monkeypatch):  # noqa: F811
    freeze(monkeypatch, NOW)
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.archived_at = MIDNIGHT - timedelta(days=100)
        add_run(session, task, "COMPLETED", MIDNIGHT - timedelta(days=400), MIDNIGHT - timedelta(days=400),
                usage=billed(9))
    status = statuses(engine)[0]
    assert status["task"]["knownUsd"] == 9 and status["daily"]["knownUsd"] == 0


def test_snapshots_do_not_load_unrelated_history(engine, tmp_path, monkeypatch):  # noqa: F811
    """Old completed runs of other tasks, their evidence and old deleted-task spend stay in the
    database: a snapshot's budget never materializes them, however much history there is."""
    freeze(monkeypatch, NOW)
    seed(engine, tmp_path)
    with engine.db.session() as session:
        seeded_runs = set(session.scalars(select(NodeRun.id)))
        seeded_ledger = set(session.scalars(select(DeletedRunSpend.id)))
        old = add_task(session, "OLD-1", tmp_path)
        unrelated = {add_run(session, old, "COMPLETED", MIDNIGHT - timedelta(days=60 + n),
                             MIDNIGHT - timedelta(days=60 + n), usage={**billed(1), "transcript": "x" * 2000})
                     for n in range(200)}
        for n in range(50):
            session.add(DeletedRunSpend(jira_key="GONE-2", run_id=10_000 + n, billing_mode="metered", cost_usd=1,
                                        charged_at=MIDNIGHT - timedelta(days=90 + n)))
    loaded = {NodeRun: set(), RunEvidence: set(), DeletedRunSpend: set()}
    listeners = [(model, lambda target, _context, model=model: loaded[model].add(target)) for model in loaded]
    for model, listener in listeners:
        event.listen(model, "load", listener)
    try:
        snapshot = engine.snapshot("TEST-1")
        with engine.db.session() as session:
            daily = daily_spend(session, CONTROLS)
    finally:
        for model, listener in listeners:
            event.remove(model, "load", listener)
    assert {run.id for run in loaded[NodeRun]}.isdisjoint(unrelated)
    assert {row.run_id for row in loaded[RunEvidence]}.isdisjoint(unrelated)
    # Bounded by the seeded cases (13 runs, 5 ledger rows), not by the 250 rows of history.
    assert {run.id for run in loaded[NodeRun]} <= seeded_runs
    assert {row.id for row in loaded[DeletedRunSpend]} <= seeded_ledger
    # ...and the figures are those of the full history.
    monkeypatch.setattr(budgets, "_load", load_everything)
    assert engine.snapshot("TEST-1")["budget"] == snapshot["budget"]
    with engine.db.session() as session:
        assert daily_spend(session, CONTROLS) == daily
