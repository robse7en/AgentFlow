//! Harness adapters, ported from `backend/agentflow/harnesses/`.
//!
//! Workflow logic never touches CLI syntax: it asks an adapter to build a launch and to deliver
//! prompts. The Python backend has one subclass per CLI; they differ only in a few overrides, so
//! here one [`HarnessAdapter`] carries a [`HarnessKind`] and matches on it where a subclass would
//! have overridden a method. A configured `type` this port doesn't know runs as a generic CLI, as
//! in Python.
//!
//! Checked against Python by `tests/harness.rs`: launch and resume argv for every type and flag
//! combination, screen classification, prompt references and `trust_workspace` writes, all recorded
//! by `contract/record_python_goldens.py`.

pub mod claude;
pub mod codex;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use regex::RegexSet;

use crate::config::{Config, HarnessConfig};
use crate::process::{self, resolve_executable};

/// How many trailing non-blank rows of a pane's rendered viewport [`HarnessAdapter::classify_screen`]
/// looks at. A permission dialog or spinner line is always near the bottom; the scrollback above it
/// (past conversation) is noise for this purpose.
const SCREEN_TAIL_ROWS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessKind {
    Claude,
    Codex,
    Copilot,
    Generic,
}

impl HarnessKind {
    /// The adapter for a configured `type`; unknown types run as a generic CLI.
    pub fn from_type(kind: &str) -> Self {
        match kind {
            "claude" => HarnessKind::Claude,
            "codex" => HarnessKind::Codex,
            "copilot" => HarnessKind::Copilot,
            _ => HarnessKind::Generic,
        }
    }

    /// Python's `type_name` class attribute.
    pub fn type_name(self) -> &'static str {
        match self {
            HarnessKind::Claude => "claude",
            HarnessKind::Codex => "codex",
            HarnessKind::Copilot => "copilot",
            HarnessKind::Generic => "generic-cli",
        }
    }

    pub fn supports_resume(self) -> bool {
        self != HarnessKind::Generic
    }

    /// Typed into an idle agent before its tab is closed, for CLIs that only write their final
    /// usage when they exit normally. None: the tab is just closed.
    pub fn exit_command(self) -> Option<&'static str> {
        // Copilot writes a session's final credits at shutdown (scripts/usage_copilot.py).
        (self == HarnessKind::Copilot).then_some("/exit")
    }
}

/// What a harness's CLI is doing right now, read off its rendered screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl ScreenState {
    /// The value stored in `agent_sessions.screen_state` and sent to the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            ScreenState::Idle => "idle",
            ScreenState::Working => "working",
            ScreenState::Blocked => "blocked",
            ScreenState::Unknown => "unknown",
        }
    }
}

/// Case-insensitive, multi-line regexes matched against the tail of a pane's rendered screen,
/// checked in priority order blocked > working > idle, so a permission prompt rendered under a
/// still-active spinner is reported as blocked, not working. AgentFlow's equivalent of herdr's
/// screen manifests.
struct ScreenPatterns {
    blocked: RegexSet,
    working: RegexSet,
    idle: RegexSet,
}

impl ScreenPatterns {
    fn new(blocked: &[&str], working: &[&str], idle: &[&str]) -> Self {
        let set = |patterns: &[&str]| {
            RegexSet::new(patterns.iter().map(|p| format!("(?im){p}"))).expect("screen patterns are valid")
        };
        ScreenPatterns { blocked: set(blocked), working: set(working), idle: set(idle) }
    }
}

/// Claude Code's own permission/trust dialogs and its "esc to interrupt" spinner while it's
/// working. `prepare_workspace` pre-clears the trust dialog for trusted repos, but a first-run
/// prompt in an untrusted one, or a Bash/edit permission prompt mid-run, still needs this fallback
/// to be seen at all. The blocked patterns were tuned against a real captured trust dialog
/// ("Quick safety check: ... ❯ No, exit / Yes, I trust this folder / Enter to confirm"); "❯" is
/// Claude Code's cursor for a selectable option in any of its dialogs.
static CLAUDE_SCREEN: LazyLock<ScreenPatterns> = LazyLock::new(|| {
    ScreenPatterns::new(
        &[r"do you want to proceed", r"quick safety check", r"trust this folder", r"enter to confirm", r"❯"],
        &[r"esc to interrupt", r"\bthinking\b", r"\bgenerating\b"],
        &[r"\?\s*for shortcuts", r"^\s*>\s*$"],
    )
});

/// Codex CLI's command-approval prompts and its "Working"/"esc to interrupt" activity indicator.
static CODEX_SCREEN: LazyLock<ScreenPatterns> = LazyLock::new(|| {
    ScreenPatterns::new(
        &[r"allow command", r"approve this (command|action)", r"do you want to allow"],
        &[r"\bworking\b", r"esc to interrupt", r"\bthinking\b"],
        &[r"send a message", r"^\s*›\s*$"],
    )
});

/// GitHub Copilot CLI's action-approval prompts and selection dialogs (folder trust, hook
/// permission), its ask_user question box ("enter accept · ctrl+d decline"), its
/// "Working"/thinking indicator, and the "Loading: ..." status while it starts.
/// At rest (1.0.89) the footer lists "/ commands".
static COPILOT_SCREEN: LazyLock<ScreenPatterns> = LazyLock::new(|| {
    ScreenPatterns::new(
        &[r"allow this action", r"do you want to allow", r"\(y/n\)", r"enter to select", r"ctrl\+d decline"],
        &[r"\bworking\b", r"\bthinking\b", r"\bloading:"],
        &[r"^\s*>\s*$", r"/ commands\b.*\? help"],
    )
});

/// What `GET /api/harnesses` reports for one configured harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCapabilities {
    pub key: String,
    pub display_name: String,
    pub kind: String,
    pub enabled: bool,
    pub available: bool,
    pub command: String,
    pub resolved_path: Option<String>,
    pub version: Option<String>,
    pub supports_model: bool,
    pub supports_reasoning: bool,
    pub supports_resume: bool,
    pub message: Option<String>,
}

/// Everything needed to start a harness process for one node run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub env: IndexMap<String, String>,
    pub harness_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HarnessAdapter {
    pub key: String,
    pub config: HarnessConfig,
    pub kind: HarnessKind,
    /// The user's home directory: where Claude's `.claude.json` and Codex's session logs live.
    home: PathBuf,
}

impl HarnessAdapter {
    pub fn new(key: &str, config: HarnessConfig) -> Self {
        Self::with_home(key, config, std::env::home_dir().unwrap_or_else(|| PathBuf::from(".")))
    }

    /// An adapter that reads and writes the harness's own files under `home` (tests).
    pub fn with_home(key: &str, config: HarnessConfig, home: PathBuf) -> Self {
        let kind = HarnessKind::from_type(&config.kind);
        HarnessAdapter { key: key.to_string(), config, kind, home }
    }

    // detection ---------------------------------------------------------------------------------

    pub async fn detect(&self) -> HarnessCapabilities {
        let mut caps = self.unprobed();
        let Some(path) = caps.resolved_path.clone() else {
            caps.message = Some(format!("'{}' not found", self.config.command));
            return caps;
        };
        let argv = vec![path, "--version".to_string()];
        match process::run(&argv, None, &Default::default(), Duration::from_secs(20), None).await {
            Ok(result) => {
                let text = if result.stdout.is_empty() { &result.stderr } else { &result.stdout };
                let text = text.trim();
                if !text.is_empty() {
                    let first = text.lines().next().unwrap_or_default();
                    caps.version = Some(first.chars().take(120).collect());
                }
            }
            Err(error) => caps.message = Some(format!("version check failed: {error}")),
        }
        caps
    }

    /// What detection reports without running the CLI: availability by PATH lookup, no version.
    pub fn unprobed(&self) -> HarnessCapabilities {
        let path = resolve_executable(&self.config.command);
        HarnessCapabilities {
            key: self.key.clone(),
            display_name: if self.config.display_name.is_empty() {
                self.key.clone()
            } else {
                self.config.display_name.clone()
            },
            kind: self.config.kind.clone(),
            enabled: self.config.enabled,
            available: path.is_some(),
            command: self.config.command.clone(),
            resolved_path: path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            version: None,
            supports_model: self.model_flag().is_some(),
            supports_reasoning: self.reasoning_flag().is_some(),
            supports_resume: self.kind.supports_resume(),
            message: None,
        }
    }

    // argv construction -------------------------------------------------------------------------

    pub fn model_flag(&self) -> Option<String> {
        let configured = self.config.model_argument.clone();
        match self.kind {
            HarnessKind::Generic => configured,
            _ => Some(configured.unwrap_or_else(|| "--model".into())),
        }
    }

    pub fn reasoning_flag(&self) -> Option<String> {
        let configured = self.config.reasoning_argument.clone();
        match self.kind {
            HarnessKind::Generic => configured,
            HarnessKind::Claude | HarnessKind::Copilot => Some(configured.unwrap_or_else(|| "--effort".into())),
            // Codex expresses reasoning effort through a config override.
            HarnessKind::Codex => Some(configured.unwrap_or_else(|| "-c".into())),
        }
    }

    pub fn map_reasoning(&self, reasoning: Option<&str>) -> Option<String> {
        let reasoning = reasoning?;
        Some(self.config.reasoning_map.get(reasoning).cloned().unwrap_or_else(|| reasoning.to_string()))
    }

    pub fn prompt_args(&self, prompt: &str) -> Vec<String> {
        let flag = self.config.prompt_argument.as_deref();
        if self.kind == HarnessKind::Copilot {
            // -i starts interactive mode and executes the prompt (-p would be non-interactive).
            // Python passes any configured value as the flag, "positional" included.
            return vec![flag.unwrap_or("-i").to_string(), prompt.to_string()];
        }
        match flag {
            None | Some("positional") => vec![prompt.to_string()],
            Some(flag) => vec![flag.to_string(), prompt.to_string()],
        }
    }

    pub fn base_argv(&self) -> Vec<String> {
        let exe = resolve_executable(&self.config.command)
            .map_or_else(|| self.config.command.clone(), |p| p.to_string_lossy().into_owned());
        std::iter::once(exe).chain(self.config.args.iter().cloned()).collect()
    }

    /// Python's truthiness: an empty model or reasoning is the same as none.
    pub fn option_args(&self, model: Option<&str>, reasoning: Option<&str>) -> Vec<String> {
        let model = model.filter(|m| !m.is_empty());
        let mapped = self.map_reasoning(reasoning).filter(|r| !r.is_empty());
        let mut args = Vec::new();
        if self.kind == HarnessKind::Codex {
            if let Some(model) = model {
                args.extend([self.model_flag().expect("codex has a model flag"), model.to_string()]);
            }
            if let Some(mapped) = mapped {
                let flag = self.reasoning_flag().expect("codex has a reasoning flag");
                if flag == "-c" {
                    args.extend(["-c".to_string(), format!("model_reasoning_effort=\"{mapped}\"")]);
                } else {
                    args.extend([flag, mapped]);
                }
            }
            return args;
        }
        if let (Some(model), Some(flag)) = (model, self.model_flag()) {
            args.extend([flag, model.to_string()]);
        }
        if let (Some(mapped), Some(flag)) = (mapped, self.reasoning_flag()) {
            args.extend([flag, mapped]);
        }
        args
    }

    pub fn build_launch(&self, model: Option<&str>, reasoning: Option<&str>, initial_prompt: &str) -> LaunchSpec {
        let mut argv = self.base_argv();
        // Claude and Copilot take a session id upfront: it lets a later run resume the same
        // conversation, and lets a usage collector find this run's own session log.
        let session_id = matches!(self.kind, HarnessKind::Claude | HarnessKind::Copilot).then(new_session_id);
        if let Some(id) = &session_id {
            argv.extend(["--session-id".to_string(), id.clone()]);
        }
        argv.extend(self.option_args(model, reasoning));
        argv.extend(self.prompt_args(initial_prompt));
        LaunchSpec { argv, env: self.config.env.clone(), harness_session_id: session_id }
    }

    /// Argv to resume a previous conversation; None when the harness can't, or when no id of
    /// that conversation is known.
    pub fn build_resume(
        &self,
        harness_session_id: Option<&str>,
        model: Option<&str>,
        reasoning: Option<&str>,
        initial_prompt: &str,
    ) -> Option<LaunchSpec> {
        let id = harness_session_id.filter(|id| !id.is_empty())?;
        let mut argv = self.base_argv();
        match self.kind {
            HarnessKind::Generic => return None,
            HarnessKind::Claude | HarnessKind::Copilot => {
                argv.extend(["--resume".to_string(), id.to_string()]);
                argv.extend(self.option_args(model, reasoning));
                argv.extend(self.prompt_args(initial_prompt));
            }
            // The id comes from capture_session_id: Codex assigns its own. The prompt is always
            // positional here, as in Python.
            HarnessKind::Codex => {
                argv.extend(["resume".to_string(), id.to_string()]);
                argv.extend(self.option_args(model, reasoning));
                argv.push(initial_prompt.to_string());
            }
        }
        Some(LaunchSpec { argv, env: self.config.env.clone(), harness_session_id: Some(id.to_string()) })
    }

    /// Best-effort discovery of a session id the CLI assigned itself after launch, for harnesses
    /// with no launch-time flag to choose one upfront (Codex). None when unsupported, or when
    /// discovery can't be done safely.
    pub async fn capture_session_id(&self, cwd: &Path, since: DateTime<Utc>) -> Option<String> {
        if self.kind != HarnessKind::Codex {
            return None;
        }
        codex::capture_session_id(&self.home.join(".codex").join("sessions"), cwd, since, codex::Polling::default())
            .await
    }

    /// Best-effort live state from a pane's rendered screen.
    ///
    /// Heuristic, like herdr's screen manifests: an unfamiliar CLI release can misclassify.
    /// Unrecognized text stays Unknown rather than guessing Idle: a false idle hides an agent
    /// that's actually stuck. Blank rows are dropped *before* taking the tail: a short dialog near
    /// the top of a tall pane is followed by dozens of blank padding rows, and slicing the raw tail
    /// first would land entirely on that padding.
    pub fn classify_screen(&self, viewport: &[String]) -> ScreenState {
        let patterns: &ScreenPatterns = match self.kind {
            HarnessKind::Claude => &CLAUDE_SCREEN,
            HarnessKind::Codex => &CODEX_SCREEN,
            HarnessKind::Copilot => &COPILOT_SCREEN,
            // No fixed CLI screen to key off (generic-cli is config-driven).
            HarnessKind::Generic => return ScreenState::Unknown,
        };
        let non_blank: Vec<&str> = viewport.iter().map(String::as_str).filter(|line| !line.trim().is_empty()).collect();
        let text = non_blank[non_blank.len().saturating_sub(SCREEN_TAIL_ROWS)..].join("\n");
        if patterns.blocked.is_match(&text) {
            ScreenState::Blocked
        } else if patterns.working.is_match(&text) {
            ScreenState::Working
        } else if patterns.idle.is_match(&text) {
            ScreenState::Idle
        } else {
            ScreenState::Unknown
        }
    }

    /// Best-effort: pre-clear a first-run-in-this-directory prompt the CLI itself would show.
    /// Called before every launch/resume. Only Claude has one (its workspace trust dialog), and it
    /// is cleared only for repositories the user listed in `trustedRepositories`: every task runs
    /// in a fresh worktree, and unattended that dialog would stall the run forever.
    pub fn prepare_workspace(&self, cwd: &Path, repository_root: Option<&Path>) {
        if self.kind != HarnessKind::Claude {
            return;
        }
        let trusted_root = repository_root.unwrap_or(cwd);
        let trusted = self
            .config
            .trusted_repositories
            .iter()
            .any(|path| crate::paths::same_path(process::expand_user(path), trusted_root));
        if trusted {
            claude::trust_workspace(&self.home.join(".claude.json"), cwd);
        }
    }

    /// The short line typed into the harness: prompts themselves live in files (Windows argv
    /// safety).
    pub fn prompt_reference(prompt_file: &Path) -> String {
        format!(
            "Read the file {} and follow the instructions in it exactly. It is your task briefing.",
            as_posix(prompt_file)
        )
    }
}

pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Python's `PurePath.as_posix()`: forward slashes, with empty and `.` segments and a trailing
/// separator dropped (`..` is kept).
pub fn as_posix(path: &Path) -> String {
    use std::path::Component;
    let mut prefix = String::new();
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => prefix = p.as_os_str().to_string_lossy().replace('\\', "/"),
            Component::RootDir => prefix.push('/'),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".into()),
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
        }
    }
    let joined = format!("{prefix}{}", parts.join("/"));
    if joined.is_empty() { ".".into() } else { joined }
}

/// Every configured harness's adapter, and the result of the last detection.
pub struct HarnessRegistry {
    pub adapters: IndexMap<String, HarnessAdapter>,
    pub capabilities: IndexMap<String, HarnessCapabilities>,
}

impl HarnessRegistry {
    pub fn new(config: &Config) -> Self {
        let adapters =
            config.harnesses.iter().map(|(key, cfg)| (key.clone(), HarnessAdapter::new(key, cfg.clone()))).collect();
        HarnessRegistry { adapters, capabilities: IndexMap::new() }
    }

    /// Capabilities from the configuration and a PATH lookup alone, without running any harness
    /// (tests, and anywhere starting the CLIs is unwanted). No versions.
    pub fn assume_capabilities(&mut self) {
        self.capabilities = self.adapters.iter().map(|(key, adapter)| (key.clone(), adapter.unprobed())).collect();
    }

    pub async fn detect_all(&mut self) -> &IndexMap<String, HarnessCapabilities> {
        let mut capabilities = IndexMap::new();
        for (key, adapter) in &self.adapters {
            capabilities.insert(key.clone(), adapter.detect().await);
        }
        self.capabilities = capabilities;
        &self.capabilities
    }

    pub fn get(&self, key: &str) -> Result<&HarnessAdapter, String> {
        self.adapters.get(key).ok_or_else(|| format!("Unknown harness '{key}'"))
    }

    pub fn is_usable(&self, key: &str) -> bool {
        self.capabilities.get(key).is_some_and(|caps| caps.enabled && caps.available)
    }
}
