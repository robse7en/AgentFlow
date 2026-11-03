"""`agentflow setup`: an interactive wizard that writes a working config.json.

Every prompt is prefilled from the existing file, and the wizard edits that file's raw JSON rather
than dumping the whole model, so re-running it changes only what the user changes. The result is
validated with Config before anything is written.

After saving, it offers to add Atlassian's Jira MCP to each harness the roles use (§4.3), through the
harness's own CLI, and to add the AgentFlow folder to each installed CLI's trusted folders; those are the
only places the wizard touches harness config. Not here yet: installing
missing tools (the bootstrap scripts will offer that); the wizard detects, explains and re-checks.
"""

from __future__ import annotations

import argparse
import asyncio
import getpass
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import httpx
from pydantic import ValidationError

from . import doctor, installers
from .config import (SLACK_COMMAND_PATTERN, Config, ConfigError, HarnessConfig, JiraConfig, SlackConfig, config_path,
                     default_harnesses, fresh_config)
from .harnesses.claude import trust_workspace
from .harnesses.registry import make_adapter
from .services.jira_client import JiraError, JiraService
from .services.process import clean_env, resolve_executable
from .workflow.usage import bundled_usage_command

ROOT = Path(__file__).resolve().parents[2]
ROLE_KEYS = {"architect": "architect", "orchestrator": "orchestrator", "implementer": "implementer",
             "code_reviewer": "codeReviewer", "security_reviewer": "securityReviewer"}
SIGN_IN = {
    "claude": "run `claude` and sign in when it asks (or type /login inside it)",
    "codex": "run `codex login`",
    "copilot": "run `copilot` and type /login inside it",
}
BILLING = ("subscription", "metered", "unknown")
TOOL_FOR_CHECK = {"Git": "git", "Node.js": "node", "Zellij": "zellij"}


class SetupAborted(Exception):
    pass


class Console:
    """Terminal prompts. Tests pass their own object with the same four methods."""

    def say(self, text: str = "") -> None:
        print(text, flush=True)

    def ask(self, question: str, default: str = "") -> str:
        answer = input(f"{question}{f' [{default}]' if default else ''}: ").strip()
        return answer or default

    def confirm(self, question: str, default: bool = False) -> bool:
        while True:
            answer = input(f"{question} [{'Y/n' if default else 'y/N'}]: ").strip().lower()
            if not answer:
                return default
            if answer in ("y", "yes", "n", "no"):
                return answer.startswith("y")

    def secret(self, question: str) -> str:
        if os.name == "nt" and not sys.stdin.isatty():
            # getpass reads the Windows console directly and can hang under a mintty without one.
            print("(no console detected: what you type will be visible)", flush=True)
            return input(f"{question}: ").strip()
        return getpass.getpass(f"{question}: ").strip()


# config file ----------------------------------------------------------------------------
def read_raw(path: Path) -> dict:
    """The file's JSON as written (not validated: fixing an invalid file is the wizard's job)."""
    if not path.is_file():
        return fresh_config().model_dump(by_alias=True)  # keeps data inside a source checkout
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise SetupAborted(f"{path} is not valid JSON ({exc}); fix or move it, then run setup again") from exc
    if not isinstance(raw, dict):
        raise SetupAborted(f"{path} does not contain a JSON object; fix or move it, then run setup again")
    return raw


def validate(raw: dict) -> Config:
    try:
        config = Config.model_validate(raw)
    except ValidationError as exc:
        raise ConfigError(str(exc)) from exc
    config.validate_references()
    return config


def write_config(path: Path, raw: dict) -> Path | None:
    """Write atomically, keeping the previous file as <name>.bak. Returns the backup path."""
    path.parent.mkdir(parents=True, exist_ok=True)
    backup = None
    if path.is_file():
        backup = path.with_name(path.name + ".bak")
        shutil.copy2(path, backup)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(raw, indent=2) + "\n", encoding="utf-8")
    # The file holds tokens: keep the permissions a user gave it, and make a new one private.
    if backup:
        shutil.copymode(path, temporary)
    elif os.name != "nt":
        os.chmod(temporary, 0o600)
    os.replace(temporary, path)
    return backup


def tracked_by_git(path: Path) -> bool:
    git = resolve_executable("git")
    if not git or not path.parent.is_dir():
        return False
    result = subprocess.run([git, "ls-files", "--error-unmatch", path.name], cwd=path.parent,
                            capture_output=True)
    return result.returncode == 0


def _mask(token: str | None) -> str:
    if not token:
        return ""
    return f"...{token[-4:]}" if len(token) > 12 else "set"


def _section(raw: dict, key: str) -> dict:
    if not isinstance(raw.get(key), dict):
        raw[key] = {}
    return raw[key]


# steps ----------------------------------------------------------------------------------
async def prerequisites(io, raw: dict) -> None:
    io.say("\n1. Prerequisites")
    try:
        config = Config.model_validate(raw)
    except ValidationError:
        config = Config()
    offered = set()  # each install is offered once per run; after that it's up to the user
    while True:
        checks = await doctor._tools(config)
        for check in checks:
            io.say(f"   {doctor._LABELS[check.status]} {check.name}: {check.detail}")
        installed = False
        for check in checks:
            tool = TOOL_FOR_CHECK.get(check.name)
            if check.status == "fail" and tool and tool not in offered:
                offered.add(tool)
                installed |= installers.offer_install(io, tool, check.name, upgrade="too old" in check.detail)
        if installed:
            continue
        ui = next(check for check in checks if check.name == "UI")
        if (ui.status != "ok" and "ui" not in offered and resolve_executable("npm")
                and io.confirm("   Build the UI now (npm install, npm run build)?", True)):
            offered.add("ui")
            build_ui(io)
            continue
        if not any(check.status == "fail" for check in checks):
            return
        if io.ask("   Install what is missing, then press Enter to check again (or type 'skip')").lower() == "skip":
            return


def build_ui(io) -> None:
    npm, frontend = resolve_executable("npm"), ROOT / "frontend"
    for argv in ([npm, "install"], [npm, "run", "build"]):
        if subprocess.run(argv, cwd=frontend).returncode != 0:
            io.say(f"   `npm {' '.join(argv[1:])}` failed; see the output above")
            return


async def harnesses(io, raw: dict) -> list[str]:
    io.say("\n2. Coding harnesses")
    defaults = {key: harness.model_dump(by_alias=True) for key, harness in default_harnesses().items()}
    if not isinstance(raw.get("harnesses"), dict):
        # No `harnesses` key means the built-in defaults; write them out so the roles keep resolving.
        raw["harnesses"] = dict(defaults)
    section = raw["harnesses"]
    current = _current_harnesses(raw)
    # Built-in harnesses missing from the file are offered too, and added only if chosen.
    offered = {**section, **{key: entry for key, entry in defaults.items() if key not in section}}
    keys = list(offered)
    for number, key in enumerate(keys, 1):
        entry = offered[key]
        _, status = await _detected(key, entry)
        io.say(f"   {number}. {key}: {entry.get('displayName') or key} ({status})")
    while not (chosen := _parse_choice(
            io.ask("   Which should AgentFlow use? Numbers or names, comma separated", ", ".join(current)), keys)):
        io.say(f"   Choose from: {', '.join(keys)}")

    for key in keys:
        if key in chosen:
            section[key] = offered[key]
            await _set_up_harness(io, key, section[key])
        elif key in section:
            section[key]["enabled"] = False
    return chosen


async def _detected(key: str, entry: dict) -> tuple[bool, str]:
    try:
        caps = await make_adapter(key, HarnessConfig.model_validate(entry)).detect()
    except ValidationError:
        return False, "invalid entry"
    if not caps.available:
        return False, caps.message or "not found"
    return True, caps.version or "installed"


def _current_harnesses(raw: dict) -> list[str]:
    """Enabled harnesses the roles use now: the default answer to "which harnesses"."""
    try:
        config = Config.model_validate(raw)
    except ValidationError:
        return ["claude"]
    used = []
    for role in ROLE_KEYS:
        key = config.agents.for_role(role).harness
        harness = config.harnesses.get(key)
        if harness and harness.enabled and key not in used:
            used.append(key)
    return used or ["claude"]


def _parse_choice(answer: str, keys: list[str]) -> list[str]:
    chosen = []
    for part in (p.strip() for p in answer.split(",")):
        key = keys[int(part) - 1] if part.isdigit() and 0 < int(part) <= len(keys) else part
        if key not in keys:
            return []
        if key not in chosen:
            chosen.append(key)
    return chosen


async def _set_up_harness(io, key: str, entry: dict) -> None:
    entry["enabled"] = True
    name = entry.get("displayName") or key
    io.say(f"\n   {name}")
    harness_type = entry.get("type", "generic-cli")
    # Installing only helps when the harness runs the standard command, not a custom path.
    installable = harness_type in installers.WINGET and entry.get("command") == harness_type
    while not (found := await _detected(key, entry))[0]:
        io.say(f"   {found[1]}")
        if installable:
            installable = False
            if installers.offer_install(io, harness_type, name):
                continue
        if io.ask("   Install it, then press Enter to check again (or type 'skip')").lower() == "skip":
            break
    else:
        io.say(f"   found: {found[1]}")
    if harness_type in SIGN_IN:
        io.ask(f"   Sign in if you haven't: {SIGN_IN[harness_type]}. Press Enter when done")
    while (billing := io.ask("   Billing: subscription, metered or unknown",
                             entry.get("billingMode", "unknown"))) not in BILLING:
        io.say(f"   Choose one of: {', '.join(BILLING)}")
    entry["billingMode"] = billing
    if harness_type == "claude":
        answer = io.ask("   Trusted repositories, comma separated (skips Claude's trust prompt; '-' for none)",
                        ", ".join(entry.get("trustedRepositories") or []))
        entry["trustedRepositories"] = [] if answer == "-" else [p.strip() for p in answer.split(",") if p.strip()]
    command = bundled_usage_command(harness_type)
    if command and not entry.get("usageCommand"):
        entry["usageCommand"] = command


def roles(io, raw: dict, chosen: list[str]) -> None:
    io.say("\n3. Agent roles")
    agents = _section(raw, "agents")
    # Roles already spread over the chosen harnesses stay that way unless the user says otherwise.
    mixed = len({harness for harness in _role_harnesses(agents).values() if harness in chosen}) > 1
    separately = len(chosen) > 1 and io.confirm("   Assign each role to a harness separately?", mixed)
    for role, alias in ROLE_KEYS.items():
        if role == "orchestrator" and not isinstance(agents.get(alias), dict):
            continue  # Unset, the Architect plans Parallel tasks itself; an existing entry is kept up to date.
        current = agents.get(alias) if isinstance(agents.get(alias), dict) else {}
        harness = chosen[0]
        if separately:
            default = current.get("harness") if current.get("harness") in chosen else chosen[0]
            while (harness := io.ask(f"   {role.replace('_', ' ')}", default)) not in chosen:
                io.say(f"   Choose from: {', '.join(chosen)}")
        if current.get("harness") != harness:
            # A model picked for another harness would not exist on this one.
            current = {"harness": harness, "model": None, "reasoning": current.get("reasoning", "high")}
        agents[alias] = current
    io.say("   " + ", ".join(f"{role.replace('_', ' ')}: {agents[alias]['harness']}"
                              for role, alias in ROLE_KEYS.items() if isinstance(agents.get(alias), dict)))


def _role_harnesses(agents: dict) -> dict[str, str | None]:
    """The harness each role uses now, as AgentsConfig resolves it (orchestrator falls back to architect)."""
    found = {}
    for role, alias in ROLE_KEYS.items():
        entry = agents.get(alias)
        found[role] = entry.get("harness") if isinstance(entry, dict) else None
    found["orchestrator"] = found["orchestrator"] or found["architect"]
    return found


async def jira(io, raw: dict, transport: httpx.BaseTransport | None = None) -> None:
    io.say("\n4. Jira (optional)")
    section = _section(raw, "jira")
    if not io.confirm("   Configure Jira integration?", bool(section.get("enabled"))):
        section["enabled"] = False
        return
    cloud = bool(section.get("email")) or not section.get("baseUrl")
    while True:
        while (kind := io.ask("   Jira Cloud or Server/Data Center? (cloud/server)",
                              "cloud" if cloud else "server").lower()) not in ("cloud", "server"):
            io.say("   Type cloud or server")
        cloud = kind == "cloud"
        while not (url := io.ask("   Jira URL", section.get("baseUrl") or "").rstrip("/")).startswith("https://"):
            io.say("   The URL must start with https://")
        section["baseUrl"] = url
        if cloud:
            while not (email := io.ask("   Your Atlassian account email", section.get("email") or "")):
                io.say("   Cloud needs the email the API token belongs to")
            section["email"] = email
        else:
            section["email"] = None  # no email = a PAT sent as a Bearer token
        label = "API token (id.atlassian.com > Security > API tokens)" if cloud else "Personal access token"
        existing = section.get("apiToken")
        token = io.secret(f"   {label}{f' (Enter keeps {_mask(existing)})' if existing else ''}") or existing
        if not token:
            io.say("   A token is required")
            continue
        section["apiToken"] = token
        section["enabled"] = True
        try:
            account = await JiraService(JiraConfig.model_validate(section), transport=transport).fetch_myself()
        except (JiraError, ValidationError) as exc:
            io.say(f"   Sign-in failed: {exc}")
            if not io.confirm("   Try again?", True):
                raise SetupAborted("Jira setup stopped without writing anything; restart setup and skip Jira to use local tasks")
            continue
        io.say(f"   Signed in as {account}")
        break
    section["fetchComments"] = io.confirm("   Copy ticket comments into the task context?",
                                          section.get("fetchComments", True))
    status = io.ask("   Move the ticket to this status when the Architect starts ('-' for never)",
                    section.get("autoTransitionStatus") or "-")
    section["autoTransitionStatus"] = None if status == "-" else status
    review = io.ask("   Move the ticket to this status when its pull request is created ('-' for never)",
                    section.get("prCreatedTransitionStatus", "In Code Review") or "-")
    section["prCreatedTransitionStatus"] = None if review == "-" else review
    if not cloud:
        io.say("   The official Atlassian MCP is Jira Cloud only: add a Jira MCP to each harness by hand "
               "(docs/installation.md, \"MCP prerequisites\")")


async def optional(io, raw: dict) -> None:
    io.say("\n5. Optional integrations")
    try:
        config = Config.model_validate(raw)
    except ValidationError:
        config = Config()
    for check in [*await doctor._github(config), *doctor._ffmpeg(config)]:
        io.say(f"   {doctor._LABELS[check.status]} {check.name}: {check.detail}")
        tool = {"GitHub CLI": "gh", "ffmpeg": "ffmpeg"}[check.name]
        # Only a missing tool at its standard command can be fixed by installing: a gh that isn't
        # logged in needs `gh auth login`, and a custom path needs correcting in config.json.
        standard = config.github.command == "gh" if tool == "gh" else config.jira.ffmpeg_path is None
        if standard and "not found" in check.detail and installers.offer_install(io, tool, check.name):
            recheck = await doctor._github(config) if tool == "gh" else doctor._ffmpeg(config)
            for again in recheck:
                io.say(f"   {doctor._LABELS[again.status]} {again.name}: {again.detail}")
    slack(io, raw)


def install_slack_extra(io) -> None:
    command = installers.extra_install_command("slack")
    if command is None:
        io.say('   The Slack extra is not installed, and this venv has neither uv nor pip: install uv '
               '(https://docs.astral.sh/uv/), then run  uv pip install -e "backend[slack]"')
        return
    io.say(f"   The Slack extra is not installed. AgentFlow can install it with:\n     {' '.join(command)}")
    if io.confirm("   Install it now?", False) and installers.run_foreground(io, command):
        io.say("   Installed")


def slack_manifest(name: str, command: str | None, bot_name: str | None = None) -> str:
    """The Slack app manifest (JSON, so any app name is safe) for one person's AgentFlow app;
    `bot_name` is what messages and the DM show, `name` by default. Without a command the app has
    no slash command, nor the `commands` scope that only slash commands need.
    Keep it in step with the YAML in docs/slack.md, "Setup"."""
    features = {
        "app_home": {"messages_tab_enabled": True, "messages_tab_read_only_enabled": False},
        "bot_user": {"display_name": bot_name or name},
    }
    scopes = ["chat:write", "im:write", "im:history", "reactions:write"]
    if command:
        features["slash_commands"] = [{"command": command, "description": "AgentFlow task status",
                                       "usage_hint": "list | status KEY | details KEY | digest | new"}]
        scopes.append("commands")
    return json.dumps({
        "display_information": {"name": name},
        "features": features,
        "oauth_config": {"scopes": {"bot": scopes}},
        "settings": {"socket_mode_enabled": True, "interactivity": {"is_enabled": True},
                     "event_subscriptions": {"bot_events": ["message.im"]}},
    }, indent=2)


COMMAND_PREFIX = "/agentflow-"


def _default_name(section: dict) -> str:
    """The name part of the configured command, or the whole command when it has none (an app set
    up before commands were configurable keeps /agentflow). A fresh config already holds the
    default /agentflow, and an app without a slash command keeps no name, so either is offered
    this machine's user name instead."""
    command = section.get("command", "/agentflow")
    if command and command.startswith(COMMAND_PREFIX):
        return command.removeprefix(COMMAND_PREFIX)
    if command and (command != "/agentflow" or section.get("userId")):
        return command
    words = getpass.getuser().split()
    return words[0] if words else ""


def _command_for(name: str) -> str | None:
    """"Jarvis" -> /agentflow-jarvis; a whole "/command" is kept as typed. None if Slack would refuse it."""
    name = name.strip().lower()
    command = name if name.startswith("/") else COMMAND_PREFIX + re.sub(r"[^a-z0-9_-]+", "-", name).strip("-")
    return command if re.fullmatch(SLACK_COMMAND_PATTERN, command) and command != COMMAND_PREFIX else None


def slack_app(io, section: dict) -> None:
    """Name the agent, which names the Slack app and the slash command, ask whether the app has a
    slash command at all, and print the manifest to create (or update) the app with.
    slack.command can still be edited by hand afterwards (null: no slash command)."""
    io.say("   Everyone who runs AgentFlow creates their own Slack app. Give your agent a name: it names\n"
           "   the app you DM, and your slash command is /agentflow-<name>, which nobody else will use")
    while (typed := _command_for(name := io.ask("   Give your AgentFlow agent a name", _default_name(section)))) is None:
        io.say("   Use letters, digits, - or _ (up to 21), or type a whole command such as /agentflow-jarvis")
    previous = section.get("command", "/agentflow")
    io.say("   A slash command shows in the / menu of everyone in the workspace. Without one, you type the\n"
           "   same commands in your DM with the app, and nobody else sees anything")
    command = typed if io.confirm(f"   Add the slash command {typed}?", previous is not None) else None
    if section.get("userId") and command != previous:
        io.say(f"   Your command changes from {previous or 'none'} to {command or 'none'}: update your Slack app\n"
               "   with the manifest below and reinstall it, or AgentFlow won't hear the command")
    section["command"] = command
    label = typed.removeprefix("/agentflow").strip("-_") if name.strip().startswith("/") else name.strip()
    # Slack refuses symbols such as brackets in the app name, so it keeps to plain words; the bot's
    # display name (on messages and the DM) accepts them.
    label = " ".join(re.sub(r"[^A-Za-z0-9._-]+", " ", label).split())
    app_name = f"AgentFlow {label}".strip()[:35]
    bot_name = f"[AgentFlow] {label}" if label else "AgentFlow"
    if command:
        io.say(f"   Your command is {command}. To change it later, set slack.command in config.json and rename\n"
               "   the command in the Slack app to match")
    else:
        io.say("   No slash command: type list, status, new and the rest in your DM with the app. To add one\n"
               "   later, run agentflow setup again")
    io.say("   Create the app at https://api.slack.com/apps: Create New App > From a manifest, choose your\n"
           "   workspace, select JSON and paste this. If you already have the app, paste it into its\n"
           "   App Manifest page instead, save, then reinstall it (Install App > Reinstall).")
    io.say(slack_manifest(app_name, command, bot_name))
    io.say("   Then copy the two tokens: docs/slack-initial-setup-instructions.md, steps 3-4")


def slack(io, raw: dict) -> None:
    section = _section(raw, "slack")
    if not io.confirm("   Use Slack DMs?", bool(section.get("enabled"))):
        if section.get("enabled"):
            section["enabled"] = False
        return
    try:
        import aiohttp  # noqa: F401
        import slack_bolt  # noqa: F401
    except ImportError:
        install_slack_extra(io)
    slack_app(io, section)
    for field, env, prefix in (("botToken", "AGENTFLOW_SLACK_BOT_TOKEN", "xoxb-"),
                               ("appToken", "AGENTFLOW_SLACK_APP_TOKEN", "xapp-")):
        if os.environ.get(env):
            io.say(f"   {env} is set and takes precedence over slack.{field}")
        existing = section.get(field)
        while True:
            token = io.secret(f"   {prefix} token{f' (Enter keeps {_mask(existing)})' if existing else ''}") or existing
            if (token or "").startswith(prefix) or os.environ.get(env):
                break
            io.say(f"   The token must start with {prefix}")
        section[field] = token
    while not re.fullmatch(r"[UW][A-Z0-9]+", user := io.ask("   Your Slack member ID (U...)",
                                                            section.get("userId") or "")):
        io.say("   A member ID looks like U0123ABCD (your Slack profile > More > Copy member ID)")
    section["userId"] = user
    section["enabled"] = True
    try:
        problems = SlackConfig.model_validate(section).problems()
    except ValidationError as exc:
        problems = [str(exc)]
    for problem in problems:
        io.say(f"   [warn] {problem}")
    io.say("   Slack changes take effect after a full restart, not a config reload")


# Jira MCP (§4.3) ------------------------------------------------------------------------
MCP_NAME = "atlassian"
MCP_SIGN_IN = {
    "claude": "start `claude`, type /mcp, choose atlassian and sign in through the browser",
    "copilot": f"start `copilot` and type /mcp auth {MCP_NAME}, then sign in through the browser",
}


def mcp_add_command(harness: HarnessConfig, executable: str) -> list[str] | None:
    """The harness CLI's own command for adding the Atlassian MCP at user scope (checked against each
    CLI's --help: claude 2.1.284, codex 0.158.0, copilot 1.0.89). None for harnesses without one."""
    url = doctor.ATLASSIAN_MCP_URL
    return {
        "claude": [executable, "mcp", "add", "--transport", "http", "--scope", "user", MCP_NAME, url],
        "codex": [executable, "mcp", "add", MCP_NAME, "--url", url],  # codex has only user scope
        "copilot": [executable, "mcp", "add", "--transport", "http", MCP_NAME, url],  # always user config
    }.get(harness.type)


def jira_mcp(io, config: Config) -> None:
    """Add the Atlassian MCP to each harness the roles use, with consent. Never edits or removes an
    existing entry, and a failure here is only reported: Jira itself already works over REST."""
    if not config.jira.email:
        return  # Server/Data Center: the Jira step already said the official MCP is Cloud only
    io.say("\n7. Jira MCP in each harness")
    for key in doctor._roles_by_harness(config):
        _jira_mcp_for(io, key, config.harnesses[key])


def _jira_mcp_for(io, key: str, harness: HarnessConfig) -> None:
    name = harness.display_name or key
    io.say(f"\n   {name}")
    # Run with exactly the environment agents get, so the CLI writes the config they read.
    env = clean_env(harness.env)
    path = doctor.mcp_config_path(harness)
    if path is not None:
        try:
            servers = doctor.mcp_servers(harness.type, path)
        except (OSError, ValueError) as exc:
            io.say(f"   [warn] cannot read {path} ({exc}); add the Jira MCP by hand")
            return
        existing = {n: e for n, e in servers.items() if n == MCP_NAME or doctor.mentions_jira(n, e)}
        for server, entry in existing.items():
            note = "" if doctor.server_enabled(entry) else " but it is disabled; enable it there"
            io.say(f"   Already has '{server}' in {path}{note}; left as it is")
            if doctor.retired_endpoint(entry):
                io.say(f"   [warn] it uses the retired /v1/sse endpoint: remove it and run setup again to "
                       f"add {doctor.ATLASSIAN_MCP_URL}")
        if existing:
            return
    executable = resolve_executable(harness.command)
    command = mcp_add_command(harness, executable or harness.command)
    if command is None:
        io.say(f"   Add an MCP server for {doctor.ATLASSIAN_MCP_URL} by hand, following this harness's docs")
        return
    if not executable:
        io.say(f"   [warn] '{harness.command}' not found; cannot add the Jira MCP")
        return
    shown = " ".join([harness.command, *command[1:]])
    overrides = "".join(f" {k}={v}" for k, v in harness.env.items())
    io.say(f"   This adds Atlassian's Jira MCP to your user-level {name} settings, so all your {name} "
           f"sessions get it, not only AgentFlow's agents:\n     {shown}{f'   (with{overrides})' if overrides else ''}")
    if harness.type == "codex":
        io.say("   Codex opens a browser sign-in to Atlassian straight after adding it (Ctrl-C to do that later)")
    if not io.confirm("   Add it?", False):
        io.say("   Skipped; `agentflow doctor` will keep reporting it")
        return
    if not _run_mcp_command(io, command, env, interactive=harness.type == "codex"):
        return
    if path is None:
        io.say("   Added (AgentFlow can't check where CLAUDE_CONFIG_DIR puts it)")
    elif MCP_NAME in doctor.mcp_servers(harness.type, path):
        io.say(f"   Added to {path}")
    else:
        io.say(f"   [warn] the command succeeded but {path} has no '{MCP_NAME}' entry, so AgentFlow's agents "
               f"may not see it")
        return
    _mcp_sign_in(io, harness, executable, env, overrides)


def _run_mcp_command(io, command: list[str], env: dict[str, str], *, interactive: bool) -> bool:
    try:
        if interactive:  # its output and browser prompt are for the user
            result = subprocess.run(command, env=env)
        else:
            result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL, capture_output=True,
                                    text=True, errors="replace", timeout=120)
    except KeyboardInterrupt:
        io.say("\n   Interrupted")
        return True  # codex writes the entry before its sign-in, so check what is there
    except (OSError, subprocess.TimeoutExpired) as exc:
        io.say(f"   [warn] {exc}")
        return False
    if result.returncode != 0:
        output = ((result.stderr or "") + (result.stdout or "")).strip() if not interactive else ""
        io.say(f"   [warn] it failed (exit {result.returncode}){f': {output}' if output else ''}")
        return False
    return True


def _mcp_sign_in(io, harness: HarnessConfig, executable: str, env: dict[str, str], overrides: str) -> None:
    if harness.type != "codex":
        # Signing in from a session without the harness env would use a different config than agents do.
        io.say(f"   To sign in, {MCP_SIGN_IN[harness.type]}"
               f"{f' (start it with{overrides} set, as agents do)' if overrides else ''}")
        io.ask("   Press Enter once you have signed in (or to do it later)")
        return
    login = [executable, "mcp", "login", MCP_NAME]
    while (status := _codex_auth_status(executable, env)) == "not_logged_in":
        io.say(f"   Not signed in to Atlassian yet (later: {harness.command} mcp login {MCP_NAME}"
               f"{f', with{overrides}' if overrides else ''})")
        if not io.confirm("   Sign in now?", True):
            return
        try:
            subprocess.run(login, env=env)
        except KeyboardInterrupt:
            io.say("\n   Interrupted")
            return
    io.say(f"   Sign-in status: {status}" if status else "   [warn] cannot tell whether Codex is signed in")


def _codex_auth_status(executable: str, env: dict[str, str]) -> str | None:
    """auth_status from `codex mcp list --json` ("not_logged_in" before the OAuth sign-in)."""
    try:
        result = subprocess.run([executable, "mcp", "list", "--json"], env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, errors="replace", timeout=60)
        servers = json.loads(result.stdout)
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return None
    for server in servers if isinstance(servers, list) else []:
        if isinstance(server, dict) and server.get("name") == MCP_NAME:
            return server.get("auth_status")
    return None


# Folder trust ---------------------------------------------------------------------------
TRUST_NAMES = {"claude": "Claude Code", "codex": "Codex CLI", "copilot": "GitHub Copilot CLI"}


def trust_files(config: Config) -> list[tuple[str, Path]]:
    """(CLI type, file) for each coding CLI's user-level record of trusted folders, where that CLI's
    config already exists: AgentFlow never creates ~/.codex or ~/.copilot. Follows CODEX_HOME and
    COPILOT_HOME in a harness's env as doctor.mcp_config_path does; CLAUDE_CONFIG_DIR is not followed,
    since where it puts .claude.json isn't verified."""
    home = Path.home()
    candidates = [("claude", home / ".claude.json"), ("codex", home / ".codex" / "config.toml"),
                  ("copilot", home / ".copilot" / "config.json")]
    for harness in config.harnesses.values():
        if harness.type == "codex" and harness.env.get("CODEX_HOME"):
            candidates.append(("codex", Path(harness.env["CODEX_HOME"]).expanduser() / "config.toml"))
        elif harness.type == "copilot" and harness.env.get("COPILOT_HOME"):
            candidates.append(("copilot", Path(harness.env["COPILOT_HOME"]).expanduser() / "config.json"))
    found = []
    for kind, path in candidates:
        present = path.is_file() if kind == "claude" else path.parent.is_dir()
        if present and (kind, path) not in found:
            found.append((kind, path))
    return found


def _same_folder(a: str, b: str) -> bool:
    return os.path.normcase(os.path.normpath(a)) == os.path.normcase(os.path.normpath(b))


def _codex_project_key(folder: Path) -> str:
    # Codex records Windows paths lowercased with backslashes, which is what normcase gives.
    return os.path.normcase(str(folder))


def _split_comments(text: str) -> tuple[str, str]:
    """Copilot's config.json starts with // comment lines, so it is not plain JSON: (comments, JSON)."""
    lines = text.splitlines(keepends=True)
    count = 0
    while count < len(lines) and (not lines[count].strip() or lines[count].lstrip().startswith("//")):
        count += 1
    return "".join(lines[:count]), "".join(lines[count:])


def _read_text(path: Path) -> str:
    if not path.is_file():
        return ""
    with path.open("r", encoding="utf-8", newline="") as f:  # keep the file's own line endings
        return f.read()


def _replace_text(path: Path, text: str) -> None:
    """Write via a temp file and a rename, so the CLI never reads a half-written file."""
    tmp = path.with_name(path.name + ".agentflow-tmp")
    with tmp.open("w", encoding="utf-8", newline="") as f:
        f.write(text)
    tmp.replace(path)


def _copilot_config(path: Path) -> tuple[str, dict]:
    header, body = _split_comments(_read_text(path))
    data = json.loads(body) if body.strip() else {}
    if not isinstance(data, dict):
        raise ValueError("not a JSON object")
    return header, data


def trust_state(kind: str, path: Path, folder: Path) -> str | None:
    """How `path` records `folder`: "trusted", another Codex trust_level such as "untrusted", or None
    when it has no entry. Raises OSError or ValueError when the file cannot be read."""
    if kind == "claude":
        data = json.loads(_read_text(path) or "{}")
        projects = data.get("projects") if isinstance(data, dict) else None
        entry = projects.get(folder.as_posix()) if isinstance(projects, dict) else None
        return "trusted" if isinstance(entry, dict) and entry.get("hasTrustDialogAccepted") is True else None
    if kind == "codex":
        projects = tomllib.loads(_read_text(path)).get("projects")
        for key, entry in (projects.items() if isinstance(projects, dict) else ()):
            if _same_folder(key, str(folder)):
                return str(entry.get("trust_level")) if isinstance(entry, dict) else "set"
        return None
    _, data = _copilot_config(path)
    folders = data.get("trustedFolders")
    if isinstance(folders, list) and any(isinstance(f, str) and _same_folder(f, str(folder)) for f in folders):
        return "trusted"
    return None


def add_trust(kind: str, path: Path, folder: Path) -> None:
    """Record `folder` as trusted in `path`, exactly as accepting that CLI's own trust prompt would.
    Reads fresh and changes only that one entry; every other setting is kept. Raises OSError or
    ValueError, leaving the file as it was, when it cannot."""
    if kind == "claude":
        trust_workspace(folder)  # swallows its own errors, hence the check below
    elif kind == "codex":
        text = _read_text(path)
        newline = "\r\n" if "\r\n" in text else "\n"
        key = _codex_project_key(folder)
        # A literal key keeps Windows backslashes as they are; one with a quote needs a basic string.
        quoted = f"'{key}'" if "'" not in key else json.dumps(key)
        if text and not text.endswith("\n"):
            text += newline
        text += f"{newline if text else ''}[projects.{quoted}]{newline}trust_level = \"trusted\"{newline}"
        # Checked before writing: e.g. `projects` as an inline table cannot take a new [projects.x] table.
        entry = tomllib.loads(text).get("projects", {}).get(key)
        if not isinstance(entry, dict) or entry.get("trust_level") != "trusted":
            raise ValueError("the new entry does not parse as trusted")
        _replace_text(path, text)
    else:
        header, data = _copilot_config(path)
        folders = data.get("trustedFolders", [])
        if not isinstance(folders, list):
            raise ValueError("trustedFolders is not a list")
        data["trustedFolders"] = [*folders, str(folder)]
        _replace_text(path, header + json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    if trust_state(kind, path, folder) != "trusted":
        raise OSError(f"{path} still does not trust it")


def trust_agentflow_folder(io, config: Config, folder: Path = ROOT) -> None:
    """Offer to add the AgentFlow folder to each installed coding CLI's trusted folders, so none of them
    asks whether to trust it when started there. Existing entries, even untrusted ones, are left alone."""
    notes, pending = [], []
    for kind, path in trust_files(config):
        name = TRUST_NAMES[kind]
        try:
            state = trust_state(kind, path, folder)
        except (OSError, ValueError) as exc:  # TOMLDecodeError is a ValueError
            notes.append(f"   [warn] cannot read {path} ({exc}); trust {folder} in {name} by hand")
            continue
        if state == "trusted":
            notes.append(f"   {name} already trusts it ({path})")
        elif state is not None:
            notes.append(f"   {name} has it as '{state}' in {path}; left as it is")
        else:
            pending.append((kind, path))
    if not notes and not pending:
        return
    io.say(f"\n8. Trust the AgentFlow folder ({folder})")
    for note in notes:
        io.say(note)
    if not pending:
        return
    io.say("   This adds it to the trusted folders in your user-level settings, so these CLIs stop asking "
           "whether to trust it when started there:")
    for kind, path in pending:
        io.say(f"     {TRUST_NAMES[kind]}: {path}")
    if not io.confirm("   Trust it?", False):
        io.say("   Skipped")
        return
    for kind, path in pending:
        try:
            add_trust(kind, path, folder)
        except (OSError, ValueError) as exc:
            io.say(f"   [warn] {TRUST_NAMES[kind]}: {exc}; {path} was left as it was")
        else:
            io.say(f"   Trusted in {TRUST_NAMES[kind]}")


# wizard ---------------------------------------------------------------------------------
async def run_setup(io, config_file: Path, *, jira_transport: httpx.BaseTransport | None = None) -> int:
    io.say("AgentFlow setup. Press Enter to keep the value in [brackets].")
    config_file = Path(io.ask("Config file", str(config_file))).expanduser()
    raw = read_raw(config_file)
    try:
        Config.model_validate(raw)
    except ValidationError as exc:
        # Harness references are the wizard's to fix; anything else must be fixed by hand first.
        raise SetupAborted(f"{config_file} is invalid, so setup cannot update it. Fix this first:\n{exc}") from exc
    io.say(f"   {'Updating' if config_file.is_file() else 'Creating'} {config_file}")

    await prerequisites(io, raw)
    chosen = await harnesses(io, raw)
    roles(io, raw, chosen)
    await jira(io, raw, jira_transport)
    await optional(io, raw)

    io.say("\n6. Save")
    try:
        validate(raw)
    except ConfigError as exc:
        raise SetupAborted(f"The result is not a valid configuration, so nothing was written:\n{exc}") from exc
    if tracked_by_git(config_file):
        io.say(f"   [warn] {config_file} is tracked by git and now holds your Jira token; untrack it")
    if not io.confirm(f"   Write {config_file}?", True):
        raise SetupAborted("Nothing was written")
    backup = write_config(config_file, raw)
    io.say(f"   Saved{f' (previous version in {backup.name})' if backup else ''}")
    io.say(f"   Data folder: {validate(raw).data_dir}")

    try:
        # After the save, so declining it leaves the harness configs alone too.
        config = validate(raw)
        if config.jira.enabled:
            jira_mcp(io, config)
        trust_agentflow_folder(io, config)
        io.say("")
        checks = await doctor.run_checks(config_file, jira_transport=jira_transport)
    except (KeyboardInterrupt, EOFError) as exc:
        raise SetupAborted(f"{config_file} was saved, but setup stopped before it finished. "
                           "Run `agentflow doctor` to see what is left, or `agentflow setup` again.") from exc
    io.say(doctor.format_report(config_file, checks))
    io.say("\nStart AgentFlow with ./start.sh (or `agentflow`).")
    return 1 if any(check.status == "fail" for check in checks) else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="agentflow setup", description="Set up AgentFlow interactively")
    parser.add_argument("--config", help="Path to config.json (defaults to the one the backend would use)")
    args = parser.parse_args(argv)
    config_file = Path(args.config).expanduser() if args.config else config_path()
    doctor.tolerate_console_encoding()
    # Not asyncio.run: its SIGINT handler only cancels the task at the next await, so Ctrl-C at a
    # prompt (a blocking input()) would do nothing until Enter.
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(run_setup(Console(), config_file))
    except SetupAborted as exc:
        print(f"\n{exc}", file=sys.stderr)
    except (KeyboardInterrupt, EOFError):
        print("\nSetup cancelled; nothing was written.", file=sys.stderr)
    finally:
        loop.close()
    return 1
