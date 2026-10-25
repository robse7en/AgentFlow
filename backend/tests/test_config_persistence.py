"""Settings saves and reload share one serialization (AF-AUDIT-001): overlapping saves to
different fields keep every change, a failed save changes nothing, and a reload cannot put
back a configuration that a later save replaced."""

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

import agentflow.main as main
from agentflow.api.config import (CostSettingsRequest, SetDefaultRepositoryRequest, SetExchangeRateRequest,
                                  reload_config, set_cost_controls, set_default_repository, set_exchange_rate)
from agentflow.api.deps import AppState
from agentflow.config import SKIP_JIRA_CHECK, Config, CostControls


@pytest.fixture
def state(tmp_path):
    # Everything the file names lives under tmp_path: reload's load_config creates those folders.
    raw = {
        "app": {"dataDirectory": str(tmp_path / "data"), "databasePath": str(tmp_path / "data" / "test.db")},
        "workspace": {"taskDirectory": str(tmp_path / "tasks"), "worktreeDirectory": str(tmp_path / "trees")},
    }
    config_file = tmp_path / "config.json"
    config_file.write_text(json.dumps(raw), encoding="utf-8")
    return AppState(config=Config.model_validate(raw), config_file=config_file, db=None, git=None, zellij=None,
                    harnesses=SimpleNamespace(capabilities={}), broadcaster=None,
                    engine=SimpleNamespace(_launch_lock=asyncio.Lock()), system={})


def saved(state) -> Config:
    return Config.model_validate_json(state.config_file.read_text(encoding="utf-8"))


async def settle():
    """Let every started request run until it waits on a lock (no I/O is awaited here)."""
    for _ in range(10):
        await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_overlapping_saves_to_different_fields_keep_both(state, tmp_path):
    repo = str(tmp_path / "Repo")
    before = state.config_file.read_text(encoding="utf-8")
    async with state.config_lock:
        rate = asyncio.create_task(set_exchange_rate(SetExchangeRateRequest(usdToNzdRate=2.5), state))
        repository = asyncio.create_task(set_default_repository(SetDefaultRepositoryRequest(path=repo), state))
        await settle()
        # Both wait for the shared lock before reading the file or touching live state.
        assert not rate.done() and not repository.done()
        assert state.config_file.read_text(encoding="utf-8") == before
        assert state.config.workflow.usd_to_nzd_rate == 1.65
    await asyncio.gather(rate, repository)
    assert saved(state).workflow.usd_to_nzd_rate == 2.5
    assert saved(state).workspace.default_repository_directory == repo
    assert state.config.workflow.usd_to_nzd_rate == 2.5
    assert state.config.workspace.default_repository_directory == repo


@pytest.mark.asyncio
async def test_cost_controls_and_ordinary_saves_cannot_lose_policy_changes(state, tmp_path):
    """Cost controls take the shared lock before the launch lock, so a launch holding the
    launch lock delays the other saves too instead of letting them write a stale copy."""
    repo = str(tmp_path / "Repo")
    async with state.engine._launch_lock:  # a launch is deciding admission
        cost = asyncio.create_task(set_cost_controls(CostSettingsRequest(
            controls=CostControls(dailyBudgetUsd=20), billing={"copilot": "subscription"}), state))
        await settle()
        rate = asyncio.create_task(set_exchange_rate(SetExchangeRateRequest(usdToNzdRate=2.5), state))
        repository = asyncio.create_task(set_default_repository(SetDefaultRepositoryRequest(path=repo), state))
        await settle()
        assert not any(task.done() for task in (cost, rate, repository))
        assert state.config.workflow.cost_controls.daily_budget_usd is None
    await asyncio.gather(cost, rate, repository)
    on_disk = saved(state)
    assert on_disk.workflow.cost_controls.daily_budget_usd == 20
    assert on_disk.harnesses["copilot"].billing_mode == "subscription"
    assert on_disk.workflow.usd_to_nzd_rate == 2.5
    assert on_disk.workspace.default_repository_directory == repo
    assert state.config.workflow.cost_controls.daily_budget_usd == 20
    assert state.config.harnesses["copilot"].billing_mode == "subscription"
    assert state.config.workflow.usd_to_nzd_rate == 2.5


@pytest.mark.asyncio
async def test_reload_cannot_restore_an_older_configuration_after_a_newer_save(state, monkeypatch):
    monkeypatch.setenv(SKIP_JIRA_CHECK, "1")
    building = asyncio.Event()
    release = asyncio.Event()

    async def slow_build_state(config, config_file, broadcaster, existing_db=None):
        building.set()
        await release.wait()  # harness detection, still working from the file it read
        return SimpleNamespace(config=config, git=None, zellij=None,
                               harnesses=SimpleNamespace(capabilities={}), system={})

    monkeypatch.setattr(main, "build_state", slow_build_state)
    reload = asyncio.create_task(reload_config(state))
    await building.wait()
    save = asyncio.create_task(set_exchange_rate(SetExchangeRateRequest(usdToNzdRate=2.5), state))
    await settle()
    assert not save.done()  # the save waits for the reload instead of being undone by it
    release.set()
    await asyncio.gather(reload, save)
    assert state.config.workflow.usd_to_nzd_rate == 2.5
    assert state.engine.config is state.config
    assert saved(state).workflow.usd_to_nzd_rate == 2.5


@pytest.mark.asyncio
async def test_reload_swaps_configuration_under_the_launch_lock(state, monkeypatch):
    monkeypatch.setenv(SKIP_JIRA_CHECK, "1")

    async def build_state(config, config_file, broadcaster, existing_db=None):
        return SimpleNamespace(config=config, git=None, zellij=None,
                               harnesses=SimpleNamespace(capabilities={}), system={})

    monkeypatch.setattr(main, "build_state", build_state)
    old = state.config
    async with state.engine._launch_lock:  # a launch reading the policy
        reload = asyncio.create_task(reload_config(state))
        await settle()
        assert not reload.done() and state.config is old
    await reload
    assert state.config is not old and state.engine.config is state.config


SAVES = {
    "repository": (lambda state: set_default_repository(SetDefaultRepositoryRequest(path="D:/Elsewhere"), state),
                   lambda config: config.workspace.default_repository_directory),
    "exchange-rate": (lambda state: set_exchange_rate(SetExchangeRateRequest(usdToNzdRate=2.5), state),
                      lambda config: config.workflow.usd_to_nzd_rate),
    "cost-controls": (lambda state: set_cost_controls(CostSettingsRequest(
                          controls=CostControls(dailyBudgetUsd=20), billing={"copilot": "subscription"}), state),
                      lambda config: (config.workflow.cost_controls.daily_budget_usd,
                                      config.harnesses["copilot"].billing_mode)),
}


@pytest.mark.asyncio
@pytest.mark.parametrize("name", SAVES)
@pytest.mark.parametrize("failing", ["write", "replace"])
async def test_failed_save_keeps_the_prior_configuration_and_reports_failure(state, monkeypatch, name, failing):
    save, field = SAVES[name]
    before_text = state.config_file.read_text(encoding="utf-8")
    before_live = field(state.config)
    real = {"write": Path.write_text, "replace": Path.replace}[failing]

    def fail(self, *args, **kwargs):
        if failing == "replace":
            raise PermissionError(13, "Access is denied")  # e.g. another program has the file open
        if self.name.endswith(".tmp"):
            real(self, '{"workflow": {', encoding="utf-8")  # interrupted part-way
            raise OSError(28, "No space left on device")
        return real(self, *args, **kwargs)

    monkeypatch.setattr(Path, "write_text" if failing == "write" else "replace", fail)
    with pytest.raises(HTTPException) as refused:
        await save(state)
    monkeypatch.undo()
    assert refused.value.status_code == 500 and "config.json" in refused.value.detail
    assert state.config_file.read_text(encoding="utf-8") == before_text
    assert field(state.config) == before_live
    assert [p.name for p in state.config_file.parent.iterdir() if p.name.endswith(".tmp")] == []
    assert not state.config_lock.locked()


@pytest.mark.asyncio
async def test_rejected_cost_settings_change_neither_file_nor_live_state(state):
    before_text = state.config_file.read_text(encoding="utf-8")
    with pytest.raises(HTTPException) as refused:
        await set_cost_controls(CostSettingsRequest(controls=CostControls(dailyBudgetUsd=20),
                                                    billing={"copilot": "not-a-mode"}), state)
    assert refused.value.status_code == 422
    assert state.config_file.read_text(encoding="utf-8") == before_text
    assert state.config.workflow.cost_controls.daily_budget_usd is None
    assert state.config.harnesses["copilot"].billing_mode != "not-a-mode"


@pytest.mark.asyncio
async def test_saves_reload_and_launches_finish_together_without_deadlock(state, monkeypatch):
    """Lock order is config lock, then launch lock; launches never take the config lock."""
    monkeypatch.setenv(SKIP_JIRA_CHECK, "1")

    async def build_state(config, config_file, broadcaster, existing_db=None):
        await asyncio.sleep(0)
        return SimpleNamespace(config=config, git=None, zellij=None,
                               harnesses=SimpleNamespace(capabilities={}), system={})

    monkeypatch.setattr(main, "build_state", build_state)
    launches = []

    async def launch():
        async with state.engine._launch_lock:
            launches.append(state.config.workflow.cost_controls.daily_budget_usd)
            await asyncio.sleep(0)

    work = [launch(), set_cost_controls(CostSettingsRequest(controls=CostControls(dailyBudgetUsd=20), billing={}), state),
            reload_config(state), launch(), set_exchange_rate(SetExchangeRateRequest(usdToNzdRate=2.5), state),
            reload_config(state), launch()]
    # A deadlock guard, not a timing assertion: everything here is in-process and immediate.
    await asyncio.wait_for(asyncio.gather(*work), timeout=10)
    assert len(launches) == 3
    assert saved(state).workflow.cost_controls.daily_budget_usd == 20
    assert state.config.workflow.cost_controls.daily_budget_usd == 20
    assert state.config.workflow.usd_to_nzd_rate == 2.5
