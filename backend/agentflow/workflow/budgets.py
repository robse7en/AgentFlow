"""Launch-time API safeguards shared across tickets; never claim a live hard spending cap."""
from datetime import datetime, time, timedelta, timezone

from sqlalchemy import and_, func, or_, select

from ..database.models import DeletedRunSpend, NodeRun, RunEvidence
from .states import ACTIVE_STATES, NodeState
from .usage import cost_kind


def run_charge(record):
    """(billing mode, provider-billed metered cost or None) for a run's evidence row."""
    mode = record.billing_mode if record else "unknown"
    # List-price estimates are for display only; a limit needs the provider's own figure.
    cost = (record.usage["costUsd"] if record and mode == "metered"
            and cost_kind(record.usage) == "billed" else None)
    return mode, cost


def charged_at(run):
    """The whole run is charged to one UTC day: that of completion, or start while active."""
    return run.completed_at or run.started_at


def _spend(runs, evidence, reserve):
    known = 0.0
    reserved = 0.0
    unknown = 0
    for run in runs:
        mode, cost = run_charge(evidence.get(run.id))
        if mode == "subscription":
            continue
        if cost is not None:
            known += cost
        if run.state in (*ACTIVE_STATES, NodeState.INTERRUPTED):
            reserved += reserve
        elif cost is None:
            unknown += 1
    return {"knownUsd": known, "reservedUsd": reserved, "unknownRuns": unknown}


def _daily(runs, evidence, controls, deleted=()):
    today = datetime.now(timezone.utc).date()
    daily = [r for r in runs if charged_at(r).date() == today
             or r.state in (*ACTIVE_STATES, NodeState.INTERRUPTED)]
    group = _spend(daily, evidence, controls.launch_reserve_usd)
    # Runs of deleted tasks still count (deleting a task must not free budget or hide unknown cost).
    for row in deleted:
        if row.charged_at.date() != today or row.billing_mode == "subscription":
            continue
        if row.cost_usd is not None:
            group["knownUsd"] += row.cost_usd
        else:
            group["unknownRuns"] += 1
    return {**group, "limitUsd": controls.daily_budget_usd}


def _warn(group, controls):
    limit = group["limitUsd"]
    group["warning"] = bool(limit and (group["knownUsd"] + group["reservedUsd"]) >= limit * controls.warning_percent / 100)
    return group


def _load(session, task_id):
    """The rows a budget can need, not the whole history: every run of the task, and every run
    or deleted-task charge that may count today. The SQL window is a day wider on each side,
    because SQLite compares stored text; `_daily` still decides exactly what counts today."""
    midnight = datetime.combine(datetime.now(timezone.utc).date(), time.min)
    since, until = midnight - timedelta(days=1), midnight + timedelta(days=2)
    charged = func.coalesce(NodeRun.completed_at, NodeRun.started_at)  # charged_at(), in SQL
    wanted = or_(NodeRun.task_id == task_id, NodeRun.state.in_([*ACTIVE_STATES, NodeState.INTERRUPTED]),
                 and_(charged >= since, charged < until))
    runs = session.scalars(select(NodeRun).where(wanted)).all()
    evidence = {e.run_id: e for e in session.scalars(select(RunEvidence).join(NodeRun).where(wanted)).all()}
    deleted = session.scalars(select(DeletedRunSpend).where(DeletedRunSpend.charged_at >= since,
                                                            DeletedRunSpend.charged_at < until)).all()
    return runs, evidence, deleted


def budget_status(session, task, controls):
    runs, evidence, deleted = _load(session, task.id)
    return {
        "task": _warn({**_spend([r for r in runs if r.task_id == task.id], evidence, controls.launch_reserve_usd),
                       "limitUsd": task.api_budget_usd or controls.task_budget_usd}, controls),
        "daily": _warn(_daily(runs, evidence, controls, deleted), controls),
        "warningPercent": controls.warning_percent, "launchReserveUsd": controls.launch_reserve_usd,
        "dayUtc": str(datetime.now(timezone.utc).date()),
    }


def daily_spend(session, controls):
    """Today's API spend across all tickets: the `daily` group of `budget_status`."""
    runs, evidence, deleted = _load(session, None)
    return _warn(_daily(runs, evidence, controls, deleted), controls)


def unknown_cost_runs_today(runs, evidence, controls):
    """How many of these runs leave today's API spend incomplete (what `launch_blocker` refuses on)."""
    return _daily(runs, evidence, controls)["unknownRuns"]


def launch_blocker(status):
    for key, label in (("task", "Ticket"), ("daily", "Daily")):
        group = status[key]
        limit = group["limitUsd"]
        if limit is None:
            continue
        if group["unknownRuns"]:
            return f"{label} API spend is incomplete; import missing run costs before starting another metered agent"
        if group["knownUsd"] + group["reservedUsd"] + status["launchReserveUsd"] > limit:
            return f"{label} API budget reached or insufficient launch reserve; adjust the budget or use a subscription harness"
    return None
