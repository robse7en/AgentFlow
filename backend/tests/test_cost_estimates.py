"""Cost estimates from past runs: what a stage, or a new task's route, usually costs (display only)."""

from datetime import datetime, timedelta, timezone

from agentflow.database.models import NodeRun, RunEvidence
from agentflow.workflow import default_workflow as wf
from agentflow.workflow import estimates
from agentflow.workflow.budgets import budget_status
from agentflow.workflow.models import AgentOverride
from test_workflow_safety import SHA, engine  # noqa: F401 - fixture


def usd(cost, kind=None, **extra):
    return {"costUsd": cost, **({"costKind": kind} if kind else {}), "totalTokens": None, **extra}


def test_workers_share_one_stage():
    assert estimates.stage_key("worker_3f2a9c") == "worker" == estimates.stage_key("worker_b")
    assert estimates.stage_key("implementer") == "implementer"


def test_not_enough_history_gives_no_numbers():
    result = estimates.estimate([usd(1.0), usd(2.0)], "metered")
    assert result == {"unit": "usd", "runs": 2, "enough": False, "typical": None, "high": None, "costKind": None}


def test_typical_and_high_are_nearest_rank_median_and_90th_percentile():
    samples = [usd(c) for c in (0.9, 0.1, 0.5, 0.3, 0.7)]
    result = estimates.estimate(samples, "metered")
    assert (result["typical"], result["high"], result["runs"], result["costKind"]) == (0.5, 0.9, 5, "billed")
    ten = [usd(float(n)) for n in range(1, 11)]
    assert (estimates.estimate(ten, "unknown")["typical"], estimates.estimate(ten, "unknown")["high"]) == (5.0, 9.0)


def test_only_the_most_recent_complete_samples_count():
    recent = [usd(1.0)] * 20
    older = [usd(100.0)] * 5
    assert estimates.estimate(recent + older, "metered")["high"] == 1.0
    partial = [usd(50.0, partial=True)] * 3 + [usd(1.0)] * 3
    assert estimates.estimate(partial, "metered")["high"] == 1.0
    assert estimates.estimate([None, usd(None), usd(1.0), usd(2.0), usd(3.0)], "metered")["runs"] == 3


def test_subscription_is_measured_in_tokens_and_list_prices_are_labelled():
    samples = [{"costUsd": 0.4, "costKind": "estimated", "totalTokens": n} for n in (1000, 3000, 2000)]
    assert estimates.estimate(samples, "subscription") == {
        "unit": "tokens", "runs": 3, "enough": True, "typical": 2000, "high": 3000, "costKind": None}
    assert estimates.estimate(samples, "metered")["costKind"] == "estimated"
    mixed = samples[:2] + [usd(0.4)]
    assert estimates.estimate(mixed, "metered")["costKind"] == "mixed"


def test_route_totals_one_pass_per_worker_and_per_fix_round():
    def stage(key, label, typical, high, unit="usd", enough=True):
        return {"key": key, "label": label, "estimate": {"unit": unit, "runs": 5 if enough else 1, "enough": enough,
                                                         "typical": typical if enough else None,
                                                         "high": high if enough else None, "costKind": None}}
    result = estimates.route([
        stage("architect", "Architect", 0.5, 1.0), stage("orchestrator", "Architect Planning", 0.25, 0.5),
        stage("worker", "Worker", 1.0, 2.0), stage("implementer", "Implementer", 1.5, 3.0),
        stage("code_review", "Code Review", 4000, 9000, unit="tokens"),
        stage("security_review", "Security Review", 0, 0, enough=False),
        stage("final_architect", "Final Architect Verification", 0.25, 0.5),
    ])
    assert result["onePass"] == {"usd": {"typical": 2.5, "high": 5.0}, "tokens": {"typical": 4000, "high": 9000}}
    assert result["perWorker"] == {"usd": {"typical": 1.0, "high": 2.0}, "tokens": None}
    assert result["perFixRound"] == {"usd": {"typical": 1.5, "high": 3.0}, "tokens": {"typical": 4000, "high": 9000}}
    assert result["missing"] == ["Security Review"]
    assert estimates.route([stage("implementer", "Implementer", 1.0, 1.0)])["perWorker"] is None


def finished_runs(engine, key, costs, *, harness=None, model=None, state="COMPLETED", mode="metered", day=1):
    """Finished runs of `key` on TEST-1 with the given costs, an hour apart on November `day`, oldest first."""
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        node = engine._node(task, key)
        cfg = engine.resolve_agent_config(task, node)
        start = datetime(2026, 11, day, tzinfo=timezone.utc)
        for index, cost in enumerate(costs):
            run = NodeRun(task_id=task.id, node_id=node.id, run_number=index + 1, state=state, expected_sha=SHA,
                          iteration=1, harness=harness or cfg["harness"], model=model or cfg["model"],
                          started_at=start + timedelta(hours=index), completed_at=start + timedelta(hours=index, minutes=30))
            session.add(run)
            session.flush()
            session.add(RunEvidence(run_id=run.id, billing_mode=mode, usage=usd(cost)))


def implementer(snapshot):
    return next(n for n in snapshot["nodes"] if n["key"] == wf.IMPLEMENTER)


def test_snapshot_estimates_each_agent_stage_from_its_past_runs(engine):
    engine.config.harnesses["codex"].billing_mode = "metered"
    snapshot = engine.snapshot("TEST-1")
    assert implementer(snapshot)["costEstimate"]["enough"] is False
    assert next(n for n in snapshot["nodes"] if n["key"] == wf.REQUIREMENTS_GATE)["costEstimate"] is None

    finished_runs(engine, wf.IMPLEMENTER, [0.25, 0.75, 0.5])
    finished_runs(engine, wf.IMPLEMENTER, [9.0, 9.0, 9.0], model="another-model")  # not this stage's model
    finished_runs(engine, wf.CODE_REVIEW, [9.0, 9.0, 9.0])  # not this stage
    estimate = implementer(engine.snapshot("TEST-1"))["costEstimate"]
    assert (estimate["typical"], estimate["high"], estimate["runs"], estimate["unit"]) == (0.5, 0.75, 3, "usd")


def test_the_most_recent_runs_are_the_ones_counted(engine):
    engine.config.harnesses["codex"].billing_mode = "metered"
    finished_runs(engine, wf.IMPLEMENTER, [9.0] * 3, day=1)  # older than the last 20
    finished_runs(engine, wf.IMPLEMENTER, [1.0] * 20, day=2)
    assert implementer(engine.snapshot("TEST-1"))["costEstimate"]["high"] == 1.0


def test_failed_runs_count_and_running_ones_do_not(engine):
    engine.config.harnesses["codex"].billing_mode = "metered"
    finished_runs(engine, wf.IMPLEMENTER, [1.0, 2.0], state="FAILED")
    finished_runs(engine, wf.IMPLEMENTER, [3.0], state="RUNNING")
    assert implementer(engine.snapshot("TEST-1"))["costEstimate"]["runs"] == 2
    finished_runs(engine, wf.IMPLEMENTER, [3.0])
    assert implementer(engine.snapshot("TEST-1"))["costEstimate"]["typical"] == 2.0


def test_an_estimate_never_changes_the_budget(engine):
    engine.config.harnesses["codex"].billing_mode = "metered"
    with engine.db.session() as session:
        before = budget_status(session, engine._load_task(session, "TEST-1"), engine.config.workflow.cost_controls)
    engine.snapshot("TEST-1")
    engine.cost_estimate("direct", {})
    with engine.db.session() as session:
        after = budget_status(session, engine._load_task(session, "TEST-1"), engine.config.workflow.cost_controls)
    assert before == after


def test_route_estimate_lists_each_routes_stages_with_the_agents_they_would_get(engine):
    engine.config.harnesses["codex"].billing_mode = "metered"
    finished_runs(engine, wf.IMPLEMENTER, [0.25, 0.5, 0.75])
    direct = engine.cost_estimate("direct", {})
    assert [s["key"] for s in direct["stages"]] == [
        "architect", "implementer", "code_review", "security_review", "final_architect"]
    assert direct["onePass"]["usd"] == {"typical": 0.5, "high": 0.75} and direct["perWorker"] is None
    assert direct["perFixRound"]["usd"] == {"typical": 0.5, "high": 0.75}
    assert "Architect" in direct["missing"] and "Implementer" not in direct["missing"]

    parallel = engine.cost_estimate("parallel", {})
    assert [(s["key"], s["label"]) for s in parallel["stages"]][1:3] == [
        ("orchestrator", "Architect Planning"), ("worker", "Worker")]
    assert parallel["perWorker"] == {"usd": None, "tokens": None}  # no worker history yet

    # A different implementer harness on the form has no history of its own.
    other = engine.cost_estimate("direct", {"implementer": AgentOverride(harness="claude")})
    stage = next(s for s in other["stages"] if s["key"] == "implementer")
    assert stage["harness"] == "claude" and stage["estimate"]["enough"] is False
    assert engine.cost_estimate(None, {})["implementationMode"] == engine.config.workflow.implementation_mode
