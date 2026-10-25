//! The zellij backend against the installed zellij (0.44+), in a session of its own.
//!
//! Ignored: it starts a zellij server and a hidden sizing client. It creates, uses and kills only
//! a session named `agentflow-rusttest-<pid>`; no other session is touched. Run with:
//!
//!     AGENTFLOW_LIVE_ZELLIJ=1 cargo test --test live_zellij -- --ignored --nocapture
#![cfg(windows)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agentflow_core::config::Config;
use agentflow_core::process;
use agentflow_core::terminal::zellij::{ZellijService, parse_pane_event};
use agentflow_core::terminal::{Backend, PaneEvent, SessionState};

/// Kills and deletes the test's own session however the test ends.
struct OwnSession {
    exe: String,
    name: String,
}

impl Drop for OwnSession {
    fn drop(&mut self) {
        for args in [vec!["kill-session", &self.name], vec!["delete-session", &self.name, "--force"]] {
            let _ = std::process::Command::new(&self.exe).args(args).output();
        }
    }
}

async fn raw(zellij: &ZellijService, session: &str, args: &[&str]) -> String {
    let mut argv = vec![zellij.executable().unwrap()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let env = HashMap::from([("ZELLIJ_SESSION_NAME".to_string(), session.to_string())]);
    process::run(&argv, None, &env, Duration::from_secs(30), None).await.unwrap().stdout
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts a zellij server; set AGENTFLOW_LIVE_ZELLIJ=1"]
async fn the_zellij_backend_drives_a_session_of_its_own() {
    std::env::var("AGENTFLOW_LIVE_ZELLIJ").expect("AGENTFLOW_LIVE_ZELLIJ=1");
    let zellij = ZellijService::new(Config::default().zellij);
    let version = zellij.version().await;
    println!("zellij version: {version:?}");
    zellij.require_subscribe_support().await.unwrap();
    let name = format!("agentflow-rusttest-{}", std::process::id());
    let _guard = OwnSession { exe: zellij.executable().unwrap(), name: name.clone() };
    let cwd = agentflow_core::paths::resolve(&std::env::temp_dir());

    let started = Instant::now();
    assert!(zellij.ensure_session(&name).await.unwrap(), "created");
    println!("session ready (sized) after {:.1}s", started.elapsed().as_secs_f64());
    assert!(!zellij.ensure_session(&name).await.unwrap(), "already running");
    assert_eq!(zellij.session_state(&name).await.unwrap(), Some(SessionState::Running));
    let listed = raw(&zellij, &name, &["list-sessions", "--no-formatting"]).await;
    println!("list-sessions (own line): {:?}", listed.lines().find(|l| l.contains(&name)));

    let probe: Vec<String> =
        ["cmd", "/c", "echo zellij-%USERNAME:~0,0%probe& ping -n 120 127.0.0.1 >nul"].map(String::from).into();
    let (tab, pane) = zellij.new_tab(&name, "Probe", &cwd, Some(&probe)).await.unwrap();
    let pane = pane.expect("the probe tab has a pane");
    println!("new tab {tab} pane {pane}");
    assert_eq!(Backend::owning(&pane), Some(Backend::Zellij));
    println!("list-tabs raw:\n{}", raw(&zellij, &name, &["action", "list-tabs"]).await);
    println!("list-panes raw:\n{}", raw(&zellij, &name, &["action", "list-panes", "--json", "--all"]).await);
    assert_eq!(zellij.tab_id_by_name(&name, "Probe").await, tab);
    let info = zellij.pane_state(&name, &pane).await.unwrap().expect("listed");
    assert_eq!((info.tab_id, info.tab_name.as_str(), info.exited), (tab, "Probe", false));

    let deadline = Instant::now() + Duration::from_secs(15);
    while !zellij.screen(&name, &pane).await.unwrap().join("\n").contains("zellij-probe") {
        assert!(Instant::now() < deadline, "the probe output never appeared");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let mut stream = zellij.subscribe(&name, &pane);
    let first = tokio::time::timeout(Duration::from_secs(15), stream.recv()).await.expect("a first event");
    match &first {
        Some(PaneEvent::Update { is_initial, viewport, scrollback }) => println!(
            "subscribe first event: is_initial={is_initial} viewport rows={} scrollback rows={:?}",
            viewport.len(),
            scrollback.as_ref().map(Vec::len)
        ),
        other => panic!("unexpected first event {other:?}"),
    }
    drop(stream);

    let shell: Vec<String> = ["cmd", "/q"].map(String::from).into();
    let (shell_tab, shell_pane) = zellij.new_tab(&name, "Shell", &cwd, Some(&shell)).await.unwrap();
    let shell_pane = shell_pane.unwrap();
    zellij.write_line(&name, &shell_pane, "echo typed-%USERNAME:~0,0%marker").await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !zellij.screen(&name, &shell_pane).await.unwrap().join("\n").contains("typed-marker") {
        assert!(Instant::now() < deadline, "the typed line never ran");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    zellij.close_tab(&name, tab).await.unwrap();
    assert_eq!(zellij.pane_state(&name, &pane).await.unwrap(), None);
    assert!(zellij.pane_for_tab(&name, shell_tab).await.is_some());
    println!("attach command: {}", zellij.attach_command(&name));

    zellij.kill_session(&name).await.unwrap();
    assert_eq!(zellij.session_state(&name).await.unwrap(), None);
    println!("killed after {:.1}s total", started.elapsed().as_secs_f64());
    let line = r#"{"event":"pane_update","viewport":["a"],"is_initial":true}"#;
    assert!(matches!(parse_pane_event(line), Some(PaneEvent::Update { is_initial: true, .. })));
}
