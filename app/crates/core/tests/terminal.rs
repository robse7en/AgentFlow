//! Embedded terminals against real pseudoconsoles: lifecycles, screens, input and process cleanup.
#![cfg(windows)]

use std::path::Path;
use std::time::{Duration, Instant};

use agentflow_core::terminal::embedded::EmbeddedTerminals;
use agentflow_core::terminal::{Backend, PaneEvent, SessionState};

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn terminals() -> EmbeddedTerminals {
    EmbeddedTerminals::new(30, 100, argv(&["cmd", "/q"]))
}

/// Poll `check` until it returns Some, or panic naming `what`.
fn wait_for<T>(what: &str, timeout: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn screen_text(terminals: &EmbeddedTerminals, session: &str, pane: &str) -> String {
    terminals.screen(session, pane).map(|rows| rows.join("\n")).unwrap_or_default()
}

/// Running pings whose command line contains `marker` (via CIM, which reports command lines).
fn processes_with(marker: &str) -> Vec<String> {
    let query = format!(
        "Get-CimInstance Win32_Process | Where-Object {{ $_.Name -eq 'PING.EXE' -and $_.CommandLine -like '*{marker}*' }} | ForEach-Object {{ $_.ProcessId }}"
    );
    let output = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &query]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// A ping that runs for a minute, findable by its unique `-w` timeout.
fn marker_ping(marker: u32) -> String {
    format!("ping -n 60 -w {marker} 127.0.0.1")
}

fn unique_marker() -> u32 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
    40_000 + (std::process::id() % 1_000) * 10 + nanos % 9_973
}

#[test]
fn a_finished_command_leaves_its_screen_and_exit_code() {
    let terminals = terminals();
    assert!(terminals.ensure_session("t1"));
    assert!(!terminals.ensure_session("t1"));
    let (tab, pane) = terminals
        .new_tab("t1", "Echo", Path::new("."), Some(&argv(&["cmd", "/c", "echo hello-from-the-pane& exit 3"])))
        .unwrap();
    let pane = pane.unwrap();
    assert_eq!(Backend::owning(&pane), Some(Backend::Embedded));
    let info = wait_for("the command to exit", Duration::from_secs(15), || {
        terminals.pane_state("t1", &pane).unwrap().filter(|p| p.exited)
    });
    assert_eq!(info.exit_status, Some(3));
    assert_eq!(info.tab_id, tab);
    assert_eq!(info.tab_name, "Echo");
    assert!(screen_text(&terminals, "t1", &pane).contains("hello-from-the-pane"));
    // The pane stays (like a zellij pane whose command ended) until its tab is closed.
    assert_eq!(terminals.pane_for_tab("t1", tab), Some(pane.clone()));
    terminals.close_tab("t1", tab).unwrap();
    assert_eq!(terminals.pane_state("t1", &pane).unwrap(), None);
    assert_eq!(terminals.close_tab("t1", tab), Ok(()), "closing twice is fine");
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_lines_reach_the_shell() {
    let terminals = terminals();
    terminals.ensure_session("t2");
    let (_, pane) = terminals.new_tab("t2", "Shell", Path::new("."), None).unwrap();
    let pane = pane.unwrap();
    terminals.write_line("t2", &pane, "echo typed-%USERNAME:~0,0%marker").await.unwrap();
    wait_for("the echoed line", Duration::from_secs(15), || {
        screen_text(&terminals, "t2", &pane).contains("typed-marker").then_some(())
    });
    terminals.write_line("t2", &pane, "exit 7").await.unwrap();
    let info = wait_for("the shell to exit", Duration::from_secs(15), || {
        terminals.pane_state("t2", &pane).unwrap().filter(|p| p.exited)
    });
    assert_eq!(info.exit_status, Some(7));
    let error = terminals.write_line("t2", &pane, "too late").await.unwrap_err();
    assert!(error.0.contains("has exited"), "{error}");
}

/// A decision-tree answer line (`1.1: A → B`) carries a non-ASCII separator: the pane must hand the
/// program exactly its UTF-8 bytes, as zellij does.
#[tokio::test(flavor = "multi_thread")]
async fn typed_lines_keep_non_ascii_text() {
    let terminals = terminals();
    terminals.ensure_session("t2u");
    let reader = "import sys; print('HEX:' + sys.stdin.buffer.readline().strip().hex() + ':END')";
    let (_, pane) = terminals.new_tab("t2u", "Reader", Path::new("."), Some(&argv(&["python", "-c", reader]))).unwrap();
    let pane = pane.unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let line = "1.1: Soft-delete \u{b7} keep \u{2192} After";
    terminals.write_line("t2u", &pane, line).await.unwrap();
    let expected = format!("HEX:{}:END", line.bytes().map(|b| format!("{b:02x}")).collect::<String>());
    wait_for("the echoed bytes", Duration::from_secs(20), || {
        screen_text(&terminals, "t2u", &pane).replace(['\n', ' '], "").contains(&expected).then_some(())
    });
}

/// Closing the pseudoconsole ends only the processes attached to it. A descendant with a console
/// of its own (or none, like a hidden MCP server) dies because the pane's Job Object holds it, and
/// the job is assigned right after the child starts: this child starts that grandchild at once.
#[test]
fn closing_a_tab_kills_a_grandchild_started_at_once() {
    let marker = unique_marker();
    let terminals = terminals();
    terminals.ensure_session("t3");
    // `start /min` gives the first ping a new (minimized) console, detached from the pane's.
    let line = format!("start /min {} & {} >nul", marker_ping(marker), marker_ping(marker + 1));
    let (tab, _) = terminals.new_tab("t3", "Tree", Path::new("."), Some(&argv(&["cmd", "/c", &line]))).unwrap();
    wait_for("both pings to start", Duration::from_secs(20), || {
        let running =
            processes_with(&format!("-w {marker} ")).len() + processes_with(&format!("-w {} ", marker + 1)).len();
        (running == 2).then_some(())
    });
    terminals.close_tab("t3", tab).unwrap();
    wait_for("the process tree to be killed", Duration::from_secs(10), || {
        (processes_with(&format!("-w {marker} ")).is_empty()
            && processes_with(&format!("-w {} ", marker + 1)).is_empty())
        .then_some(())
    });
}

/// Dropping the terminals (the app exiting) returns promptly and kills what was running.
#[test]
fn dropping_the_terminals_kills_live_panes() {
    let marker = unique_marker() + 5_000;
    let terminals = terminals();
    terminals.ensure_session("t4");
    terminals.new_tab("t4", "Long", Path::new("."), Some(&argv(&["cmd", "/c", &marker_ping(marker)]))).unwrap();
    wait_for("the ping to start", Duration::from_secs(20), || {
        (!processes_with(&format!("-w {marker} ")).is_empty()).then_some(())
    });
    let started = Instant::now();
    drop(terminals);
    assert!(started.elapsed() < Duration::from_secs(2), "drop took {:?}", started.elapsed());
    wait_for("the ping to be killed", Duration::from_secs(10), || {
        processes_with(&format!("-w {marker} ")).is_empty().then_some(())
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_sends_the_screen_then_changes_then_closed() {
    let terminals = terminals();
    terminals.ensure_session("t5");
    let (tab, pane) = terminals.new_tab("t5", "Shell", Path::new("."), None).unwrap();
    let pane = pane.unwrap();
    let mut stream = terminals.subscribe("t5", &pane).unwrap();
    let Some(PaneEvent::Update { is_initial: true, scrollback: Some(_), .. }) = stream.recv().await else {
        panic!("the first event is the initial screen");
    };
    terminals.write_line("t5", &pane, "echo streamed-%USERNAME:~0,0%line").await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let event = tokio::time::timeout_at(deadline.into(), stream.recv()).await.expect("a frame with the line");
        if let Some(PaneEvent::Update { viewport, is_initial: false, scrollback: None }) = event
            && viewport.iter().any(|row| row.contains("streamed-line"))
        {
            break;
        }
    }
    terminals.close_tab("t5", tab).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.recv().await {
                Some(PaneEvent::Closed) | None => return,
                Some(_) => {}
            }
        }
    });
    closed.await.expect("Closed after the tab closes");
    assert_eq!(
        PaneEvent::Update { viewport: vec!["a".into()], scrollback: None, is_initial: false }.to_ndjson(),
        "{\"event\":\"pane_update\",\"viewport\":[\"a\"],\"is_initial\":false}\n"
    );
}

#[test]
fn an_attached_terminal_gets_the_replay_then_new_output() {
    let terminals = terminals();
    terminals.ensure_session("t6");
    let (_, pane) = terminals.new_tab("t6", "Shell", Path::new("."), None).unwrap();
    let pane = pane.unwrap();
    terminals.write_input("t6", &pane, b"echo first-%USERNAME:~0,0%output\r").unwrap();
    wait_for("the first output", Duration::from_secs(15), || {
        screen_text(&terminals, "t6", &pane).contains("first-output").then_some(())
    });
    let (replay, mut receiver) = terminals.attach("t6", &pane).unwrap();
    assert!(String::from_utf8_lossy(&replay).contains("first-output"));
    // A terminal's own cursor report is dropped; the typed command goes through.
    terminals.write_input("t6", &pane, b"\x1b[3;9Recho second-%USERNAME:~0,0%output\r").unwrap();
    let mut received = Vec::new();
    wait_for("the second output on the receiver", Duration::from_secs(15), || {
        while let Ok(chunk) = receiver.try_recv() {
            received.extend_from_slice(&chunk);
        }
        String::from_utf8_lossy(&received).contains("second-output").then_some(())
    });
    assert!(!screen_text(&terminals, "t6", &pane).contains("[3;9R"));
}

#[test]
fn resizing_resizes_the_screen_model() {
    let terminals = terminals();
    terminals.ensure_session("t7");
    let (_, pane) = terminals.new_tab("t7", "Shell", Path::new("."), None).unwrap();
    let pane = pane.unwrap();
    assert_eq!(terminals.screen("t7", &pane).unwrap().len(), 30);
    terminals.resize("t7", &pane, 12, 60).unwrap();
    assert_eq!(terminals.screen("t7", &pane).unwrap().len(), 12);
}

#[test]
fn sessions_come_and_go() {
    let terminals = terminals();
    assert_eq!(terminals.session_state("gone"), None);
    assert!(terminals.list_panes("gone").is_err());
    assert!(terminals.new_tab("gone", "x", Path::new("."), None).is_err());
    terminals.ensure_session("t8");
    assert_eq!(terminals.session_state("t8"), Some(SessionState::Running));
    terminals.new_tab("t8", "A", Path::new("."), None).unwrap();
    let (b, _) = terminals.new_tab("t8", "B", Path::new("."), None).unwrap();
    let panes = terminals.list_panes("t8").unwrap();
    assert_eq!(panes.iter().map(|p| p.tab_name.as_str()).collect::<Vec<_>>(), ["A", "B"]);
    assert_eq!(agentflow_core::terminal::tab_id_by_name(&panes, "B"), b);
    assert_eq!(terminals.close_tab("t8", -1).unwrap_err().0, "Cannot stop an agent without a known tab ID");
    terminals.kill_session("t8");
    assert_eq!(terminals.session_state("t8"), None);
    assert_eq!(terminals.list_sessions(), Vec::<String>::new());
    terminals.kill_session("t8");
}
