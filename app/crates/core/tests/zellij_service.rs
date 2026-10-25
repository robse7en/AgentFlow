//! The zellij service's session, client and window logic, ported by name from
//! backend/tests/test_questions_terminal.py. Python patched `sys.platform`, the service's own
//! methods and the process helpers; here a fake [`ZellijHost`] answers each zellij command by its
//! arguments and records every call, and the platform is the service's `windows` flag. Time is
//! paused, so the 250ms polls cost nothing.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentflow_core::config::{Config, ZellijConfig};
use agentflow_core::process::{CommandResult, ProcessError};
use agentflow_core::terminal::zellij::{ChildHandle, ZellijHost, ZellijService, parse_pane_event};
use agentflow_core::terminal::{PaneEvent, SessionState};
use serde_json::json;

/// A launcher or hidden client: running until given an exit code.
#[derive(Default)]
struct FakeChild {
    code: Mutex<Option<i64>>,
    waits: AtomicUsize,
    terminations: AtomicUsize,
}

impl FakeChild {
    fn running() -> Arc<FakeChild> {
        Arc::new(FakeChild::default())
    }

    fn exited(code: i64) -> Arc<FakeChild> {
        let child = FakeChild::default();
        *child.code.lock().unwrap() = Some(code);
        Arc::new(child)
    }
}

#[async_trait::async_trait]
impl ChildHandle for FakeChild {
    fn exit_code(&self) -> Option<i64> {
        *self.code.lock().unwrap()
    }
    async fn wait(&self, _: Duration) -> bool {
        self.waits.fetch_add(1, Ordering::SeqCst);
        true
    }
    async fn terminate_tree(&self) {
        self.terminations.fetch_add(1, Ordering::SeqCst);
    }
}

struct Answer {
    prefix: String,
    queued: VecDeque<CommandResult>,
    sticky: Option<CommandResult>,
}

#[derive(Default)]
struct FakeHost {
    exe: Option<String>,
    answers: Mutex<Vec<Answer>>,
    /// Every run ("run <args after the exe>") and start ("launcher ...", "hidden ...",
    /// "detached ...") in order.
    log: Mutex<Vec<String>>,
    launchers: Mutex<VecDeque<Result<Arc<FakeChild>, String>>>,
    hidden: Mutex<VecDeque<Arc<FakeChild>>>,
}

fn result(code: i32, stdout: &str, stderr: &str) -> CommandResult {
    CommandResult { argv: Vec::new(), returncode: code, stdout: stdout.into(), stderr: stderr.into() }
}

fn ok(stdout: &str) -> CommandResult {
    result(0, stdout, "")
}

impl FakeHost {
    fn new() -> Arc<FakeHost> {
        Arc::new(FakeHost { exe: Some("zellij".into()), ..Default::default() })
    }

    fn with_exe(exe: &str) -> Arc<FakeHost> {
        Arc::new(FakeHost { exe: Some(exe.into()), ..Default::default() })
    }

    fn answer(&self, prefix: &str, queued: Vec<CommandResult>, sticky: Option<CommandResult>) {
        self.answers.lock().unwrap().push(Answer { prefix: prefix.into(), queued: queued.into(), sticky });
    }

    fn launcher(&self, child: Result<Arc<FakeChild>, &str>) {
        self.launchers.lock().unwrap().push_back(child.map_err(String::from));
    }

    fn hidden(&self, child: Arc<FakeChild>) {
        self.hidden.lock().unwrap().push_back(child);
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// The log without the `list-sessions` polls.
    fn actions(&self) -> Vec<String> {
        self.log().into_iter().filter(|l| !l.contains("list-sessions")).collect()
    }
}

#[async_trait::async_trait]
impl ZellijHost for FakeHost {
    fn executable(&self, _: &str) -> Option<String> {
        self.exe.clone()
    }

    async fn run(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<CommandResult, ProcessError> {
        let args = argv[1..].join(" ");
        self.log.lock().unwrap().push(format!("run {args}"));
        let mut answers = self.answers.lock().unwrap();
        for answer in answers.iter_mut().filter(|a| args.starts_with(&a.prefix)) {
            if let Some(next) = answer.queued.pop_front() {
                return Ok(next);
            }
            if let Some(sticky) = &answer.sticky {
                return Ok(sticky.clone());
            }
        }
        Ok(ok(""))
    }

    fn start_launcher(&self, argv: &[String]) -> Result<Arc<dyn ChildHandle>, ProcessError> {
        self.log.lock().unwrap().push(format!("launcher {}", argv.join(" ")));
        match self.launchers.lock().unwrap().pop_front().unwrap_or(Ok(FakeChild::running())) {
            Ok(child) => Ok(child),
            Err(message) => Err(ProcessError(message)),
        }
    }

    fn start_hidden_shell(&self, line: &str) -> Result<Arc<dyn ChildHandle>, ProcessError> {
        self.log.lock().unwrap().push(format!("hidden {line}"));
        Ok(self.hidden.lock().unwrap().pop_front().unwrap_or_else(FakeChild::running))
    }

    fn spawn_detached(&self, argv: &[String], detach: bool, breakaway: bool) -> Result<(), ProcessError> {
        self.log.lock().unwrap().push(format!("detached {argv:?} detach={detach} breakaway={breakaway}"));
        Ok(())
    }
}

const SESSION: &str = "agentflow-TEST-1";

fn running_listing() -> CommandResult {
    ok("agentflow-TEST-1 [Created 1s ago] \n")
}

fn exited_listing() -> CommandResult {
    ok("agentflow-TEST-1 [Created 6days ago] (EXITED - attach to resurrect)\n")
}

fn service(host: &Arc<FakeHost>, windows: bool) -> ZellijService {
    ZellijService::with_host(ZellijConfig::default(), host.clone(), windows)
}

/// `list-panes` output whose widest pane (the tab bar) is `width` columns.
fn panes(width: i64) -> CommandResult {
    ok(&json!([{"id": 1, "is_plugin": true, "pane_columns": width}, {"id": 0, "is_plugin": false, "pane_columns": width}])
        .to_string())
}

fn clients(count: usize) -> CommandResult {
    let mut text = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n".to_string();
    for i in 0..count {
        text.push_str(&format!("{} terminal_{} bash\n", i + 1, i + 2));
    }
    ok(&text)
}

fn same(child: &Arc<dyn ChildHandle>, expected: &Arc<FakeChild>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(child), Arc::as_ptr(expected))
}

#[tokio::test(start_paused = true)]
async fn test_embedded_terminal_requires_zellij_044_or_newer() {
    let host = FakeHost::new();
    host.answer("--version", vec![ok("zellij 0.43.1"), ok("zellij 0.44.0")], None);
    let zellij = service(&host, false);
    let error = zellij.require_subscribe_support().await.unwrap_err();
    assert!(error.0.contains("requires Zellij 0.44"), "{error}");
    zellij.require_subscribe_support().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn test_ensure_session_reports_process_start_errors() {
    let host = FakeHost::new();
    host.launcher(Err("access denied"));
    let error = service(&host, false).ensure_session(SESSION).await.unwrap_err();
    assert_eq!(error.0, "Could not start Zellij session agentflow-TEST-1: access denied");
}

#[tokio::test(start_paused = true)]
async fn test_ensure_session_starts_a_missing_session() {
    let host = FakeHost::new();
    host.answer("list-sessions", vec![ok("")], Some(running_listing()));
    host.launcher(Ok(FakeChild::exited(0)));
    assert!(service(&host, false).ensure_session(SESSION).await.unwrap());
    let launched: Vec<String> = host.log().into_iter().filter(|l| l.starts_with("launcher")).collect();
    assert_eq!(launched, ["launcher zellij attach --create-background agentflow-TEST-1"]);
}

#[tokio::test(start_paused = true)]
async fn test_ensure_session_reports_early_zellij_exit() {
    let host = FakeHost::new();
    host.launcher(Ok(FakeChild::exited(1)));
    let error = service(&host, false).ensure_session(SESSION).await.unwrap_err();
    assert!(error.0.contains("exited with code 1 before creating session agentflow-TEST-1"), "{error}");
}

#[tokio::test(start_paused = true)]
async fn test_ensure_session_starts_a_headless_server_and_sizing_client_on_windows() {
    let host = FakeHost::new();
    host.answer("list-sessions", vec![ok("")], Some(running_listing()));
    host.answer("action list-panes", vec![panes(50), panes(200)], None);
    host.launcher(Ok(FakeChild::exited(0)));
    let client = FakeChild::running();
    host.hidden(client.clone());
    let zellij = service(&host, true);
    assert!(zellij.ensure_session(SESSION).await.unwrap());
    let starts: Vec<String> = host.log().into_iter().filter(|l| !l.starts_with("run")).collect();
    assert_eq!(
        starts,
        [
            "launcher zellij attach --create-background agentflow-TEST-1",
            "hidden mode con: cols=200 lines=50 & \"zellij\" attach agentflow-TEST-1",
        ],
        "no visible window any more"
    );
    assert!(same(&zellij.sizing_client(SESSION).unwrap(), &client));
}

/// A disconnected or locked session caps console windows near 118 columns, whatever `mode con` asked for.
#[tokio::test(start_paused = true)]
async fn test_new_windows_session_accepts_a_client_window_narrower_than_headless_cols() {
    let host = FakeHost::new();
    host.answer("list-sessions", vec![ok("")], Some(running_listing()));
    host.answer("action list-panes", vec![panes(50), panes(118)], None);
    host.launcher(Ok(FakeChild::exited(0)));
    let client = FakeChild::running();
    host.hidden(client.clone());
    let zellij = service(&host, true);
    assert!(zellij.ensure_session(SESSION).await.unwrap());
    assert!(same(&zellij.sizing_client(SESSION).unwrap(), &client));
    assert_eq!(client.terminations.load(Ordering::SeqCst), 0);
    assert!(!host.log().contains(&"run kill-session agentflow-TEST-1".to_string()));
}

/// `list-clients` lists the launcher's phantom client throughout; only the viewport width counts.
#[tokio::test(start_paused = true)]
async fn test_new_windows_session_is_not_fooled_by_the_launchers_phantom_client() {
    let host = FakeHost::new();
    // Missing, then running while it is sized and killed, then gone.
    host.answer("list-sessions", vec![ok(""), running_listing(), running_listing()], Some(ok("")));
    host.answer("action list-clients", vec![], Some(clients(1)));
    host.answer("action list-panes", vec![], Some(panes(50)));
    host.launcher(Ok(FakeChild::exited(0)));
    let client = FakeChild::running();
    host.hidden(client.clone());
    let error = service(&host, true).ensure_session(SESSION).await.unwrap_err();
    assert!(error.0.contains("did not attach"), "{error}");
    assert_eq!(client.terminations.load(Ordering::SeqCst), 1);
    assert!(host.log().contains(&"run kill-session agentflow-TEST-1".to_string()), "a retry starts clean");
}

#[tokio::test(start_paused = true)]
async fn test_client_count_parses_list_clients_output() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![clients(2), clients(0), result(1, "", "no such session")], None);
    let zellij = service(&host, false);
    assert_eq!(zellij.client_count(SESSION).await, 2);
    assert_eq!(zellij.client_count(SESSION).await, 0);
    assert_eq!(zellij.client_count(SESSION).await, 0);
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_trusts_a_live_tracked_client_without_asking_zellij() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(0))); // list-clients has missed working hidden clients
    let zellij = service(&host, true);
    zellij.track_sizing_client(SESSION, FakeChild::running());
    zellij.ensure_client_attached(SESSION).await.unwrap();
    assert!(host.log().is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_reuses_an_untracked_client_after_a_restart() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(1)));
    service(&host, true).ensure_client_attached(SESSION).await.unwrap();
    assert!(!host.log().iter().any(|l| l.starts_with("hidden")));
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_is_a_noop_on_non_windows() {
    let host = FakeHost::new();
    service(&host, false).ensure_client_attached(SESSION).await.unwrap();
    assert!(host.log().is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_starts_a_sized_hidden_client() {
    let host = FakeHost::with_exe(r"C:\Program Files\Zellij\zellij.exe");
    host.answer("action list-clients", vec![clients(0), clients(0)], Some(clients(1)));
    let client = FakeChild::running();
    host.hidden(client.clone());
    let config = Config::from_value(json!({"zellij": {"headlessCols": 180, "headlessRows": 45}})).unwrap().zellij;
    let zellij = ZellijService::with_host(config, host.clone(), true);
    zellij.ensure_client_attached(SESSION).await.unwrap();
    let hidden: Vec<String> = host.log().into_iter().filter(|l| l.starts_with("hidden")).collect();
    assert_eq!(
        hidden,
        [r#"hidden mode con: cols=180 lines=45 & "C:\Program Files\Zellij\zellij.exe" attach agentflow-TEST-1"#]
    );
    assert!(same(&zellij.sizing_client(SESSION).unwrap(), &client));
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_replaces_a_dead_tracked_client_even_if_one_is_listed() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(1)));
    let replacement = FakeChild::running();
    host.hidden(replacement.clone());
    let zellij = service(&host, true);
    zellij.track_sizing_client(SESSION, FakeChild::exited(1));
    zellij.ensure_client_attached(SESSION).await.unwrap();
    assert_eq!(host.log().iter().filter(|l| l.starts_with("hidden")).count(), 1);
    assert!(same(&zellij.sizing_client(SESSION).unwrap(), &replacement));
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_raises_if_the_hidden_client_never_attaches() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(0)));
    let stuck = FakeChild::running();
    host.hidden(stuck.clone());
    let error = service(&host, true).ensure_client_attached(SESSION).await.unwrap_err();
    assert!(error.0.contains("did not attach"), "{error}");
    assert_eq!(stuck.terminations.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_raises_if_the_hidden_client_exits() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(0)));
    host.hidden(FakeChild::exited(2));
    let error = service(&host, true).ensure_client_attached(SESSION).await.unwrap_err();
    assert!(error.0.contains("exited with code 2"), "{error}");
}

#[tokio::test(start_paused = true)]
async fn test_ensure_client_attached_refuses_session_names_cmd_would_reinterpret() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(0)));
    let error = service(&host, true).ensure_client_attached("agentflow-X & calc").await.unwrap_err();
    assert!(error.0.contains("Unsupported Zellij session name"), "{error}");
    assert!(!host.log().iter().any(|l| l.starts_with("hidden")));
}

#[tokio::test(start_paused = true)]
async fn test_open_window_focuses_the_tab_before_attaching_a_maximized_window() {
    let host = FakeHost::new();
    service(&host, true).open_window(SESSION, Some("Architect")).await.unwrap();
    assert_eq!(
        host.log(),
        [
            "run action go-to-tab-name Architect".to_string(),
            format!(
                "detached {:?} detach=true breakaway=true",
                ["cmd", "/c", "start", "/max", "", "zellij", "attach", SESSION]
            ),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn test_open_window_explains_manual_attach_off_windows() {
    let host = FakeHost::new();
    let error = service(&host, false).open_window(SESSION, None).await.unwrap_err();
    assert!(error.0.contains("zellij attach agentflow-TEST-1"), "{error}");
}

#[tokio::test(start_paused = true)]
async fn test_kill_session_stops_the_tracked_hidden_client() {
    let host = FakeHost::new();
    host.answer("list-sessions", vec![running_listing()], Some(ok("")));
    let client = FakeChild::running();
    let zellij = service(&host, true);
    zellij.track_sizing_client(SESSION, client.clone());
    zellij.kill_session(SESSION).await.unwrap();
    assert_eq!(client.waits.load(Ordering::SeqCst), 1);
    assert!(zellij.sizing_client(SESSION).is_none());
}

/// `zellij action` against an exited session hangs until the timeout, so nothing may query one:
/// cleanup used to time out after 30s on every attempt.
#[tokio::test(start_paused = true)]
async fn test_exited_sessions_are_not_treated_as_running() {
    let host = FakeHost::new();
    // Verbatim shape of `zellij list-sessions --no-formatting` (0.45.1, Windows).
    let listing = "agentflow-AF-14687 [Created 6days 21h 34m 25s ago] (EXITED - attach to resurrect)\n\
                   agentflow-AF-14791 [Created 1day 23h 50m 34s ago] (EXITED - attach to resurrect)\n\
                   agentflow-AF-14419 [Created 12h 3m 2s ago] \n\
                   af-14687 [Created 1day 20h 34m 42s ago] (current)\n";
    host.answer("list-sessions", vec![], Some(ok(listing)));
    let zellij = service(&host, false);
    assert!(!zellij.session_exists("agentflow-AF-14687").await.unwrap());
    assert_eq!(zellij.session_state("agentflow-AF-14687").await.unwrap(), Some(SessionState::Exited));
    assert!(zellij.session_exists("agentflow-AF-14419").await.unwrap());
    assert!(zellij.session_exists("af-14687").await.unwrap());
    assert_eq!(zellij.session_state("agentflow-NOPE-1").await.unwrap(), None);
    assert_eq!(zellij.list_sessions().await.unwrap(), ["agentflow-AF-14419", "af-14687"]);
}

#[tokio::test(start_paused = true)]
async fn test_kill_session_deletes_an_exited_session_without_killing_it() {
    let host = FakeHost::new();
    host.answer("list-sessions", vec![exited_listing()], Some(ok("")));
    service(&host, false).kill_session(SESSION).await.unwrap();
    assert_eq!(host.actions(), ["run delete-session agentflow-TEST-1 --force"]);
}

#[tokio::test(start_paused = true)]
async fn test_kill_session_does_nothing_for_an_unknown_session() {
    let host = FakeHost::new();
    service(&host, false).kill_session(SESSION).await.unwrap();
    assert!(host.actions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_ensure_session_replaces_an_exited_session_with_a_fresh_one() {
    let host = FakeHost::new();
    host.answer("list-sessions", vec![exited_listing()], Some(running_listing()));
    host.launcher(Ok(FakeChild::exited(0)));
    assert!(service(&host, false).ensure_session(SESSION).await.unwrap());
    assert_eq!(
        host.actions(),
        ["run delete-session agentflow-TEST-1 --force", "launcher zellij attach --create-background agentflow-TEST-1"]
    );
}

#[tokio::test(start_paused = true)]
async fn test_new_tab_ensures_a_client_is_attached_before_creating_the_pane() {
    let host = FakeHost::new();
    host.answer("action list-clients", vec![], Some(clients(1)));
    host.answer("action new-tab", vec![ok("3\n")], None);
    let pane = json!([{"id": 9, "is_plugin": false, "exited": false, "tab_id": 3, "tab_name": "architect"}]);
    host.answer("action list-panes", vec![], Some(ok(&pane.to_string())));
    let (tab, pane) = service(&host, true)
        .new_tab(SESSION, "architect", Path::new("."), Some(&["echo".into(), "hi".into()]))
        .await
        .unwrap();
    assert_eq!((tab, pane.as_deref()), (3, Some("terminal_9")));
    let log = host.log();
    let attached = log.iter().position(|l| l == "run action list-clients").unwrap();
    let created = log.iter().position(|l| l.starts_with("run action new-tab")).unwrap();
    assert!(attached < created, "{log:?}");
}

#[test]
fn test_subscribe_streams_ndjson_and_targets_selected_session() {
    let args = ZellijService::subscribe_args(SESSION, "terminal_7");
    assert_eq!(args[..4], ["--session", SESSION, "subscribe", "--pane-id"]);
    assert_eq!(args[4..6], ["terminal_7", "--scrollback"]);
    let record = json!({"event": "pane_update", "pane_id": "terminal_7", "viewport": ["output"]});
    match parse_pane_event(&record.to_string()) {
        Some(PaneEvent::Update { viewport, .. }) => assert_eq!(viewport, ["output"]),
        other => panic!("{other:?}"),
    }
}
