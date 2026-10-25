"""Run accounting must remain exact across resumed sessions and repeated collection."""
import importlib.util
import json
from pathlib import Path

import pytest

from agentflow.workflow.usage import UsagePending, UsageReport, run_usage
from agentflow.database.models import AgentSession, NodeRun, RunEvidence
from agentflow.workflow.engine import WorkflowEngine
from types import SimpleNamespace, SimpleNamespace as Item
from sqlalchemy import select
from unittest.mock import AsyncMock
from test_workflow_safety import engine, add_run
from agentflow.workflow import default_workflow as wf


def report(tokens, cost=None):
    return UsageReport(inputTokens=tokens, outputTokens=10, cachedInputTokens=0,
                       model="test", source="fixture", scope="session", costUsd=cost)


def test_resumed_usage_is_repeatable_and_excludes_prior_run():
    baseline = report(100, 1).model_dump(by_alias=True)
    current = report(150, 1.5)
    first = run_usage(current, baseline)
    assert first.input_tokens == 50
    assert first.output_tokens == 0
    assert first.cost == .5
    assert run_usage(current, baseline) == first


def test_missing_baseline_does_not_claim_session_total_as_run_usage():
    with pytest.raises(ValueError, match="baseline"):
        run_usage(report(150), None)


def test_fresh_session_has_zero_baseline():
    assert run_usage(report(150), {}).input_tokens == 150


def test_custom_run_collector_remains_compatible():
    value = UsageReport(inputTokens=12, outputTokens=3, model="test", source="custom")
    assert run_usage(value, None) == value


def test_reset_cumulative_counter_is_unknown():
    with pytest.raises(ValueError, match="decreased"):
        run_usage(report(90), report(100).model_dump(by_alias=True))


def test_missing_cost_baseline_never_assigns_prior_spend_to_new_run():
    assert run_usage(report(150, 3), report(100).model_dump(by_alias=True)).cost is None


def test_codex_session_lookup_requires_exact_identity(tmp_path, monkeypatch):
    path = Path(__file__).parents[2] / "scripts" / "usage_codex.py"
    spec = importlib.util.spec_from_file_location("usage_codex", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module.Path, "home", lambda: tmp_path)
    folder = tmp_path / ".codex/sessions/2026/09/22"
    folder.mkdir(parents=True)
    for identity in ("first", "second"):
        (folder / f"{identity}.jsonl").write_text(json.dumps({"type": "session_meta",
            "payload": {"id": identity, "source": "cli", "cwd": "same-directory"}}))
    assert module.find_session_file("first").name == "first.jsonl"
    assert module.find_session_file("missing") is None
    assert module.find_session_file("") is None


def load_script(name):
    path = Path(__file__).parents[2] / "scripts" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CATALOG = {"openai": {"models": {"gpt-test": {"cost": {
    "input": 2, "output": 10, "cache_read": 0.2, "cache_write": 2.5,
    "tiers": [{"input": 4, "output": 15, "cache_read": 0.4, "cache_write": 5,
               "tier": {"type": "context", "size": 272000}}]}}}},
    "anthropic": {"models": {"claude-test-5-5": {"cost": {
        "input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5}}}}}


@pytest.fixture
def home(tmp_path, monkeypatch):
    """A fake home with the Copilot proxy's cached price catalog, so no test touches the network."""
    monkeypatch.setattr(Path, "home", lambda: tmp_path)
    monkeypatch.delenv("COPILOT_API_HOME", raising=False)
    proxy = tmp_path / ".local" / "share" / "copilot-api"
    proxy.mkdir(parents=True)
    (proxy / "models-dev-api.json").write_text(json.dumps(CATALOG), encoding="utf-8")
    return tmp_path


def copilot_events(home, session_id, *events):
    path = home / ".copilot" / "session-state" / session_id / "events.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(json.dumps(e) for e in events), encoding="utf-8")


def copilot_shutdown(nano_aiu=None, per_model=None):
    metrics = {"usage": {"inputTokens": 100, "outputTokens": 20, "cacheReadTokens": 40}}
    if per_model is not None:
        metrics["totalNanoAiu"] = per_model
    data = {"modelMetrics": {"gpt-test": metrics}}
    if nano_aiu is not None:
        data["totalNanoAiu"] = nano_aiu
    return {"type": "session.shutdown", "data": data}


def copilot_checkpoint(nano_aiu):
    return {"type": "session.usage_checkpoint", "data": {"totalNanoAiu": nano_aiu}}


COPILOT_TURN = ({"type": "user.message", "data": {}}, {"type": "assistant.turn_start", "data": {}},
                {"type": "assistant.message", "data": {"model": "gpt-test"}},
                {"type": "assistant.turn_end", "data": {}})


def test_copilot_reports_billed_ai_credits_cumulatively_without_tokens(home, monkeypatch, capsys):
    module = load_script("usage_copilot")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    copilot_events(home, "session", {"type": "session.start"}, *COPILOT_TURN, copilot_shutdown(125_000_000_000))
    assert module.main() == 0
    current = UsageReport.model_validate(json.loads(capsys.readouterr().out))
    assert current.cost == 1.25 and current.cost_kind == "billed" and current.model == "gpt-test"
    assert current.input_tokens is None and current.record("fixture")["totalTokens"] is None
    assert run_usage(current, {}).cost == 1.25


def test_copilot_sums_per_model_credits_without_a_session_total(home):
    module = load_script("usage_copilot")
    copilot_events(home, "s", copilot_shutdown(per_model=50_000_000_000))
    assert module.collect_cost(home / ".copilot/session-state/s/events.jsonl")["costUsd"] == .5


def test_copilot_reads_the_checkpoint_written_after_each_turn(home):
    module = load_script("usage_copilot")
    copilot_events(home, "s", {"type": "session.start", "data": {"selectedModel": "gpt-test"}},
                   *COPILOT_TURN, copilot_checkpoint(634_300_000), {"type": "hook.end"})
    assert module.collect_cost(home / ".copilot/session-state/s/events.jsonl")["costUsd"] == .006343


def test_copilot_turn_after_the_latest_snapshot_is_pending(home, monkeypatch):
    module = load_script("usage_copilot")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    # The agent's own `flowctl complete` runs mid-turn, before that turn's checkpoint exists.
    copilot_events(home, "session", {"type": "session.start"}, *COPILOT_TURN, copilot_checkpoint(1),
                   {"type": "user.message"}, {"type": "assistant.turn_start"})
    assert module.main() == module.PENDING


def test_resumed_copilot_turn_is_pending_not_the_previous_shutdown(home, monkeypatch):
    module = load_script("usage_copilot")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    # Reporting the prior shutdown here would equal the resumed run's baseline: a permanent $0.
    copilot_events(home, "session", {"type": "session.start"}, copilot_shutdown(1_000_000_000),
                   {"type": "session.resume"}, *COPILOT_TURN)
    assert module.main() == module.PENDING


def test_resumed_copilot_session_before_any_turn_measures_the_baseline(home):
    module = load_script("usage_copilot")
    copilot_events(home, "s", {"type": "session.start"}, *COPILOT_TURN, copilot_shutdown(2_000_000_000),
                   {"type": "session.resume"})
    assert module.collect_cost(home / ".copilot/session-state/s/events.jsonl")["costUsd"] == .02


def test_copilot_snapshot_without_credits_is_unavailable(home, monkeypatch):
    module = load_script("usage_copilot")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    copilot_events(home, "session", copilot_shutdown(2_000_000_000), {"type": "session.resume"},
                   {"type": "session.shutdown", "data": {"modelMetrics": {}}})
    assert module.main() == 1


def test_cost_only_reports_need_a_cost_and_both_or_no_token_counts():
    assert UsageReport(model="m", source="s", costUsd=1).cached_input_tokens is None
    with pytest.raises(ValueError, match="token counts or a cost"):
        UsageReport(model="m", source="s")
    with pytest.raises(ValueError, match="or neither"):
        UsageReport(inputTokens=1, model="m", source="s", costUsd=1)
    assert UsageReport(inputTokens=1, outputTokens=1, model="m", source="s").cached_input_tokens == 0


def test_cost_only_session_usage_subtracts_the_cost_baseline():
    def cost(value):
        return UsageReport(model="m", source="s", scope="session", costUsd=value)
    assert run_usage(cost(3), cost(1).model_dump(by_alias=True)).cost == 2
    # Token counters cannot be subtracted from a cost-only baseline, so they become unknown.
    tokens = UsageReport(inputTokens=5, outputTokens=5, model="m", source="s", scope="session", costUsd=3)
    assert run_usage(tokens, cost(1).model_dump(by_alias=True)).input_tokens is None


def test_token_totals_cover_only_runs_that_report_tokens():
    runs = [Item(id=i) for i in range(2)]
    evidence = {0: Item(billing_mode="subscription", usage=report(100, 1).record("fixture")),
                1: Item(billing_mode="subscription",
                        usage=UsageReport(model="m", source="s", costUsd=2).record("fixture"))}
    summary = WorkflowEngine._usage_summary(runs, evidence)
    assert summary["runsWithUsage"] == 2 and summary["runsWithTokens"] == 1
    assert summary["totalTokens"] == 110 and summary["billedCostUsd"] == 3


def claude_transcript(home, session_id, model, usage):
    path = home / ".claude" / "projects" / "project" / f"{session_id}.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"type": "assistant", "message": {
        "id": "m1", "model": model, "usage": usage}}), encoding="utf-8")


def proxy_ledger(home, rows):
    import sqlite3
    connection = sqlite3.connect(home / ".local" / "share" / "copilot-api" / "copilot-api.sqlite")
    connection.execute("CREATE TABLE token_usage_events (session_id TEXT, total_cost_nanos INTEGER,"
                       " cost_currency TEXT)")
    connection.executemany("INSERT INTO token_usage_events VALUES (?, ?, ?)", rows)
    connection.commit()
    connection.close()


def test_claude_through_copilot_proxy_reports_billed_session_cost(home, monkeypatch, capsys):
    module = load_script("usage_claude")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    claude_transcript(home, "session", "gpt-test", {"input_tokens": 10, "output_tokens": 5})
    proxy_ledger(home, [("session", 2_000_000_000, "USD"), ("session", 500_000_000, "USD"),
                        ("other", 9_000_000_000, "USD")])
    assert module.main() == 0
    report = json.loads(capsys.readouterr().out)
    assert report["costUsd"] == 2.5 and report["costKind"] == "billed"


def test_claude_proxy_rows_without_cost_fall_back_to_estimate(home, monkeypatch, capsys):
    module = load_script("usage_claude")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    claude_transcript(home, "session", "gpt-test", {"input_tokens": 1000, "output_tokens": 100})
    proxy_ledger(home, [("session", 2_000_000_000, "USD"), ("session", None, None)])
    assert module.main() == 0
    report = json.loads(capsys.readouterr().out)
    assert report["costKind"] == "estimated"
    assert report["costUsd"] == pytest.approx((1000 * 2 + 100 * 10) / 1e6)


def test_claude_estimate_prices_long_context_and_one_hour_cache_writes(home):
    module = load_script("usage_claude")
    claude_transcript(home, "long", "gpt-test[1m]", {"input_tokens": 300_000, "output_tokens": 10})
    long, _ = module.collect(home / ".claude/projects/project/long.jsonl")
    assert long["costUsd"] == pytest.approx((300_000 * 4 + 10 * 15) / 1e6)
    claude_transcript(home, "cache", "claude-test-5.5", {"input_tokens": 0, "output_tokens": 0,
        "cache_creation_input_tokens": 300, "cache_creation": {"ephemeral_1h_input_tokens": 100}})
    cached, _ = module.collect(home / ".claude/projects/project/cache.jsonl")
    assert cached["costUsd"] == pytest.approx((200 * 5 + 100 * 4 * 2) / 1e6)


def test_codex_tokens_are_priced_as_an_estimate(home):
    module = load_script("usage_codex")
    rollout = home / "rollout.jsonl"
    rollout.write_text("\n".join(json.dumps(e) for e in [
        {"type": "turn_context", "payload": {"model": "gpt-test"}},
        {"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {
            "input_tokens": 1000, "cached_input_tokens": 600, "output_tokens": 50}}}}]), encoding="utf-8")
    report = module.collect(rollout)
    assert report["costKind"] == "estimated"
    assert report["costUsd"] == pytest.approx((400 * 2 + 600 * 0.2 + 50 * 10) / 1e6)


def test_billed_and_estimated_counters_are_never_subtracted_from_each_other():
    baseline = UsageReport(inputTokens=100, outputTokens=10, model="m", source="s", scope="session",
                           costUsd=1, costKind="estimated").model_dump(by_alias=True)
    current = UsageReport(inputTokens=150, outputTokens=10, model="m", source="s", scope="session",
                          costUsd=3, costKind="billed")
    assert run_usage(current, baseline).cost is None
    assert run_usage(current, {**baseline, "costKind": "billed"}).cost == 2


def test_cost_coverage_is_separate_from_token_coverage_and_subscription_spend():
    runs = [Item(id=i) for i in range(3)]
    evidence = {i: Item(billing_mode="subscription" if i == 2 else "metered",
                        usage=report(100, 1 if i != 1 else None).record("fixture")) for i in range(3)}
    summary = WorkflowEngine._usage_summary(runs, evidence)
    assert summary["runsWithUsage"] == 3
    assert summary["meteredRuns"] == 2
    assert summary["meteredRunsWithCost"] == 1
    assert summary["subscriptionRuns"] == 1
    assert summary["apiCostUsd"] == 1
    assert summary["billedCostUsd"] == 2 and summary["runsWithBilledCost"] == 2


def test_estimates_are_shown_apart_from_billed_cost_and_api_spend():
    def usage(cost, kind):
        return UsageReport(inputTokens=10, outputTokens=1, model="m", source="s",
                           costUsd=cost, costKind=kind).record("fixture")
    runs = [Item(id=i) for i in range(3)]
    evidence = {0: Item(billing_mode="subscription", usage=usage(2, "billed")),
                1: Item(billing_mode="subscription", usage=usage(3, "estimated")),
                2: Item(billing_mode="metered", usage=usage(4, "estimated"))}
    summary = WorkflowEngine._usage_summary(runs, evidence)
    assert summary["billedCostUsd"] == 2
    assert summary["estimatedCostUsd"] == 7 and summary["runsWithEstimatedCost"] == 2
    assert summary["meteredRunsWithCost"] == 0 and summary["apiCostUsd"] is None


def test_additive_billing_migration_preserves_old_records(tmp_path):
    from agentflow.database.database import Database
    from sqlalchemy import text, inspect
    database = Database(tmp_path / "legacy.db")
    database.create_all()
    with database.engine.begin() as connection:
        connection.exec_driver_sql("ALTER TABLE run_evidence DROP COLUMN billing_mode")
        connection.exec_driver_sql("ALTER TABLE tasks DROP COLUMN api_budget_usd")
    database.create_all()
    database.create_all()
    with database.engine.begin() as connection:
        assert "billing_mode" in {c["name"] for c in inspect(connection).get_columns("run_evidence")}
        assert "api_budget_usd" in {c["name"] for c in inspect(connection).get_columns("tasks")}


@pytest.mark.asyncio
async def test_late_exit_does_not_overwrite_accepted_or_human_corrected_usage(engine, monkeypatch):
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    engine.config.harnesses["codex"].usage_command = ["collector"]
    collector = AsyncMock()
    monkeypatch.setattr("agentflow.workflow.engine.collect_usage", collector)
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.harness = "codex"
        evidence = RunEvidence(run_id=run_id, usage=report(100).record("configured-provider-collector"))
        session.add(evidence)
        session.flush()
        await engine._collect_usage(session, engine._load_task(session, "TEST-1"), run)
        collector.assert_not_awaited()
        assert evidence.usage["inputTokens"] == 100


def copilot_cost(value):
    return UsageReport(model="gpt-test", source="copilot-cli-events", costUsd=value)


@pytest.mark.asyncio
async def test_pending_usage_is_collected_again_in_the_background(engine, monkeypatch):
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    engine.config.harnesses["copilot"].usage_command = ["collector"]
    monkeypatch.setattr("agentflow.workflow.engine.USAGE_RETRY_DELAYS", (0, 0))
    collector = AsyncMock(side_effect=[UsagePending("mid-turn"), UsagePending("mid-turn"), copilot_cost(.5)])
    monkeypatch.setattr("agentflow.workflow.engine.collect_usage", collector)
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.harness = "copilot"
        assert await engine._collect_usage(session, engine._load_task(session, "TEST-1"), run) == "pending"
    await engine._usage_retries[run_id]
    with engine.db.session() as session:
        usage = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run_id)).usage
    assert usage["costUsd"] == .5 and usage["totalTokens"] is None
    assert collector.await_count == 3 and run_id not in engine._usage_retries


def copilot_agent(engine, run_id, screen_state):
    engine.config.harnesses["copilot"].usage_command = ["collector"]
    adapter = SimpleNamespace(exit_command="/exit", classify_screen=lambda viewport: screen_state)
    engine.harnesses.get = lambda key: adapter
    engine.zellij.screen = AsyncMock(return_value=["❯"])
    engine.zellij.list_panes.return_value = [SimpleNamespace(tab_id=7)]
    with engine.db.session() as session:
        session.get(NodeRun, run_id).harness = "copilot"
        session.add(AgentSession(task_id=session.get(NodeRun, run_id).task_id, role="implementer",
                                 harness="copilot", zellij_tab_name="Implementer", zellij_tab_id=7,
                                 zellij_pane_id="terminal_7", current_run_id=run_id))


@pytest.mark.asyncio
async def test_stopping_an_idle_copilot_agent_exits_it_and_collects_its_cost(engine, monkeypatch):
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    copilot_agent(engine, run_id, "idle")
    order = []
    engine.zellij.write_line.side_effect = lambda *args: order.append(("write", args[2]))
    engine.zellij.close_tab.side_effect = lambda *args: order.append(("close", args[1]))
    # Copilot writes its shutdown snapshot a moment after /exit.
    collector = AsyncMock(side_effect=[UsagePending("exiting"), copilot_cost(.25)])
    monkeypatch.setattr("agentflow.workflow.engine.collect_usage", collector)
    await engine.cancel_task("TEST-1")
    assert order == [("write", "/exit"), ("close", 7)]
    with engine.db.session() as session:
        usage = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run_id)).usage
    assert usage["costUsd"] == .25 and not engine._usage_retries


@pytest.mark.asyncio
@pytest.mark.parametrize("screen_state", ["working", "blocked", "unknown"])
async def test_a_busy_or_prompting_copilot_agent_is_closed_without_typing(engine, monkeypatch, screen_state):
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    copilot_agent(engine, run_id, screen_state)
    collector = AsyncMock()
    monkeypatch.setattr("agentflow.workflow.engine.collect_usage", collector)
    await engine.cancel_task("TEST-1")
    engine.zellij.write_line.assert_not_awaited()
    collector.assert_not_awaited()
    engine.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)


@pytest.mark.asyncio
async def test_a_worker_inside_its_completion_call_is_not_asked_to_exit(engine):
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    copilot_agent(engine, run_id, "idle")
    with engine.db.session() as session:
        await engine._stop_sessions(session, engine._load_task(session, "TEST-1"), graceful=False)
    engine.zellij.screen.assert_not_awaited()
    engine.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)


# Copilot CLI 1.0.89 screens, trimmed from a real pane.
COPILOT_FOOTER_IDLE = " ← open sidebar · / commands · ? help · tab next tab        GitHub Copilot • GPT-5.6 Luna"


@pytest.mark.parametrize("footer, state", [
    (COPILOT_FOOTER_IDLE, "idle"),
    (" ◉ Working · 141 B esc interrupt                                GitHub Copilot • GPT-5.6 Luna", "working"),
    (" ◉ Loading: 11 instructions, 9 agents, 2 plugins                GitHub Copilot • GPT-5.6 Luna", "working"),
    ("│ ❯ 1. Yes\n│   3. No (Esc)\n│ ↑/↓ to navigate · enter to select · esc to cancel", "blocked"),
    # ask_user: the agent is waiting on a human inside the pane, so it must raise attention
    # rather than look idle (its empty input line alone matches the idle pattern).
    ("Copilot needs information.\nPaste prompt.md or provide an in-workspace copy path.\n>\n"
     "enter accept · ctrl+d decline · esc cancel", "blocked"),
])
def test_copilot_screen_states(footer, state):
    from agentflow.config import HarnessConfig
    from agentflow.harnesses.copilot import CopilotAdapter
    screen = [" D:\Dev\AgentFlow [⎇ main]                     Session: 0.63 AIC used", "─" * 40, "❯", "─" * 40,
              *footer.splitlines()]
    assert CopilotAdapter("copilot", HarnessConfig(command="copilot")).classify_screen(screen) == state


def test_usage_command_resolves_python_and_install_relative_scripts():
    import sys
    from agentflow.workflow.usage import SCRIPTS_DIR, resolve_usage_command
    resolved = resolve_usage_command(["python", "scripts/usage_copilot.py", "--flag"])
    assert resolved == [sys.executable, (SCRIPTS_DIR / "usage_copilot.py").as_posix(), "--flag"]
    assert resolve_usage_command(["C:/tools/collector.exe", "report.json"]) == ["C:/tools/collector.exe", "report.json"]


@pytest.mark.asyncio
@pytest.mark.parametrize("harness, adapter_type", [("claude", "ClaudeAdapter"), ("codex", "CodexAdapter")])
async def test_claude_and_codex_tabs_are_closed_as_before(engine, monkeypatch, harness, adapter_type):
    from agentflow.config import HarnessConfig
    from agentflow.harnesses import claude, codex
    adapter_class = getattr(claude if harness == "claude" else codex, adapter_type)
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    engine.config.harnesses[harness].usage_command = ["collector"]
    engine.harnesses.get = lambda key: adapter_class(key, HarnessConfig(type=harness, command=harness))
    engine.zellij.screen = AsyncMock()
    engine.zellij.list_panes.return_value = [SimpleNamespace(tab_id=7)]
    collector = AsyncMock()
    monkeypatch.setattr("agentflow.workflow.engine.collect_usage", collector)
    with engine.db.session() as session:
        session.get(NodeRun, run_id).harness = harness
        session.add(AgentSession(task_id=session.get(NodeRun, run_id).task_id, role="implementer", harness=harness,
                                 zellij_tab_name="Implementer", zellij_tab_id=7, zellij_pane_id="terminal_7",
                                 current_run_id=run_id))
    await engine.cancel_task("TEST-1")
    engine.zellij.screen.assert_not_awaited()
    engine.zellij.write_line.assert_not_awaited()
    collector.assert_not_awaited()
    engine.zellij.close_tab.assert_awaited_once_with("agentflow-TEST-1", 7)


def claude_turn(path, message_id, stop_reason, output_tokens):
    with path.open("a", encoding="utf-8") as f:
        f.write(json.dumps({"type": "assistant", "message": {"id": message_id, "model": "gpt-test",
            "stop_reason": stop_reason, "usage": {"input_tokens": 100, "output_tokens": output_tokens}}}) + "\n")


def test_claude_turn_inside_a_tool_call_reports_usage_so_far_as_pending(home, monkeypatch, capsys):
    module = load_script("usage_claude")
    monkeypatch.setenv("AGENTFLOW_SESSION_ID", "session")
    path = home / ".claude" / "projects" / "project" / "session.jsonl"
    path.parent.mkdir(parents=True)
    # The agent's `flowctl complete` is a tool call: its turn continues after it returns.
    claude_turn(path, "m1", "tool_use", 10)
    assert module.main() == module.PENDING
    assert json.loads(capsys.readouterr().out)["outputTokens"] == 10
    claude_turn(path, "m2", "end_turn", 5)
    assert module.main() == 0
    assert json.loads(capsys.readouterr().out)["outputTokens"] == 15


@pytest.mark.asyncio
async def test_partial_claude_usage_is_recorded_then_replaced_when_the_turn_ends(engine, monkeypatch):
    run_id = add_run(engine, wf.IMPLEMENTER, state="COMPLETED")
    engine.config.harnesses["claude"].usage_command = ["collector"]
    monkeypatch.setattr("agentflow.workflow.engine.USAGE_RETRY_DELAYS", (0,))
    so_far = UsageReport(inputTokens=100, outputTokens=10, model="m", source="s", costUsd=1)
    final = UsageReport(inputTokens=150, outputTokens=20, model="m", source="s", costUsd=1.5)
    monkeypatch.setattr("agentflow.workflow.engine.collect_usage",
                        AsyncMock(side_effect=[UsagePending("open turn", so_far), final]))
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.harness = "claude"
        assert await engine._collect_usage(session, engine._load_task(session, "TEST-1"), run) == "pending"
        usage = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run_id)).usage
        assert usage["outputTokens"] == 10 and usage["partial"] is True
    await engine._usage_retries[run_id]
    with engine.db.session() as session:
        usage = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run_id)).usage
    assert usage["outputTokens"] == 20 and usage["costUsd"] == 1.5 and "partial" not in usage


@pytest.mark.asyncio
async def test_resumed_baseline_uses_the_usage_a_cut_off_turn_left(monkeypatch):
    from agentflow.workflow.usage import prepare_baseline
    so_far = UsageReport(inputTokens=100, outputTokens=10, model="m", source="s", scope="session")
    monkeypatch.setattr("agentflow.workflow.usage.collect_usage", AsyncMock(side_effect=UsagePending("open", so_far)))
    assert (await prepare_baseline(["collector"], Path("."), 1, "session", True))["outputTokens"] == 10
    monkeypatch.setattr("agentflow.workflow.usage.collect_usage", AsyncMock(side_effect=UsagePending("open")))
    assert await prepare_baseline(["collector"], Path("."), 1, "session", True) is None
