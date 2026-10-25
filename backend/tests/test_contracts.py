import json
from pathlib import Path

import pytest

import agentflow.config as config_module
from agentflow.config import Config, ConfigError, load_config
from agentflow.workflow.models import FinalAssessment, Finding, ReviewResult


def test_example_config_is_valid():
    raw = json.loads((Path(__file__).resolve().parents[2] / "config.example.json").read_text())
    config = Config.model_validate(raw)
    config.validate_references()
    assert config.agents.implementer.harness == "codex"


def test_unknown_harness_reference_rejected(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"agents": {"architect": {"harness": "nope"}}}))
    with pytest.raises(ConfigError):
        load_config(path)


def test_unknown_key_rejected(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"workflow": {"maxReviewIteration": 3}}))
    with pytest.raises(ConfigError):
        load_config(path)


def _fake_checkout(monkeypatch, tmp_path):
    project_root = tmp_path / "AgentFlow"
    package_dir = project_root / "backend" / "agentflow"
    package_dir.mkdir(parents=True)
    (project_root / "backend" / "pyproject.toml").write_text("[project]\n")
    monkeypatch.setattr(config_module, "__file__", str(package_dir / "config.py"))
    return project_root


def test_default_config_written_on_first_launch_keeps_data_in_the_checkout(monkeypatch, tmp_path):
    project_root = _fake_checkout(monkeypatch, tmp_path)
    path = tmp_path / "config.json"
    config = load_config(path)
    assert path.exists() and config.workflow.max_review_iterations == 5
    raw = json.loads(path.read_text())
    assert raw["app"] == {"dataDirectory": "data/.agentflow", "databasePath": "data/.agentflow/agentflow.db"}
    assert raw["workspace"]["worktreeDirectory"] == "data/.agentflow/worktrees"
    data = project_root / "data" / ".agentflow"
    assert config.data_dir == data and config.database_file == data / "agentflow.db"
    assert config.worktree_dir == data / "worktrees" and config.task_dir == data / "tasks"
    assert config.worktree_dir.is_dir() and config.task_dir.is_dir()


def test_installed_copies_keep_data_in_the_home_directory(monkeypatch, tmp_path):
    monkeypatch.setattr(config_module, "source_checkout", lambda: None)
    assert config_module.fresh_config().app.data_directory == "~/.agentflow"


def test_an_agentflow_home_instance_does_not_share_the_checkout_data(monkeypatch, tmp_path):
    _fake_checkout(monkeypatch, tmp_path)
    monkeypatch.setenv("AGENTFLOW_HOME", str(tmp_path / "other"))
    assert config_module.fresh_config().app.data_directory == "~/.agentflow"


def test_relative_paths_are_relative_to_the_agentflow_folder_not_the_current_directory(monkeypatch, tmp_path):
    project_root = _fake_checkout(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    config = Config.model_validate({"app": {"dataDirectory": "data/x"}})
    assert config.data_dir == project_root / "data" / "x"
    assert Config().data_dir == Path.home() / ".agentflow"  # a file that omits it keeps the old default


def test_project_config_is_default_in_source_checkout(monkeypatch, tmp_path):
    monkeypatch.delenv("AGENTFLOW_CONFIG", raising=False)
    monkeypatch.delenv("AGENTFLOW_HOME", raising=False)
    project_root = tmp_path / "AgentFlow"
    package_dir = project_root / "backend" / "agentflow"
    package_dir.mkdir(parents=True)
    (project_root / "backend" / "pyproject.toml").write_text("[project]\n")
    project_config = project_root / "config.json"
    project_config.write_text("{}")
    monkeypatch.setattr(config_module, "__file__", str(package_dir / "config.py"))

    assert config_module.config_path() == project_config


def test_a_fresh_checkout_puts_config_in_the_agentflow_folder(monkeypatch, tmp_path):
    monkeypatch.delenv("AGENTFLOW_CONFIG", raising=False)
    monkeypatch.delenv("AGENTFLOW_HOME", raising=False)
    monkeypatch.setattr(config_module.Path, "home", lambda: tmp_path / "user")
    project_root = _fake_checkout(monkeypatch, tmp_path)

    assert config_module.config_path() == project_root / "config.json"


def test_an_existing_home_config_is_still_used_until_the_checkout_has_one(monkeypatch, tmp_path):
    monkeypatch.delenv("AGENTFLOW_CONFIG", raising=False)
    monkeypatch.delenv("AGENTFLOW_HOME", raising=False)
    monkeypatch.setattr(config_module.Path, "home", lambda: tmp_path / "user")
    home_config = tmp_path / "user" / ".agentflow" / "config.json"
    home_config.parent.mkdir(parents=True)
    home_config.write_text("{}")
    project_root = _fake_checkout(monkeypatch, tmp_path)

    assert config_module.config_path() == home_config
    (project_root / "config.json").write_text("{}")
    assert config_module.config_path() == project_root / "config.json"


def test_installed_copies_keep_config_in_the_home_directory(monkeypatch, tmp_path):
    monkeypatch.delenv("AGENTFLOW_CONFIG", raising=False)
    monkeypatch.delenv("AGENTFLOW_HOME", raising=False)
    monkeypatch.setattr(config_module.Path, "home", lambda: tmp_path / "user")
    monkeypatch.setattr(config_module, "source_checkout", lambda: None)

    assert config_module.config_path() == tmp_path / "user" / ".agentflow" / "config.json"


def test_explicit_config_environment_variable_wins(monkeypatch, tmp_path):
    expected = tmp_path / "custom.json"
    monkeypatch.setenv("AGENTFLOW_CONFIG", str(expected))
    monkeypatch.setenv("AGENTFLOW_HOME", str(tmp_path / "home"))

    assert config_module.config_path() == expected


def test_agentflow_home_overrides_project_config(monkeypatch, tmp_path):
    home = tmp_path / "home"
    monkeypatch.delenv("AGENTFLOW_CONFIG", raising=False)
    monkeypatch.setenv("AGENTFLOW_HOME", str(home))

    assert config_module.config_path() == home / "config.json"


def test_finding_blocking_defaults_from_severity():
    assert Finding(severity="high", title="x").is_blocking()
    assert not Finding(severity="low", title="x").is_blocking()
    assert not Finding(severity="critical", blocking=False, title="x").is_blocking()
    assert Finding(severity="Major", title="x").severity == "high"


def test_review_result_camel_case():
    result = ReviewResult.model_validate({"status": "completed", "reviewedSha": "abc", "verdict": "approved", "findings": []})
    assert result.reviewed_sha == "abc"


def test_final_assessment_risk_normalised():
    assessment = FinalAssessment.model_validate({"status": "completed", "overallRisk": "low"})
    assert assessment.overall_risk == "LOW"
