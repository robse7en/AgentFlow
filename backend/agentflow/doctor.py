"""`agentflow doctor`: read-only checks of everything AgentFlow needs to run.

Nothing here changes anything: no config file or directory is created and no harness config is
edited. The setup wizard runs the same checks. Only failed
checks are required ones; warnings are about optional features or things AgentFlow cannot verify.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

import httpx

from .config import Config, ConfigError, HarnessConfig, config_path, read_config
from .harnesses.claude import _claude_config_path
from .harnesses.registry import make_adapter
from .services.git_workspace import GitService
from .services.github import GitHubCli, GitHubError
from .services.jira_client import JiraError, JiraService
from .services.media_frames import ToolMissing, resolve_tools
from .services.process import ProcessError, resolve_executable, run
from .services.zellij import ZellijService, version_supported
from .workflow.usage import bundled_usage_command

Status = Literal["ok", "warn", "fail"]
ATLASSIAN_MCP_URL = "https://mcp.atlassian.com/v2/mcp"
ROLES = ("architect", "orchestrator", "implementer", "code_reviewer", "security_reviewer")


@dataclass(frozen=True)
class Check:
    name: str
    status: Status
    detail: str


async def run_checks(config_file: Path, *, jira_transport: httpx.BaseTransport | None = None) -> list[Check]:
    try:
        if not config_file.is_file():
            raise ConfigError(f"{config_file} not found; run `agentflow setup`")
        config = read_config(config_file)
    except ConfigError as exc:
        # Still check the tools, so a fresh machine sees everything it is missing at once.
        return [Check("Config", "fail", str(exc)), *await _tools(Config())]
    groups = await asyncio.gather(
        _tools(config), _harnesses(config), _jira(config, jira_transport), _github(config))
    checks = [Check("Config", "ok", str(config_file))]
    for group in groups:
        checks.extend(group)
    checks.extend(_jira_mcp(config))
    checks.extend(_ffmpeg(config))
    checks.extend(_slack(config))
    return checks


# required tools -------------------------------------------------------------------------
async def _tools(config: Config) -> list[Check]:
    git, node, zellij = await asyncio.gather(_git(), _node(), _zellij(config))
    return [git, node, _ui(), zellij]


async def _git() -> Check:
    version = await GitService().version()
    return Check("Git", "ok", version) if version else Check("Git", "fail", "'git' not found on PATH")


async def _node() -> Check:
    node, npm = resolve_executable("node"), resolve_executable("npm")
    if not node or not npm:
        missing = " and ".join(name for name, path in (("node", node), ("npm", npm)) if not path)
        return Check("Node.js", "fail", f"{missing} not found on PATH; needed to build the UI")
    return Check("Node.js", "ok", f"node {await _version(node)}")


def _ui() -> Check:
    # Where main.create_app serves the built frontend from.
    index = Path(__file__).resolve().parents[2] / "frontend" / "dist" / "index.html"
    if not index.is_file():
        return Check("UI", "warn", "not built; run `npm install` and `npm run build` in frontend/")
    return Check("UI", "ok", str(index.parent))


async def _zellij(config: Config) -> Check:
    version = await ZellijService(config.zellij).version()
    if not version:
        return Check("Zellij", "fail", f"'{config.zellij.command}' not found; needs 0.44 or newer")
    if not version_supported(version):
        return Check("Zellij", "fail", f"{version} is too old; needs 0.44 or newer")
    return Check("Zellij", "ok", version)


async def _version(executable: str) -> str:
    try:
        output = (await run([executable, "--version"], timeout=20)).stdout.strip()
    except (ProcessError, OSError):
        return "(version unknown)"
    return output.splitlines()[0] if output else "(version unknown)"


# harnesses ------------------------------------------------------------------------------
def _roles_by_harness(config: Config) -> dict[str, list[str]]:
    """Harnesses the agent roles actually use, with the roles using each."""
    groups: dict[str, list[str]] = {}
    for role in ROLES:
        groups.setdefault(config.agents.for_role(role).harness, []).append(role.replace("_", " "))
    return groups


async def _harnesses(config: Config) -> list[Check]:
    groups = _roles_by_harness(config)
    keys = list(groups)
    capabilities = await asyncio.gather(*(make_adapter(key, config.harnesses[key]).detect() for key in keys))
    checks = []
    for key, caps in zip(keys, capabilities):
        name, roles = f"Harness {key}", ", ".join(groups[key])
        if not caps.enabled:
            checks.append(Check(name, "fail", f"disabled, but used by {roles}"))
        elif not caps.available:
            checks.append(Check(name, "fail", f"{caps.message or 'not available'}; used by {roles}"))
        else:
            checks.append(Check(name, "ok", f"{caps.version or caps.resolved_path}; used by {roles}"))
        command = bundled_usage_command(config.harnesses[key].type)
        if command and not config.harnesses[key].usage_command:
            checks.append(Check(f"Usage {key}", "warn", "no usageCommand, so its runs' usage and cost stay "
                                f"unknown; run `agentflow setup` or set it to {json.dumps(command)}"))
    return checks


# Jira -----------------------------------------------------------------------------------
async def _jira(config: Config, transport: httpx.BaseTransport | None) -> list[Check]:
    jira = config.jira
    if not jira.enabled:
        return [Check("Jira", "ok", "optional integration disabled; local tasks are available")]
    if missing := jira.missing():
        return [Check("Jira", "fail", f"not configured ({', '.join(missing)}); run `agentflow setup`")]
    try:
        account = await JiraService(jira, transport=transport).fetch_myself()
    except JiraError as exc:
        return [Check("Jira", "fail", f"{jira.base_url}: {exc}")]
    kind = "Cloud" if jira.email else "Server/Data Center"
    return [Check("Jira", "ok", f"signed in as {account} at {jira.base_url} ({kind})")]


def _jira_mcp(config: Config) -> list[Check]:
    jira = config.jira
    if not jira.enabled:
        return []
    if jira.base_url and jira.api_token and not jira.email:
        return [Check("Jira MCP", "warn", "not checked: the official Atlassian MCP is Jira Cloud only; "
                                          "configure one in each harness by hand")]
    checks = []
    for key in _roles_by_harness(config):
        name = f"Jira MCP ({key})"
        harness_type = config.harnesses[key].type
        path = mcp_config_path(config.harnesses[key])
        if path is None:
            checks.append(Check(name, "warn", "not checked: AgentFlow doesn't know where this harness keeps its MCP servers"))
            continue
        try:
            servers = mcp_servers(harness_type, path)
        except (OSError, ValueError) as exc:
            checks.append(Check(name, "warn", f"cannot read {path}: {exc}"))
            continue
        found = [key for key, entry in servers.items() if mentions_jira(key, entry) and server_enabled(entry)]
        retired = [key for key in found if retired_endpoint(servers[key])]
        if retired:
            checks.append(Check(name, "warn", f"{', '.join(retired)} in {path} uses the retired /v1/sse endpoint; "
                                              f"point it at {ATLASSIAN_MCP_URL}"))
        elif found:
            checks.append(Check(name, "ok", f"{', '.join(found)} in {path}"))
        else:
            checks.append(Check(name, "warn", f"none found in {path}; `agentflow setup` can add one"))
    return checks


def mcp_config_path(harness: HarnessConfig) -> Path | None:
    """The user-scope MCP config the harness reads when AgentFlow launches it (agents run in
    worktrees, so project scope never applies). None when AgentFlow can't tell where that is.

    Launchers drop every variable outside SAFE_ENV_NAMES and then apply the harness's own `env`,
    so a CODEX_HOME in this shell never reaches an agent; only the harness `env` counts.
    """
    env = harness.env
    if harness.type == "claude":
        # CLAUDE_CONFIG_DIR moves Claude's config; where it puts the MCP list isn't verified.
        return None if env.get("CLAUDE_CONFIG_DIR") else _claude_config_path()
    if harness.type == "codex":
        return Path(env.get("CODEX_HOME") or Path.home() / ".codex").expanduser() / "config.toml"
    if harness.type == "copilot":
        return Path(env.get("COPILOT_HOME") or Path.home() / ".copilot").expanduser() / "mcp-config.json"
    return None


def mcp_servers(harness_type: str, path: Path) -> dict[str, dict]:
    """Every MCP server entry in `path`, enabled or not. A missing file has none."""
    if not path.is_file():
        return {}
    text = path.read_text(encoding="utf-8")
    if harness_type == "codex":
        servers = tomllib.loads(text).get("mcp_servers")
    else:
        data = json.loads(text)
        servers = data.get("mcpServers") if isinstance(data, dict) else None
    if not isinstance(servers, dict):
        return {}
    return {name: entry for name, entry in servers.items() if isinstance(entry, dict)}


def jira_mcp_servers(harness_type: str, path: Path) -> list[str]:
    """Names of enabled MCP servers in `path` that look like Jira/Atlassian ones."""
    return [name for name, entry in mcp_servers(harness_type, path).items()
            if mentions_jira(name, entry) and server_enabled(entry)]


def mentions_jira(name: str, entry: dict) -> bool:
    args = entry.get("args") if isinstance(entry.get("args"), list) else []
    text = " ".join(str(part) for part in (name, entry.get("url"), entry.get("command"), *args) if part).lower()
    return "atlassian" in text or "jira" in text


def server_enabled(entry: dict) -> bool:
    return entry.get("enabled") is not False and entry.get("disabled") is not True


def retired_endpoint(entry: dict) -> bool:
    """Atlassian's legacy SSE endpoint, unsupported since 30 June 2026."""
    return "mcp.atlassian.com/v1/sse" in str(entry.get("url") or "")


# optional integrations ------------------------------------------------------------------
async def _github(config: Config) -> list[Check]:
    if not config.github.pr_sync:
        return []
    cli = GitHubCli(config.github.command, timeout=20)
    if not resolve_executable(config.github.command):
        return [Check("GitHub CLI", "warn", f"'{config.github.command}' not found; PR sync cannot detect PRs")]
    checks = []
    for host in config.github.hosts:
        try:
            logged_in = await cli.logged_in(host)
        except GitHubError as exc:
            checks.append(Check("GitHub CLI", "warn", f"{host}: {exc}"))
            continue
        if logged_in:
            checks.append(Check("GitHub CLI", "ok", f"logged in to {host}"))
        else:
            checks.append(Check("GitHub CLI", "warn", f"not logged in to {host}; run `gh auth login`"))
    return checks


def _ffmpeg(config: Config) -> list[Check]:
    if config.jira.max_frames_per_video == 0:
        return []
    try:
        ffmpeg, _ = resolve_tools(config.jira.ffmpeg_path)
    except ToolMissing as exc:
        return [Check("ffmpeg", "warn", f"{exc}; video attachments get no keyframes")]
    return [Check("ffmpeg", "ok", ffmpeg)]


def _slack(config: Config) -> list[Check]:
    if not config.slack.enabled:
        return []
    problems = config.slack.problems()
    try:
        import aiohttp  # noqa: F401
        import slack_bolt  # noqa: F401
    except ImportError:
        problems.append('install the Slack extra: `agentflow setup` offers it, or run '
                        'uv pip install --python <backend venv python> -e "backend[slack]"')
    if problems:
        return [Check("Slack", "warn", "; ".join(problems))]
    return [Check("Slack", "ok", "configured (tokens are not tested)")]


# command line ---------------------------------------------------------------------------
_LABELS = {"ok": "[ok]  ", "warn": "[warn]", "fail": "[FAIL]"}


def format_report(config_file: Path, checks: list[Check]) -> str:
    width = max(len(check.name) for check in checks)
    lines = [f"AgentFlow doctor ({config_file})", ""]
    lines += [f"  {_LABELS[check.status]} {check.name.ljust(width)}  {check.detail}" for check in checks]
    failed = sum(check.status == "fail" for check in checks)
    warnings = sum(check.status == "warn" for check in checks)
    lines += ["", f"{failed} failed, {warnings} warning{'' if warnings == 1 else 's'}."
              + (" Run `agentflow setup` to fix what failed." if failed else "")]
    return "\n".join(lines)


def tolerate_console_encoding() -> None:
    """A legacy Windows console (cp1252) can't print every character in check details; print a
    replacement character rather than crash."""
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="replace")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="agentflow doctor",
                                     description="Check AgentFlow's configuration and tools without changing anything")
    parser.add_argument("--config", help="Path to config.json (defaults to the one the backend would use)")
    args = parser.parse_args(argv)
    config_file = Path(args.config).expanduser() if args.config else config_path()
    tolerate_console_encoding()
    checks = asyncio.run(run_checks(config_file))
    print(format_report(config_file, checks))
    return 1 if any(check.status == "fail" for check in checks) else 0
