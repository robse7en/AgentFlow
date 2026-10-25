//! Where agents run: terminal sessions with one tab (one pane) per agent.
//!
//! Two backends with the same operations, shaped after what the Python engine asks of
//! `services/zellij.py`, so the engine port can call either:
//!
//! - [`embedded`]: the default. Each pane is a ConPTY owned by the app, with a headless screen
//!   model for classification and the UI's terminal view. Panes end with the app (Job Object).
//! - [`zellij`] (opt-in): sessions in an external zellij server that outlive the app and
//!   can be attached from any terminal.
//!
//! A pane is identified by its id string, as stored in `agent_sessions.zellij_pane_id`: zellij's are
//! `terminal_<n>`, embedded ones `pty_<n>`, so a stored row always says which backend owns it
//! ([`Backend::owning`]). Embedded panes do not survive an app restart: after one, their session
//! is unknown ([`SessionState`] None), exactly like a zellij session that is gone, and the engine
//! treats the agent as no longer running.

pub mod embedded;
pub mod testing;
pub mod zellij;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TerminalError(pub String);

/// Zellij for [`crate::engine::Engine::leftover_zellij`] when agents run embedded, or None when it
/// isn't installed (then no task can have left a session there).
pub fn leftover_zellij(config: &crate::config::ZellijConfig) -> Option<std::sync::Arc<dyn Terminals>> {
    let zellij = zellij::ZellijService::new(config.clone());
    zellij.executable().is_some().then(|| std::sync::Arc::new(zellij) as std::sync::Arc<dyn Terminals>)
}

/// Which backend a stored pane id belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Embedded,
    Zellij,
}

impl Backend {
    pub fn owning(pane_id: &str) -> Option<Backend> {
        if pane_id.starts_with(embedded::PANE_PREFIX) {
            Some(Backend::Embedded)
        } else if pane_id.starts_with("terminal_") {
            Some(Backend::Zellij)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Running,
    /// zellij only: a saved layout with no server ("EXITED - attach to resurrect").
    Exited,
}

/// One pane, as zellij's `list-panes --json` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub pane_id: String,
    pub tab_id: i64,
    pub tab_name: String,
    /// The pane's command has ended; its last screen is still shown until the tab is closed.
    pub exited: bool,
    pub exit_status: Option<i64>,
    pub command: Option<String>,
}

/// What a pane stream delivers: the rendered screen as rows of plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    Update { viewport: Vec<String>, scrollback: Option<Vec<String>>, is_initial: bool },
    Closed,
    Error(String),
}

impl PaneEvent {
    /// The NDJSON line `GET /api/tasks/{key}/terminal/{role}` sends, in the shape zellij's
    /// `subscribe --format json` uses (and `ZellijViewer.vue` reads).
    pub fn to_ndjson(&self) -> String {
        let value = match self {
            PaneEvent::Update { viewport, scrollback, is_initial } => {
                let mut event = json!({"event": "pane_update", "viewport": viewport, "is_initial": is_initial});
                if let Some(scrollback) = scrollback {
                    event["scrollback"] = json!(scrollback);
                }
                event
            }
            PaneEvent::Closed => json!({"event": "pane_closed"}),
            PaneEvent::Error(message) => json!({"event": "error", "message": message}),
        };
        format!("{}\n", Value::to_string(&value))
    }
}

/// The operations the engine needs from a terminal backend (Python's ZellijService surface).
#[async_trait::async_trait]
pub trait Terminals: Send + Sync {
    fn backend(&self) -> Backend;

    /// Create the session if needed. Returns true when newly created.
    async fn ensure_session(&self, name: &str) -> Result<bool, TerminalError>;

    async fn session_state(&self, name: &str) -> Result<Option<SessionState>, TerminalError>;

    /// Whether the session is running, i.e. its tabs and panes can be queried.
    async fn session_exists(&self, name: &str) -> Result<bool, TerminalError> {
        Ok(self.session_state(name).await? == Some(SessionState::Running))
    }

    /// Open a tab running `command` (the shell when None). Returns (tab_id, pane_id).
    async fn new_tab(
        &self,
        session: &str,
        name: &str,
        cwd: &std::path::Path,
        command: Option<&[String]>,
    ) -> Result<(i64, Option<String>), TerminalError>;

    async fn list_panes(&self, session: &str) -> Result<Vec<PaneInfo>, TerminalError>;

    async fn pane_for_tab(&self, session: &str, tab_id: i64) -> Option<String> {
        self.list_panes(session).await.ok()?.into_iter().find(|p| p.tab_id == tab_id).map(|p| p.pane_id)
    }

    async fn pane_state(&self, session: &str, pane_id: &str) -> Result<Option<PaneInfo>, TerminalError> {
        Ok(self.list_panes(session).await?.into_iter().find(|p| p.pane_id == pane_id))
    }

    /// The pane's current screen, one string per row.
    async fn screen(&self, session: &str, pane_id: &str) -> Result<Vec<String>, TerminalError>;

    /// Type a single line into a pane and press Enter.
    async fn write_line(&self, session: &str, pane_id: &str, text: &str) -> Result<(), TerminalError>;

    /// Send explicit human input to an embedded pane without appending Enter.
    async fn write_input(&self, _session: &str, _pane_id: &str, _input: &[u8]) -> Result<(), TerminalError> {
        Err(TerminalError("Use the external terminal window for this backend".into()))
    }

    /// Stream the pane's rendered screen. Needs a tokio runtime.
    fn subscribe(&self, session: &str, pane_id: &str) -> Result<tokio::sync::mpsc::Receiver<PaneEvent>, TerminalError>;

    async fn focus_tab(&self, session: &str, tab_name: &str);

    async fn close_tab(&self, session: &str, tab_id: i64) -> Result<(), TerminalError>;

    /// Kill and forget a whole session; a session that doesn't exist is fine.
    async fn kill_session(&self, name: &str) -> Result<(), TerminalError>;

    async fn require_subscribe_support(&self) -> Result<(), TerminalError>;

    /// How a human attaches from their own terminal (zellij only).
    fn attach_command(&self, session: &str) -> Option<String>;

    fn web_url(&self, session: &str) -> Option<String>;

    /// Open an interactive window on the session (zellij on Windows only).
    async fn open_window(&self, session: &str, tab_name: Option<&str>) -> Result<(), TerminalError>;

    /// Shell that runs launcher scripts in tabs.
    fn shell_command(&self) -> Vec<String>;
}

#[async_trait::async_trait]
impl Terminals for zellij::ZellijService {
    fn backend(&self) -> Backend {
        Backend::Zellij
    }
    async fn ensure_session(&self, name: &str) -> Result<bool, TerminalError> {
        zellij::ZellijService::ensure_session(self, name).await
    }
    async fn session_state(&self, name: &str) -> Result<Option<SessionState>, TerminalError> {
        zellij::ZellijService::session_state(self, name).await
    }
    async fn new_tab(
        &self,
        session: &str,
        name: &str,
        cwd: &std::path::Path,
        command: Option<&[String]>,
    ) -> Result<(i64, Option<String>), TerminalError> {
        zellij::ZellijService::new_tab(self, session, name, cwd, command).await
    }
    async fn list_panes(&self, session: &str) -> Result<Vec<PaneInfo>, TerminalError> {
        zellij::ZellijService::list_panes(self, session).await
    }
    async fn screen(&self, session: &str, pane_id: &str) -> Result<Vec<String>, TerminalError> {
        zellij::ZellijService::screen(self, session, pane_id).await
    }
    async fn write_line(&self, session: &str, pane_id: &str, text: &str) -> Result<(), TerminalError> {
        zellij::ZellijService::write_line(self, session, pane_id, text).await
    }
    fn subscribe(&self, session: &str, pane_id: &str) -> Result<tokio::sync::mpsc::Receiver<PaneEvent>, TerminalError> {
        Ok(zellij::ZellijService::subscribe(self, session, pane_id))
    }
    async fn focus_tab(&self, session: &str, tab_name: &str) {
        zellij::ZellijService::focus_tab(self, session, tab_name).await
    }
    async fn close_tab(&self, session: &str, tab_id: i64) -> Result<(), TerminalError> {
        zellij::ZellijService::close_tab(self, session, tab_id).await
    }
    async fn kill_session(&self, name: &str) -> Result<(), TerminalError> {
        zellij::ZellijService::kill_session(self, name).await
    }
    async fn require_subscribe_support(&self) -> Result<(), TerminalError> {
        zellij::ZellijService::require_subscribe_support(self).await
    }
    fn attach_command(&self, session: &str) -> Option<String> {
        Some(zellij::ZellijService::attach_command(self, session))
    }
    fn web_url(&self, session: &str) -> Option<String> {
        zellij::ZellijService::web_url(self, session)
    }
    async fn open_window(&self, session: &str, tab_name: Option<&str>) -> Result<(), TerminalError> {
        zellij::ZellijService::open_window(self, session, tab_name).await
    }
    fn shell_command(&self) -> Vec<String> {
        shell_command(&self.config)
    }
}

#[async_trait::async_trait]
impl Terminals for embedded::EmbeddedTerminals {
    fn backend(&self) -> Backend {
        Backend::Embedded
    }
    async fn ensure_session(&self, name: &str) -> Result<bool, TerminalError> {
        Ok(embedded::EmbeddedTerminals::ensure_session(self, name))
    }
    async fn session_state(&self, name: &str) -> Result<Option<SessionState>, TerminalError> {
        Ok(embedded::EmbeddedTerminals::session_state(self, name))
    }
    async fn new_tab(
        &self,
        session: &str,
        name: &str,
        cwd: &std::path::Path,
        command: Option<&[String]>,
    ) -> Result<(i64, Option<String>), TerminalError> {
        embedded::EmbeddedTerminals::new_tab(self, session, name, cwd, command)
    }
    async fn list_panes(&self, session: &str) -> Result<Vec<PaneInfo>, TerminalError> {
        embedded::EmbeddedTerminals::list_panes(self, session)
    }
    async fn screen(&self, session: &str, pane_id: &str) -> Result<Vec<String>, TerminalError> {
        embedded::EmbeddedTerminals::screen(self, session, pane_id)
    }
    async fn write_line(&self, session: &str, pane_id: &str, text: &str) -> Result<(), TerminalError> {
        embedded::EmbeddedTerminals::write_line(self, session, pane_id, text).await
    }
    async fn write_input(&self, session: &str, pane_id: &str, input: &[u8]) -> Result<(), TerminalError> {
        embedded::EmbeddedTerminals::write_input(self, session, pane_id, input)
    }
    fn subscribe(&self, session: &str, pane_id: &str) -> Result<tokio::sync::mpsc::Receiver<PaneEvent>, TerminalError> {
        embedded::EmbeddedTerminals::subscribe(self, session, pane_id)
    }
    async fn focus_tab(&self, _session: &str, _tab_name: &str) {}
    async fn close_tab(&self, session: &str, tab_id: i64) -> Result<(), TerminalError> {
        embedded::EmbeddedTerminals::close_tab(self, session, tab_id)
    }
    async fn kill_session(&self, name: &str) -> Result<(), TerminalError> {
        embedded::EmbeddedTerminals::kill_session(self, name);
        Ok(())
    }
    async fn require_subscribe_support(&self) -> Result<(), TerminalError> {
        Ok(())
    }
    fn attach_command(&self, _session: &str) -> Option<String> {
        None
    }
    fn web_url(&self, _session: &str) -> Option<String> {
        None
    }
    async fn open_window(&self, _session: &str, _tab_name: Option<&str>) -> Result<(), TerminalError> {
        Err(TerminalError("Embedded terminals open in the app's terminal panel".into()))
    }
    fn shell_command(&self) -> Vec<String> {
        embedded::EmbeddedTerminals::shell(self).to_vec()
    }
}

/// Shell that runs generated launcher scripts inside tabs (and a tab opened without a command):
/// `zellij.shell` when configured, else PowerShell 7 or Windows PowerShell, or `sh`.
pub fn shell_command(config: &crate::config::ZellijConfig) -> Vec<String> {
    if let Some(shell) = config.shell.as_ref().filter(|s| !s.is_empty()) {
        return vec![shell.clone()];
    }
    if cfg!(windows) {
        let found = ["pwsh", "powershell"].into_iter().find(|c| crate::process::resolve_executable(c).is_some());
        return vec![found.unwrap_or("powershell").to_string()];
    }
    vec!["sh".to_string()]
}

/// Highest tab id carrying `name` (tabs are created in id order), or -1.
pub fn tab_id_by_name(panes: &[PaneInfo], name: &str) -> i64 {
    panes.iter().filter(|p| p.tab_name == name).map(|p| p.tab_id).max().unwrap_or(-1)
}
