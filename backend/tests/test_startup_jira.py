import json
import os
from types import SimpleNamespace

import pytest
import uvicorn
from fastapi import HTTPException

from agentflow.api.config import reload_config
from agentflow.api.deps import AppState
from agentflow.config import SKIP_JIRA_CHECK, Config, ConfigError, JiraConfig, jira_startup_problem
from agentflow.main import create_app, run

CLOUD = {"enabled": True, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t"}
SERVER = {"enabled": True, "baseUrl": "https://jira.example.com", "apiToken": "pat"}


@pytest.fixture(autouse=True)
def jira_check_on(monkeypatch):
    # setenv first so monkeypatch restores whatever run(--skip-jira-check) writes.
    monkeypatch.setenv(SKIP_JIRA_CHECK, "0")


@pytest.fixture
def started(monkeypatch):
    calls = []
    monkeypatch.setattr(uvicorn, "run", lambda *args, **kwargs: calls.append((args, kwargs)))
    return calls


def _config_file(tmp_path, jira=None):
    data = tmp_path / "data"
    raw = {"app": {"dataDirectory": str(data), "databasePath": str(data / "agentflow.db")},
           "workspace": {"worktreeDirectory": str(data / "worktrees"), "taskDirectory": str(data / "tasks")}}
    if jira:
        raw["jira"] = jira
    path = tmp_path / "config.json"
    path.write_text(json.dumps(raw), encoding="utf-8")
    return path


@pytest.mark.parametrize("jira", [CLOUD, SERVER])
def test_configured_jira_passes(jira):
    assert jira_startup_problem(Config(jira=JiraConfig(**jira))) is None


def test_missing_jira_settings_are_named():
    problem = jira_startup_problem(Config(jira=JiraConfig(enabled=True, baseUrl="https://x.atlassian.net")))
    assert "jira.apiToken" in problem and "agentflow setup" in problem


def test_disabled_jira_allows_local_use():
    assert jira_startup_problem(Config()) is None
    assert jira_startup_problem(Config(jira=JiraConfig(baseUrl="https://x.atlassian.net"))) is None


def test_the_environment_variable_skips_the_check(monkeypatch):
    monkeypatch.setenv(SKIP_JIRA_CHECK, "1")
    assert jira_startup_problem(Config(jira=JiraConfig(enabled=True))) is None


def test_backend_starts_without_jira(tmp_path, started):
    run(["--config", str(_config_file(tmp_path))])
    assert len(started) == 1


def test_backend_refuses_to_start_with_incomplete_jira(tmp_path, started, capsys):
    with pytest.raises(SystemExit) as exited:
        run(["--config", str(_config_file(tmp_path, {"enabled": True}))])
    assert exited.value.code == 2 and not started
    err = capsys.readouterr().err
    assert "Jira is enabled but not configured" in err and "agentflow setup" in err and "--skip-jira-check" in err


def test_backend_starts_with_jira_configured(tmp_path, started):
    run(["--config", str(_config_file(tmp_path, CLOUD))])
    assert len(started) == 1


def test_skip_flag_starts_and_carries_over_to_reloads(tmp_path, started):
    run(["--config", str(_config_file(tmp_path)), "--skip-jira-check"])
    assert len(started) == 1
    # The --reload worker and POST /api/config/reload read it from the environment.
    assert os.environ[SKIP_JIRA_CHECK] == "1"


def test_reload_worker_uses_the_config_that_was_checked(tmp_path, started, monkeypatch):
    monkeypatch.setenv("AGENTFLOW_CONFIG", "unused")  # restored after the test
    path = _config_file(tmp_path, CLOUD)
    run(["--config", str(path), "--reload"])
    assert started[0][1]["factory"] is True
    assert os.environ["AGENTFLOW_CONFIG"] == str(path)


def test_uvicorn_factory_path_allows_local_tasks(tmp_path):
    create_app(config_file=_config_file(tmp_path))


def test_uvicorn_factory_path_refuses_incomplete_jira(tmp_path):
    with pytest.raises(ConfigError, match="Jira is enabled but not configured"):
        create_app(config_file=_config_file(tmp_path, {"enabled": True}))


def test_tests_building_an_app_from_a_bare_config_are_unaffected(tmp_path):
    create_app(Config(), tmp_path / "config.json")


@pytest.mark.asyncio
async def test_config_reload_allows_removing_jira(tmp_path, monkeypatch):
    import asyncio
    import agentflow.main as main
    config_file = _config_file(tmp_path)
    from agentflow.config import load_config
    config = load_config(config_file)
    old = config.model_copy(update={"jira": JiraConfig(**CLOUD)})
    engine = SimpleNamespace(_launch_lock=asyncio.Lock())
    state = AppState(config=old, config_file=config_file, db=None, git=None,
                     zellij=None, harnesses=SimpleNamespace(capabilities={}), broadcaster=None, engine=engine,
                     system={})
    async def build(*args, **kwargs):
        return SimpleNamespace(config=config, git=None, zellij=None,
                               harnesses=SimpleNamespace(capabilities={}), system={})
    monkeypatch.setattr(main, "build_state", build)
    await reload_config(state)
    assert not state.config.jira.enabled


@pytest.mark.asyncio
async def test_config_reload_refuses_incomplete_jira(tmp_path):
    config_file = _config_file(tmp_path, {"enabled": True, "baseUrl": "https://x.atlassian.net"})
    state = AppState(config=Config(jira=JiraConfig(**CLOUD)), config_file=config_file, db=None, git=None,
                     zellij=None, harnesses=SimpleNamespace(capabilities={}), broadcaster=None, engine=None,
                     system={})
    with pytest.raises(HTTPException) as refused:
        await reload_config(state)
    assert refused.value.status_code == 400 and "jira.apiToken" in refused.value.detail
