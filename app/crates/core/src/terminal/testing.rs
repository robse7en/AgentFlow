//! A recording, in-memory terminal backend for tests: what Python's engine tests did with mocks of
//! ZellijService. Sessions and panes live in memory; every call is recorded; a test can set a
//! pane's screen (and push it to open streams), mark it exited, or make the next calls fail.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use tokio::sync::mpsc;

use super::{Backend, PaneEvent, PaneInfo, SessionState, TerminalError, Terminals};

#[derive(Default)]
struct State {
    backend: Option<Backend>,
    sessions: HashMap<String, Vec<PaneInfo>>,
    screens: HashMap<String, Vec<String>>,
    next_tab: i64,
    calls: Vec<String>,
    /// Calls (by name, e.g. "kill_session") that fail with this message.
    failing: HashMap<String, String>,
    /// Sessions zellij lists as exited (saved layout, no server).
    exited: std::collections::HashSet<String>,
    /// Open pane streams, by pane id; they stay open until the fake is dropped.
    streams: HashMap<String, Vec<mpsc::Sender<PaneEvent>>>,
}

#[derive(Default)]
pub struct FakeTerminals {
    state: Mutex<State>,
}

impl FakeTerminals {
    /// Select the capabilities advertised by this recording backend.
    pub fn set_backend(&self, backend: Backend) {
        self.state().backend = Some(backend);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn call(&self, name: &str, detail: String) -> Result<(), TerminalError> {
        let mut state = self.state();
        state.calls.push(format!("{name} {detail}").trim_end().to_string());
        match state.failing.get(name) {
            Some(message) => Err(TerminalError(message.clone())),
            None => Ok(()),
        }
    }

    /// Every call so far, as "name args".
    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    pub fn fail(&self, call: &str, message: &str) {
        self.state().failing.insert(call.into(), message.into());
    }

    /// Let a call that [`FakeTerminals::fail`] broke succeed again.
    pub fn succeed(&self, call: &str) {
        self.state().failing.remove(call);
    }

    pub fn set_screen(&self, pane_id: &str, rows: &[&str]) {
        self.state().screens.insert(pane_id.into(), rows.iter().map(|r| r.to_string()).collect());
    }

    pub fn mark_exited(&self, pane_id: &str, status: i64) {
        for panes in self.state().sessions.values_mut() {
            for pane in panes.iter_mut().filter(|p| p.pane_id == pane_id) {
                pane.exited = true;
                pane.exit_status = Some(status);
            }
        }
    }

    /// Add a pane the backend didn't open itself (e.g. one a human opened).
    pub fn add_pane(&self, session: &str, pane: PaneInfo) {
        self.state().sessions.entry(session.into()).or_default().push(pane);
    }

    /// Forget a session, as if its server had gone.
    pub fn remove_session(&self, session: &str) {
        self.state().sessions.remove(session);
    }

    /// List a session as exited, as zellij does for one whose server has gone.
    pub fn set_exited(&self, session: &str) {
        let mut state = self.state();
        state.sessions.remove(session);
        state.exited.insert(session.into());
    }

    /// Show `rows` on a pane, sending them to its open streams.
    pub fn push_screen(&self, pane_id: &str, rows: &[&str]) {
        let rows: Vec<String> = rows.iter().map(|r| r.to_string()).collect();
        let mut state = self.state();
        state.screens.insert(pane_id.into(), rows.clone());
        for stream in state.streams.get(pane_id).into_iter().flatten() {
            let update = PaneEvent::Update { viewport: rows.clone(), scrollback: None, is_initial: false };
            let _ = stream.try_send(update);
        }
    }

    pub fn panes(&self, session: &str) -> Vec<PaneInfo> {
        self.state().sessions.get(session).cloned().unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl Terminals for FakeTerminals {
    fn backend(&self) -> Backend {
        self.state().backend.unwrap_or(Backend::Zellij)
    }

    async fn ensure_session(&self, name: &str) -> Result<bool, TerminalError> {
        self.call("ensure_session", name.into())?;
        let mut state = self.state();
        state.exited.remove(name);
        if state.sessions.contains_key(name) {
            return Ok(false);
        }
        state.sessions.insert(name.into(), Vec::new());
        Ok(true)
    }

    async fn session_state(&self, name: &str) -> Result<Option<SessionState>, TerminalError> {
        self.call("session_state", name.into())?;
        let state = self.state();
        if state.exited.contains(name) {
            return Ok(Some(SessionState::Exited));
        }
        Ok(state.sessions.contains_key(name).then_some(SessionState::Running))
    }

    async fn new_tab(
        &self,
        session: &str,
        name: &str,
        cwd: &Path,
        command: Option<&[String]>,
    ) -> Result<(i64, Option<String>), TerminalError> {
        self.call("new_tab", format!("{session} {name} {}", cwd.display()))?;
        let mut state = self.state();
        state.next_tab += 1;
        let tab = state.next_tab;
        let pane = PaneInfo {
            pane_id: format!("terminal_{tab}"),
            tab_id: tab,
            tab_name: name.into(),
            exited: false,
            exit_status: None,
            command: command.map(|c| c.join(" ")),
        };
        let panes = state
            .sessions
            .get_mut(session)
            .ok_or_else(|| TerminalError(format!("new-tab failed: no session named {session}")))?;
        panes.push(pane.clone());
        Ok((tab, Some(pane.pane_id)))
    }

    async fn list_panes(&self, session: &str) -> Result<Vec<PaneInfo>, TerminalError> {
        self.call("list_panes", session.into())?;
        self.state()
            .sessions
            .get(session)
            .cloned()
            .ok_or_else(|| TerminalError(format!("list-panes failed for {session}")))
    }

    async fn screen(&self, session: &str, pane_id: &str) -> Result<Vec<String>, TerminalError> {
        self.call("screen", format!("{session} {pane_id}"))?;
        Ok(self.state().screens.get(pane_id).cloned().unwrap_or_default())
    }

    async fn write_line(&self, session: &str, pane_id: &str, text: &str) -> Result<(), TerminalError> {
        self.call("write_line", format!("{session} {pane_id} {text}"))
    }

    async fn write_input(&self, session: &str, pane_id: &str, input: &[u8]) -> Result<(), TerminalError> {
        self.call("write_input", format!("{session} {pane_id} {}", String::from_utf8_lossy(input)))
    }

    fn subscribe(&self, session: &str, pane_id: &str) -> Result<mpsc::Receiver<PaneEvent>, TerminalError> {
        self.call("subscribe", format!("{session} {pane_id}"))?;
        let (sender, receiver) = mpsc::channel(8);
        let mut state = self.state();
        let rows = state.screens.get(pane_id).cloned().unwrap_or_default();
        let _ = sender.try_send(PaneEvent::Update { viewport: rows, scrollback: Some(Vec::new()), is_initial: true });
        state.streams.entry(pane_id.into()).or_default().push(sender);
        Ok(receiver)
    }

    async fn focus_tab(&self, session: &str, tab_name: &str) {
        let _ = self.call("focus_tab", format!("{session} {tab_name}"));
    }

    async fn close_tab(&self, session: &str, tab_id: i64) -> Result<(), TerminalError> {
        if tab_id < 0 {
            return Err(TerminalError("Cannot stop an agent without a known tab ID".into()));
        }
        self.call("close_tab", format!("{session} {tab_id}"))?;
        if let Some(panes) = self.state().sessions.get_mut(session) {
            panes.retain(|p| p.tab_id != tab_id);
        }
        Ok(())
    }

    async fn kill_session(&self, name: &str) -> Result<(), TerminalError> {
        self.call("kill_session", name.into())?;
        self.state().sessions.remove(name);
        Ok(())
    }

    async fn require_subscribe_support(&self) -> Result<(), TerminalError> {
        Ok(())
    }

    fn attach_command(&self, session: &str) -> Option<String> {
        Some(format!("zellij attach {session}"))
    }

    fn web_url(&self, _session: &str) -> Option<String> {
        None
    }

    async fn open_window(&self, session: &str, tab_name: Option<&str>) -> Result<(), TerminalError> {
        self.call("open_window", format!("{session} {}", tab_name.unwrap_or_default()))
    }

    fn shell_command(&self) -> Vec<String> {
        vec!["pwsh".into()]
    }
}
