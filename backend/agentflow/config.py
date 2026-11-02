"""Configuration loading and validation.

Precedence (lowest → highest):
    built-in defaults → config.json → task-level overrides → node-level overrides

In a source checkout, config.json lives in the AgentFlow project directory (an existing
~/.agentflow/config.json is still used while the project has none). Installed copies use
~/.agentflow/config.json.

A config written on first launch or by `agentflow setup` in a source checkout keeps its data in the
checkout's data/.agentflow. Relative data paths are relative to the AgentFlow folder, never the
current directory; the built-in defaults (for files that omit them) stay ~/.agentflow.

The task/node overrides are applied by the workflow engine (see ``resolve_agent_config``);
this module owns the file-backed part.
"""

from __future__ import annotations

import json
import os
import re
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator

ExecutionMode = Literal["manual", "automatic", "approval_required"]
ReasoningLevel = Literal["low", "medium", "high"]


def agentflow_home() -> Path:
    """Directory holding config.json, the database and task metadata."""
    override = os.environ.get("AGENTFLOW_HOME")
    return Path(override).expanduser() if override else Path.home() / ".agentflow"


def config_path() -> Path:
    override = os.environ.get("AGENTFLOW_CONFIG")
    if override:
        return Path(override).expanduser()

    # Keep source-checkout settings with the project, while retaining the
    # per-user location for installed copies and explicit AGENTFLOW_HOME use.
    home_override = os.environ.get("AGENTFLOW_HOME")
    if home_override:
        return agentflow_home() / "config.json"

    # A checkout keeps its config with the project; an existing ~/.agentflow/config.json from before
    # that default is still used until a project config.json exists.
    home_config = agentflow_home() / "config.json"
    project_root = source_checkout()
    if project_root and ((project_root / "config.json").is_file() or not home_config.is_file()):
        return project_root / "config.json"

    return home_config


def source_checkout() -> Path | None:
    """The AgentFlow folder when running from a source checkout (as setup.ps1 and setup.sh install it)."""
    project_root = Path(__file__).resolve().parents[2]
    return project_root if (project_root / "backend" / "pyproject.toml").is_file() else None


def resolve_path(value: str) -> Path:
    """A configured path: `~` expands, and a relative path is relative to the AgentFlow folder (the
    AgentFlow home for installed copies), so it does not depend on where AgentFlow was started."""
    path = Path(value).expanduser()
    return path if path.is_absolute() else (source_checkout() or agentflow_home()) / path


class _Model(BaseModel):
    model_config = ConfigDict(populate_by_name=True, extra="forbid")


class AppConfig(_Model):
    data_directory: str = Field("~/.agentflow", alias="dataDirectory")
    database_path: str = Field("~/.agentflow/agentflow.db", alias="databasePath")


class ServerConfig(_Model):
    host: Literal["127.0.0.1", "localhost", "::1"] = "127.0.0.1"
    port: int = 8765
    # Windows: start.sh restarts itself elevated (UAC prompt), so worktree cleanup can close
    # processes running as administrator. Everything the backend launches, agents included,
    # then runs as administrator too.
    run_as_administrator: bool = Field(False, alias="runAsAdministrator")


def copy_path_problem(path: str) -> str | None:
    """Why a worktree setup copy path is unsafe, or None (the Rust core's `copy_path_problem`)."""
    trimmed = path.strip()
    if not trimmed or "\x00" in path:
        return "Copy paths must be nonempty"
    parts = [part for part in re.split(r"[/\\]", trimmed) if part and part != "."]
    if trimmed.startswith(("/", "\\")) or ":" in trimmed:
        return "Copy paths must be relative to the repository root"
    if not parts or ".." in parts:
        return "Copy paths must stay inside the repository"
    if parts[0].lower() == ".git":
        return "Copy paths must not touch .git"
    return None


class WorktreeSetup(_Model):
    """How fresh worktrees of one repository are prepared. Accepted here so both backends read the
    same config.json; only the Rust core runs it."""

    copy_paths: list[str] = Field(default_factory=list, alias="copy")
    commands: list[list[str]] = Field(default_factory=list)
    timeout_seconds: int = Field(900, alias="timeoutSeconds", ge=1, le=86400)

    @field_validator("copy_paths")
    @classmethod
    def safe_copy_paths(cls, paths):
        for path in paths:
            if problem := copy_path_problem(path):
                raise ValueError(problem)
        return paths

    @field_validator("commands")
    @classmethod
    def valid_commands(cls, commands):
        if any(not argv or any(not part.strip() or "\x00" in part for part in argv) for argv in commands):
            raise ValueError("Setup commands must contain nonempty argv arrays")
        return commands


class WorkspaceConfig(_Model):
    worktree_directory: str = Field("~/.agentflow/worktrees", alias="worktreeDirectory")
    task_directory: str = Field("~/.agentflow/tasks", alias="taskDirectory")
    branch_prefix: str = Field("", alias="branchPrefix")
    # Prefilled (but always editable) in the New Task form's repository directory field.
    default_repository_directory: str | None = Field(None, alias="defaultRepositoryDirectory")
    # Keyed by repository root.
    worktree_setup: dict[str, WorktreeSetup] = Field(default_factory=dict, alias="worktreeSetup")

    @field_validator("worktree_setup")
    @classmethod
    def nonempty_roots(cls, setups):
        if any(not root.strip() for root in setups):
            raise ValueError("Repository roots must be nonempty")
        return setups


class CostControls(_Model):
    task_budget_usd: float | None = Field(None, alias="taskBudgetUsd", gt=0, allow_inf_nan=False)
    daily_budget_usd: float | None = Field(None, alias="dailyBudgetUsd", gt=0, allow_inf_nan=False)
    warning_percent: int = Field(80, alias="warningPercent", ge=1, le=100)
    launch_reserve_usd: float = Field(1, alias="launchReserveUsd", gt=0, allow_inf_nan=False)
    copilot_roles: list[Literal["architect", "orchestrator", "implementer", "code_reviewer", "security_reviewer"]] = Field(default_factory=list, alias="copilotRoles")
    require_metered_approval: bool = Field(False, alias="requireMeteredApproval")


class WorkflowConfig(_Model):
    heartbeat_warning_seconds: int = Field(600, alias="heartbeatWarningSeconds", ge=300, le=86400)
    cost_controls: CostControls = Field(default_factory=CostControls, alias="costControls")
    implementation_mode: Literal["direct", "parallel"] = Field("direct", alias="implementationMode")
    max_concurrent_agents: int = Field(4, alias="maxConcurrentAgents", ge=1, le=32)
    # Fixed display-only conversion for showing reported USD costs in NZD (see
    # frontend ExecutionEvidence.vue); not a live FX lookup, so it drifts and is
    # meant to be bumped by hand from Settings when it does.
    usd_to_nzd_rate: float = Field(1.65, alias="usdToNzdRate", gt=0, allow_inf_nan=False)
    verification_commands: list[list[str]] = Field(default_factory=list, alias="verificationCommands")

    @field_validator("verification_commands")
    @classmethod
    def valid_commands(cls, commands):
        if any(not argv or any(not part.strip() or "\x00" in part for part in argv) for argv in commands):
            raise ValueError("Verification commands must contain nonempty argv arrays")
        return commands
    max_review_iterations: int = Field(5, alias="maxReviewIterations", ge=1, le=50)
    max_requirements_revisions: int = Field(3, alias="maxRequirementsRevisions", ge=1, le=50)
    default_execution_mode: ExecutionMode = Field("manual", alias="defaultExecutionMode")
    require_requirements_approval: bool = Field(True, alias="requireRequirementsApproval")
    require_final_approval: bool = Field(True, alias="requireFinalApproval")
    commit_checkpoints: bool = Field(True, alias="commitCheckpoints")


class ZellijConfig(_Model):
    command: str = "zellij"
    session_prefix: str = Field("agentflow-", alias="sessionPrefix")
    web_client_enabled: bool = Field(False, alias="webClientEnabled")
    web_client_base_url: str = Field("http://127.0.0.1:8082", alias="webClientBaseUrl")
    # Shell used inside agent tabs to run the generated launcher script.
    # Auto-detected when null (pwsh → powershell on Windows, sh elsewhere).
    shell: str | None = None
    # Windows only: terminal size of the hidden client that keeps each session sized
    # (zellij lays out new tabs against an attached client's viewport). Kept above zellij's
    # 50-column clientless default, which is how a new session's client is confirmed.
    headless_cols: int = Field(200, alias="headlessCols", ge=60, le=1000)
    headless_rows: int = Field(50, alias="headlessRows", ge=10, le=500)
    # Desktop app only: run agents in zellij sessions instead of its built-in terminals.
    # This web backend always uses zellij and ignores it.
    use_in_desktop: bool = Field(False, alias="useInDesktop")


class JiraConfig(_Model):
    attachment_hosts: list[str] = Field(default_factory=lambda: ["api.media.atlassian.com"], alias="attachmentHosts")
    max_attachment_bytes: int = Field(20000000, alias="maxAttachmentBytes", ge=1)
    max_total_attachment_bytes: int = Field(100000000, alias="maxTotalAttachmentBytes", ge=1)
    max_attachments: int = Field(30, alias="maxAttachments", ge=0, le=1000)
    # Comments (with their embedded screenshots marked inline) go into context/jira-issue.md.
    fetch_comments: bool = Field(True, alias="fetchComments")
    # Keyframes pulled from video attachments with ffmpeg so the Architect can view them; 0 = off.
    max_frames_per_video: int = Field(20, alias="maxFramesPerVideo", ge=0, le=200)
    # Total ffmpeg time per sync (it runs before an Architect launch); leftover videos wait for the next.
    frame_extraction_timeout_seconds: float = Field(120, alias="frameExtractionTimeoutSeconds", gt=0)
    # Null = `ffmpeg`/`ffprobe` on PATH; otherwise the ffmpeg binary, with ffprobe beside it.
    ffmpeg_path: str | None = Field(None, alias="ffmpegPath")
    enabled: bool = False
    base_url: str | None = Field(None, alias="baseUrl")
    # Jira Cloud: set both (Basic auth with an API token). Server/Data Center PAT: leave
    # email unset and put the PAT in api_token (sent as a Bearer token instead).
    email: str | None = None
    api_token: str | None = Field(None, alias="apiToken")
    # Unset (default) = feature off. When set, AgentFlow itself transitions the ticket to this
    # status the moment the human clicks Run on the Architect node — separate from `enabled`
    # above so turning on the (read-only) search/attachment integration never silently starts
    # writing to Jira too.
    auto_transition_status: str | None = Field(None, alias="autoTransitionStatus")
    auto_transition_from_statuses: list[str] = Field(
        default_factory=lambda: ["To Do"], alias="autoTransitionFromStatuses")
    # Null or empty = off. When a task becomes PR Created (marked by hand or found by GitHub PR
    # sync), AgentFlow moves the ticket to this status. "*" in the from-list matches any status.
    pr_created_transition_status: str | None = Field("In Code Review", alias="prCreatedTransitionStatus")
    pr_created_transition_from_statuses: list[str] = Field(
        default_factory=lambda: ["*"], alias="prCreatedTransitionFromStatuses")

    def missing(self) -> list[str]:
        """Settings the integration needs that are unset (Server/Data Center has no email)."""
        return [f"jira.{name}" for name, value in (("enabled", self.enabled), ("baseUrl", self.base_url),
                                                   ("apiToken", self.api_token)) if not value]


# Node states (a stage needs you) and task statuses (the ticket changed) that may trigger a DM.
SlackNotifyOn = Literal["WAITING_FOR_USER", "WAITING_FOR_APPROVAL", "FAILED", "INTERRUPTED", "READY",
                        "NEEDS_HUMAN_REVIEW", "READY_FOR_PR", "CANCELLED"]
# Task detail sections the operator may ask for in Slack (see workflow/task_details.py).
SlackDetail = Literal["requirements", "findings", "errors", "cost", "activity", "pr", "ticket"]


class SlackAskConfig(_Model):
    """Free-text questions answered by the operator's own harness CLI, run headless with every tool
    off (services/chat/ask.py). AgentFlow itself holds no model credentials."""

    enabled: bool = False
    # A configured harness of type "claude" or "copilot"; its command and env are used, never its args.
    harness: str = "claude"
    model: str | None = None  # None = the CLI's own default
    timeout_seconds: float = Field(90, alias="timeoutSeconds", gt=0, le=600)
    # A harness whose billingMode is not "subscription" answers only when this is set.
    allow_metered: bool = Field(False, alias="allowMetered")


# Slack's rule for slash command names: lowercase, no spaces, at most 32 characters.
SLACK_COMMAND_PATTERN = r"^/[a-z0-9_-]{1,31}$"


class SlackConfig(_Model):
    """Opt-in Slack DMs over Socket Mode (an outbound connection; nothing listens for Slack)."""

    enabled: bool = False
    # Prefer AGENTFLOW_SLACK_BOT_TOKEN / AGENTFLOW_SLACK_APP_TOKEN; these fields are the fallback.
    bot_token: str | None = Field(None, alias="botToken")
    app_token: str | None = Field(None, alias="appToken")
    # The one operator's Slack member ID (U…). Only this user is DMed and may use the slash command.
    user_id: str | None = Field(None, alias="userId", pattern=r"^[UW][A-Z0-9]+$")
    # This app's slash command. Slash commands are shared by every app in a workspace, so when
    # several people run AgentFlow there, each app needs its own (e.g. "/agentflow-rob"). None = no
    # slash command, so nothing shows in anyone's / menu; commands are typed in the DM instead.
    command: str | None = Field("/agentflow", pattern=SLACK_COMMAND_PATTERN)
    # Agent question text may be posted. Notifications never carry findings or status messages;
    # those reach Slack only when asked for, through `details`.
    include_question_text: bool = Field(True, alias="includeQuestionText")
    # Sections `details`/`activity` may post (they are stored in Slack). [] = none.
    details: list[SlackDetail] = Field(default_factory=list)
    # "HH:MM" (this machine's local time): post the digest daily, when any task is active. None = off.
    digest_at: str | None = Field(None, alias="digestAt", pattern=r"^([01]\d|2[0-3]):[0-5]\d$")
    # Answer free-text questions about tasks from `details`-filtered data; off by default.
    ask: SlackAskConfig = SlackAskConfig()
    notify: list[SlackNotifyOn] = Field(default_factory=lambda: [
        "WAITING_FOR_USER", "WAITING_FOR_APPROVAL", "FAILED", "INTERRUPTED",
        "NEEDS_HUMAN_REVIEW", "READY_FOR_PR"])
    ui_base_url: str = Field("http://127.0.0.1:5173", alias="uiBaseUrl", pattern=r"^https?://")
    # What the operator may do from Slack. "answer": reply to agent questions (needs
    # includeQuestionText, since you must see the question). "create": create tasks.
    # "run": start a READY stage (never one that needs API launch approval). "requestChanges": send
    # requirements back to the Architect. "approveRequirements": approve them after Slack posts them
    # in full (the approval is bound to what was posted). "cancel": cancel a task. "markPr": mark a
    # PR created or merged. [] = notifications only.
    allow_actions: list[Literal["answer", "create", "run", "requestChanges", "approveRequirements", "cancel",
                                "markPr"]] = Field(default_factory=lambda: ["answer"], alias="allowActions")

    @property
    def answering(self) -> bool:
        return "answer" in self.allow_actions and self.include_question_text

    @property
    def creating(self) -> bool:
        return "create" in self.allow_actions

    @property
    def running(self) -> bool:
        return "run" in self.allow_actions

    @property
    def requesting_changes(self) -> bool:
        return "requestChanges" in self.allow_actions

    @property
    def approving_requirements(self) -> bool:
        return "approveRequirements" in self.allow_actions

    @property
    def cancelling(self) -> bool:
        return "cancel" in self.allow_actions

    @property
    def marking_pr(self) -> bool:
        return "markPr" in self.allow_actions

    def resolved_bot_token(self) -> str | None:
        return os.environ.get("AGENTFLOW_SLACK_BOT_TOKEN") or self.bot_token

    def resolved_app_token(self) -> str | None:
        return os.environ.get("AGENTFLOW_SLACK_APP_TOKEN") or self.app_token

    def problems(self) -> list[str]:
        """Why an enabled integration cannot start, in terms the operator can act on."""
        problems = []
        if not (self.resolved_bot_token() or "").startswith("xoxb-"):
            problems.append("set AGENTFLOW_SLACK_BOT_TOKEN (or slack.botToken) to the xoxb- bot token")
        if not (self.resolved_app_token() or "").startswith("xapp-"):
            problems.append("set AGENTFLOW_SLACK_APP_TOKEN (or slack.appToken) to the xapp- app-level token")
        if not self.user_id:
            problems.append("set slack.userId to your Slack member ID (U…)")
        return problems


class GitHubConfig(_Model):
    """Read-only PR tracking through the `gh` CLI. Polled rather than webhook-driven, since
    AgentFlow binds to loopback and GitHub cannot reach it."""

    # Moves Ready for PR -> PR Created -> Merged when GitHub shows the task branch's PR open or merged.
    pr_sync: bool = Field(True, alias="prSync")
    poll_interval_seconds: int = Field(120, alias="pollIntervalSeconds", ge=30, le=3600)
    command: str = "gh"
    # Only remotes and PR URLs on these hosts are looked up (add a GitHub Enterprise host here).
    hosts: list[str] = Field(default_factory=lambda: ["github.com"])


class HarnessConfig(_Model):
    billing_mode: Literal["metered", "subscription", "unknown"] = Field("unknown", alias="billingMode")
    trusted_repositories: list[str] = Field(default_factory=list, alias="trustedRepositories")
    usage_command: list[str] | None = Field(None, alias="usageCommand")

    @field_validator("env", check_fields=False)
    @classmethod
    def valid_environment_names(cls, value):
        import re
        if any(not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key) for key in value):
            raise ValueError("Harness environment variable names must be shell-safe identifiers")
        return value
    enabled: bool = True
    type: str = "generic-cli"
    display_name: str = Field("", alias="displayName")
    command: str
    args: list[str] = Field(default_factory=list)
    env: dict[str, str] = Field(default_factory=dict)
    model_argument: str | None = Field(None, alias="modelArgument")
    reasoning_argument: str | None = Field(None, alias="reasoningArgument")
    # How the initial prompt is delivered: "positional" (claude/codex), or a flag such as "-i".
    prompt_argument: str | None = Field(None, alias="promptArgument")
    # Harness-agnostic reasoning level → harness specific value (e.g. {"high": "xhigh"}).
    reasoning_map: dict[str, str] = Field(default_factory=dict, alias="reasoningMap")
    # Model the harness itself falls back to when an agent's `model` is null;
    # shown to the user in place of the model flag AgentFlow never actually passes.
    default_model: str | None = Field(None, alias="defaultModel")
    # Curated list offered in the node configuration picker; kept here (not detected)
    # since none of the harness CLIs expose a way to query their own model catalog.
    available_models: list[str] = Field(default_factory=list, alias="availableModels")


class AgentConfig(_Model):
    harness: str
    model: str | None = None
    reasoning: ReasoningLevel | None = "high"


class AgentsConfig(_Model):
    architect: AgentConfig = AgentConfig(harness="claude")
    orchestrator: AgentConfig | None = None
    implementer: AgentConfig = AgentConfig(harness="codex")
    code_reviewer: AgentConfig = Field(AgentConfig(harness="claude"), alias="codeReviewer")
    security_reviewer: AgentConfig = Field(AgentConfig(harness="claude"), alias="securityReviewer")

    def for_role(self, role: str) -> AgentConfig:
        return {
            "architect": self.architect,
            "orchestrator": self.orchestrator or self.architect,
            "implementer": self.implementer,
            "code_reviewer": self.code_reviewer,
            "security_reviewer": self.security_reviewer,
        }[role]


# Copilot runs without approval prompts, except for the shell commands it is refused outright (deny
# rules always win over --allow-all-tools). Copilot matches the command name, or a git subcommand,
# never flags, so shell(rm) refuses every rm; the PowerShell names cover Windows, where Copilot runs
# PowerShell.
COPILOT_ARGS = ["--allow-all-tools", "--allow-all-paths", *(
    f"--deny-tool=shell({command})"
    for command in ("rm", "rmdir", "del", "erase", "rd", "Remove-Item", "ri", "git push", "git clean")
)]


def default_harnesses() -> dict[str, HarnessConfig]:
    return {
        "claude": HarnessConfig(
            type="claude", displayName="Claude Code", command="claude",
            defaultModel="claude-sonnet-5",
            availableModels=["claude-opus-5", "claude-sonnet-5", "claude-fable-5-1", "claude-haiku-4-5-20251001"],
        ),
        "codex": HarnessConfig(
            type="codex", displayName="Codex CLI", command="codex",
            defaultModel="gpt-5.6-luna",
            availableModels=["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"],
        ),
        "copilot": HarnessConfig(
            type="copilot", displayName="GitHub Copilot CLI", command="copilot",
            args=list(COPILOT_ARGS), defaultModel="gpt-5.6-luna",
            availableModels=["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra", "claude-opus-5", "claude-sonnet-5", "claude-haiku-4.5"],
        ),
    }


class Config(_Model):
    app: AppConfig = AppConfig()
    server: ServerConfig = ServerConfig()
    workspace: WorkspaceConfig = WorkspaceConfig()
    workflow: WorkflowConfig = WorkflowConfig()
    zellij: ZellijConfig = ZellijConfig()
    jira: JiraConfig = JiraConfig()
    slack: SlackConfig = SlackConfig()
    github: GitHubConfig = GitHubConfig()
    harnesses: dict[str, HarnessConfig] = Field(default_factory=default_harnesses)
    agents: AgentsConfig = AgentsConfig()

    @field_validator("harnesses")
    @classmethod
    def _fill_display_names(cls, value: dict[str, HarnessConfig]) -> dict[str, HarnessConfig]:
        for key, harness in value.items():
            if not harness.display_name:
                harness.display_name = key
        return value

    def validate_references(self) -> None:
        """Every agent must point at a configured harness."""
        for role in ("architect", "orchestrator", "implementer", "code_reviewer", "security_reviewer"):
            agent = self.agents.for_role(role)
            if agent.harness not in self.harnesses:
                raise ConfigError(
                    f"agents.{role}.harness = {agent.harness!r} is not a configured harness "
                    f"(known: {', '.join(sorted(self.harnesses))})"
                )

    # Resolved paths -----------------------------------------------------------------
    @property
    def data_dir(self) -> Path:
        return resolve_path(self.app.data_directory)

    @property
    def database_file(self) -> Path:
        return resolve_path(self.app.database_path)

    @property
    def worktree_dir(self) -> Path:
        return resolve_path(self.workspace.worktree_directory)

    @property
    def task_dir(self) -> Path:
        return resolve_path(self.workspace.task_directory)

    def to_json(self) -> str:
        return json.dumps(self.model_dump(by_alias=True), indent=2)


class ConfigError(Exception):
    """Raised when the configuration file is missing, unparsable or invalid."""


SKIP_JIRA_CHECK = "AGENTFLOW_SKIP_JIRA_CHECK"


def jira_startup_problem(config: Config) -> str | None:
    """Why the backend must not start or reload. Jira is optional: disabled, it never blocks local
    tasks; enabled, it must have its settings. Only checks that the settings are present, since
    startup should not wait on the network; `agentflow doctor` signs in for real. Kept out of
    load_config so code and tests that build a bare Config are unaffected."""
    if not config.jira.enabled:
        return None
    if os.environ.get(SKIP_JIRA_CHECK, "").lower() in ("1", "true", "yes"):
        return None
    missing = config.jira.missing()
    if not missing:
        return None
    return (f"Jira is enabled but not configured ({', '.join(missing)}). Run `agentflow setup`, "
            f"or for development pass --skip-jira-check (or set {SKIP_JIRA_CHECK}=1).")


def read_config(path: Path) -> Config:
    """Parse and validate an existing config file without touching the filesystem."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ConfigError(f"{path} is not valid JSON: {exc}") from exc
    try:
        config = Config.model_validate(raw)
    except ValidationError as exc:
        raise ConfigError(f"{path} is invalid:\n{exc}") from exc
    config.validate_references()
    return config


CHECKOUT_DATA_DIRECTORY = "data/.agentflow"


def fresh_config() -> Config:
    """The configuration a first launch or `agentflow setup` writes: the built-in defaults, except that
    a source checkout keeps its data inside the AgentFlow folder (relative, so it moves with it). A separate
    instance under AGENTFLOW_HOME must not share that data, so it keeps the defaults."""
    if source_checkout() is None or os.environ.get("AGENTFLOW_HOME"):
        return Config()
    data = CHECKOUT_DATA_DIRECTORY
    return Config.model_validate({
        "app": {"dataDirectory": data, "databasePath": f"{data}/agentflow.db"},
        "workspace": {"worktreeDirectory": f"{data}/worktrees", "taskDirectory": f"{data}/tasks"},
    })


def load_config(path: Path | None = None) -> Config:
    """Load and validate configuration, creating a default file on first launch."""
    path = path or config_path()
    if not path.exists():
        config = fresh_config()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(config.to_json() + "\n", encoding="utf-8")
        config.validate_references()
    else:
        config = read_config(path)
    for directory in (config.data_dir, config.worktree_dir, config.task_dir, config.database_file.parent):
        directory.mkdir(parents=True, exist_ok=True)
    return config
