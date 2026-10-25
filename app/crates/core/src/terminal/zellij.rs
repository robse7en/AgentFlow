//! The zellij backend (opt-in), ported from `backend/agentflow/services/zellij.py`.
//!
//! All calls target a *named* session by setting ZELLIJ_SESSION_NAME in the child environment,
//! after scrubbing any inherited zellij variables (see [`crate::process::clean_env`]). Sessions live
//! in zellij's own server and outlive the app, so a human can `zellij attach` from any terminal.
//!
//! On Windows a session is headless, so each one gets a hidden `zellij attach` console that keeps
//! it sized (the "sizing client"): zellij lays new tabs out against an attached client's viewport.
//!
//! Every process the service runs or starts goes through a [`ZellijHost`], and the platform is a
//! field, so tests can drive both platforms' logic with a fake (Python's tests patched
//! `sys.platform` and the process helpers).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use super::{PaneEvent, PaneInfo, SessionState, TerminalError};
use crate::config::ZellijConfig;
use crate::process::{self, CommandResult, ProcessError, resolve_executable};

/// Interpolated into a `cmd /c` line for the hidden client, so kept to characters cmd leaves alone.
static SAFE_SESSION_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+$").unwrap());
static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\.(\d+)\.(\d+)").unwrap());

/// `subscribe --format json`, `write-chars --pane-id`, `list-panes --json`, `attach --create-background`.
const MIN_VERSION: (u64, u64, u64) = (0, 44, 0);
const ZELLIJ_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);
/// Width of a background session that has no client attached.
const CLIENTLESS_COLUMNS: i64 = 50;

pub fn version_supported(version: Option<&str>) -> bool {
    let Some(captures) = version.and_then(|v| VERSION.captures(v)) else { return false };
    let part = |i: usize| captures[i].parse::<u64>().unwrap_or(0);
    (part(1), part(2), part(3)) >= MIN_VERSION
}

fn not_found() -> TerminalError {
    TerminalError("zellij executable not found (configure zellij.command)".into())
}

fn detail(result: &CommandResult) -> String {
    let text = if result.stderr.is_empty() { &result.stdout } else { &result.stderr };
    text.trim().to_string()
}

impl From<ProcessError> for TerminalError {
    fn from(error: ProcessError) -> Self {
        TerminalError(error.0)
    }
}

/// A process the service started and keeps an eye on: a session's launcher or sizing client.
#[async_trait::async_trait]
pub trait ChildHandle: Send + Sync {
    /// The exit code, or None while it runs.
    fn exit_code(&self) -> Option<i64>;
    /// Wait up to `timeout` for it to exit; whether it did.
    async fn wait(&self, timeout: Duration) -> bool;
    /// Stop it and everything it started.
    async fn terminate_tree(&self);
}

/// Where the service runs zellij and starts its helper processes.
#[async_trait::async_trait]
pub trait ZellijHost: Send + Sync {
    /// The zellij executable for `command`, if it can be found.
    fn executable(&self, command: &str) -> Option<String>;
    /// Run a command to completion, against `session` when given (ZELLIJ_SESSION_NAME).
    async fn run(
        &self,
        argv: &[String],
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<CommandResult, ProcessError>;
    /// Start the process that creates a session's server (`attach --create-background`).
    fn start_launcher(&self, argv: &[String]) -> Result<Arc<dyn ChildHandle>, ProcessError>;
    /// Start a hidden console running a `cmd /c` line (Windows): a session's sizing client.
    fn start_hidden_shell(&self, line: &str) -> Result<Arc<dyn ChildHandle>, ProcessError>;
    /// Start a detached process (a visible window attached to a session).
    fn spawn_detached(&self, argv: &[String], detach_from_console: bool, breakaway: bool) -> Result<(), ProcessError>;
}

/// The real processes.
pub struct SystemHost;

#[cfg(windows)]
#[async_trait::async_trait]
impl ChildHandle for process::hidden::HiddenConsole {
    fn exit_code(&self) -> Option<i64> {
        process::hidden::HiddenConsole::exit_code(self)
    }
    async fn wait(&self, timeout: Duration) -> bool {
        process::hidden::HiddenConsole::wait(self, timeout).await
    }
    async fn terminate_tree(&self) {
        process::hidden::HiddenConsole::terminate_tree(self).await
    }
}

/// A detached child (the launcher outside Windows).
#[cfg(not(windows))]
struct Detached(tokio::sync::Mutex<tokio::process::Child>);

#[cfg(not(windows))]
#[async_trait::async_trait]
impl ChildHandle for Detached {
    fn exit_code(&self) -> Option<i64> {
        let mut child = self.0.try_lock().ok()?;
        child.try_wait().ok().flatten().map(|s| i64::from(s.code().unwrap_or(-1)))
    }
    async fn wait(&self, timeout: Duration) -> bool {
        let mut child = self.0.lock().await;
        tokio::time::timeout(timeout, child.wait()).await.is_ok()
    }
    async fn terminate_tree(&self) {
        process::terminate_process_tree(&mut *self.0.lock().await).await;
    }
}

#[async_trait::async_trait]
impl ZellijHost for SystemHost {
    fn executable(&self, command: &str) -> Option<String> {
        resolve_executable(command).map(|p| p.to_string_lossy().into_owned())
    }

    async fn run(
        &self,
        argv: &[String],
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<CommandResult, ProcessError> {
        let env =
            session.map(|s| HashMap::from([("ZELLIJ_SESSION_NAME".to_string(), s.to_string())])).unwrap_or_default();
        process::run(argv, None, &env, timeout, None).await
    }

    /// A hidden console on Windows (see spawn_hidden_console for why it can't be detached), a
    /// detached process elsewhere (a captured run can hang because the background server inherits
    /// its pipes).
    fn start_launcher(&self, argv: &[String]) -> Result<Arc<dyn ChildHandle>, ProcessError> {
        #[cfg(windows)]
        {
            use process::hidden::{HiddenCommand, spawn_hidden_console};
            spawn_hidden_console(HiddenCommand::Argv(argv), &HashMap::new())
                .map(|c| Arc::new(c) as Arc<dyn ChildHandle>)
        }
        #[cfg(not(windows))]
        {
            process::spawn_detached(argv, None, &HashMap::new(), false, false)
                .map(|c| Arc::new(Detached(tokio::sync::Mutex::new(c))) as Arc<dyn ChildHandle>)
        }
    }

    fn start_hidden_shell(&self, line: &str) -> Result<Arc<dyn ChildHandle>, ProcessError> {
        #[cfg(windows)]
        {
            use process::hidden::{HiddenCommand, spawn_hidden_console};
            spawn_hidden_console(HiddenCommand::Shell(line), &HashMap::new())
                .map(|c| Arc::new(c) as Arc<dyn ChildHandle>)
        }
        #[cfg(not(windows))]
        {
            let _ = line;
            Err(ProcessError("hidden consoles exist only on Windows".into()))
        }
    }

    fn spawn_detached(&self, argv: &[String], detach_from_console: bool, breakaway: bool) -> Result<(), ProcessError> {
        process::spawn_detached(argv, None, &HashMap::new(), detach_from_console, breakaway).map(drop)
    }
}

/// How a sizing client counts as attached (see [`ZellijService::ensure_client_attached`]).
#[derive(Clone, Copy)]
enum Attached {
    /// The viewport reached the client's width (a just-created session lists a phantom client).
    ViewportSized,
    /// `list-clients` shows a client.
    ClientListed,
}

pub struct ZellijService {
    pub config: ZellijConfig,
    host: Arc<dyn ZellijHost>,
    /// Whether to behave as on Windows (headless sessions with hidden sizing clients).
    windows: bool,
    /// Session name -> the hidden `zellij attach` console keeping it sized (Windows).
    sizing_clients: Mutex<HashMap<String, Arc<dyn ChildHandle>>>,
}

impl ZellijService {
    pub fn new(config: ZellijConfig) -> Self {
        ZellijService::with_host(config, Arc::new(SystemHost), cfg!(windows))
    }

    /// A service on another host and platform (tests).
    pub fn with_host(config: ZellijConfig, host: Arc<dyn ZellijHost>, windows: bool) -> Self {
        ZellijService { config, host, windows, sizing_clients: Mutex::new(HashMap::new()) }
    }

    /// The tracked sizing client for a session (tests).
    pub fn sizing_client(&self, session: &str) -> Option<Arc<dyn ChildHandle>> {
        self.sizing_clients.lock().unwrap().get(session).cloned()
    }

    /// Track a sizing client as a previous call would have (tests).
    pub fn track_sizing_client(&self, session: &str, client: Arc<dyn ChildHandle>) {
        self.sizing_clients.lock().unwrap().insert(session.to_string(), client);
    }

    // detection ---------------------------------------------------------------------------------

    pub fn executable(&self) -> Option<String> {
        self.host.executable(&self.config.command)
    }

    pub async fn version(&self) -> Option<String> {
        let exe = self.executable()?;
        let argv = vec![exe, "--version".to_string()];
        let result = self.host.run(&argv, None, Duration::from_secs(15)).await.ok()?;
        Some(result.stdout.trim().to_string())
    }

    pub async fn require_subscribe_support(&self) -> Result<(), TerminalError> {
        let version = self.version().await;
        if !version_supported(version.as_deref()) {
            let found = version.unwrap_or_else(|| "unavailable".into());
            return Err(TerminalError(format!("The embedded terminal requires Zellij 0.44 or newer (found {found})")));
        }
        Ok(())
    }

    // low level -----------------------------------------------------------------------------------

    async fn zellij(&self, args: &[&str], session: Option<&str>) -> Result<CommandResult, TerminalError> {
        let exe = self.executable().ok_or_else(not_found)?;
        let argv: Vec<String> = std::iter::once(exe).chain(args.iter().map(|a| a.to_string())).collect();
        Ok(self.host.run(&argv, session, ZELLIJ_TIMEOUT).await?)
    }

    /// Every session `list-sessions` knows, as running or exited.
    ///
    /// An exited session ("EXITED - attach to resurrect") is only saved layout: no server is
    /// running it. It must never count as running, because `zellij action` against it hangs until
    /// the timeout rather than failing.
    async fn session_states(&self) -> Result<IndexMap<String, SessionState>, TerminalError> {
        let result = self.zellij(&["list-sessions", "--no-formatting"], None).await?;
        if !result.ok() {
            let detail = detail(&result);
            let lower = detail.to_lowercase();
            if lower.contains("no active zellij sessions") || lower.contains("no active sessions") {
                return Ok(IndexMap::new());
            }
            return Err(TerminalError(format!("Could not inspect Zellij sessions: {detail}")));
        }
        Ok(parse_sessions(&result.stdout))
    }

    /// Running sessions only, in the order zellij lists them.
    pub async fn list_sessions(&self) -> Result<Vec<String>, TerminalError> {
        let states = self.session_states().await?;
        Ok(states.into_iter().filter(|(_, s)| *s == SessionState::Running).map(|(name, _)| name).collect())
    }

    /// Running, exited, or None when zellij doesn't know the session at all.
    pub async fn session_state(&self, name: &str) -> Result<Option<SessionState>, TerminalError> {
        Ok(self.session_states().await?.get(name).copied())
    }

    /// Whether the session is running, i.e. its tabs and panes can be queried.
    pub async fn session_exists(&self, name: &str) -> Result<bool, TerminalError> {
        Ok(self.session_state(name).await? == Some(SessionState::Running))
    }

    async fn delete_session(&self, name: &str) -> Result<(), TerminalError> {
        let result = self.zellij(&["delete-session", name, "--force"], None).await?;
        if !result.ok() {
            let detail = detail(&result);
            let lower = detail.to_lowercase();
            if !lower.contains("no session") && !lower.contains("not found") {
                return Err(TerminalError(format!("Could not delete Zellij session {name}: {detail}")));
            }
        }
        Ok(())
    }

    /// Create a session if needed. Returns true when newly created.
    pub async fn ensure_session(&self, name: &str) -> Result<bool, TerminalError> {
        crate::diagnostics::event("zellij.session.requested", json!({"session": name}));
        let created = self.ensure_session_inner(name).await;
        match &created {
            Ok(created) => {
                crate::diagnostics::event("zellij.session.ready", json!({"session": name, "created": created}))
            }
            Err(error) => tracing::error!(session = name, %error, "zellij.session.failed"),
        }
        created
    }

    async fn ensure_session_inner(&self, name: &str) -> Result<bool, TerminalError> {
        match self.session_state(name).await? {
            Some(SessionState::Running) => return Ok(false),
            Some(SessionState::Exited) => {
                // Start fresh rather than resurrect: a resurrected layout brings back the old agent
                // tabs under their names, which untracked-tab matching could take for live agents.
                crate::diagnostics::event("zellij.session.exited_replaced", json!({"session": name}));
                self.delete_session(name).await?;
            }
            None => {}
        }
        let exe = self.executable().ok_or_else(not_found)?;
        let argv = vec![exe.clone(), "attach".into(), "--create-background".into(), name.to_string()];
        let launcher = self
            .host
            .start_launcher(&argv)
            .map_err(|e| TerminalError(format!("Could not start Zellij session {name}: {e}")))?;
        for _ in 0..40 {
            tokio::time::sleep(POLL).await;
            if self.session_exists(name).await? {
                if self.windows {
                    self.size_new_session(&exe, name).await?;
                }
                return Ok(true);
            }
            if let Some(code) = launcher.exit_code()
                && code != 0
            {
                crate::diagnostics::event("zellij.launch.exited", json!({"session": name, "exitCode": code}));
                return Err(TerminalError(format!("Zellij exited with code {code} before creating session {name}")));
            }
        }
        let code = launcher.exit_code();
        crate::diagnostics::event(
            "zellij.launch.timeout",
            json!({"session": name, "exitCode": code, "timeoutSeconds": 10}),
        );
        if code == Some(0) {
            return Err(TerminalError(format!(
                "Zellij exited successfully, but session {name} did not appear within 10s"
            )));
        }
        Err(TerminalError(format!("zellij session {name} did not appear within 10s")))
    }

    /// Attach the sizing client to a just-created session, confirmed by geometry.
    ///
    /// `list-clients` can't confirm it here: a new background session lists a phantom client (the
    /// launcher's own) for seconds at least, which looks exactly like a real one and sizes nothing.
    /// Instead, wait for the viewport to grow past the 50 columns a session has with no client.
    /// That is the client's actual window width, which can be narrower than headlessCols: a console
    /// window can't be wider than the screen, and a disconnected or locked Windows session has a
    /// small one (about 118 columns), so waiting for the full headlessCols would never succeed
    /// there. On failure the half-made session is killed so a retry starts clean.
    async fn size_new_session(&self, exe: &str, name: &str) -> Result<(), TerminalError> {
        if let Err(error) = self.start_sizing_client(exe, name, Attached::ViewportSized).await {
            if let Err(cleanup) = self.kill_session(name).await {
                tracing::error!(session = name, %cleanup, "zellij.session.cleanup_failed");
            }
            return Err(error);
        }
        Ok(())
    }

    async fn viewport_sized(&self, session: &str) -> bool {
        let Ok(result) = self.zellij(&["action", "list-panes", "--json", "--all"], Some(session)).await else {
            return false;
        };
        if !result.ok() {
            return false;
        }
        let Ok(Value::Array(panes)) =
            serde_json::from_str::<Value>(if result.stdout.is_empty() { "[]" } else { &result.stdout })
        else {
            return false;
        };
        let widest =
            panes.iter().map(|p| p.get("pane_columns").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0);
        widest > CLIENTLESS_COLUMNS
    }

    pub async fn client_count(&self, session: &str) -> usize {
        match self.zellij(&["action", "list-clients"], Some(session)).await {
            // The first line is the CLIENT_ID header.
            Ok(result) if result.ok() => {
                result.stdout.lines().filter(|l| !l.trim().is_empty()).count().saturating_sub(1)
            }
            _ => 0,
        }
    }

    /// Make sure a client is sizing `session` before adding a pane (Windows only).
    ///
    /// Zellij lays a new tab out against an attached client's viewport. With no client it still
    /// spawns the tab's command, but fails the layout ("Not enough room for panes"): the tab
    /// vanishes and its process keeps running invisibly until the whole session is killed. So a
    /// client must be attached before every new_tab, and failing to confirm one is an error, never
    /// something to retry past (a retry would run the command twice).
    ///
    /// A tracked client counts by its process's liveness (`list-clients` has missed a working
    /// hidden client). Without a tracked client (e.g. after a restart) any listed client counts:
    /// the previous run's hidden client, or a window the human opened.
    pub async fn ensure_client_attached(&self, session: &str) -> Result<(), TerminalError> {
        if !self.windows {
            return Ok(());
        }
        let tracked = self.sizing_client(session);
        match tracked {
            Some(client) if client.exit_code().is_none() => return Ok(()),
            None if self.client_count(session).await > 0 => return Ok(()),
            _ => {}
        }
        let exe = self.executable().ok_or_else(not_found)?;
        // No phantom client here (that only haunts a just-created session), so a listed client is
        // good enough to confirm the replacement.
        self.start_sizing_client(&exe, session, Attached::ClientListed).await
    }

    /// Start the hidden, sized `zellij attach` console for `session` and wait until `attached`.
    async fn start_sizing_client(&self, exe: &str, session: &str, attached: Attached) -> Result<(), TerminalError> {
        if !SAFE_SESSION_NAME.is_match(session) {
            return Err(TerminalError(format!("Unsupported Zellij session name: '{session}'")));
        }
        if !self.windows {
            return Ok(());
        }
        let restarted = self.sizing_clients.lock().unwrap().contains_key(session);
        crate::diagnostics::event("zellij.client.starting", json!({"session": session, "restarted": restarted}));
        let (cols, rows) = (self.config.headless_cols, self.config.headless_rows);
        let line = format!("mode con: cols={cols} lines={rows} & \"{exe}\" attach {session}");
        let client = self
            .host
            .start_hidden_shell(&line)
            .map_err(|e| TerminalError(format!("Could not start a hidden Zellij client for {session}: {}", e.0)))?;
        self.track_sizing_client(session, client.clone());
        for _ in 0..40 {
            tokio::time::sleep(POLL).await;
            if let Some(code) = client.exit_code() {
                return Err(TerminalError(format!("Hidden Zellij client for {session} exited with code {code}")));
            }
            let ok = match attached {
                Attached::ViewportSized => self.viewport_sized(session).await,
                Attached::ClientListed => self.client_count(session).await > 0,
            };
            if ok {
                return Ok(());
            }
        }
        client.terminate_tree().await;
        Err(TerminalError(format!("Hidden Zellij client did not attach to session {session} within 10s")))
    }

    /// Open a visible, interactive window attached to `session`, on `tab_name` (Windows only).
    ///
    /// A newly attached client joins whichever tab the existing (hidden) client is on, so focusing
    /// the tab first is what lands the window on it. `/max` keeps the window no smaller than the
    /// hidden client: a tab is laid out at the smallest viewing client's size and doesn't grow back
    /// once that client leaves. Deliberately not Windows Terminal: `wt` re-tokenizes its command
    /// line and has mangled an exe path with a space in it. Breaks away from the hosting job so the
    /// window survives closing whatever hosts the app.
    pub async fn open_window(&self, session: &str, tab_name: Option<&str>) -> Result<(), TerminalError> {
        if !self.windows {
            return Err(TerminalError(format!(
                "Opening a terminal window is only supported on Windows; run: {}",
                self.attach_command(session)
            )));
        }
        let exe = self.executable().ok_or_else(not_found)?;
        if let Some(tab) = tab_name {
            self.focus_tab(session, tab).await;
        }
        let argv: Vec<String> = ["cmd", "/c", "start", "/max", "", &exe, "attach", session].map(String::from).into();
        self.host
            .spawn_detached(&argv, true, true)
            .map_err(|e| TerminalError(format!("Could not open a terminal window for {session}: {}", e.0)))
    }

    // tabs and panes ------------------------------------------------------------------------------

    pub async fn list_panes(&self, session: &str) -> Result<Vec<PaneInfo>, TerminalError> {
        let result = self.zellij(&["action", "list-panes", "--json", "--all"], Some(session)).await?;
        if !result.ok() {
            return Err(TerminalError(format!("list-panes failed for {session}: {}", detail(&result))));
        }
        parse_panes(&result.stdout)
    }

    /// Create a tab, optionally running a command. Returns (tab_id, pane_id).
    pub async fn new_tab(
        &self,
        session: &str,
        name: &str,
        cwd: &Path,
        command: Option<&[String]>,
    ) -> Result<(i64, Option<String>), TerminalError> {
        self.ensure_client_attached(session).await?;
        let cwd = cwd.to_string_lossy();
        let mut args: Vec<&str> = vec!["action", "new-tab", "--name", name, "--cwd", &cwd];
        if let Some(command) = command.filter(|c| !c.is_empty()) {
            args.push("--");
            args.extend(command.iter().map(String::as_str));
        }
        let result = self.zellij(&args, Some(session)).await?;
        if !result.ok() {
            return Err(TerminalError(format!("new-tab failed: {}", detail(&result))));
        }
        let mut tab_id = result
            .stdout
            .trim()
            .lines()
            .last()
            .map(str::trim)
            .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
            .and_then(|l| l.parse().ok())
            .unwrap_or(-1);
        // The pane (and sometimes the tab id itself) shows up slightly after new-tab returns.
        let mut pane_id = None;
        for _ in 0..12 {
            if tab_id < 0 {
                tab_id = self.tab_id_by_name(session, name).await;
            }
            if tab_id >= 0 {
                pane_id = self.pane_for_tab(session, tab_id).await;
                if pane_id.is_some() {
                    break;
                }
            }
            tokio::time::sleep(POLL).await;
        }
        if pane_id.is_none() {
            // Usually the layout failed for want of a sizing client (see ensure_client_attached).
            crate::diagnostics::event("zellij.tab.paneless", json!({"session": session, "tab": name, "tabId": tab_id}));
        }
        Ok((tab_id, pane_id))
    }

    /// Highest tab id carrying `name` (tabs are created in id order), or -1.
    pub async fn tab_id_by_name(&self, session: &str, name: &str) -> i64 {
        match self.zellij(&["action", "list-tabs"], Some(session)).await {
            Ok(result) if result.ok() => parse_tab_id_by_name(&result.stdout, name),
            _ => -1,
        }
    }

    pub async fn pane_for_tab(&self, session: &str, tab_id: i64) -> Option<String> {
        self.list_panes(session).await.ok()?.into_iter().find(|p| p.tab_id == tab_id).map(|p| p.pane_id)
    }

    pub async fn pane_state(&self, session: &str, pane_id: &str) -> Result<Option<PaneInfo>, TerminalError> {
        Ok(self.list_panes(session).await?.into_iter().find(|p| p.pane_id == pane_id))
    }

    /// The pane's current viewport, one string per row.
    pub async fn screen(&self, session: &str, pane_id: &str) -> Result<Vec<String>, TerminalError> {
        let result = self.zellij(&["action", "dump-screen", "--pane-id", pane_id], Some(session)).await?;
        if !result.ok() {
            return Err(TerminalError(format!("dump-screen failed: {}", detail(&result))));
        }
        Ok(result.stdout.lines().map(String::from).collect())
    }

    /// Type a single line into a pane and press Enter.
    pub async fn write_line(&self, session: &str, pane_id: &str, text: &str) -> Result<(), TerminalError> {
        let result = self.zellij(&["action", "write-chars", "--pane-id", pane_id, text], Some(session)).await?;
        if !result.ok() {
            return Err(TerminalError(format!("write-chars failed: {}", detail(&result))));
        }
        let result = self.zellij(&["action", "write", "--pane-id", pane_id, "13"], Some(session)).await?;
        if !result.ok() {
            return Err(TerminalError(format!("write failed: {}", detail(&result))));
        }
        Ok(())
    }

    /// The `subscribe` arguments that stream one pane of one session.
    pub fn subscribe_args(session: &str, pane_id: &str) -> Vec<String> {
        ["--session", session, "subscribe", "--pane-id", pane_id, "--scrollback", "100", "--format", "json"]
            .map(String::from)
            .into()
    }

    /// Stream the rendered output of one pane (`zellij subscribe --format json`). Errors arrive as
    /// [`PaneEvent::Error`] on the stream, as Python yielded them; the `subscribe` process is
    /// killed when the receiver is dropped. Needs a tokio runtime.
    pub fn subscribe(&self, session: &str, pane_id: &str) -> mpsc::Receiver<PaneEvent> {
        let (sender, receiver) = mpsc::channel(8);
        let Some(exe) = self.executable() else {
            let _ = sender.try_send(PaneEvent::Error("zellij executable not found".into()));
            return receiver;
        };
        let mut command = tokio::process::Command::new(exe);
        command
            .args(Self::subscribe_args(session, pane_id))
            .env_clear()
            .envs(process::clean_env(&HashMap::new()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        tokio::spawn(async move {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = sender.send(PaneEvent::Error(format!("Could not start Zellij pane stream: {error}"))).await;
                    return;
                }
            };
            let mut lines = tokio::io::BufReader::new(child.stdout.take().expect("piped")).lines();
            loop {
                tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => {
                            if let Some(event) = parse_pane_event(&line)
                                && sender.send(event).await.is_err()
                            {
                                return; // dropping `child` kills it
                            }
                        }
                        _ => break,
                    },
                    () = sender.closed() => return,
                }
            }
            if let Ok(status) = child.wait().await
                && let Some(code) = status.code().filter(|c| *c != 0)
            {
                let message = format!("Zellij pane stream exited with code {code}");
                let _ = sender.send(PaneEvent::Error(message)).await;
            }
        });
        receiver
    }

    pub async fn focus_tab(&self, session: &str, tab_name: &str) {
        let _ = self.zellij(&["action", "go-to-tab-name", tab_name], Some(session)).await;
    }

    /// Kill and delete a whole session, verifying it's actually gone.
    ///
    /// Stopping agents only closes the tabs the database tracks, so any other pane in the task's
    /// session (the "Tests"/"Shell" tabs, with a cwd inside the worktree) keeps holding that
    /// directory open, and on Windows that blocks `git worktree remove`. Killing the whole session
    /// is the only way to be sure nothing is left with its cwd inside the worktree.
    pub async fn kill_session(&self, name: &str) -> Result<(), TerminalError> {
        let Some(state) = self.session_state(name).await? else { return Ok(()) };
        if state == SessionState::Running {
            let result = self.zellij(&["kill-session", name], None).await?;
            if !result.ok() {
                let detail = detail(&result);
                let lower = detail.to_lowercase();
                if !lower.contains("no session") && !lower.contains("not found") {
                    return Err(TerminalError(format!("Could not kill Zellij session {name}: {detail}")));
                }
            }
        }
        self.delete_session(name).await?; // also clears an exited session's saved layout
        for _ in 0..40 {
            if self.session_state(name).await?.is_none() {
                self.stop_sizing_client(name).await;
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        Err(TerminalError(format!("Zellij session {name} still exists after kill+delete")))
    }

    /// The hidden client exits on its own once its session is gone; make sure of it.
    async fn stop_sizing_client(&self, session: &str) {
        let client = self.sizing_clients.lock().unwrap().remove(session);
        if let Some(client) = client
            && !client.wait(Duration::from_secs(2)).await
        {
            client.terminate_tree().await;
        }
    }

    pub async fn close_tab(&self, session: &str, tab_id: i64) -> Result<(), TerminalError> {
        if tab_id < 0 {
            return Err(TerminalError("Cannot stop an agent without a known tab ID".into()));
        }
        let result = self.zellij(&["action", "close-tab-by-id", &tab_id.to_string()], Some(session)).await?;
        if !result.ok() {
            let output = if result.stderr.is_empty() { &result.stdout } else { &result.stderr };
            return Err(TerminalError(format!("Could not stop agent tab {tab_id}: {output}")));
        }
        for _ in 0..20 {
            if !self.session_exists(session).await? {
                return Ok(());
            }
            if !self.list_panes(session).await?.iter().any(|p| p.tab_id == tab_id) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(TerminalError(format!("Agent tab {tab_id} did not stop")))
    }

    // attaching -----------------------------------------------------------------------------------

    pub fn attach_command(&self, session: &str) -> String {
        format!("{} attach {}", self.config.command, shlex_quote(session))
    }

    pub fn web_url(&self, session: &str) -> Option<String> {
        self.config
            .web_client_enabled
            .then(|| format!("{}/{session}", self.config.web_client_base_url.trim_end_matches('/')))
    }
}

// parsing -----------------------------------------------------------------------------------------

/// `list-sessions --no-formatting`: one session per line, its name first (in listing order).
pub fn parse_sessions(stdout: &str) -> IndexMap<String, SessionState> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            let state =
                if line.to_uppercase().contains("(EXITED") { SessionState::Exited } else { SessionState::Running };
            Some((name.to_string(), state))
        })
        .collect()
}

/// `list-tabs`: a header line, then `<id> <position> <name>` rows.
pub fn parse_tab_id_by_name(stdout: &str, name: &str) -> i64 {
    stdout
        .lines()
        .skip(1)
        .filter_map(|line| {
            // Python's line.split(None, 2): runs of whitespace separate, the rest is the name.
            let (id, rest) = split_word(line)?;
            let (_position, rest) = split_word(rest)?;
            let tab = rest.trim();
            let numeric = !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());
            (numeric && !tab.is_empty() && tab == name).then(|| id.parse().ok())?
        })
        .max()
        .unwrap_or(-1)
}

/// The first whitespace-separated word of `text` and what follows it, or None when there is none.
fn split_word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Some((&text[..end], &text[end..]))
}

/// `list-panes --json --all`, without plugin panes.
pub fn parse_panes(stdout: &str) -> Result<Vec<PaneInfo>, TerminalError> {
    let raw: Value = serde_json::from_str(if stdout.is_empty() { "[]" } else { stdout })
        .map_err(|e| TerminalError(format!("list-panes returned invalid JSON: {e}")))?;
    let items = raw.as_array().cloned().unwrap_or_default();
    let mut panes = Vec::new();
    for item in items {
        if item.get("is_plugin").is_some_and(truthy) {
            continue;
        }
        let id =
            item.get("id").map(|v| v.as_i64().map_or_else(|| v.to_string(), |n| n.to_string())).unwrap_or_default();
        let command = ["pane_command", "terminal_command"]
            .into_iter()
            .filter_map(|key| item.get(key).and_then(Value::as_str))
            .find(|c| !c.is_empty())
            .map(String::from);
        panes.push(PaneInfo {
            pane_id: format!("terminal_{id}"),
            tab_id: item.get("tab_id").and_then(Value::as_i64).unwrap_or(-1),
            tab_name: item.get("tab_name").and_then(Value::as_str).unwrap_or_default().to_string(),
            exited: item.get("exited").is_some_and(truthy),
            exit_status: item.get("exit_status").and_then(Value::as_i64),
            command,
        });
    }
    Ok(panes)
}

/// One `subscribe --format json` line as an event, or None for anything else on that stream.
/// Python forwarded the raw line; the viewer reads only `event`, `viewport`, `scrollback`,
/// `is_initial` and `message`, which this keeps (rows given as one string are split into lines,
/// as the viewer did).
pub fn parse_pane_event(line: &str) -> Option<PaneEvent> {
    let event: Value = serde_json::from_str(line).ok()?;
    let rows = |value: Option<&Value>| -> Option<Vec<String>> {
        match value? {
            Value::Array(items) => {
                Some(items.iter().map(|i| i.as_str().map_or_else(|| i.to_string(), String::from)).collect())
            }
            Value::String(text) => Some(text.split('\n').map(String::from).collect()),
            _ => None,
        }
    };
    match event.get("event").and_then(Value::as_str)? {
        "pane_update" => Some(PaneEvent::Update {
            viewport: rows(event.get("viewport"))?,
            scrollback: rows(event.get("scrollback")),
            is_initial: event.get("is_initial").is_some_and(truthy),
        }),
        "pane_closed" => Some(PaneEvent::Closed),
        "error" => Some(PaneEvent::Error(event.get("message").and_then(Value::as_str).unwrap_or_default().to_string())),
        _ => None,
    }
}

/// Python's truthiness for a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `shlex.quote`.
pub fn shlex_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if text.chars().all(safe) {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}
