//! Embedded terminals: every pane is a pseudoconsole (ConPTY on Windows) owned by the app.
//!
//! Replaces zellij as the default backend (docs/desktop-app-investigation.md §5). Each pane has:
//!
//! - a headless screen model (`vt100`) the engine classifies and the read-only viewer shows,
//! - a bounded buffer of raw output, replayed to a UI terminal (xterm.js) that attaches later,
//! - on Windows, a kill-on-close Job Object holding the pane's process tree, so closing the tab or
//!   the app (however it exits) leaves no agent, MCP server or shell behind.
//!
//! Lifecycle: a reader thread drains the pseudoconsole's output for as long as it is open, and
//! answers the terminal queries a headless pane must answer itself. A waiter thread notices the
//! child's exit, marks the pane exited and closes the pseudoconsole; the pane keeps its last screen
//! (like a zellij pane whose command ended) until its tab is closed. The threads never own the PTY
//! handles: every path that ends a pane closes them explicitly, so no ownership cycle keeps a
//! pseudoconsole open.
//!
//! Before macOS/Linux: there is no Job Object there, so closing a pane kills only its direct child;
//! the pane would need its own process group (setsid) to be killed as a tree.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use indexmap::IndexMap;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{broadcast, mpsc, watch};

use super::{PaneEvent, PaneInfo, SessionState, TerminalError};
use crate::config::ZellijConfig;
#[cfg(windows)]
use crate::process::job::Job;

/// Embedded pane ids are `pty_<n>`; zellij's are `terminal_<n>` (see [`super::Backend`]).
pub const PANE_PREFIX: &str = "pty_";

/// Raw output chunks as the pane writes them, for an attached UI terminal.
pub type RawOutput = broadcast::Receiver<Arc<[u8]>>;

/// Raw output kept per pane for a UI terminal attaching later.
const RAW_BUFFER_BYTES: usize = 2 * 1024 * 1024;
/// Rows the screen model keeps above the visible screen.
const SCROLLBACK_ROWS: usize = 1000;
/// Rows of scrollback in a stream's first event, as `zellij subscribe --scrollback 100` sends.
const STREAM_SCROLLBACK_ROWS: usize = 100;
/// A stream sends at most one frame per interval; a spinner redrawing faster is coalesced.
const FRAME_INTERVAL: Duration = Duration::from_millis(50);
/// Between typing a line and pressing Enter. zellij sent them as two commands (`write-chars`, then
/// `write 13`); a TUI can read text and Enter arriving in one chunk as a paste and not submit it.
const SUBMIT_DELAY: Duration = Duration::from_millis(100);
/// After the child exits, how long its last output may still be arriving before the pseudoconsole
/// is closed.
const EXIT_SETTLE: Duration = Duration::from_millis(150);

pub struct EmbeddedTerminals {
    /// Session name -> its panes in tab order.
    sessions: Mutex<IndexMap<String, Vec<Arc<Pane>>>>,
    next_pane: AtomicI64,
    rows: u16,
    cols: u16,
    shell: Vec<String>,
}

impl EmbeddedTerminals {
    /// Panes of `rows` x `cols`; a tab opened without a command runs `shell`.
    pub fn new(rows: u16, cols: u16, shell: Vec<String>) -> Self {
        EmbeddedTerminals { sessions: Mutex::new(IndexMap::new()), next_pane: AtomicI64::new(1), rows, cols, shell }
    }

    /// Sized like zellij's headless client, with zellij's shell choice.
    pub fn from_config(config: &ZellijConfig) -> Self {
        let clamp = |value: i64| value.clamp(10, 1000) as u16;
        Self::new(clamp(config.headless_rows), clamp(config.headless_cols), super::shell_command(config))
    }

    /// The shell a tab opened without a command runs.
    pub fn shell(&self) -> &[String] {
        &self.shell
    }

    fn sessions(&self) -> MutexGuard<'_, IndexMap<String, Vec<Arc<Pane>>>> {
        self.sessions.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // sessions ------------------------------------------------------------------------------------

    /// Create the session if needed. Returns true when newly created.
    pub fn ensure_session(&self, name: &str) -> bool {
        let mut sessions = self.sessions();
        if sessions.contains_key(name) {
            return false;
        }
        sessions.insert(name.to_string(), Vec::new());
        true
    }

    /// Running, or None: embedded sessions never outlive the app, so there is no exited state.
    pub fn session_state(&self, name: &str) -> Option<SessionState> {
        self.sessions().contains_key(name).then_some(SessionState::Running)
    }

    pub fn session_exists(&self, name: &str) -> bool {
        self.session_state(name).is_some()
    }

    pub fn list_sessions(&self) -> Vec<String> {
        self.sessions().keys().cloned().collect()
    }

    /// Close every pane of the session and forget it. A session that doesn't exist is fine.
    pub fn kill_session(&self, name: &str) {
        let panes = self.sessions().shift_remove(name).unwrap_or_default();
        for pane in panes {
            pane.close();
        }
    }

    // tabs and panes ------------------------------------------------------------------------------

    /// Open a tab running `command` (the shell when None) in `cwd`. Returns (tab_id, pane_id).
    pub fn new_tab(
        &self,
        session: &str,
        name: &str,
        cwd: &Path,
        command: Option<&[String]>,
    ) -> Result<(i64, Option<String>), TerminalError> {
        if !self.session_exists(session) {
            return Err(TerminalError(format!("new-tab failed: no session named {session}")));
        }
        let argv = command.map(<[String]>::to_vec).unwrap_or_else(|| self.shell.clone());
        let number = self.next_pane.fetch_add(1, Ordering::Relaxed);
        let pane = Pane::spawn(number, name, cwd, &argv, self.rows, self.cols)?;
        let mut sessions = self.sessions();
        let Some(panes) = sessions.get_mut(session) else {
            // The session was killed while the pane started.
            pane.close();
            return Err(TerminalError(format!("new-tab failed: session {session} was closed")));
        };
        panes.push(pane.clone());
        Ok((number, Some(pane.id())))
    }

    pub fn list_panes(&self, session: &str) -> Result<Vec<PaneInfo>, TerminalError> {
        let sessions = self.sessions();
        let panes = sessions
            .get(session)
            .ok_or_else(|| TerminalError(format!("list-panes failed for {session}: no such session")))?;
        Ok(panes.iter().map(|pane| pane.info()).collect())
    }

    pub fn pane_for_tab(&self, session: &str, tab_id: i64) -> Option<String> {
        self.list_panes(session).ok()?.into_iter().find(|p| p.tab_id == tab_id).map(|p| p.pane_id)
    }

    pub fn pane_state(&self, session: &str, pane_id: &str) -> Result<Option<PaneInfo>, TerminalError> {
        Ok(self.list_panes(session)?.into_iter().find(|p| p.pane_id == pane_id))
    }

    /// Close a tab: its process tree is killed and its pane forgotten. An unknown tab is already
    /// closed.
    pub fn close_tab(&self, session: &str, tab_id: i64) -> Result<(), TerminalError> {
        if tab_id < 0 {
            return Err(TerminalError("Cannot stop an agent without a known tab ID".into()));
        }
        let pane = {
            let mut sessions = self.sessions();
            let Some(panes) = sessions.get_mut(session) else { return Ok(()) };
            let Some(index) = panes.iter().position(|p| p.number == tab_id) else { return Ok(()) };
            panes.remove(index)
        };
        pane.close();
        Ok(())
    }

    fn pane(&self, session: &str, pane_id: &str) -> Result<Arc<Pane>, TerminalError> {
        let sessions = self.sessions();
        sessions
            .get(session)
            .and_then(|panes| panes.iter().find(|p| p.id() == pane_id))
            .cloned()
            .ok_or_else(|| TerminalError(format!("No pane {pane_id} in session {session}")))
    }

    // screen and input ----------------------------------------------------------------------------

    /// The pane's current screen, one string per row.
    pub fn screen(&self, session: &str, pane_id: &str) -> Result<Vec<String>, TerminalError> {
        Ok(self.pane(session, pane_id)?.viewport())
    }

    /// Type a single line into a pane and press Enter.
    pub async fn write_line(&self, session: &str, pane_id: &str, text: &str) -> Result<(), TerminalError> {
        let pane = self.pane(session, pane_id)?;
        pane.write(text.as_bytes())?;
        tokio::time::sleep(SUBMIT_DELAY).await;
        pane.write(b"\r")
    }

    /// Keystrokes from an attached UI terminal. Cursor position reports are dropped: the pane
    /// answers those queries itself, and a second answer would arrive as typed text.
    pub fn write_input(&self, session: &str, pane_id: &str, input: &[u8]) -> Result<(), TerminalError> {
        let input = strip_cursor_reports(input);
        if input.is_empty() {
            return Ok(());
        }
        self.pane(session, pane_id)?.write(&input)
    }

    pub fn resize(&self, session: &str, pane_id: &str, rows: u16, cols: u16) -> Result<(), TerminalError> {
        self.pane(session, pane_id)?.resize(rows.max(1), cols.max(1))
    }

    /// The pane's buffered raw output and a receiver for what follows, with nothing lost or
    /// repeated in between: for a UI terminal attaching to a running pane. A receiver that falls
    /// too far behind gets `Lagged` and should attach again.
    pub fn attach(&self, session: &str, pane_id: &str) -> Result<(Vec<u8>, RawOutput), TerminalError> {
        let pane = self.pane(session, pane_id)?;
        let screen = pane.lock_screen();
        let replay: Vec<u8> = screen.raw.iter().copied().collect();
        let receiver = pane.raw.subscribe();
        drop(screen);
        Ok((replay, receiver))
    }

    /// Stream the pane's rendered screen: a first event with scrollback, then a frame whenever the
    /// visible screen changes (at most one per [`FRAME_INTERVAL`]), then `Closed` when the tab is
    /// closed. The stream's task ends when the receiver is dropped. Needs a tokio runtime.
    pub fn subscribe(&self, session: &str, pane_id: &str) -> Result<mpsc::Receiver<PaneEvent>, TerminalError> {
        let pane = self.pane(session, pane_id)?;
        let (sender, receiver) = mpsc::channel(8);
        let mut changes = pane.changed.subscribe();
        tokio::spawn(async move {
            let (mut last, scrollback) = pane.snapshot();
            let first = PaneEvent::Update { viewport: last.clone(), scrollback: Some(scrollback), is_initial: true };
            if sender.send(first).await.is_err() {
                return;
            }
            loop {
                if pane.closed.load(Ordering::SeqCst) {
                    let _ = sender.send(PaneEvent::Closed).await;
                    return;
                }
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() {
                        let _ = sender.send(PaneEvent::Closed).await;
                        return;
                    },
                    () = sender.closed() => return,
                }
                tokio::time::sleep(FRAME_INTERVAL).await;
                let viewport = pane.viewport();
                if viewport != last && !pane.closed.load(Ordering::SeqCst) {
                    last = viewport.clone();
                    let frame = PaneEvent::Update { viewport, scrollback: None, is_initial: false };
                    if sender.send(frame).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok(receiver)
    }
}

impl Drop for EmbeddedTerminals {
    /// Agents never outlive the terminals that host them.
    fn drop(&mut self) {
        let sessions = std::mem::take(&mut *self.sessions());
        for pane in sessions.into_values().flatten() {
            pane.close();
        }
    }
}

/// One tab's pane.
struct Pane {
    number: i64,
    name: String,
    command: String,
    screen: Mutex<Screen>,
    /// Bumped on every change to `screen`; streams wait on it.
    changed: watch::Sender<u64>,
    raw: broadcast::Sender<Arc<[u8]>>,
    /// None once the pseudoconsole is closed.
    io: Mutex<Option<Io>>,
    closed: AtomicBool,
    #[cfg(windows)]
    job: Job,
}

struct Screen {
    parser: vt100::Parser<Replies>,
    raw: VecDeque<u8>,
    exited: bool,
    exit_status: Option<i64>,
    generation: u64,
}

struct Io {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

/// Answers to terminal queries, collected while parsing output and written back after.
#[derive(Default)]
struct Replies {
    pending: Vec<u8>,
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        // Device status report 6 (cursor position). ConPTY sends one at startup (portable-pty
        // creates the pseudoconsole with INHERIT_CURSOR) and holds back all output until it is
        // answered; CLIs ask too. A headless pane has no terminal to answer for it.
        if c == 'n' && i1.is_none() && params == [&[6u16][..]] {
            let (row, col) = screen.cursor_position();
            self.pending.extend(format!("\x1b[{};{}R", row + 1, col + 1).bytes());
        }
    }
}

impl Pane {
    fn spawn(
        number: i64,
        name: &str,
        cwd: &Path,
        argv: &[String],
        rows: u16,
        cols: u16,
    ) -> Result<Arc<Pane>, TerminalError> {
        let failed = |what: &str, error: &dyn std::fmt::Display| {
            TerminalError(format!("Could not start {}: {what}: {error}", crate::process::describe(argv)))
        };
        if argv.is_empty() {
            return Err(TerminalError("new-tab failed: empty command".into()));
        }
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| failed("pseudoconsole", &e))?;
        let mut command = CommandBuilder::from_argv(argv.iter().map(OsString::from).collect());
        command.cwd(cwd);
        command.env_clear();
        for (key, value) in crate::process::clean_env(&HashMap::new()) {
            command.env(key, value);
        }
        // Prepare every fallible PTY resource, and the required process-tree owner, before
        // starting the command. A failed setup must never leave a live, untracked child.
        let reader = pair.master.try_clone_reader().map_err(|e| failed("output", &e))?;
        let writer = pair.master.take_writer().map_err(|e| failed("input", &e))?;
        #[cfg(windows)]
        let job = Job::kill_on_close().map_err(|e| failed("process job", &e))?;
        let mut child = pair.slave.spawn_command(command).map_err(|e| failed("spawn", &e))?;
        drop(pair.slave);
        #[cfg(windows)]
        {
            // portable-pty starts a running process, so assignment cannot be atomic with spawn.
            // Never accept an uncontained pane if assignment fails (including a missing PID).
            let pid = child.process_id();
            assign_job(&job, child.as_mut(), pid).map_err(|e| failed("assign process job", &e))?;
        }
        let killer = child.clone_killer();
        let pane = Arc::new(Pane {
            number,
            name: name.to_string(),
            command: argv.join(" "),
            screen: Mutex::new(Screen {
                parser: vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK_ROWS, Replies::default()),
                raw: VecDeque::new(),
                exited: false,
                exit_status: None,
                generation: 0,
            }),
            changed: watch::Sender::new(0),
            raw: broadcast::Sender::new(256),
            io: Mutex::new(Some(Io { master: pair.master, writer, killer })),
            closed: AtomicBool::new(false),
            #[cfg(windows)]
            job,
        });

        let output = pane.clone();
        std::thread::Builder::new().name(format!("pty-output-{number}")).spawn(move || output.drain(reader)).map_err(
            |e| {
                pane.close();
                let _ = child.wait();
                failed("output thread", &e)
            },
        )?;
        let waiter = pane.clone();
        std::thread::Builder::new()
            .name(format!("pty-wait-{number}"))
            .spawn(move || {
                let status = child.wait();
                std::thread::sleep(EXIT_SETTLE);
                waiter.mark_exited(status.ok().map(|s| i64::from(s.exit_code())));
                // The output thread is still draining, so closing cannot block on a full pipe.
                waiter.close_io();
            })
            .map_err(|e| {
                pane.close();
                failed("wait thread", &e)
            })?;
        Ok(pane)
    }

    fn id(&self) -> String {
        format!("{PANE_PREFIX}{}", self.number)
    }

    fn lock_screen(&self) -> MutexGuard<'_, Screen> {
        self.screen.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_io(&self) -> MutexGuard<'_, Option<Io>> {
        self.io.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn info(&self) -> PaneInfo {
        let screen = self.lock_screen();
        PaneInfo {
            pane_id: self.id(),
            tab_id: self.number,
            tab_name: self.name.clone(),
            exited: screen.exited,
            exit_status: screen.exit_status,
            command: Some(self.command.clone()),
        }
    }

    /// Output thread: until the pseudoconsole closes.
    fn drain(&self, mut reader: Box<dyn Read + Send>) {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(n) => self.feed(&buffer[..n]),
            }
        }
    }

    fn feed(&self, bytes: &[u8]) {
        let (replies, generation) = {
            let mut screen = self.lock_screen();
            screen.parser.process(bytes);
            let replies = std::mem::take(&mut screen.parser.callbacks_mut().pending);
            screen.raw.extend(bytes);
            trim_raw(&mut screen.raw);
            screen.generation += 1;
            // Sent under the lock, so attach() sees each chunk either in its replay or its receiver.
            let _ = self.raw.send(Arc::from(bytes));
            (replies, screen.generation)
        };
        self.changed.send_replace(generation);
        if !replies.is_empty() {
            let _ = self.write(&replies);
        }
    }

    fn mark_exited(&self, status: Option<i64>) {
        let generation = {
            let mut screen = self.lock_screen();
            screen.exited = true;
            screen.exit_status = status;
            screen.generation += 1;
            screen.generation
        };
        self.changed.send_replace(generation);
    }

    /// Close the pseudoconsole. Blocks until its console host exits, so callers on an async thread
    /// use [`Pane::close`].
    fn close_io(&self) {
        let io = self.lock_io().take();
        drop(io);
    }

    /// End the pane for good: kill its process tree and close the pseudoconsole (off this thread).
    fn close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        #[cfg(windows)]
        self.job.terminate(1);
        if let Some(mut io) = self.lock_io().take() {
            let _ = io.killer.kill();
            let _ = std::thread::Builder::new().name(format!("pty-close-{}", self.number)).spawn(move || drop(io));
        }
        let generation = {
            let mut screen = self.lock_screen();
            screen.generation += 1;
            screen.generation
        };
        self.changed.send_replace(generation);
    }

    fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        let mut io = self.lock_io();
        let io = io.as_mut().ok_or_else(|| TerminalError(format!("Pane {} has exited", self.id())))?;
        io.writer
            .write_all(bytes)
            .and_then(|()| io.writer.flush())
            .map_err(|e| TerminalError(format!("write failed: {e}")))
    }

    fn resize(&self, rows: u16, cols: u16) -> Result<(), TerminalError> {
        {
            let io = self.lock_io();
            let io = io.as_ref().ok_or_else(|| TerminalError(format!("Pane {} has exited", self.id())))?;
            io.master
                .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
                .map_err(|e| TerminalError(format!("resize failed: {e}")))?;
        }
        // The screen model follows the pseudoconsole, so classification reads what the CLI drew.
        self.lock_screen().parser.screen_mut().set_size(rows, cols);
        Ok(())
    }

    fn viewport(&self) -> Vec<String> {
        let screen = self.lock_screen();
        let (_, cols) = screen.parser.screen().size();
        screen.parser.screen().rows(0, cols).collect()
    }

    /// The visible rows, and up to [`STREAM_SCROLLBACK_ROWS`] rows above them (oldest first).
    fn snapshot(&self) -> (Vec<String>, Vec<String>) {
        let mut screen = self.lock_screen();
        let view = screen.parser.screen_mut();
        let (_, cols) = view.size();
        view.set_scrollback(STREAM_SCROLLBACK_ROWS);
        let available = view.scrollback();
        // At offset k, the top visible row is the k-th scrollback row from the bottom.
        let scrollback = (1..=available)
            .rev()
            .map(|offset| {
                view.set_scrollback(offset);
                view.rows(0, cols).next().unwrap_or_default()
            })
            .collect();
        view.set_scrollback(0);
        (view.rows(0, cols).collect(), scrollback)
    }
}

/// A missing PID or rejected assignment must stop and reap the newly spawned child.
#[cfg(windows)]
fn assign_job(job: &Job, child: &mut dyn portable_pty::Child, pid: Option<u32>) -> Result<(), TerminalError> {
    let assigned = pid
        .ok_or_else(|| TerminalError("Spawned terminal has no process ID".into()))
        .and_then(|pid| job.assign(pid).map_err(|e| TerminalError(e.to_string())));
    if assigned.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assigned
}

/// Keep the raw buffer bounded, cutting at a line start so a replay doesn't begin mid-sequence.
fn trim_raw(raw: &mut VecDeque<u8>) {
    if raw.len() <= RAW_BUFFER_BYTES {
        return;
    }
    raw.drain(..raw.len() - RAW_BUFFER_BYTES);
    if let Some(newline) = raw.iter().take(64 * 1024).position(|&b| b == b'\n') {
        raw.drain(..=newline);
    }
}

/// `input` without cursor position reports (`ESC [ <row> ; <col> R`).
fn strip_cursor_reports(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == 0x1b
            && let Some(len) = cursor_report_len(&input[i..])
        {
            i += len;
            continue;
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

fn cursor_report_len(bytes: &[u8]) -> Option<usize> {
    let rest = bytes.strip_prefix(b"\x1b[")?;
    let row = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    let rest_after_row = rest.get(row..)?;
    if row == 0 || rest_after_row.first() != Some(&b';') {
        return None;
    }
    let col = rest_after_row[1..].iter().take_while(|b| b.is_ascii_digit()).count();
    if col == 0 || rest_after_row.get(1 + col) != Some(&b'R') {
        return None;
    }
    Some(2 + row + 1 + col + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn failed_job_assignment_stops_the_child() {
        for pid in [None, Some(u32::MAX)] {
            let pair =
                native_pty_system().openpty(PtySize { rows: 10, cols: 40, pixel_width: 0, pixel_height: 0 }).unwrap();
            let mut command = CommandBuilder::new("cmd");
            command.args(["/q", "/k"]);
            let mut child = pair.slave.spawn_command(command).unwrap();
            let job = Job::kill_on_close().unwrap();
            assert!(assign_job(&job, child.as_mut(), pid).is_err());
            assert!(child.try_wait().unwrap().is_some(), "failed assignment left a child running");
        }
    }

    #[test]
    fn strips_only_cursor_reports() {
        assert_eq!(strip_cursor_reports(b"a\x1b[12;40Rb"), b"ab");
        assert_eq!(strip_cursor_reports(b"\x1b[1;1R\x1b[2;3R"), b"");
        // Arrow keys and other sequences pass through.
        assert_eq!(strip_cursor_reports(b"\x1b[A\x1b[1;5A\x1b[R"), b"\x1b[A\x1b[1;5A\x1b[R");
        assert_eq!(strip_cursor_reports(b"\x1b[12;R"), b"\x1b[12;R");
    }

    #[test]
    fn answers_cursor_position_queries() {
        let mut parser = vt100::Parser::new_with_callbacks(5, 20, 0, Replies::default());
        parser.process(b"ab\r\ncd\x1b[6n");
        assert_eq!(parser.callbacks().pending, b"\x1b[2;3R");
        parser.callbacks_mut().pending.clear();
        parser.process(b"\x1b[5n\x1b[?6n");
        assert!(parser.callbacks().pending.is_empty());
    }

    #[test]
    fn raw_buffer_is_trimmed_at_a_line_start() {
        // 100-byte lines, enough that the cut lands mid-line.
        let line: Vec<u8> = std::iter::repeat_n(b'x', 99).chain(*b"\n").collect();
        let mut raw: VecDeque<u8> = VecDeque::new();
        while raw.len() <= RAW_BUFFER_BYTES + 50 {
            raw.extend(&line);
        }
        raw.extend(b"partial");
        trim_raw(&mut raw);
        assert!(raw.len() <= RAW_BUFFER_BYTES);
        assert_eq!((raw.len() - "partial".len()) % 100, 0, "starts at a line start");
        assert_eq!(raw.front(), Some(&b'x'));
    }
}
