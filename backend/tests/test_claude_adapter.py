import json
from pathlib import Path

from agentflow.harnesses.claude import ClaudeAdapter, trust_workspace


def test_trust_workspace_is_a_noop_when_no_claude_config_exists(tmp_path, monkeypatch):
    monkeypatch.setattr("agentflow.harnesses.claude._claude_config_path", lambda: tmp_path / "missing" / ".claude.json")
    trust_workspace(tmp_path / "worktree")  # must not raise


def test_trust_workspace_adds_a_new_project_entry(tmp_path, monkeypatch):
    config_path = tmp_path / ".claude.json"
    config_path.write_text(json.dumps({"projects": {"C:/Other/Project": {"hasTrustDialogAccepted": True}}}), encoding="utf-8")
    monkeypatch.setattr("agentflow.harnesses.claude._claude_config_path", lambda: config_path)

    worktree = Path("D:/agentflow/worktrees/DE-1")
    trust_workspace(worktree)

    data = json.loads(config_path.read_text(encoding="utf-8"))
    assert data["projects"][worktree.as_posix()]["hasTrustDialogAccepted"] is True
    # The unrelated existing project entry must survive untouched.
    assert data["projects"]["C:/Other/Project"]["hasTrustDialogAccepted"] is True


def test_trust_workspace_preserves_other_fields_on_an_existing_entry(tmp_path, monkeypatch):
    config_path = tmp_path / ".claude.json"
    worktree = Path("D:/agentflow/worktrees/DE-1")
    config_path.write_text(json.dumps({
        "projects": {worktree.as_posix(): {"hasTrustDialogAccepted": False, "allowedTools": ["Bash"]}}
    }), encoding="utf-8")
    monkeypatch.setattr("agentflow.harnesses.claude._claude_config_path", lambda: config_path)

    trust_workspace(worktree)

    data = json.loads(config_path.read_text(encoding="utf-8"))
    entry = data["projects"][worktree.as_posix()]
    assert entry["hasTrustDialogAccepted"] is True
    assert entry["allowedTools"] == ["Bash"]


def test_trust_workspace_skips_writing_when_already_trusted(tmp_path, monkeypatch):
    config_path = tmp_path / ".claude.json"
    worktree = Path("D:/agentflow/worktrees/DE-1")
    config_path.write_text(json.dumps({"projects": {worktree.as_posix(): {"hasTrustDialogAccepted": True}}}), encoding="utf-8")
    monkeypatch.setattr("agentflow.harnesses.claude._claude_config_path", lambda: config_path)
    original_mtime = config_path.stat().st_mtime_ns

    trust_workspace(worktree)

    assert config_path.stat().st_mtime_ns == original_mtime


def test_trust_workspace_ignores_corrupt_config(tmp_path, monkeypatch):
    config_path = tmp_path / ".claude.json"
    config_path.write_text("not valid json", encoding="utf-8")
    monkeypatch.setattr("agentflow.harnesses.claude._claude_config_path", lambda: config_path)

    trust_workspace(tmp_path / "worktree")  # must not raise


def test_claude_adapter_prepare_workspace_delegates_to_trust_workspace(tmp_path, monkeypatch):
    calls = []
    monkeypatch.setattr("agentflow.harnesses.claude.trust_workspace", lambda cwd: calls.append(cwd))
    from agentflow.config import HarnessConfig
    adapter = ClaudeAdapter("claude", HarnessConfig(command="claude", trustedRepositories=[str(tmp_path)]))

    adapter.prepare_workspace(tmp_path)

    assert calls == [tmp_path]
