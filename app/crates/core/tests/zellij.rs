//! The zellij backend's parsing and naming, on output captured from zellij 0.45.1 (see
//! live_zellij.rs), plus the Python zellij test ported by name.

use std::path::Path;

use agentflow_core::config::ZellijConfig;
use agentflow_core::terminal::zellij::{
    ZellijService, parse_pane_event, parse_panes, parse_sessions, parse_tab_id_by_name, shlex_quote, version_supported,
};
use agentflow_core::terminal::{PaneEvent, SessionState};
use serde_json::Value;

// Ported from backend/tests/test_doctor.py
#[test]
fn test_zellij_version_supported() {
    let cases = [
        (Some("zellij 0.44.0"), true),
        (Some("zellij 0.45.1"), true),
        (Some("zellij 1.0.0"), true),
        (Some("zellij 0.43.9"), false),
        (Some("zellij"), false),
        (None, false),
    ];
    for (version, supported) in cases {
        assert_eq!(version_supported(version), supported, "{version:?}");
    }
}

#[test]
fn version_checks_match_python() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/harness/misc.json");
    let misc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for case in misc["zellijVersions"].as_array().unwrap() {
        assert_eq!(version_supported(case["version"].as_str()), case["supported"], "{}", case["version"]);
    }
}

#[test]
fn sessions_are_running_unless_exited() {
    let stdout = "agentflow-DE-1 [Created 2h 3m ago] \n\
                  agentflow-DE-2 [Created 1day ago] (EXITED - attach to resurrect)\n\
                  \n\
                  agentflow-rusttest-43952 [Created 0s ago] (current)\n";
    let states = parse_sessions(stdout);
    assert_eq!(states.len(), 3);
    assert_eq!(states["agentflow-DE-1"], SessionState::Running);
    assert_eq!(states["agentflow-DE-2"], SessionState::Exited);
    assert_eq!(states["agentflow-rusttest-43952"], SessionState::Running);
}

#[test]
fn tab_ids_are_found_by_name() {
    // `list-tabs` from zellij 0.45.1, plus a second "Probe" tab and odd spacing.
    let stdout = "TAB_ID  POSITION  NAME\n0  0  Tab #1\n1  1  Probe\n7   2    Probe  \nx  3  Probe\n";
    assert_eq!(parse_tab_id_by_name(stdout, "Probe"), 7);
    assert_eq!(parse_tab_id_by_name(stdout, "Tab #1"), 0);
    assert_eq!(parse_tab_id_by_name(stdout, "Missing"), -1);
    assert_eq!(parse_tab_id_by_name("0  0  Probe\n", "Probe"), -1, "the first line is a header");
}

/// Trimmed from zellij 0.45.1's `list-panes --json --all`: a plugin pane, a shell pane, a command
/// pane, and the same command pane after it exited.
const PANES: &str = r#"[
  {"id": 1, "is_plugin": true, "title": "tab-bar", "exited": false, "exit_status": null,
   "pane_columns": 200, "terminal_command": null, "plugin_url": "tab-bar", "tab_id": 0, "tab_name": "Tab #1"},
  {"id": 0, "is_plugin": false, "exited": false, "exit_status": null, "pane_columns": 200,
   "terminal_command": null, "plugin_url": null, "tab_id": 0, "tab_name": "Tab #1",
   "pane_command": "C:\\WINDOWS\\system32\\cmd.exe", "pane_cwd": "D:\\Dev\\"},
  {"id": 1, "is_plugin": false, "exited": false, "exit_status": null, "pane_columns": 200,
   "terminal_command": "cmd /c echo probe& ping -n 120 127.0.0.1 >nul", "plugin_url": null,
   "tab_id": 1, "tab_name": "Probe", "pane_command": "ping -n 120 127.0.0.1"},
  {"id": 2, "is_plugin": false, "exited": true, "exit_status": 3, "is_held": true,
   "terminal_command": "cmd /c exit 3", "tab_id": 2, "tab_name": "Done", "pane_command": ""}
]"#;

#[test]
fn panes_are_parsed_without_plugins() {
    let panes = parse_panes(PANES).unwrap();
    assert_eq!(panes.len(), 3);
    assert_eq!(panes[0].pane_id, "terminal_0");
    assert_eq!(panes[0].command.as_deref(), Some("C:\\WINDOWS\\system32\\cmd.exe"));
    assert_eq!((panes[1].pane_id.as_str(), panes[1].tab_id, panes[1].tab_name.as_str()), ("terminal_1", 1, "Probe"));
    assert_eq!(panes[1].command.as_deref(), Some("ping -n 120 127.0.0.1"), "pane_command first");
    assert!(panes[2].exited);
    assert_eq!(panes[2].exit_status, Some(3));
    assert_eq!(panes[2].command.as_deref(), Some("cmd /c exit 3"), "an empty pane_command falls back");
    assert_eq!(parse_panes("").unwrap(), vec![]);
    assert!(parse_panes("not json").unwrap_err().0.starts_with("list-panes returned invalid JSON"));
}

#[test]
fn pane_stream_lines_become_events() {
    let update =
        r#"{"event":"pane_update","pane_id":"terminal_1","viewport":["a","b"],"scrollback":["s"],"is_initial":true}"#;
    assert_eq!(
        parse_pane_event(update),
        Some(PaneEvent::Update {
            viewport: vec!["a".into(), "b".into()],
            scrollback: Some(vec!["s".into()]),
            is_initial: true
        })
    );
    let as_text = r#"{"event":"pane_update","viewport":"x\ny"}"#;
    assert_eq!(
        parse_pane_event(as_text),
        Some(PaneEvent::Update { viewport: vec!["x".into(), "y".into()], scrollback: None, is_initial: false })
    );
    assert_eq!(parse_pane_event(r#"{"event":"pane_closed","pane_id":"terminal_1"}"#), Some(PaneEvent::Closed));
    assert_eq!(parse_pane_event(r#"{"event":"error","message":"boom"}"#), Some(PaneEvent::Error("boom".into())));
    assert_eq!(parse_pane_event(r#"{"event":"mode_update"}"#), None);
    assert_eq!(parse_pane_event(r#"{"event":"pane_update"}"#), None, "no viewport");
    assert_eq!(parse_pane_event("not json"), None);
}

#[test]
fn attach_commands_quote_like_shlex() {
    // Each pair is (session, shlex.quote(session)) from CPython 3.13.
    let cases = [
        ("agentflow-DE-1", "agentflow-DE-1"),
        ("", "''"),
        ("has space", "'has space'"),
        ("it's", "'it'\"'\"'s'"),
        ("a@b%c+d=e:f,g.h/i-j", "a@b%c+d=e:f,g.h/i-j"),
        ("ünï", "'ünï'"),
        ("x;y", "'x;y'"),
    ];
    for (session, quoted) in cases {
        assert_eq!(shlex_quote(session), quoted, "{session}");
    }
    let service = ZellijService::new(ZellijConfig::default());
    assert_eq!(service.attach_command("agentflow-DE-1"), "zellij attach agentflow-DE-1");
}

#[test]
fn web_urls_only_when_enabled() {
    let mut config = ZellijConfig::default();
    assert_eq!(ZellijService::new(config.clone()).web_url("s"), None);
    config.web_client_enabled = true;
    config.web_client_base_url = "http://127.0.0.1:8082//".into();
    assert_eq!(
        ZellijService::new(config).web_url("agentflow-DE-1").as_deref(),
        Some("http://127.0.0.1:8082/agentflow-DE-1")
    );
}
