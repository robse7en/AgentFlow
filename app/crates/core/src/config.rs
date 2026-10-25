//! Configuration loading and validation, ported from `backend/agentflow/config.py`.
//!
//! The file format is the Python backend's, unchanged: the same keys (camelCase, with the snake_case
//! field names accepted too), defaults, validation rules and the same text written on first launch.
//! `contract/golden/config` records what Python accepts, rejects and dumps; `tests/config.rs`
//! checks this module against it.
//!
//! Types are strict: Python's lax coercion (`"8765"` for a number, `"true"` for a bool) is not
//! reproduced. Files written by AgentFlow never rely on it.
//!
//! Relative data paths are relative to the AgentFlow folder (a source checkout) or the AgentFlow
//! home, never the current directory; see [`Locations`].

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

type Result<T> = std::result::Result<T, ConfigError>;

// ------------------------------------------------------------------------------------------
// Enumerations (Python `Literal`s)
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Host {
    #[serde(rename = "127.0.0.1")]
    Ipv4Loopback,
    #[serde(rename = "localhost")]
    Localhost,
    #[serde(rename = "::1")]
    Ipv6Loopback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Manual,
    Automatic,
    ApprovalRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reasoning {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImplementationMode {
    Direct,
    Parallel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingMode {
    Metered,
    Subscription,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Architect,
    Orchestrator,
    Implementer,
    CodeReviewer,
    SecurityReviewer,
}

impl Role {
    pub const ALL: [Role; 5] =
        [Role::Architect, Role::Orchestrator, Role::Implementer, Role::CodeReviewer, Role::SecurityReviewer];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Architect => "architect",
            Role::Orchestrator => "orchestrator",
            Role::Implementer => "implementer",
            Role::CodeReviewer => "code_reviewer",
            Role::SecurityReviewer => "security_reviewer",
        }
    }
}

/// Node states (a stage needs you) and task statuses (the ticket changed) that may trigger a DM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SlackNotifyOn {
    WaitingForUser,
    WaitingForApproval,
    Failed,
    Interrupted,
    Ready,
    NeedsHumanReview,
    ReadyForPr,
    Cancelled,
}

/// Task detail sections the operator may ask for in Slack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlackDetail {
    Requirements,
    Findings,
    Errors,
    Cost,
    Activity,
    Pr,
    Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SlackAction {
    Answer,
    Create,
    Run,
    RequestChanges,
    ApproveRequirements,
    Cancel,
    MarkPr,
}

/// A float setting whose Python default is an integer literal. Pydantic does not validate
/// defaults, so an untouched default is written as `1`, while a value read from a file is a float
/// and written as `1.0`. This keeps config.json byte-identical to what Python writes.
#[derive(Debug, Clone, Copy)]
pub struct Float {
    value: f64,
    integer_default: bool,
}

/// Equal by value, as Python compares `1 == 1.0`: how a value was spelled is not a change (a
/// reload must not see one in a setting that only gained its default written out).
impl PartialEq for Float {
    fn eq(&self, other: &Float) -> bool {
        self.value == other.value
    }
}

impl Float {
    const fn integer_default(value: i64) -> Float {
        Float { value: value as f64, integer_default: true }
    }

    pub fn new(value: f64) -> Float {
        Float { value, integer_default: false }
    }

    pub fn get(self) -> f64 {
        self.value
    }
}

impl Serialize for Float {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        if self.integer_default {
            serializer.serialize_i64(self.value as i64)
        } else {
            serializer.serialize_f64(self.value)
        }
    }
}

impl<'de> Deserialize<'de> for Float {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Float, D::Error> {
        f64::deserialize(deserializer).map(Float::new)
    }
}

// ------------------------------------------------------------------------------------------
// Sections. Field order is the Python declaration order: it is the order config.json is written in.
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AppConfig {
    #[serde(alias = "data_directory")]
    pub data_directory: String,
    #[serde(alias = "database_path")]
    pub database_path: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig { data_directory: "~/.agentflow".into(), database_path: "~/.agentflow/agentflow.db".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ServerConfig {
    pub host: Host,
    pub port: i64,
    /// Windows: start.sh restarts itself elevated, so worktree cleanup can close processes running
    /// as administrator. Everything the backend launches then runs as administrator too.
    #[serde(alias = "run_as_administrator")]
    pub run_as_administrator: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig { host: Host::Ipv4Loopback, port: 8765, run_as_administrator: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WorkspaceConfig {
    #[serde(alias = "worktree_directory")]
    pub worktree_directory: String,
    #[serde(alias = "task_directory")]
    pub task_directory: String,
    #[serde(alias = "branch_prefix")]
    pub branch_prefix: String,
    /// Prefilled (but always editable) in the New Task form's repository directory field.
    #[serde(alias = "default_repository_directory")]
    pub default_repository_directory: Option<String>,
    /// How each fresh task and worker worktree of a repository is prepared, keyed by the
    /// repository root (`~` allowed). Run by `engine::worktree_setup`.
    #[serde(alias = "worktree_setup")]
    pub worktree_setup: IndexMap<String, WorktreeSetup>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        WorkspaceConfig {
            worktree_directory: "~/.agentflow/worktrees".into(),
            task_directory: "~/.agentflow/tasks".into(),
            branch_prefix: String::new(),
            default_repository_directory: None,
            worktree_setup: IndexMap::new(),
        }
    }
}

impl WorkspaceConfig {
    /// The setup configured for a repository root, matched like `trustedRepositories`.
    pub fn setup_for(&self, repository_root: &Path) -> Option<&WorktreeSetup> {
        self.worktree_setup
            .iter()
            .find(|(root, _)| crate::paths::same_path(crate::process::expand_user(root), repository_root))
            .map(|(_, setup)| setup)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WorktreeSetup {
    /// Git-ignored files or directories copied from the repository checkout, relative to its root.
    pub copy: Vec<String>,
    /// Argv arrays run in order at the worktree root, without a shell.
    pub commands: Vec<Vec<String>>,
    /// The limit for each command.
    #[serde(alias = "timeout_seconds")]
    pub timeout_seconds: i64,
}

impl Default for WorktreeSetup {
    fn default() -> Self {
        WorktreeSetup { copy: Vec::new(), commands: Vec::new(), timeout_seconds: 900 }
    }
}

/// Why a setup copy path is unsafe, or None: it must be relative, stay inside the repository and
/// not touch Git's own files.
pub fn copy_path_problem(path: &str) -> Option<&'static str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || path.contains('\0') {
        return Some("Copy paths must be nonempty");
    }
    let parts: Vec<&str> = trimmed.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    if Path::new(trimmed).is_absolute() || trimmed.starts_with(['/', '\\']) || trimmed.contains(':') {
        return Some("Copy paths must be relative to the repository root");
    }
    if parts.is_empty() || parts.contains(&"..") {
        return Some("Copy paths must stay inside the repository");
    }
    if parts[0].eq_ignore_ascii_case(".git") {
        return Some("Copy paths must not touch .git");
    }
    None
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct CostControls {
    #[serde(alias = "task_budget_usd")]
    pub task_budget_usd: Option<f64>,
    #[serde(alias = "daily_budget_usd")]
    pub daily_budget_usd: Option<f64>,
    #[serde(alias = "warning_percent")]
    pub warning_percent: i64,
    #[serde(alias = "launch_reserve_usd")]
    pub launch_reserve_usd: Float,
    #[serde(alias = "copilot_roles")]
    pub copilot_roles: Vec<Role>,
    #[serde(alias = "require_metered_approval")]
    pub require_metered_approval: bool,
}

impl Default for CostControls {
    fn default() -> Self {
        CostControls {
            task_budget_usd: None,
            daily_budget_usd: None,
            warning_percent: 80,
            launch_reserve_usd: Float::integer_default(1),
            copilot_roles: Vec::new(),
            require_metered_approval: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WorkflowConfig {
    /// Observational warning only; never authorizes retry or slot release.
    #[serde(alias = "heartbeat_warning_seconds")]
    pub heartbeat_warning_seconds: i64,
    #[serde(alias = "cost_controls")]
    pub cost_controls: CostControls,
    #[serde(alias = "implementation_mode")]
    pub implementation_mode: ImplementationMode,
    #[serde(alias = "max_concurrent_agents")]
    pub max_concurrent_agents: i64,
    /// Fixed display-only conversion for showing reported USD costs in NZD; bumped by hand.
    #[serde(alias = "usd_to_nzd_rate")]
    pub usd_to_nzd_rate: f64,
    #[serde(alias = "verification_commands")]
    pub verification_commands: Vec<Vec<String>>,
    #[serde(alias = "max_review_iterations")]
    pub max_review_iterations: i64,
    #[serde(alias = "max_requirements_revisions")]
    pub max_requirements_revisions: i64,
    #[serde(alias = "default_execution_mode")]
    pub default_execution_mode: ExecutionMode,
    #[serde(alias = "require_requirements_approval")]
    pub require_requirements_approval: bool,
    #[serde(alias = "require_final_approval")]
    pub require_final_approval: bool,
    #[serde(alias = "commit_checkpoints")]
    pub commit_checkpoints: bool,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        WorkflowConfig {
            heartbeat_warning_seconds: 600,
            cost_controls: CostControls::default(),
            implementation_mode: ImplementationMode::Direct,
            max_concurrent_agents: 4,
            usd_to_nzd_rate: 1.65,
            verification_commands: Vec::new(),
            max_review_iterations: 5,
            max_requirements_revisions: 3,
            default_execution_mode: ExecutionMode::Manual,
            require_requirements_approval: true,
            require_final_approval: true,
            commit_checkpoints: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ZellijConfig {
    pub command: String,
    #[serde(alias = "session_prefix")]
    pub session_prefix: String,
    #[serde(alias = "web_client_enabled")]
    pub web_client_enabled: bool,
    #[serde(alias = "web_client_base_url")]
    pub web_client_base_url: String,
    /// Shell used inside agent tabs to run the launcher script; auto-detected when null.
    pub shell: Option<String>,
    /// Windows only: terminal size of the hidden client that keeps each session sized.
    #[serde(alias = "headless_cols")]
    pub headless_cols: i64,
    #[serde(alias = "headless_rows")]
    pub headless_rows: i64,
    /// Desktop app only: run agents in zellij sessions instead of built-in terminals.
    #[serde(alias = "use_in_desktop")]
    pub use_in_desktop: bool,
}

impl Default for ZellijConfig {
    fn default() -> Self {
        ZellijConfig {
            command: "zellij".into(),
            session_prefix: "agentflow-".into(),
            web_client_enabled: false,
            web_client_base_url: "http://127.0.0.1:8082".into(),
            shell: None,
            headless_cols: 200,
            headless_rows: 50,
            use_in_desktop: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct JiraConfig {
    #[serde(alias = "attachment_hosts")]
    pub attachment_hosts: Vec<String>,
    #[serde(alias = "max_attachment_bytes")]
    pub max_attachment_bytes: i64,
    #[serde(alias = "max_total_attachment_bytes")]
    pub max_total_attachment_bytes: i64,
    #[serde(alias = "max_attachments")]
    pub max_attachments: i64,
    /// Comments (with their embedded screenshots marked inline) go into context/jira-issue.md.
    #[serde(alias = "fetch_comments")]
    pub fetch_comments: bool,
    /// Keyframes pulled from video attachments with ffmpeg; 0 = off.
    #[serde(alias = "max_frames_per_video")]
    pub max_frames_per_video: i64,
    #[serde(alias = "frame_extraction_timeout_seconds")]
    pub frame_extraction_timeout_seconds: Float,
    /// Null = `ffmpeg`/`ffprobe` on PATH; otherwise the ffmpeg binary, with ffprobe beside it.
    #[serde(alias = "ffmpeg_path")]
    pub ffmpeg_path: Option<String>,
    pub enabled: bool,
    #[serde(alias = "base_url")]
    pub base_url: Option<String>,
    /// Jira Cloud: email + API token (Basic auth). Server/Data Center PAT: no email, PAT in api_token.
    pub email: Option<String>,
    #[serde(alias = "api_token")]
    pub api_token: Option<String>,
    /// Unset = off. When set, AgentFlow moves the ticket to this status when the Architect runs.
    #[serde(alias = "auto_transition_status")]
    pub auto_transition_status: Option<String>,
    #[serde(alias = "auto_transition_from_statuses")]
    pub auto_transition_from_statuses: Vec<String>,
}

impl Default for JiraConfig {
    fn default() -> Self {
        JiraConfig {
            attachment_hosts: vec!["api.media.atlassian.com".into()],
            max_attachment_bytes: 20_000_000,
            max_total_attachment_bytes: 100_000_000,
            max_attachments: 30,
            fetch_comments: true,
            max_frames_per_video: 20,
            frame_extraction_timeout_seconds: Float::integer_default(120),
            ffmpeg_path: None,
            enabled: false,
            base_url: None,
            email: None,
            api_token: None,
            auto_transition_status: None,
            auto_transition_from_statuses: vec!["To Do".into()],
        }
    }
}

impl JiraConfig {
    /// Settings the integration needs that are unset (Server/Data Center has no email).
    pub fn missing(&self) -> Vec<&'static str> {
        let set = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
        [("jira.enabled", self.enabled), ("jira.baseUrl", set(&self.base_url)), ("jira.apiToken", set(&self.api_token))]
            .into_iter()
            .filter(|(_, present)| !present)
            .map(|(name, _)| name)
            .collect()
    }
}

/// Free-text questions answered by the operator's own harness CLI, run headless with every tool off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct SlackAskConfig {
    pub enabled: bool,
    /// A configured harness of type "claude" or "copilot"; its command and env are used, never its args.
    pub harness: String,
    pub model: Option<String>,
    #[serde(alias = "timeout_seconds")]
    pub timeout_seconds: Float,
    /// A harness whose billingMode is not "subscription" answers only when this is set.
    #[serde(alias = "allow_metered")]
    pub allow_metered: bool,
}

impl Default for SlackAskConfig {
    fn default() -> Self {
        SlackAskConfig {
            enabled: false,
            harness: "claude".into(),
            model: None,
            timeout_seconds: Float::integer_default(90),
            allow_metered: false,
        }
    }
}

/// Opt-in Slack DMs over Socket Mode (an outbound connection; nothing listens for Slack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct SlackConfig {
    pub enabled: bool,
    /// Prefer AGENTFLOW_SLACK_BOT_TOKEN / AGENTFLOW_SLACK_APP_TOKEN; these fields are the fallback.
    #[serde(alias = "bot_token")]
    pub bot_token: Option<String>,
    #[serde(alias = "app_token")]
    pub app_token: Option<String>,
    /// The one operator's Slack member ID (U…). Only this user is DMed and may use the slash command.
    #[serde(alias = "user_id")]
    pub user_id: Option<String>,
    /// This app's slash command; each AgentFlow in a shared workspace needs its own. None = no
    /// slash command, so nothing shows in anyone's / menu; commands are typed in the DM instead.
    #[serde(default = "default_slack_command")]
    pub command: Option<String>,
    #[serde(alias = "include_question_text")]
    pub include_question_text: bool,
    pub details: Vec<SlackDetail>,
    /// "HH:MM" local time to post the daily digest when any task is active. None = off.
    #[serde(alias = "digest_at")]
    pub digest_at: Option<String>,
    pub ask: SlackAskConfig,
    pub notify: Vec<SlackNotifyOn>,
    #[serde(alias = "ui_base_url")]
    pub ui_base_url: String,
    #[serde(alias = "allow_actions")]
    pub allow_actions: Vec<SlackAction>,
}

impl Default for SlackConfig {
    fn default() -> Self {
        use SlackNotifyOn::*;
        SlackConfig {
            enabled: false,
            bot_token: None,
            app_token: None,
            user_id: None,
            command: default_slack_command(),
            include_question_text: true,
            details: Vec::new(),
            digest_at: None,
            ask: SlackAskConfig::default(),
            notify: vec![WaitingForUser, WaitingForApproval, Failed, Interrupted, NeedsHumanReview, ReadyForPr],
            ui_base_url: "http://127.0.0.1:5173".into(),
            allow_actions: vec![SlackAction::Answer],
        }
    }
}

fn default_slack_command() -> Option<String> {
    Some("/agentflow".into())
}

impl SlackConfig {
    pub fn allows(&self, action: SlackAction) -> bool {
        self.allow_actions.contains(&action)
    }

    /// Answering needs the question text, since the operator must see the question.
    pub fn answering(&self) -> bool {
        self.allows(SlackAction::Answer) && self.include_question_text
    }

    pub fn resolved_bot_token(&self) -> Option<String> {
        env_value("AGENTFLOW_SLACK_BOT_TOKEN").or_else(|| self.bot_token.clone())
    }

    pub fn resolved_app_token(&self) -> Option<String> {
        env_value("AGENTFLOW_SLACK_APP_TOKEN").or_else(|| self.app_token.clone())
    }

    /// Why an enabled integration cannot start, in terms the operator can act on.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if !self.resolved_bot_token().unwrap_or_default().starts_with("xoxb-") {
            problems.push("set AGENTFLOW_SLACK_BOT_TOKEN (or slack.botToken) to the xoxb- bot token");
        }
        if !self.resolved_app_token().unwrap_or_default().starts_with("xapp-") {
            problems.push("set AGENTFLOW_SLACK_APP_TOKEN (or slack.appToken) to the xapp- app-level token");
        }
        if self.user_id.as_deref().unwrap_or_default().is_empty() {
            problems.push("set slack.userId to your Slack member ID (U…)");
        }
        problems
    }
}

/// Read-only PR tracking through the `gh` CLI, polled since GitHub cannot reach a loopback server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct GitHubConfig {
    #[serde(alias = "pr_sync")]
    pub pr_sync: bool,
    #[serde(alias = "poll_interval_seconds")]
    pub poll_interval_seconds: i64,
    pub command: String,
    /// Only remotes and PR URLs on these hosts are looked up.
    pub hosts: Vec<String>,
}

impl Default for GitHubConfig {
    fn default() -> Self {
        GitHubConfig {
            pr_sync: true,
            poll_interval_seconds: 120,
            command: "gh".into(),
            hosts: vec!["github.com".into()],
        }
    }
}

fn yes() -> bool {
    true
}

fn generic_cli() -> String {
    "generic-cli".into()
}

fn unknown_billing() -> BillingMode {
    BillingMode::Unknown
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessConfig {
    #[serde(alias = "billing_mode", default = "unknown_billing")]
    pub billing_mode: BillingMode,
    #[serde(alias = "trusted_repositories", default)]
    pub trusted_repositories: Vec<String>,
    #[serde(alias = "usage_command", default)]
    pub usage_command: Option<Vec<String>>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(rename = "type", default = "generic_cli")]
    pub kind: String,
    /// Filled with the harness key when empty.
    #[serde(alias = "display_name", default)]
    pub display_name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(alias = "model_argument", default)]
    pub model_argument: Option<String>,
    #[serde(alias = "reasoning_argument", default)]
    pub reasoning_argument: Option<String>,
    /// How the initial prompt is delivered: positional (claude/codex), or a flag such as "-i".
    #[serde(alias = "prompt_argument", default)]
    pub prompt_argument: Option<String>,
    /// Harness-agnostic reasoning level → harness specific value (e.g. {"high": "xhigh"}).
    #[serde(alias = "reasoning_map", default)]
    pub reasoning_map: IndexMap<String, String>,
    /// Model the harness falls back to when an agent's model is null; shown in place of the flag.
    #[serde(alias = "default_model", default)]
    pub default_model: Option<String>,
    /// Curated list offered in the node configuration picker.
    #[serde(alias = "available_models", default)]
    pub available_models: Vec<String>,
}

impl HarnessConfig {
    fn new(kind: &str, display_name: &str, command: &str, default_model: &str, models: &[&str]) -> Self {
        HarnessConfig {
            billing_mode: BillingMode::Unknown,
            trusted_repositories: Vec::new(),
            usage_command: None,
            enabled: true,
            kind: kind.into(),
            display_name: display_name.into(),
            command: command.into(),
            args: Vec::new(),
            env: IndexMap::new(),
            model_argument: None,
            reasoning_argument: None,
            prompt_argument: None,
            reasoning_map: IndexMap::new(),
            default_model: Some(default_model.into()),
            available_models: models.iter().map(|m| m.to_string()).collect(),
        }
    }
}

/// Copilot runs without approval prompts, except for the shell commands it is refused outright.
/// Copilot matches the command name (or a git subcommand), never flags; the PowerShell names cover
/// Windows, where Copilot runs PowerShell.
pub fn copilot_args() -> Vec<String> {
    let denied = ["rm", "rmdir", "del", "erase", "rd", "Remove-Item", "ri", "git push", "git clean"];
    ["--allow-all-tools".to_string(), "--allow-all-paths".to_string()]
        .into_iter()
        .chain(denied.iter().map(|command| format!("--deny-tool=shell({command})")))
        .collect()
}

pub fn default_harnesses() -> IndexMap<String, HarnessConfig> {
    let mut copilot = HarnessConfig::new(
        "copilot",
        "GitHub Copilot CLI",
        "copilot",
        "gpt-5.6-luna",
        &["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra", "claude-opus-5", "claude-sonnet-5", "claude-haiku-4.5"],
    );
    copilot.args = copilot_args();
    IndexMap::from([
        (
            "claude".to_string(),
            HarnessConfig::new(
                "claude",
                "Claude Code",
                "claude",
                "claude-sonnet-5",
                &["claude-opus-5", "claude-sonnet-5", "claude-fable-5-1", "claude-haiku-4-5-20251001"],
            ),
        ),
        (
            "codex".to_string(),
            HarnessConfig::new(
                "codex",
                "Codex CLI",
                "codex",
                "gpt-5.6-luna",
                &["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"],
            ),
        ),
        ("copilot".to_string(), copilot),
    ])
}

fn high() -> Option<Reasoning> {
    Some(Reasoning::High)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub harness: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Defaults to "high"; an explicit null means the harness's own default.
    #[serde(default = "high")]
    pub reasoning: Option<Reasoning>,
}

impl AgentConfig {
    fn on(harness: &str) -> Self {
        AgentConfig { harness: harness.into(), model: None, reasoning: high() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AgentsConfig {
    pub architect: AgentConfig,
    /// Falls back to the architect's configuration when unset.
    pub orchestrator: Option<AgentConfig>,
    pub implementer: AgentConfig,
    #[serde(alias = "code_reviewer")]
    pub code_reviewer: AgentConfig,
    #[serde(alias = "security_reviewer")]
    pub security_reviewer: AgentConfig,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig {
            architect: AgentConfig::on("claude"),
            orchestrator: None,
            implementer: AgentConfig::on("codex"),
            code_reviewer: AgentConfig::on("claude"),
            security_reviewer: AgentConfig::on("claude"),
        }
    }
}

impl AgentsConfig {
    pub fn for_role(&self, role: Role) -> &AgentConfig {
        match role {
            Role::Architect => &self.architect,
            Role::Orchestrator => self.orchestrator.as_ref().unwrap_or(&self.architect),
            Role::Implementer => &self.implementer,
            Role::CodeReviewer => &self.code_reviewer,
            Role::SecurityReviewer => &self.security_reviewer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub app: AppConfig,
    pub server: ServerConfig,
    pub workspace: WorkspaceConfig,
    pub workflow: WorkflowConfig,
    pub zellij: ZellijConfig,
    pub jira: JiraConfig,
    pub slack: SlackConfig,
    pub github: GitHubConfig,
    pub harnesses: IndexMap<String, HarnessConfig>,
    pub agents: AgentsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            app: AppConfig::default(),
            server: ServerConfig::default(),
            workspace: WorkspaceConfig::default(),
            workflow: WorkflowConfig::default(),
            zellij: ZellijConfig::default(),
            jira: JiraConfig::default(),
            slack: SlackConfig::default(),
            github: GitHubConfig::default(),
            harnesses: default_harnesses(),
            agents: AgentsConfig::default(),
        }
    }
}

impl Config {
    /// Parse and validate a configuration document (the contents of config.json).
    pub fn from_value(raw: Value) -> std::result::Result<Config, Vec<String>> {
        let mut problems = Vec::new();
        require_objects(&raw, &mut problems);
        if !problems.is_empty() {
            return Err(problems);
        }
        // serde_path_to_error names the field, as Python's "slack.command: Input should be ..." does.
        let mut config: Config = serde_path_to_error::deserialize(raw).map_err(|e| {
            let path = e.path().to_string();
            vec![if path == "." { e.inner().to_string() } else { format!("{path}: {}", e.inner()) }]
        })?;
        config.check(&mut problems);
        if !problems.is_empty() {
            return Err(problems);
        }
        for (key, harness) in config.harnesses.iter_mut() {
            if harness.display_name.is_empty() {
                harness.display_name = key.clone();
            }
        }
        Ok(config)
    }

    /// Every agent must point at a configured harness.
    pub fn validate_references(&self) -> Result<()> {
        for role in Role::ALL {
            let agent = self.agents.for_role(role);
            if !self.harnesses.contains_key(&agent.harness) {
                let mut known: Vec<&str> = self.harnesses.keys().map(String::as_str).collect();
                known.sort_unstable();
                return Err(ConfigError(format!(
                    "agents.{}.harness = '{}' is not a configured harness (known: {})",
                    role.as_str(),
                    agent.harness,
                    known.join(", ")
                )));
            }
        }
        Ok(())
    }

    /// The text written to config.json: Python's `json.dumps(model_dump(by_alias=True), indent=2)`.
    pub fn to_json(&self) -> String {
        pyjson::dumps(&serde_json::to_value(self).expect("Config always serializes"))
    }

    pub fn data_dir(&self, at: &Locations) -> PathBuf {
        at.resolve(&self.app.data_directory)
    }

    pub fn database_file(&self, at: &Locations) -> PathBuf {
        at.resolve(&self.app.database_path)
    }

    pub fn worktree_dir(&self, at: &Locations) -> PathBuf {
        at.resolve(&self.workspace.worktree_directory)
    }

    pub fn task_dir(&self, at: &Locations) -> PathBuf {
        at.resolve(&self.workspace.task_directory)
    }

    /// The value constraints serde's types cannot express (Python's Field(ge=, le=, pattern=) and
    /// validators), as "path: problem" lines.
    fn check(&self, problems: &mut Vec<String>) {
        let mut range = |path: &str, value: i64, min: i64, max: Option<i64>| {
            if value < min || max.is_some_and(|max| value > max) {
                let bound = max.map_or(format!("at least {min}"), |max| format!("between {min} and {max}"));
                problems.push(format!("{path}: must be {bound}, got {value}"));
            }
        };
        let w = &self.workflow;
        range("workflow.heartbeatWarningSeconds", w.heartbeat_warning_seconds, 300, Some(86400));
        range("workflow.maxConcurrentAgents", w.max_concurrent_agents, 1, Some(32));
        range("workflow.maxReviewIterations", w.max_review_iterations, 1, Some(50));
        range("workflow.maxRequirementsRevisions", w.max_requirements_revisions, 1, Some(50));
        range("workflow.costControls.warningPercent", w.cost_controls.warning_percent, 1, Some(100));
        range("zellij.headlessCols", self.zellij.headless_cols, 60, Some(1000));
        range("zellij.headlessRows", self.zellij.headless_rows, 10, Some(500));
        range("jira.maxAttachmentBytes", self.jira.max_attachment_bytes, 1, None);
        range("jira.maxTotalAttachmentBytes", self.jira.max_total_attachment_bytes, 1, None);
        range("jira.maxAttachments", self.jira.max_attachments, 0, Some(1000));
        range("jira.maxFramesPerVideo", self.jira.max_frames_per_video, 0, Some(200));
        range("github.pollIntervalSeconds", self.github.poll_interval_seconds, 30, Some(3600));

        let mut positive = |path: &str, value: Option<f64>, max: Option<f64>| {
            if let Some(value) = value
                && (!value.is_finite() || value <= 0.0 || max.is_some_and(|max| value > max))
            {
                let bound = max.map_or("greater than 0".to_string(), |max| format!("greater than 0 and at most {max}"));
                problems.push(format!("{path}: must be {bound}, got {value}"));
            }
        };
        positive("workflow.costControls.taskBudgetUsd", w.cost_controls.task_budget_usd, None);
        positive("workflow.costControls.dailyBudgetUsd", w.cost_controls.daily_budget_usd, None);
        positive("workflow.costControls.launchReserveUsd", Some(w.cost_controls.launch_reserve_usd.get()), None);
        positive("workflow.usdToNzdRate", Some(w.usd_to_nzd_rate), None);
        positive("jira.frameExtractionTimeoutSeconds", Some(self.jira.frame_extraction_timeout_seconds.get()), None);
        positive("slack.ask.timeoutSeconds", Some(self.slack.ask.timeout_seconds.get()), Some(600.0));

        if w.verification_commands
            .iter()
            .any(|argv| argv.is_empty() || argv.iter().any(|part| part.trim().is_empty() || part.contains('\0')))
        {
            problems
                .push("workflow.verificationCommands: Verification commands must contain nonempty argv arrays".into());
        }
        for (root, setup) in &self.workspace.worktree_setup {
            let at = format!("workspace.worktreeSetup.{root}");
            if root.trim().is_empty() {
                problems.push(format!("{at}: Repository roots must be nonempty"));
            }
            if let Some(problem) = setup.copy.iter().find_map(|path| copy_path_problem(path)) {
                problems.push(format!("{at}.copy: {problem}"));
            }
            if setup
                .commands
                .iter()
                .any(|argv| argv.is_empty() || argv.iter().any(|part| part.trim().is_empty() || part.contains('\0')))
            {
                problems.push(format!("{at}.commands: Setup commands must contain nonempty argv arrays"));
            }
            if !(1..=86400).contains(&setup.timeout_seconds) {
                problems
                    .push(format!("{at}.timeoutSeconds: must be between 1 and 86400, got {}", setup.timeout_seconds));
            }
        }
        for (key, harness) in &self.harnesses {
            if harness.env.keys().any(|name| !is_identifier(name)) {
                problems.push(format!(
                    "harnesses.{key}.env: Harness environment variable names must be shell-safe identifiers"
                ));
            }
        }

        let slack = &self.slack;
        if let Some(user) = &slack.user_id
            && !(user.len() > 1
                && user.starts_with(['U', 'W'])
                && user[1..].chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
        {
            problems.push(format!("slack.userId: must match ^[UW][A-Z0-9]+$, got {user:?}"));
        }
        // Slack's rule for slash command names: lowercase, no spaces, at most 32 characters.
        if let Some(command) = &slack.command {
            let name = command.strip_prefix('/').unwrap_or_default();
            if !(command.starts_with('/')
                && (1..=31).contains(&name.len())
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
            {
                problems.push(format!("slack.command: must match ^/[a-z0-9_-]{{1,31}}$, got {command:?}"));
            }
        }
        if let Some(at) = &slack.digest_at
            && !is_clock_time(at)
        {
            problems.push(format!("slack.digestAt: must be HH:MM (24-hour), got {at:?}"));
        }
        if !(slack.ui_base_url.starts_with("http://") || slack.ui_base_url.starts_with("https://")) {
            problems.push(format!("slack.uiBaseUrl: must start with http:// or https://, got {:?}", slack.ui_base_url));
        }
    }
}

/// Python rejects a non-object where a section is expected; serde would read an array into a
/// struct positionally (and an empty one as all defaults), so these are checked first.
fn require_objects(raw: &Value, problems: &mut Vec<String>) {
    fn object<'a>(
        value: &'a Value,
        path: &str,
        problems: &mut Vec<String>,
    ) -> Option<&'a serde_json::Map<String, Value>> {
        match value {
            Value::Object(map) => Some(map),
            other => {
                problems.push(format!("{path}: expected an object, got {other}"));
                None
            }
        }
    }
    let Some(top) = object(raw, "config", problems) else { return };
    for (section, nested) in [
        ("app", &[][..]),
        ("server", &[]),
        ("workspace", &[]),
        ("workflow", &["costControls", "cost_controls"]),
        ("zellij", &[]),
        ("jira", &[]),
        ("slack", &["ask"]),
        ("github", &[]),
    ] {
        if let Some(section_value) = top.get(section)
            && let Some(map) = object(section_value, section, problems)
        {
            for name in nested.iter() {
                if let Some(value) = map.get(*name) {
                    object(value, &format!("{section}.{name}"), problems);
                }
            }
        }
    }
    if let Some(harnesses) = top.get("harnesses").and_then(|h| object(h, "harnesses", problems)) {
        for (key, harness) in harnesses {
            object(harness, &format!("harnesses.{key}"), problems);
        }
    }
    if let Some(agents) = top.get("agents").and_then(|a| object(a, "agents", problems)) {
        for (role, agent) in agents {
            if !(role == "orchestrator" && agent.is_null()) {
                object(agent, &format!("agents.{role}"), problems);
            }
        }
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_clock_time(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b.iter().enumerate().all(|(i, c)| i == 2 || c.is_ascii_digit())
        && matches!((b[0], b[1]), (b'0' | b'1', _) | (b'2', b'0'..=b'3'))
        && b[3] <= b'5'
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

// ------------------------------------------------------------------------------------------
// Where things live
// ------------------------------------------------------------------------------------------

/// Directories configuration paths are resolved against. Explicit, so tests never touch the real
/// ~/.agentflow or a checkout's data/.agentflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    /// $AGENTFLOW_HOME, or ~/.agentflow.
    pub home: PathBuf,
    /// Whether $AGENTFLOW_HOME was set: a separate instance that must not share a checkout's data.
    pub home_overridden: bool,
    /// The AgentFlow folder when running from a source checkout.
    pub checkout: Option<PathBuf>,
    /// $AGENTFLOW_CONFIG.
    pub config_override: Option<PathBuf>,
}

impl Locations {
    /// From the environment, with the checkout found above the running executable.
    pub fn from_env() -> Self {
        let checkout = std::env::current_exe().ok().and_then(|exe| find_checkout(&exe));
        Self::from_env_with_checkout(checkout)
    }

    pub fn from_env_with_checkout(checkout: Option<PathBuf>) -> Self {
        let home_override = env_value("AGENTFLOW_HOME");
        Locations {
            home: home_override.as_deref().map(expand_user).unwrap_or_else(|| user_home().join(".agentflow")),
            home_overridden: home_override.is_some(),
            checkout,
            config_override: env_value("AGENTFLOW_CONFIG").as_deref().map(expand_user),
        }
    }

    /// config.json: $AGENTFLOW_CONFIG; else $AGENTFLOW_HOME/config.json; else a checkout keeps its
    /// config with the project, except that an existing ~/.agentflow/config.json from before that
    /// default is used until the project has one.
    pub fn config_path(&self) -> PathBuf {
        if let Some(path) = &self.config_override {
            return path.clone();
        }
        let home_config = self.home.join("config.json");
        if self.home_overridden {
            return home_config;
        }
        if let Some(checkout) = &self.checkout
            && (checkout.join("config.json").is_file() || !home_config.is_file())
        {
            return checkout.join("config.json");
        }
        home_config
    }

    /// A configured path: `~` expands, and a relative path is relative to the AgentFlow folder
    /// (the home for installed copies), not the current directory.
    pub fn resolve(&self, value: &str) -> PathBuf {
        let path = expand_user(value);
        if path.is_absolute() { path } else { self.checkout.as_ref().unwrap_or(&self.home).join(path) }
    }
}

/// The AgentFlow folder above `start`: the directory holding both `app/Cargo.toml` and
/// `frontend/package.json` (the Python backend looked for backend/pyproject.toml in the same folder).
pub fn find_checkout(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join("app").join("Cargo.toml").is_file() && dir.join("frontend").join("package.json").is_file())
        .map(Path::to_path_buf)
}

fn user_home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn expand_user(value: &str) -> PathBuf {
    match value.strip_prefix('~') {
        Some("") => user_home(),
        Some(rest) if rest.starts_with(['/', '\\']) => user_home().join(&rest[1..]),
        _ => PathBuf::from(value),
    }
}

// ------------------------------------------------------------------------------------------
// Loading
// ------------------------------------------------------------------------------------------

pub const SKIP_JIRA_CHECK: &str = "AGENTFLOW_SKIP_JIRA_CHECK";
pub const CHECKOUT_DATA_DIRECTORY: &str = "data/.agentflow";

/// Disabled Jira does not block local tasks. Enabled integrations must have credentials.
pub fn jira_startup_problem(config: &Config) -> Option<String> {
    if !config.jira.enabled {
        return None;
    }
    if env_value(SKIP_JIRA_CHECK).is_some_and(|v| ["1", "true", "yes"].contains(&v.to_lowercase().as_str())) {
        return None;
    }
    let missing = config.jira.missing();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "Jira is enabled but not configured ({}). Run `agentflow setup`, or for development pass \
         --skip-jira-check (or set {SKIP_JIRA_CHECK}=1).",
        missing.join(", ")
    ))
}

/// Parse and validate an existing config file without touching the filesystem otherwise.
pub fn read_config(path: &Path) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).map_err(|e| ConfigError(format!("Cannot read {}: {e}", path.display())))?;
    let raw: Value =
        serde_json::from_str(&text).map_err(|e| ConfigError(format!("{} is not valid JSON: {e}", path.display())))?;
    let config = Config::from_value(raw)
        .map_err(|problems| ConfigError(format!("{} is invalid:\n{}", path.display(), problems.join("\n"))))?;
    config.validate_references()?;
    Ok(config)
}

/// The configuration a first launch writes: the defaults, except that a source checkout keeps its
/// data inside the AgentFlow folder (relative, so it moves with it). A separate instance under
/// $AGENTFLOW_HOME keeps the defaults so it does not share that data.
pub fn fresh_config(at: &Locations) -> Config {
    let mut config = Config::default();
    if at.checkout.is_some() && !at.home_overridden {
        let data = CHECKOUT_DATA_DIRECTORY;
        config.app.data_directory = data.into();
        config.app.database_path = format!("{data}/agentflow.db");
        config.workspace.worktree_directory = format!("{data}/worktrees");
        config.workspace.task_directory = format!("{data}/tasks");
    }
    config
}

/// Load and validate configuration, writing a default file on first launch and creating the data
/// directories it names.
pub fn load_config(path: &Path, at: &Locations) -> Result<Config> {
    let config = if path.exists() {
        read_config(path)?
    } else {
        let config = fresh_config(at);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError(format!("Cannot create {}: {e}", parent.display())))?;
        }
        std::fs::write(path, config.to_json() + "\n")
            .map_err(|e| ConfigError(format!("Cannot write {}: {e}", path.display())))?;
        config.validate_references()?;
        config
    };
    let database_parent = config.database_file(at).parent().map(Path::to_path_buf);
    for dir in [config.data_dir(at), config.worktree_dir(at), config.task_dir(at)].into_iter().chain(database_parent) {
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError(format!("Cannot create {}: {e}", dir.display())))?;
    }
    Ok(config)
}
