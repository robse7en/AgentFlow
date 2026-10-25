import json
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from agentflow.api.config import SetDefaultRepositoryRequest, set_default_repository
from agentflow.api.deps import AppState
from agentflow.config import Config
from agentflow.config import CostControls
from agentflow.api.config import CostSettingsRequest, set_cost_controls
import asyncio


@pytest.fixture
def state(tmp_path):
    config_file = tmp_path / "config.json"
    config_file.write_text("{}", encoding="utf-8")
    config = Config()
    harnesses = SimpleNamespace(capabilities={})
    return AppState(config=config, config_file=config_file, db=None, git=None, zellij=None,
                     harnesses=harnesses, broadcaster=None, engine=None, system={})


@pytest.mark.asyncio
async def test_set_default_repository_persists_to_disk_and_state(state, tmp_path):
    repo = str(tmp_path / "MyRepo")
    result = await set_default_repository(SetDefaultRepositoryRequest(path=repo), state)
    assert result["defaultRepositoryDirectory"] == repo
    assert state.config.workspace.default_repository_directory == repo
    on_disk = json.loads(state.config_file.read_text(encoding="utf-8"))
    assert on_disk["workspace"]["defaultRepositoryDirectory"] == repo


@pytest.mark.asyncio
async def test_set_default_repository_trims_and_clears(state, tmp_path):
    repo = str(tmp_path / "MyRepo")
    await set_default_repository(SetDefaultRepositoryRequest(path=f"  {repo}  "), state)
    assert state.config.workspace.default_repository_directory == repo

    result = await set_default_repository(SetDefaultRepositoryRequest(path=None), state)
    assert result["defaultRepositoryDirectory"] is None
    assert state.config.workspace.default_repository_directory is None
    on_disk = json.loads(state.config_file.read_text(encoding="utf-8"))
    assert on_disk["workspace"]["defaultRepositoryDirectory"] is None


@pytest.mark.asyncio
async def test_set_default_repository_rejects_invalid_config(state):
    state.config_file.write_text(json.dumps({"server": {"host": "not-a-loopback-host"}}), encoding="utf-8")
    with pytest.raises(HTTPException) as excinfo:
        await set_default_repository(SetDefaultRepositoryRequest(path="/tmp/x"), state)
    assert excinfo.value.status_code == 400


@pytest.mark.asyncio
async def test_cost_controls_persist_without_changing_role_assignments(state):
    state.engine = SimpleNamespace(_launch_lock=asyncio.Lock())
    before = state.config.agents.model_dump()
    await set_cost_controls(CostSettingsRequest(controls=CostControls(dailyBudgetUsd=20),
                            billing={"copilot": "subscription"}), state)
    assert state.config.workflow.cost_controls.daily_budget_usd == 20
    assert state.config.harnesses["copilot"].billing_mode == "subscription"
    assert state.config.agents.model_dump() == before
    loaded = Config.model_validate_json(state.config_file.read_text())
    assert loaded.harnesses["copilot"].billing_mode == "subscription"


@pytest.mark.asyncio
async def test_billing_edit_keeps_the_default_harnesses_a_file_relies_on(state):
    """A config.json without a harnesses section relies on the defaults; saving one harness's
    billing mode must not leave a file whose agents name harnesses it no longer has."""
    from agentflow.config import read_config

    state.engine = SimpleNamespace(_launch_lock=asyncio.Lock())
    await set_cost_controls(CostSettingsRequest(controls=CostControls(), billing={"copilot": "subscription"}), state)
    saved = json.loads(state.config_file.read_text(encoding="utf-8"))
    assert sorted(saved["harnesses"]) == sorted(Config().harnesses)
    loaded = read_config(state.config_file)  # validates the agents' harness references too
    assert loaded.harnesses["copilot"].billing_mode == "subscription"
    assert loaded.harnesses["claude"].billing_mode == Config().harnesses["claude"].billing_mode
