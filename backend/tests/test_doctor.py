import json
import sys
from pathlib import Path

import httpx
import pytest

from agentflow import doctor
from agentflow.config import HarnessConfig, JiraConfig
from agentflow.main import run
from agentflow.services.jira_client import JiraError, JiraService
from agentflow.services.zellij import version_supported

CLOUD = {"enabled": True, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t"}
SERVER = {"enabled": True, "baseUrl": "https://jira.example.com", "apiToken": "pat"}


def _myself(status=200, body=None, seen=None):
    def handler(request: httpx.Request) -> httpx.Response:
        if seen is not None:
            seen.append(request)
        if body is None:
            return httpx.Response(status, json={"displayName": "Ada Lovelace"})
        return httpx.Response(status, content=body)
    return httpx.MockTransport(handler)


def _write_config(tmp_path, jira=None, harness_enabled=True):
    # A generic-cli harness backed by this Python, so detection needs no real coding CLI.
    harness = {"type": "generic-cli", "command": sys.executable, "enabled": harness_enabled}
    agent = {"harness": "local"}
    raw = {
        "harnesses": {"local": harness},
        "agents": {"architect": agent, "implementer": agent, "codeReviewer": agent, "securityReviewer": agent},
        "jira": {"maxFramesPerVideo": 0, **(jira or {})},
        "github": {"prSync": False},
    }
    path = tmp_path / "config.json"
    path.write_text(json.dumps(raw), encoding="utf-8")
    return path


def _by_name(checks):
    return {check.name: check for check in checks}


# Jira sign-in check ---------------------------------------------------------------------
@pytest.mark.asyncio
async def test_fetch_myself_uses_v3_with_basic_auth_on_cloud():
    seen = []
    service = JiraService(JiraConfig(**CLOUD), transport=_myself(seen=seen))
    assert await service.fetch_myself() == "Ada Lovelace"
    assert seen[0].url.path == "/rest/api/3/myself"
    assert seen[0].headers["authorization"].startswith("Basic ")


@pytest.mark.asyncio
async def test_fetch_myself_uses_v2_with_bearer_on_server():
    seen = []
    service = JiraService(JiraConfig(**SERVER), transport=_myself(seen=seen))
    await service.fetch_myself()
    assert seen[0].url.path == "/rest/api/2/myself"
    assert seen[0].headers["authorization"] == "Bearer pat"


@pytest.mark.asyncio
@pytest.mark.parametrize("status, body, message", [
    (401, None, "rejected the configured credentials"),
    (404, None, r"sign-in check failed \(404\)"),
    (200, b"<html>login</html>", "unreadable account"),
    (200, b"[]", "unreadable account"),
])
async def test_fetch_myself_explains_failures(status, body, message):
    service = JiraService(JiraConfig(**CLOUD), transport=_myself(status, body))
    with pytest.raises(JiraError, match=message):
        await service.fetch_myself()


# zellij ---------------------------------------------------------------------------------
@pytest.mark.parametrize("version, supported", [
    ("zellij 0.44.0", True), ("zellij 0.45.1", True), ("zellij 1.0.0", True),
    ("zellij 0.43.9", False), ("zellij", False), (None, False),
])
def test_zellij_version_supported(version, supported):
    assert version_supported(version) is supported


# Jira MCP detection ---------------------------------------------------------------------
def test_finds_remote_atlassian_server_in_claude_and_copilot_json(tmp_path):
    path = tmp_path / "mcp.json"
    path.write_text(json.dumps({"mcpServers": {
        "serena": {"type": "stdio", "command": "uvx", "args": ["serena"]},
        "atlassian": {"type": "http", "url": "https://mcp.atlassian.com/v2/mcp"},
    }}), encoding="utf-8")
    assert doctor.jira_mcp_servers("claude", path) == ["atlassian"]
    assert doctor.jira_mcp_servers("copilot", path) == ["atlassian"]


def test_finds_stdio_jira_server_in_codex_toml_and_skips_disabled_ones(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text(
        '[mcp_servers.tickets]\ncommand = "uvx"\nargs = ["mcp-atlassian"]\n\n'
        '[mcp_servers.old_jira]\ncommand = "jira-mcp"\nenabled = false\n\n'
        '[mcp_servers.serena]\ncommand = "serena"\n', encoding="utf-8")
    assert doctor.jira_mcp_servers("codex", path) == ["tickets"]


def test_missing_or_empty_harness_config_has_no_servers(tmp_path):
    assert doctor.jira_mcp_servers("claude", tmp_path / "absent.json") == []
    empty = tmp_path / "empty.json"
    empty.write_text("{}", encoding="utf-8")
    assert doctor.jira_mcp_servers("copilot", empty) == []


def _harness(type_, env=None):
    return HarnessConfig(type=type_, command=type_, env=env or {})


def test_mcp_config_paths_follow_the_harness_env(tmp_path):
    codex = _harness("codex", {"CODEX_HOME": str(tmp_path / "codex")})
    copilot = _harness("copilot", {"COPILOT_HOME": str(tmp_path / "copilot")})
    assert doctor.mcp_config_path(codex) == tmp_path / "codex" / "config.toml"
    assert doctor.mcp_config_path(copilot) == tmp_path / "copilot" / "mcp-config.json"


def test_mcp_config_paths_ignore_this_shells_overrides(tmp_path, monkeypatch):
    # Agent launchers drop these, so the harness AgentFlow starts reads the default location.
    monkeypatch.setenv("CODEX_HOME", str(tmp_path / "codex"))
    monkeypatch.setenv("COPILOT_HOME", str(tmp_path / "copilot"))
    assert doctor.mcp_config_path(_harness("codex")) == Path.home() / ".codex" / "config.toml"
    assert doctor.mcp_config_path(_harness("copilot")) == Path.home() / ".copilot" / "mcp-config.json"


def test_mcp_config_path_is_unknown_for_generic_cli_and_relocated_claude():
    assert doctor.mcp_config_path(_harness("generic-cli")) is None
    assert doctor.mcp_config_path(_harness("claude", {"CLAUDE_CONFIG_DIR": "/elsewhere"})) is None
    assert doctor.mcp_config_path(_harness("claude")) == Path.home() / ".claude.json"


# run_checks -----------------------------------------------------------------------------
@pytest.mark.asyncio
async def test_missing_config_fails_without_creating_it(tmp_path):
    path = tmp_path / "nested" / "config.json"
    checks = await doctor.run_checks(path)
    assert checks[0].name == "Config" and checks[0].status == "fail"
    assert "agentflow setup" in checks[0].detail
    assert {"Git", "Node.js", "UI", "Zellij"} <= {check.name for check in checks}
    assert not path.parent.exists()


@pytest.mark.asyncio
async def test_invalid_config_is_reported(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"agents": {"architect": {"harness": "nope"}}}), encoding="utf-8")
    checks = await doctor.run_checks(path)
    assert checks[0].status == "fail" and "not a configured harness" in checks[0].detail


@pytest.mark.asyncio
async def test_unconfigured_jira_is_optional(tmp_path):
    checks = _by_name(await doctor.run_checks(_write_config(tmp_path)))
    assert checks["Jira"].status == "ok"
    assert "local tasks" in checks["Jira"].detail


@pytest.mark.asyncio
async def test_working_cloud_jira_passes_and_harness_is_reported_with_roles(tmp_path):
    checks = _by_name(await doctor.run_checks(_write_config(tmp_path, CLOUD), jira_transport=_myself()))
    assert checks["Jira"].status == "ok"
    assert "signed in as Ada Lovelace" in checks["Jira"].detail and "(Cloud)" in checks["Jira"].detail
    assert checks["Harness local"].status == "ok"
    assert "architect, orchestrator, implementer, code reviewer, security reviewer" in checks["Harness local"].detail
    # generic-cli MCP config has no known location, so it can only be a warning.
    assert checks["Jira MCP (local)"].status == "warn"
    assert "GitHub CLI" not in checks and "ffmpeg" not in checks and "Slack" not in checks


@pytest.mark.asyncio
async def test_rejected_jira_credentials_fail(tmp_path):
    checks = _by_name(await doctor.run_checks(_write_config(tmp_path, CLOUD), jira_transport=_myself(401)))
    assert checks["Jira"].status == "fail" and "rejected" in checks["Jira"].detail


@pytest.mark.asyncio
async def test_server_jira_skips_the_cloud_only_mcp_check(tmp_path):
    checks = await doctor.run_checks(_write_config(tmp_path, SERVER), jira_transport=_myself())
    mcp = [check for check in checks if check.name.startswith("Jira MCP")]
    assert len(mcp) == 1 and mcp[0].status == "warn" and "Cloud only" in mcp[0].detail
    assert _by_name(checks)["Jira"].detail.endswith("(Server/Data Center)")


@pytest.mark.asyncio
async def test_disabled_harness_used_by_a_role_fails(tmp_path):
    path = _write_config(tmp_path, CLOUD, harness_enabled=False)
    checks = _by_name(await doctor.run_checks(path, jira_transport=_myself()))
    assert checks["Harness local"].status == "fail" and "disabled" in checks["Harness local"].detail


@pytest.mark.asyncio
async def test_harness_without_its_bundled_usage_collector_warns(tmp_path):
    path = _write_config(tmp_path, CLOUD)
    raw = json.loads(path.read_text(encoding="utf-8"))
    raw["harnesses"]["local"]["type"] = "copilot"
    path.write_text(json.dumps(raw), encoding="utf-8")
    checks = _by_name(await doctor.run_checks(path, jira_transport=_myself()))
    assert checks["Usage local"].status == "warn"
    assert "usage_copilot.py" in checks["Usage local"].detail and "agentflow setup" in checks["Usage local"].detail
    raw["harnesses"]["local"]["usageCommand"] = ["python", "usage.py"]
    path.write_text(json.dumps(raw), encoding="utf-8")
    assert "Usage local" not in _by_name(await doctor.run_checks(path, jira_transport=_myself()))


@pytest.mark.asyncio
async def test_generic_harness_has_no_usage_collector_to_warn_about(tmp_path):
    checks = _by_name(await doctor.run_checks(_write_config(tmp_path, CLOUD), jira_transport=_myself()))
    assert "Usage local" not in checks


@pytest.mark.asyncio
async def test_enabled_slack_without_tokens_warns(tmp_path, monkeypatch):
    monkeypatch.delenv("AGENTFLOW_SLACK_BOT_TOKEN", raising=False)
    monkeypatch.delenv("AGENTFLOW_SLACK_APP_TOKEN", raising=False)
    path = _write_config(tmp_path, CLOUD)
    raw = json.loads(path.read_text(encoding="utf-8"))
    raw["slack"] = {"enabled": True}
    path.write_text(json.dumps(raw), encoding="utf-8")
    checks = _by_name(await doctor.run_checks(path, jira_transport=_myself()))
    assert checks["Slack"].status == "warn" and "xoxb-" in checks["Slack"].detail


# report and command line ----------------------------------------------------------------
def test_report_summarises_failures_and_warnings(tmp_path):
    report = doctor.format_report(tmp_path / "config.json", [
        doctor.Check("Git", "ok", "git 2"), doctor.Check("Jira", "fail", "nope"),
        doctor.Check("ffmpeg", "warn", "missing"),
    ])
    assert "[FAIL] Jira" in report and "[warn] ffmpeg" in report
    assert report.endswith("1 failed, 1 warning. Run `agentflow setup` to fix what failed.")


def test_agentflow_doctor_subcommand_exits_nonzero_on_failure(tmp_path, capsys):
    with pytest.raises(SystemExit) as exited:
        run(["doctor", "--config", str(tmp_path / "config.json")])
    assert exited.value.code == 1
    assert "[FAIL] Config" in capsys.readouterr().out


def test_jira_mcp_on_the_retired_sse_endpoint_warns(tmp_path, monkeypatch):
    mcp = tmp_path / "copilot.json"
    mcp.write_text(json.dumps({"mcpServers": {"atlassian": {"type": "sse", "url": "https://mcp.atlassian.com/v1/sse"}}}),
                   encoding="utf-8")
    monkeypatch.setattr(doctor, "mcp_config_path", lambda _harness: mcp)
    config = doctor.read_config(_write_config(tmp_path, CLOUD))
    [check] = doctor._jira_mcp(config)
    assert check.status == "warn" and "retired /v1/sse" in check.detail and "/v2/mcp" in check.detail
