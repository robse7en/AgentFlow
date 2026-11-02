import json
import os
import subprocess
import tomllib

import httpx
import pytest

from agentflow import doctor, setup_wizard
from agentflow.config import Config, SlackConfig, read_config
from agentflow.main import run
from agentflow.setup_wizard import SetupAborted, run_setup

TRUST_FILES = setup_wizard.trust_files

ROLES = ("architect", "orchestrator", "implementer", "codeReviewer", "securityReviewer")


class Scripted:
    """Answers prompts in order; "" takes the prompt's default, as pressing Enter does."""

    def __init__(self, *answers):
        self.answers = list(answers)
        self.said = []
        self.asked = []

    def _next(self, question):
        self.asked.append(question)
        assert self.answers, f"unexpected prompt: {question}"
        answer = self.answers.pop(0)
        if answer is KeyboardInterrupt:
            raise KeyboardInterrupt
        return answer

    def say(self, text=""):
        self.said.append(text)

    def ask(self, question, default=""):
        return self._next(question) or default

    def confirm(self, question, default=False):
        answer = self._next(question)
        return default if not answer else answer == "y"

    def secret(self, question):
        return self._next(question)

    @property
    def output(self):
        return "\n".join(self.said)


def _jira(*statuses, seen=None):
    """Answers /myself with each status in turn (the last one repeats)."""
    statuses = list(statuses) or [200]

    def handler(request):
        if seen is not None:
            seen.append(request)
        status = statuses.pop(0) if len(statuses) > 1 else statuses[0]
        return httpx.Response(status, json={"displayName": "Ada Lovelace"})
    return httpx.MockTransport(handler)


MCP_ENTRY = {"type": "http", "url": "https://mcp.atlassian.com/v2/mcp"}


def _write_mcp(path, harness_type, servers):
    path.parent.mkdir(parents=True, exist_ok=True)
    if harness_type == "codex":
        path.write_text("".join(f'[mcp_servers.{name}]\nurl = "{entry["url"]}"\n'
                                + ("enabled = false\n" if entry.get("enabled") is False else "")
                                for name, entry in servers.items()), encoding="utf-8")
    else:
        path.write_text(json.dumps({"mcpServers": servers}), encoding="utf-8")


class FakeRun:
    """Stands in for subprocess.run: records each call and acts like the harness CLI would."""

    def __init__(self, paths):
        self.paths = paths
        self.calls = []
        self.write_entry = True
        self.returncode = 0
        self.codex_status = ["not_logged_in", "o_auth"]

    def __call__(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        command = argv[1:3]
        stdout = ""
        if command == ["mcp", "add"] and self.write_entry and self.returncode == 0:
            harness_type = argv[0].rsplit("/", 1)[-1]
            _write_mcp(self.paths[harness_type], harness_type, {"atlassian": MCP_ENTRY})
        elif command == ["mcp", "list"]:
            stdout = json.dumps([{"name": "atlassian", "auth_status": self.codex_status[0]}])
        elif command == ["mcp", "login"]:
            self.codex_status.pop(0)
        return subprocess.CompletedProcess(argv, self.returncode, stdout, "boom" if self.returncode else "")


@pytest.fixture(autouse=True)
def no_real_tools(monkeypatch, tmp_path):
    """Tool and harness detection is doctor's job and tested there; here everything is present, and
    each harness's MCP config lives under tmp_path (already holding the Atlassian server)."""
    async def tools(_config):
        return [doctor.Check(name, "ok", "present") for name in ("Git", "Node.js", "UI", "Zellij")]

    async def detected(_key, _entry):
        return True, "1.0"

    async def github(_config):
        return []

    async def checks(_path, jira_transport=None):
        return [doctor.Check("Config", "ok", "checked")]

    paths = {kind: tmp_path / "mcp" / f"{kind}.{'toml' if kind == 'codex' else 'json'}"
             for kind in ("claude", "codex", "copilot")}
    for kind, path in paths.items():
        _write_mcp(path, kind, {"atlassian": MCP_ENTRY})
    fake = FakeRun(paths)
    monkeypatch.setattr(doctor, "_tools", tools)
    monkeypatch.setattr(doctor, "_github", github)
    monkeypatch.setattr(doctor, "_ffmpeg", lambda _config: [])
    monkeypatch.setattr(doctor, "run_checks", checks)
    monkeypatch.setattr(doctor, "mcp_config_path", lambda harness: paths.get(harness.type))
    monkeypatch.setattr(setup_wizard, "_detected", detected)
    monkeypatch.setattr(setup_wizard, "resolve_executable", lambda command: f"/bin/{command}")
    monkeypatch.setattr(setup_wizard, "tracked_by_git", lambda _path: False)
    monkeypatch.setattr(setup_wizard.subprocess, "run", fake)
    monkeypatch.setattr(setup_wizard, "install_slack_extra", lambda io: None)  # tested in test_installers
    monkeypatch.setattr(setup_wizard, "trust_files", lambda config: [])  # the real one is tested below
    return fake, paths


# Answers for a run that picks one harness (claude) and a working Jira Cloud sign-in.
CLAUDE_CLOUD = (
    "",                                 # config file
    "claude",                           # harnesses
    "", "subscription", "",             # sign-in, billing, trusted repositories
    "y",  # enable optional Jira integration
    "", "https://x.atlassian.net/", "a@b.com", "token-1234567890",  # Jira kind, URL, email, token
    "", "", "",                         # comments, auto-transition, PR-created transition
    "",                                 # Slack
    "",                                 # write
)


async def _setup(path, *answers, transport=None):
    io = Scripted(*answers)
    code = await run_setup(io, path, jira_transport=transport or _jira())
    assert not io.answers, f"unused answers: {io.answers}"
    return code, io


@pytest.mark.asyncio
async def test_fresh_setup_writes_a_valid_config(tmp_path):
    path = tmp_path / "config.json"
    code, _ = await _setup(path, *CLAUDE_CLOUD)
    assert code == 0
    config = read_config(path)
    assert {config.agents.for_role(role).harness for role in doctor.ROLES} == {"claude"}
    assert config.jira.enabled and config.jira.base_url == "https://x.atlassian.net"
    assert config.jira.email == "a@b.com" and config.jira.api_token == "token-1234567890"
    assert config.harnesses["claude"].billing_mode == "subscription"
    assert config.harnesses["claude"].usage_command[1].endswith("scripts/usage_claude.py")
    assert not config.harnesses["codex"].enabled and not config.harnesses["copilot"].enabled
    assert not (tmp_path / "config.json.bak").exists()
    # This checkout is a source checkout, so data stays in its data folder.
    assert config.app.data_directory == "data/.agentflow"
    assert config.workspace.worktree_directory == "data/.agentflow/worktrees"


@pytest.mark.asyncio
async def test_rerun_pressing_enter_changes_nothing(tmp_path):
    path = tmp_path / "config.json"
    await _setup(path, *CLAUDE_CLOUD)
    before = path.read_text(encoding="utf-8")
    code, io = await _setup(path, *[""] * len(CLAUDE_CLOUD))  # the token prompt keeps the saved one too
    assert code == 0
    assert path.read_text(encoding="utf-8") == before
    assert (tmp_path / "config.json.bak").read_text(encoding="utf-8") == before
    assert any("Enter keeps ...7890" in question for question in io.asked)


@pytest.mark.asyncio
async def test_sections_the_wizard_does_not_ask_about_are_left_as_written(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"workflow": {"maxConcurrentAgents": 2}}), encoding="utf-8")
    await _setup(path, *CLAUDE_CLOUD)
    raw = json.loads(path.read_text(encoding="utf-8"))
    assert raw["workflow"] == {"maxConcurrentAgents": 2}
    assert "server" not in raw and "zellij" not in raw


@pytest.mark.asyncio
async def test_choosing_only_copilot_points_every_role_at_it(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"agents": {"orchestrator": {"harness": "claude", "model": "claude-opus-5"}}}),
                    encoding="utf-8")
    # Copilot by number; it has no trusted-repositories prompt.
    await _setup(path, "", "3", "", "", *CLAUDE_CLOUD[5:])
    raw = json.loads(path.read_text(encoding="utf-8"))
    assert {raw["agents"][role]["harness"] for role in ROLES} == {"copilot"}
    assert raw["agents"]["orchestrator"]["model"] is None  # a Claude model would not exist on Copilot
    assert raw["harnesses"]["claude"]["enabled"] is False and raw["harnesses"]["codex"]["enabled"] is False
    read_config(path).validate_references()


@pytest.mark.asyncio
async def test_roles_can_be_assigned_separately(tmp_path):
    path = tmp_path / "config.json"
    answers = ["", "claude, codex", "", "", "",      # claude: sign-in, billing, trusted repositories
               "", "",                           # codex: sign-in, billing
               "y", "", "codex", "", "",         # separately; architect, implementer, reviewers
               *CLAUDE_CLOUD[5:]]
    await _setup(path, *answers)
    config = read_config(path)
    assert config.agents.implementer.harness == "codex"
    assert {config.agents.for_role(role).harness for role in ("architect", "orchestrator", "code_reviewer",
                                                              "security_reviewer")} == {"claude"}

    # Rerunning keeps the split: "separately" now defaults to yes, and each role to its harness.
    before = path.read_text(encoding="utf-8")
    await _setup(path, *[""] * (len(answers) - len(CLAUDE_CLOUD[5:]) + 10))
    assert path.read_text(encoding="utf-8") == before
    assert json.loads(before)["agents"].get("orchestrator") is None  # the Architect plans Parallel tasks


@pytest.mark.asyncio
async def test_harness_not_on_the_list_is_asked_again(tmp_path):
    path = tmp_path / "config.json"
    _, io = await _setup(path, *CLAUDE_CLOUD[:1], "cursor", *CLAUDE_CLOUD[1:])
    assert "Choose from: claude, codex, copilot" in io.output


@pytest.mark.asyncio
async def test_rejected_jira_token_asks_again(tmp_path):
    path = tmp_path / "config.json"
    retry = ("", "", "", "token-good-123456")  # try again; keep kind, URL and email; new token
    answers = [*CLAUDE_CLOUD[:10], "", *retry, *CLAUDE_CLOUD[10:]]
    code, io = await _setup(path, *answers, transport=_jira(401, 200))
    assert code == 0 and "rejected" in io.output
    assert read_config(path).jira.api_token == "token-good-123456"


@pytest.mark.asyncio
async def test_giving_up_on_jira_writes_nothing(tmp_path):
    path = tmp_path / "config.json"
    with pytest.raises(SetupAborted, match="Jira setup stopped"):
        await run_setup(Scripted(*CLAUDE_CLOUD[:10], "n"), path, jira_transport=_jira(401))
    assert not path.exists()


@pytest.mark.asyncio
async def test_switching_to_server_clears_the_email_and_uses_a_bearer_token(tmp_path):
    path = tmp_path / "config.json"
    await _setup(path, *CLAUDE_CLOUD)
    seen = []
    answers = ["", "", "", "", "",                   # config, harnesses, sign-in, billing, trusted
               "y", "server", "https://jira.example.com", "pat-abcdefghijkl", "", "", "", "", ""]
    await _setup(path, *answers, transport=_jira(seen=seen))
    config = read_config(path)
    assert config.jira.email is None and config.jira.base_url == "https://jira.example.com"
    assert seen[0].url.path == "/rest/api/2/myself" and seen[0].headers["authorization"] == "Bearer pat-abcdefghijkl"


@pytest.mark.asyncio
async def test_http_jira_url_is_refused(tmp_path):
    path = tmp_path / "config.json"
    answers = [*CLAUDE_CLOUD[:7], "http://x.atlassian.net", *CLAUDE_CLOUD[7:]]
    code, io = await _setup(path, *answers)
    assert "must start with https://" in io.output


@pytest.mark.asyncio
async def test_slack_tokens_are_saved_and_checked(tmp_path, monkeypatch):
    monkeypatch.delenv("AGENTFLOW_SLACK_BOT_TOKEN", raising=False)
    monkeypatch.delenv("AGENTFLOW_SLACK_APP_TOKEN", raising=False)
    monkeypatch.setattr(setup_wizard.getpass, "getuser", lambda: "Ada Lovelace")
    path = tmp_path / "config.json"
    # A new agent is offered this machine's user name; a name Slack would refuse is asked again.
    slack = ("y", "/Agent Flow", "", "", "not-a-token", "xoxb-1", "xapp-1", "u123", "U123")
    code, io = await _setup(path, *CLAUDE_CLOUD[:-2], *slack, "")
    config = read_config(path)
    assert config.slack.enabled and config.slack.user_id == "U123" and config.slack.command == "/agentflow-ada"
    assert config.slack.bot_token == "xoxb-1" and config.slack.app_token == "xapp-1"
    manifest = json.loads(next(text for text in io.said if text.startswith("{")))
    assert manifest["display_information"]["name"] == "AgentFlow Ada"
    assert manifest["features"]["bot_user"]["display_name"] == "[AgentFlow] Ada"
    assert manifest["features"]["slash_commands"][0]["command"] == "/agentflow-ada"
    assert "changes from" not in io.output


def test_slack_command_and_app_are_named_after_the_agent():
    typed = {}
    io = Scripted("x" * 22, "Big [Jarvis]!", "")
    setup_wizard.slack_app(io, typed)
    # Slack refuses symbols in the bot's name, so they are dropped from it as from the command.
    assert typed["command"] == "/agentflow-big-jarvis" and '"name": "AgentFlow Big Jarvis"' in io.output
    assert '"display_name": "[AgentFlow] Big Jarvis"' in io.output
    # A configured command is offered again, by its name part.
    configured = {"userId": "U1", "command": "/agentflow-ops"}
    io = Scripted("", "")
    setup_wizard.slack_app(io, configured)
    assert configured["command"] == "/agentflow-ops" and '"name": "AgentFlow ops"' in io.output
    # A whole command is kept as typed, so slack.command can be anything Slack accepts.
    custom = {}
    setup_wizard.slack_app(Scripted("/Team-Bot", ""), custom)
    assert custom["command"] == "/team-bot"
    # An app made before commands were configurable keeps /agentflow unless given a name, and
    # renaming it says the Slack app must be updated too.
    legacy = {"userId": "U1"}
    io = Scripted("", "")
    setup_wizard.slack_app(io, legacy)
    assert legacy["command"] == "/agentflow" and '"display_name": "AgentFlow"' in io.output
    io = Scripted("rob", "")
    setup_wizard.slack_app(io, legacy)
    assert legacy["command"] == "/agentflow-rob" and "changes from /agentflow to /agentflow-rob" in io.output


def test_slack_app_can_go_without_a_slash_command(monkeypatch):
    """Without one, nothing shows in anyone else's / menu; the operator types commands in the DM."""
    monkeypatch.setattr(setup_wizard.getpass, "getuser", lambda: "ada")
    section = {}
    io = Scripted("Jarvis", "n")
    setup_wizard.slack_app(io, section)
    manifest = json.loads(next(text for text in io.said if text.startswith("{")))
    assert section["command"] is None and "slash_commands" not in manifest["features"]
    assert "commands" not in manifest["oauth_config"]["scopes"]["bot"]
    assert manifest["display_information"]["name"] == "AgentFlow Jarvis" and "No slash command" in io.output
    # Run again, the app stays without one by default; adding one says the Slack app must be updated.
    section["userId"] = "U1"
    io = Scripted("", "")
    setup_wizard.slack_app(io, section)
    assert section["command"] is None and "changes from" not in io.output
    io = Scripted("jarvis", "y")
    setup_wizard.slack_app(io, section)
    assert section["command"] == "/agentflow-jarvis" and "changes from none to /agentflow-jarvis" in io.output
    assert SlackConfig.model_validate(section).command == "/agentflow-jarvis"
    assert SlackConfig.model_validate({"command": None}).command is None


def test_slack_manifest_is_valid_json_for_any_name():
    manifest = json.loads(setup_wizard.slack_manifest("Rob's \"AgentFlow\": main", "/agentflow-rob"))
    assert manifest["display_information"]["name"] == manifest["features"]["bot_user"]["display_name"]
    assert manifest["settings"]["socket_mode_enabled"] and manifest["settings"]["interactivity"]["is_enabled"]
    assert manifest["settings"]["event_subscriptions"]["bot_events"] == ["message.im"]
    assert "commands" in manifest["oauth_config"]["scopes"]["bot"]


@pytest.mark.asyncio
async def test_invalid_json_is_not_overwritten(tmp_path):
    path = tmp_path / "config.json"
    path.write_text("{ not json", encoding="utf-8")
    with pytest.raises(SetupAborted, match="not valid JSON"):
        await run_setup(Scripted(""), path)
    assert path.read_text(encoding="utf-8") == "{ not json"


@pytest.mark.asyncio
async def test_declining_to_write_leaves_the_file_alone(tmp_path):
    path = tmp_path / "config.json"
    with pytest.raises(SetupAborted, match="Nothing was written"):
        await run_setup(Scripted(*CLAUDE_CLOUD[:-1], "n"), path, jira_transport=_jira())
    assert not path.exists()


@pytest.mark.asyncio
async def test_a_file_invalid_beyond_its_harnesses_stops_before_any_question(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"server": {"port": "not a port"}}), encoding="utf-8")
    with pytest.raises(SetupAborted, match="Fix this first"):
        await run_setup(Scripted(""), path)


@pytest.mark.asyncio
async def test_a_broken_harness_reference_is_fixed_not_refused(tmp_path):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"agents": {"architect": {"harness": "gone"}}}), encoding="utf-8")
    await _setup(path, *CLAUDE_CLOUD)
    assert read_config(path).agents.architect.harness == "claude"


@pytest.mark.skipif(os.name == "nt", reason="POSIX file modes")
def test_write_config_keeps_the_file_mode_and_makes_new_files_private(tmp_path):
    path = tmp_path / "config.json"
    setup_wizard.write_config(path, {})
    assert path.stat().st_mode & 0o777 == 0o600
    path.chmod(0o640)
    setup_wizard.write_config(path, {"new": True})
    assert path.stat().st_mode & 0o777 == 0o640


def test_write_config_is_atomic_and_keeps_a_backup(tmp_path):
    path = tmp_path / "config.json"
    path.write_text('{"old": true}', encoding="utf-8")
    backup = setup_wizard.write_config(path, {"new": True})
    assert backup == tmp_path / "config.json.bak"
    assert json.loads(backup.read_text(encoding="utf-8")) == {"old": True}
    assert json.loads(path.read_text(encoding="utf-8")) == {"new": True}
    assert not (tmp_path / "config.json.tmp").exists()


def test_agentflow_setup_subcommand_reports_a_cancel(tmp_path, monkeypatch, capsys):
    def interrupted(_prompt=""):
        raise KeyboardInterrupt
    monkeypatch.setattr("builtins.input", interrupted)
    with pytest.raises(SystemExit) as exited:
        run(["setup", "--config", str(tmp_path / "config.json")])
    assert exited.value.code == 1
    assert "nothing was written" in capsys.readouterr().err
    assert not (tmp_path / "config.json").exists()


# Jira MCP -------------------------------------------------------------------------------
def _mcp_config(harness_type="claude", env=None, jira=None):
    agent = {"harness": "h"}
    return Config.model_validate({
        "harnesses": {"h": {"type": harness_type, "command": harness_type, "displayName": "H", "env": env or {}}},
        "agents": {"architect": agent, "implementer": agent, "codeReviewer": agent, "securityReviewer": agent},
        "jira": jira or {"enabled": True, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t"},
    })


def test_adds_the_atlassian_mcp_to_claude_at_user_scope_with_the_agents_env(no_real_tools):
    fake, paths = no_real_tools
    paths["claude"].unlink()
    io = Scripted("y", "")  # add it; press Enter after signing in
    setup_wizard.jira_mcp(io, _mcp_config())
    argv, kwargs = fake.calls[0]
    assert argv == ["/bin/claude", "mcp", "add", "--transport", "http", "--scope", "user", "atlassian",
                    "https://mcp.atlassian.com/v2/mcp"]
    assert kwargs["env"] == setup_wizard.clean_env({}) and kwargs["stdin"] == subprocess.DEVNULL
    assert f"Added to {paths['claude']}" in io.output and "/mcp" in io.output


def test_copilot_uses_its_own_add_command(no_real_tools):
    fake, paths = no_real_tools
    paths["copilot"].unlink()
    io = Scripted("y", "")
    setup_wizard.jira_mcp(io, _mcp_config("copilot", env={"COPILOT_HOME": "/elsewhere/copilot"}))
    assert fake.calls[0][0] == ["/bin/copilot", "mcp", "add", "--transport", "http", "atlassian",
                                "https://mcp.atlassian.com/v2/mcp"]
    # A session started without the relocated home would sign in against a different config.
    assert "/mcp auth atlassian" in io.output
    assert "start it with COPILOT_HOME=/elsewhere/copilot set" in io.output


def test_mcp_consent_defaults_to_no(no_real_tools):
    fake, paths = no_real_tools
    paths["claude"].unlink()
    io = Scripted("")
    setup_wizard.jira_mcp(io, _mcp_config())
    assert fake.calls == [] and "Skipped" in io.output


def test_existing_jira_servers_are_never_touched(no_real_tools):
    fake, paths = no_real_tools
    _write_mcp(paths["claude"], "claude", {
        "atlassian": {**MCP_ENTRY, "disabled": True},
        "old-jira": {"type": "sse", "url": "https://mcp.atlassian.com/v1/sse"},
    })
    io = Scripted()  # no prompts at all
    setup_wizard.jira_mcp(io, _mcp_config())
    assert fake.calls == []
    assert "'atlassian'" in io.output and "disabled" in io.output
    assert "retired /v1/sse endpoint" in io.output


def test_codex_add_runs_interactively_then_checks_and_offers_sign_in(no_real_tools):
    fake, paths = no_real_tools
    paths["codex"].unlink()
    env = {"CODEX_HOME": "/somewhere/codex"}
    io = Scripted("y", "")  # add it; sign in now (the default)
    setup_wizard.jira_mcp(io, _mcp_config("codex", env=env))
    commands = [call[0][1:3] for call in fake.calls]
    assert commands == [["mcp", "add"], ["mcp", "list"], ["mcp", "login"], ["mcp", "list"]]
    add = fake.calls[0]
    assert add[0][3:] == ["atlassian", "--url", "https://mcp.atlassian.com/v2/mcp"]
    assert "capture_output" not in add[1]  # the OAuth prompt is for the user
    assert all(call[1]["env"] == setup_wizard.clean_env(env) for call in fake.calls)
    assert "CODEX_HOME=/somewhere/codex" in io.output
    assert "Sign-in status: o_auth" in io.output


def test_a_failing_add_is_reported_not_fatal(no_real_tools):
    fake, paths = no_real_tools
    paths["claude"].unlink()
    fake.returncode = 1
    io = Scripted("y")
    setup_wizard.jira_mcp(io, _mcp_config())
    assert "failed (exit 1): boom" in io.output


def test_an_add_that_writes_elsewhere_is_reported(no_real_tools):
    fake, paths = no_real_tools
    paths["copilot"].unlink()
    fake.write_entry = False
    io = Scripted("y")
    setup_wizard.jira_mcp(io, _mcp_config("copilot"))
    assert "has no 'atlassian' entry" in io.output


def test_generic_cli_gets_manual_instructions(no_real_tools):
    fake, _ = no_real_tools
    io = Scripted()
    setup_wizard.jira_mcp(io, _mcp_config("generic-cli"))
    assert fake.calls == [] and "by hand" in io.output


def test_server_data_center_skips_the_mcp_step(no_real_tools):
    fake, paths = no_real_tools
    paths["claude"].unlink()
    io = Scripted()
    setup_wizard.jira_mcp(io, _mcp_config(jira={"enabled": True, "baseUrl": "https://j.example.com",
                                                "apiToken": "pat"}))
    assert fake.calls == [] and io.said == []


@pytest.mark.asyncio
async def test_interrupting_after_the_save_says_the_file_was_saved(tmp_path, no_real_tools):
    _, paths = no_real_tools
    paths["claude"].unlink()
    path = tmp_path / "config.json"
    with pytest.raises(SetupAborted, match="was saved, but setup stopped"):
        await run_setup(Scripted(*CLAUDE_CLOUD, KeyboardInterrupt), path, jira_transport=_jira())
    assert read_config(path).jira.enabled


# Folder trust ---------------------------------------------------------------------------
COPILOT_HEADER = "// User settings belong in settings.json.\n// This file is managed automatically.\n"


@pytest.fixture
def cli_home(monkeypatch, tmp_path):
    """A home holding each CLI's config, with other settings and trusted folders already in it."""
    home = tmp_path / "home"
    (home / ".codex").mkdir(parents=True)
    (home / ".copilot").mkdir()
    (home / ".claude.json").write_text(json.dumps({"numStartups": 3, "projects": {"C:/Dev": {
        "hasTrustDialogAccepted": True}}}), encoding="utf-8")
    (home / ".codex" / "config.toml").write_text(
        "model = \"gpt-5.5\"\n\n[projects.'c:\\dev']\ntrust_level = \"trusted\"\n", encoding="utf-8")
    (home / ".copilot" / "config.json").write_text(
        COPILOT_HEADER + json.dumps({"trustedFolders": ["C:\\Dev"], "cache_path": "C:\\Users\\Zoë"}, indent=2,
                                    ensure_ascii=False) + "\n",
        encoding="utf-8")
    monkeypatch.setattr(setup_wizard.Path, "home", lambda: home)
    monkeypatch.setattr(setup_wizard, "trust_files", TRUST_FILES)
    return home


def _files(home):
    return {name: (home / name).read_text(encoding="utf-8")
            for name in (".claude.json", ".codex/config.toml", ".copilot/config.json")}


def test_trusts_the_folder_in_each_cli_as_its_own_trust_prompt_would(cli_home, tmp_path):
    folder = tmp_path / "AgentFlow"
    io = Scripted("y")
    setup_wizard.trust_agentflow_folder(io, Config(), folder)
    claude = json.loads((cli_home / ".claude.json").read_text(encoding="utf-8"))
    assert claude["projects"][folder.as_posix()] == {"hasTrustDialogAccepted": True}
    assert claude["numStartups"] == 3 and "C:/Dev" in claude["projects"]
    codex = tomllib.loads((cli_home / ".codex" / "config.toml").read_text(encoding="utf-8"))
    assert codex["projects"][os.path.normcase(str(folder))] == {"trust_level": "trusted"}
    assert codex["model"] == "gpt-5.5" and len(codex["projects"]) == 2
    copilot = (cli_home / ".copilot" / "config.json").read_text(encoding="utf-8")
    assert copilot.startswith(COPILOT_HEADER) and "Zoë" in copilot
    assert json.loads(copilot[len(COPILOT_HEADER):]) == {"trustedFolders": ["C:\\Dev", str(folder)],
                                                         "cache_path": "C:\\Users\\Zoë"}
    assert io.output.count("Trusted in") == 3

    before = _files(cli_home)
    again = Scripted()  # nothing left to ask
    setup_wizard.trust_agentflow_folder(again, Config(), folder)
    assert again.output.count("already trusts it") == 3
    assert _files(cli_home) == before


def test_trust_consent_defaults_to_no(cli_home, tmp_path):
    before = _files(cli_home)
    io = Scripted("")
    setup_wizard.trust_agentflow_folder(io, Config(), tmp_path / "AgentFlow")
    assert "Skipped" in io.output and _files(cli_home) == before


def test_clis_that_are_not_installed_are_left_out(monkeypatch, tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setattr(setup_wizard.Path, "home", lambda: home)
    monkeypatch.setattr(setup_wizard, "trust_files", TRUST_FILES)
    io = Scripted()
    setup_wizard.trust_agentflow_folder(io, Config(), tmp_path / "AgentFlow")
    assert io.said == [] and list(home.iterdir()) == []


def test_a_codex_entry_that_is_not_trusted_is_left_as_it_is(cli_home, tmp_path):
    folder = tmp_path / "AgentFlow"
    codex = cli_home / ".codex" / "config.toml"
    codex.write_text(f"[projects.{json.dumps(str(folder).upper())}]\ntrust_level = \"untrusted\"\n", encoding="utf-8")
    before = codex.read_text(encoding="utf-8")
    io = Scripted("y")
    setup_wizard.trust_agentflow_folder(io, Config(), folder)
    assert "has it as 'untrusted'" in io.output and "Codex CLI:" not in io.output
    assert codex.read_text(encoding="utf-8") == before
    assert io.output.count("Trusted in") == 2


def test_unreadable_configs_are_reported_and_left_alone(cli_home, tmp_path):
    (cli_home / ".codex" / "config.toml").write_text("projects = [", encoding="utf-8")
    (cli_home / ".copilot" / "config.json").write_text(COPILOT_HEADER + "{", encoding="utf-8")
    before = _files(cli_home)
    io = Scripted("y")
    setup_wizard.trust_agentflow_folder(io, Config(), tmp_path / "AgentFlow")
    assert io.output.count("cannot read") == 2 and io.output.count("Trusted in") == 1
    after = _files(cli_home)
    assert after[".codex/config.toml"] == before[".codex/config.toml"]
    assert after[".copilot/config.json"] == before[".copilot/config.json"]


def test_a_codex_config_that_cannot_take_the_entry_is_not_written(cli_home, tmp_path):
    codex = cli_home / ".codex" / "config.toml"
    inline = "projects = { other = { trust_level = \"trusted\" } }\n"  # a [projects.x] table can't be added
    codex.write_text(inline, encoding="utf-8")
    io = Scripted("y")
    setup_wizard.trust_agentflow_folder(io, Config(), tmp_path / "AgentFlow")
    assert "[warn] Codex CLI" in io.output and "left as it was" in io.output
    assert codex.read_text(encoding="utf-8") == inline


def test_follows_codex_home_and_copilot_home_in_the_harness_env(cli_home, tmp_path):
    codex_home, copilot_home = tmp_path / "codex-home", tmp_path / "copilot-home"
    codex_home.mkdir()
    copilot_home.mkdir()
    config = Config.model_validate({"harnesses": {
        "codex": {"type": "codex", "command": "codex", "env": {"CODEX_HOME": str(codex_home)}},
        "copilot": {"type": "copilot", "command": "copilot", "env": {"COPILOT_HOME": str(copilot_home)}},
    }})
    files = setup_wizard.trust_files(config)
    assert ("codex", codex_home / "config.toml") in files and ("copilot", copilot_home / "config.json") in files
    folder = tmp_path / "AgentFlow"
    setup_wizard.trust_agentflow_folder(Scripted("y"), config, folder)
    codex = tomllib.loads((codex_home / "config.toml").read_text(encoding="utf-8"))
    assert codex["projects"][os.path.normcase(str(folder))] == {"trust_level": "trusted"}
    copilot = json.loads((copilot_home / "config.json").read_text(encoding="utf-8"))
    assert copilot["trustedFolders"] == [str(folder)]


@pytest.mark.asyncio
async def test_setup_offers_to_trust_the_agentflow_folder_after_saving(tmp_path, monkeypatch):
    copilot = tmp_path / "copilot" / "config.json"
    copilot.parent.mkdir()
    monkeypatch.setattr(setup_wizard, "trust_files", lambda config: [("copilot", copilot)])
    await _setup(tmp_path / "config.json", *CLAUDE_CLOUD, "y")
    assert json.loads(copilot.read_text(encoding="utf-8"))["trustedFolders"] == [str(setup_wizard.ROOT)]


@pytest.mark.asyncio
async def test_jira_can_be_skipped_without_network():
    raw = {}
    await setup_wizard.jira(Scripted("n"), raw)
    assert raw["jira"]["enabled"] is False
