//! Engine safety checks, ported by name from backend/tests/test_workflow_safety.py as each engine
//! slice lands (docs/desktop-app-investigation.md §7). Slice 2: cancelling and session views;
//! slice 3 (launching and agent signals) is in engine_agents.rs.

mod engine_support;

use agentflow_core::engine::spec;
use engine_support::{Harness, add_session, engine, tab_seven};

#[tokio::test]
async fn test_cancel_stops_tab_before_marking_cancelled() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    harness.engine.cancel_task("TEST-1").await.unwrap();
    let closed: Vec<String> = harness.terminals.calls().into_iter().filter(|c| c.starts_with("close_tab")).collect();
    assert_eq!(closed, ["close_tab agentflow-TEST-1 7"]);
    assert_eq!(harness.engine.snapshot("TEST-1").unwrap()["status"], "CANCELLED");
    let state = harness.load();
    assert_eq!(state.runs[0].state, "CANCELLED");
    assert_eq!(state.agents[0].status, "stopped");
    assert!(harness.engine.agent_fail("TEST-1", "implementer", Some(run_id), "late").await.is_err());
}

#[tokio::test]
async fn test_failed_stop_does_not_claim_cancellation() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    harness.terminals.fail("close_tab", "stop failed");
    let error = harness.engine.cancel_task("TEST-1").await.unwrap_err();
    assert!(error.message.contains("Could not confirm"), "{error}");
    assert_eq!(harness.engine.snapshot("TEST-1").unwrap()["status"], "ACTIVE");
}

/// A persistent session's pane stays alive after its run finishes, so `status` alone reads as
/// 'running' forever. `activity` must report the run outcome instead.
#[tokio::test]
async fn test_session_activity_reflects_current_run_not_pane_liveness() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    let session = |harness: &Harness| harness.engine.snapshot("TEST-1").unwrap()["sessions"][0].clone();
    let pair = |s: &serde_json::Value| {
        (s["status"].as_str().unwrap().to_string(), s["activity"].as_str().unwrap().to_string())
    };
    assert_eq!(pair(&session(&harness)), ("running".into(), "running".into()));

    let set_run = |state: &str| harness.with_task(|task| task.runs[0].state = state.into());
    set_run("COMPLETED");
    assert_eq!(pair(&session(&harness)), ("running".into(), "completed".into()));
    set_run("FAILED");
    assert_eq!(session(&harness)["activity"], "failed");
    // Blocked on a human answer is not "working" either.
    set_run("WAITING_FOR_USER");
    assert_eq!(session(&harness)["activity"], "waiting_for_user");
    harness.with_task(|task| task.agents[0].status = "exited".into());
    assert_eq!(pair(&session(&harness)), ("exited".into(), "exited".into()));
}

#[tokio::test]
async fn test_failed_session_inspection_is_not_treated_as_stopped() {
    // A "zellij" that fails every call with a permission error.
    let dir = tempfile::tempdir().unwrap();
    let fake = if cfg!(windows) {
        let path = dir.path().join("zellij.cmd");
        std::fs::write(&path, "@echo permission denied 1>&2\r\n@exit /b 1\r\n").unwrap();
        path
    } else {
        let path = dir.path().join("zellij");
        std::fs::write(&path, "#!/bin/sh\necho permission denied >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    };
    let config =
        agentflow_core::config::ZellijConfig { command: fake.to_string_lossy().into_owned(), ..Default::default() };
    let service = agentflow_core::terminal::zellij::ZellijService::new(config);
    let error = service.session_exists("agentflow-TEST-1").await.unwrap_err();
    assert!(error.0.contains("Could not inspect"), "{error}");
}
