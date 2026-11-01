//! Launching agents and what they report back, ported by name from
//! backend/tests/test_workflow_safety.py (engine port slice 3).

mod engine_support;

use std::sync::Arc;

use agentflow_core::api::{self, App};
use agentflow_core::db::table::Tracked;
use agentflow_core::db::{AgentSessionRow, DbTime};
use agentflow_core::engine::launch::RunNodeRequest;
use agentflow_core::engine::spec;
use agentflow_core::harness::as_posix;
use agentflow_core::terminal::PaneInfo;
use axum::body::Body;
use axum::http::Request;
use engine_support::{Harness, add_session, engine, tab_seven};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn heartbeats_are_observations_and_reject_stale_attempts() {
    let h = engine().await;
    let old = h.add_run(spec::IMPLEMENTER, "FAILED", None);
    let run = h.add_run(spec::IMPLEMENTER, "WAITING_FOR_USER", None);
    let before = h.engine.snapshot("TEST-1").unwrap();
    for phase in ["implementing", "testing"] {
        h.engine.agent_heartbeat("TEST-1", "implementer", Some(run), phase).await.unwrap();
    }
    let snap = h.engine.snapshot("TEST-1").unwrap();
    let node = snap["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::IMPLEMENTER).unwrap();
    assert_eq!(node["lastRun"]["heartbeat"]["phase"], "testing");
    assert!(node["lastRun"]["heartbeat"]["receivedAt"].is_string());
    assert_eq!(node["state"], "WAITING_FOR_USER");
    assert_eq!(snap["events"], before["events"]);
    assert_eq!(snap["currentSha"], before["currentSha"]);
    for (role, id) in [("implementer", old), ("architect", run)] {
        assert!(h.engine.agent_heartbeat("TEST-1", role, Some(id), "working").await.is_err());
    }
    assert!(h.engine.agent_heartbeat("TEST-1", "implementer", Some(run), "invalid").await.is_err());
    for status in ["COMPLETED", "FAILED", "INTERRUPTED", "CANCELLED"] {
        h.with_task(|state| state.runs.iter_mut().find(|r| r.id == run).unwrap().state = status.into());
        assert!(h.engine.agent_heartbeat("TEST-1", "implementer", Some(run), "working").await.is_err());
    }
}

fn node_state(harness: &Harness, key: &str) -> String {
    harness.load().node(key).unwrap().state.clone()
}

fn writes(harness: &Harness) -> Vec<String> {
    harness.terminals.calls().into_iter().filter(|c| c.starts_with("write_line")).collect()
}

/// A stopped or exited agent session of `role` on `harness`, for resume tests.
fn old_session(harness: &Harness, role: &str, harness_key: &str, session_id: &str, status: &str, run: Option<i64>) {
    harness.with_task(|state| {
        state.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: state.task.id,
            role: role.into(),
            harness: harness_key.into(),
            model: None,
            reasoning: None,
            zellij_tab_name: "Implementer".into(),
            zellij_tab_id: Some(3),
            zellij_pane_id: if status == "stopped" { Some("terminal_3".into()) } else { None },
            harness_session_id: Some(session_id.into()),
            status: status.into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: run,
            started_at: DbTime::now(),
            ended_at: None,
        }));
    });
}

fn set_node(harness: &Harness, key: &str, state: &str) {
    harness.with_task(|task| {
        let index = task.node_index(key).unwrap();
        task.nodes[index].state = state.into();
    });
}

async fn run(harness: &Harness, key: &str) -> Result<Value, agentflow_core::engine::WorkflowError> {
    harness.engine.run_node("TEST-1", key, RunNodeRequest::default(), false).await
}

/// One request through the router as given (no pairing), returning the status.
async fn raw(harness: &Harness, request: Request<Body>) -> u16 {
    let app = App::around(harness.engine.clone(), harness.tmp().join("config.json"));
    api::router(app).oneshot(request).await.unwrap().status().as_u16()
}

#[tokio::test]
async fn test_stale_completion_and_failure_cannot_change_retry() {
    let harness = engine().await;
    let old = harness.add_run(spec::IMPLEMENTER, "FAILED", None);
    harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let engine = &harness.engine;
    let stale = engine
        .agent_complete("TEST-1", "implementer", Some(old), json!({"status": "completed"}).as_object().unwrap().clone())
        .await;
    assert!(stale.unwrap_err().message.contains("Stale"));
    let stale = engine.agent_fail("TEST-1", "implementer", Some(old), "late failure").await;
    assert!(stale.unwrap_err().message.contains("Stale"));
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "RUNNING");
    assert!(Arc::ptr_eq(&engine.lock("test-1"), &engine.lock("TEST-1")));
}

#[tokio::test]
async fn test_wrong_role_and_terminal_failure_rejected() {
    let harness = engine().await;
    let run = harness.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    assert!(harness.engine.agent_fail("TEST-1", "architect", Some(run), "bad").await.is_err());
    assert!(harness.engine.agent_fail("TEST-1", "implementer", Some(run), "bad").await.is_err());
}

#[tokio::test]
async fn test_question_answer_uses_only_its_active_tracked_pane() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    let engine = &harness.engine;
    let choices = ["Gradual".to_string(), "All at once".to_string()];
    engine.agent_question("TEST-1", "implementer", Some(run_id), "Choose a rollout", &choices).await.unwrap();
    let uid = engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_string();

    let snapshot = engine.answer_question("TEST-1", &uid, "Gradual", None).await.unwrap();
    assert_eq!(snapshot["questions"][0]["answer"], "Gradual");
    assert_eq!(snapshot["questions"][0]["answered"], true);
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "RUNNING");
    assert_eq!(writes(&harness), ["write_line agentflow-TEST-1 terminal_7 Gradual"]);

    engine.answer_question("TEST-1", &uid, "Gradual", None).await.unwrap();
    assert_eq!(writes(&harness).len(), 1);
    let again = engine.answer_question("TEST-1", &uid, "All at once", None).await.unwrap_err();
    assert!(again.message.contains("already been answered"), "{again}");
}

/// The answer is typed into the agent's pane and followed by Enter, so the engine itself refuses
/// anything but one printable line, whoever the caller is.
#[tokio::test]
async fn test_engine_validates_answers_for_callers_that_bypass_http() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    let engine = &harness.engine;
    engine
        .agent_question("TEST-1", "implementer", Some(run_id), "Choose a rollout", &["Gradual".into()])
        .await
        .unwrap();
    let uid = engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_string();

    for bad in ["Gradual\nrm -rf .", "Gradual\r", "Gradual\x1b[A", "   ", &"x".repeat(4001)] {
        let error = engine.answer_question("TEST-1", &uid, bad, None).await.unwrap_err();
        assert_eq!(error.status, 422, "{bad:?}");
    }
    assert!(writes(&harness).is_empty());
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "WAITING_FOR_USER");

    let snapshot = engine.answer_question("TEST-1", &uid, "  Gradual  ", None).await.unwrap();
    assert_eq!(snapshot["questions"][0]["answer"], "Gradual");
    assert_eq!(writes(&harness), ["write_line agentflow-TEST-1 terminal_7 Gradual"]);
}

#[tokio::test]
async fn test_batched_questions_only_resume_once_every_reply_is_in() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    let engine = &harness.engine;
    let choices = ["Gradual".to_string(), "All at once".to_string()];
    engine.agent_question("TEST-1", "implementer", Some(run_id), "Choose a rollout", &choices).await.unwrap();
    engine.agent_question("TEST-1", "implementer", Some(run_id), "Which region first?", &[]).await.unwrap();
    let questions = engine.snapshot("TEST-1").unwrap()["questions"].clone();
    let uid = |i: usize| questions[i]["uid"].as_str().unwrap().to_string();

    engine.answer_question("TEST-1", &uid(0), "Gradual", None).await.unwrap();
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "WAITING_FOR_USER");

    engine.answer_question("TEST-1", &uid(1), "APAC", None).await.unwrap();
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "RUNNING");
    assert_eq!(harness.load().runs[0].state, "RUNNING");

    let before = harness.count("workflow_events");
    let result = engine.agent_running("TEST-1", "implementer", Some(run_id)).await.unwrap();
    assert_eq!(result["state"], "RUNNING");
    assert_eq!(harness.count("workflow_events"), before, "the agent's own confirmation is a no-op once resumed");
}

#[tokio::test]
async fn test_question_from_a_superseded_run_is_not_actionable() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "WAITING_FOR_USER", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    let choices = ["Gradual".to_string(), "All at once".to_string()];
    harness.engine.agent_question("TEST-1", "implementer", Some(run_id), "Choose a rollout", &choices).await.unwrap();

    // Backend restarted; the human retried the interrupted node instead of answering, leaving the
    // question's run superseded without ever being answered.
    harness.with_task(|state| state.runs[0].state = "FAILED".into());
    harness.add_run(spec::IMPLEMENTER, "RUNNING", None);

    let question = harness.engine.snapshot("TEST-1").unwrap()["questions"][0].clone();
    assert_eq!((question["answered"].clone(), question["actionable"].clone()), (json!(false), json!(false)));
    let error = harness.engine.answer_question("TEST-1", question["uid"].as_str().unwrap(), "Gradual", None).await;
    assert!(error.unwrap_err().message.contains("Stale run"));
}

#[tokio::test]
async fn test_terminal_view_resolves_only_the_current_active_agent_pane() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "WAITING_FOR_USER", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    let target = harness.engine.terminal_target("TEST-1", "implementer").await.unwrap();
    assert_eq!(target, ("agentflow-TEST-1".to_string(), "terminal_7".to_string()));
    let error = harness.engine.terminal_target("TEST-1", "code_reviewer").await.unwrap_err();
    assert!(error.message.contains("active pane"), "{error}");
}

#[tokio::test]
async fn test_human_question_reply_requires_pairing_and_targets_server_tracked_pane() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "WAITING_FOR_USER", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    harness.engine.agent_question("TEST-1", "implementer", Some(run_id), "Ship it?", &[]).await.unwrap();
    let uid = harness.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_string();
    let path = format!("/api/tasks/TEST-1/questions/{uid}/answer");

    let unpaired = Request::builder()
        .method("POST")
        .uri(&path)
        .header("host", "127.0.0.1")
        .header("origin", "http://127.0.0.1")
        .header("content-type", "application/json")
        .body(Body::from(json!({"answer": "Yes"}).to_string()))
        .unwrap();
    assert_eq!(raw(&harness, unpaired).await, 401);
    let (status, _) = harness.route("POST", &path, Some(json!({"answer": "Yes"}))).await;
    assert_eq!(status, 200);
    assert_eq!(writes(&harness), ["write_line agentflow-TEST-1 terminal_7 Yes"]);
}

#[tokio::test]
async fn test_agent_cannot_approve_or_read_other_task() {
    let harness = engine().await;
    let run = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run);
    let request = |method: &str, path: &str, body: Option<Value>| {
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json");
        builder.body(body.map_or(Body::empty(), |b| Body::from(b.to_string()))).unwrap()
    };
    assert_eq!(raw(&harness, request("POST", "/api/tasks/TEST-1/nodes/requirements_gate/approve", None)).await, 403);
    for (method, path) in [
        ("POST", "/api/tasks/TEST-1/nodes/ready_for_pr/approve"),
        ("POST", "/api/tasks/TEST-1/nodes/requirements_gate/request-changes"),
        ("POST", "/api/tasks/TEST-1/review-override"),
        ("POST", "/api/tasks/TEST-1/worktree/discard-drift"),
        ("PUT", "/api/tasks/TEST-1/budget"),
        ("PUT", "/api/tasks/TEST-1/runs/1/usage"),
    ] {
        assert_eq!(raw(&harness, request(method, path, None)).await, 403, "{path}");
    }
    assert_eq!(raw(&harness, request("GET", "/api/tasks/TEST-2", None)).await, 403);
    let running =
        |role: &str| request("POST", "/api/tasks/TEST-1/agent/running", Some(json!({"role": role, "runId": run})));
    assert_eq!(raw(&harness, running("architect")).await, 403);
    assert_eq!(raw(&harness, running("implementer")).await, 200);
    let heartbeat = |role: &str, id: i64| {
        request(
            "POST",
            "/api/tasks/TEST-1/agent/heartbeat",
            Some(json!({"role": role, "runId": id, "phase": "testing"})),
        )
    };
    assert_eq!(raw(&harness, heartbeat("architect", run)).await, 403);
    assert_eq!(raw(&harness, heartbeat("implementer", run + 1)).await, 403);
    assert_eq!(raw(&harness, heartbeat("implementer", run)).await, 200);
}

/// A dead session's conversation id is offered to resume for any role, not only the Final
/// Architect.
#[tokio::test]
async fn test_resume_is_attempted_for_any_role_not_just_final_architect() {
    let harness = engine().await;
    harness.launchable();
    set_node(&harness, spec::IMPLEMENTER, "READY");
    old_session(&harness, "implementer", "claude", "dead-session-42", "stopped", None);

    run(&harness, spec::IMPLEMENTER).await.unwrap();
    let script = harness.launch_script();
    assert!(script.contains("'--resume' 'dead-session-42'"), "{script}");
    let events: Vec<String> = harness.engine.snapshot("TEST-1").unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["message"].as_str().unwrap().to_string())
        .collect();
    assert!(events.contains(&"Original Implementer process is gone; resuming its conversation".to_string()));
}

/// A resumed launch that crashed right away means the CLI's own session is unusable; retrying
/// the resume would repeat the crash forever, so the next attempt launches fresh.
#[tokio::test]
async fn test_broken_resume_falls_back_to_a_fresh_launch() {
    let harness = engine().await;
    harness.launchable();
    let failed = harness.add_run(spec::IMPLEMENTER, "FAILED", None);
    harness.with_task(|state| {
        state.runs[0].error = Some("Harness exited with code 1 before reporting completion".into());
    });
    set_node(&harness, spec::IMPLEMENTER, "READY");
    old_session(&harness, "implementer", "claude", "broken-session-1", "exited", Some(failed));

    run(&harness, spec::IMPLEMENTER).await.unwrap();
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "RUNNING");
    let script = harness.launch_script();
    assert!(!script.contains("broken-session-1") && script.contains("'--session-id'"), "{script}");
}

/// A harness's own shell tool can start with a PATH that never saw the launcher's PATH prepend,
/// so AGENTFLOW_FLOWCTL must name flowctl regardless.
#[tokio::test]
async fn test_launcher_exposes_flowctl_independent_of_path() {
    let harness = engine().await;
    harness.launchable();
    run(&harness, spec::ARCHITECT).await.unwrap();
    let script = harness.launch_script();
    assert!(script.contains("AGENTFLOW_FLOWCTL"));
    assert!(script.contains(&as_posix(&harness.engine.bin_dir.join("flowctl"))));
}

/// The desktop's resource directory is a `\\?\` path, which cmd.exe cannot run, so the shims and the
/// launcher's `flowctl exited` use the plain form.
#[tokio::test]
async fn test_flowctl_shims_drop_the_verbatim_prefix() {
    let harness = engine().await;
    let engine = harness.restarted_with_flowctl(r"\\?\D:\Apps\AgentFlow\flowctl.exe".into());
    assert_eq!(engine.flowctl, std::path::PathBuf::from(r"D:\Apps\AgentFlow\flowctl.exe"));
    let cmd = std::fs::read_to_string(engine.bin_dir.join("flowctl.cmd")).unwrap();
    assert!(cmd.contains(r#""D:\Apps\AgentFlow\flowctl.exe" %*"#), "{cmd}");
    let sh = std::fs::read_to_string(engine.bin_dir.join("flowctl")).unwrap();
    assert!(sh.contains(r#"exec "D:/Apps/AgentFlow/flowctl.exe""#), "{sh}");
}

/// Claude's workspace-trust dialog would stall an unattended run forever, so the workspace is
/// prepared before the process starts.
#[tokio::test]
async fn test_fresh_launch_prepares_the_harness_workspace_before_starting() {
    let harness = engine().await;
    let repository = harness.load().task.repository_root.clone();
    harness.configure(|config| {
        config.harnesses.get_mut("claude").unwrap().trusted_repositories = vec![repository.clone()];
    });
    let home = harness.launchable();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".claude.json"), "{}").unwrap();
    run(&harness, spec::ARCHITECT).await.unwrap();
    let trusted: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    let cwd = as_posix(std::path::Path::new(&harness.load().task.worktree_path));
    assert_eq!(trusted["projects"][&cwd]["hasTrustDialogAccepted"], true, "{trusted}");
}

#[tokio::test]
async fn test_exit_report_cannot_mark_replacement_session_exited() {
    let harness = engine().await;
    let old = harness.add_run(spec::IMPLEMENTER, "FAILED", None);
    let new = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, new);
    // The old launch does not own the new session and must not fail its current run.
    harness.engine.agent_exited("TEST-1", "implementer", Some(old), 1).await.unwrap();
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "RUNNING");
    assert_eq!(harness.engine.snapshot("TEST-1").unwrap()["sessions"][0]["status"], "running");
}

/// The Architect's launch fails at the terminal; the next launch must then work.
async fn fail_architect_launch(harness: &Harness) {
    harness.terminals.fail("new_tab", "zellij action list-clients timed out after 30.0s");
    let error = run(harness, spec::ARCHITECT).await.unwrap_err();
    assert!(error.message.contains("Could not open zellij tab"), "{error}");
    assert_eq!(node_state(harness, spec::ARCHITECT), "FAILED");
    harness.terminals.succeed("new_tab");
}

fn architect_history(harness: &Harness) -> (Vec<String>, Vec<String>) {
    let state = harness.load();
    let architect = state.node(spec::ARCHITECT).unwrap().id;
    let runs = state.runs.iter().filter(|r| r.node_id == architect).map(|r| r.state.clone()).collect();
    (runs, state.agents.iter().map(|a| a.status.clone()).collect())
}

fn closed_tabs(harness: &Harness) -> Vec<String> {
    harness.terminals.calls().into_iter().filter(|c| c.starts_with("close_tab")).collect()
}

#[tokio::test]
async fn test_retry_after_failed_launch_releases_a_session_with_no_tab() {
    let harness = engine().await;
    harness.launchable();
    fail_architect_launch(&harness).await;
    run(&harness, spec::ARCHITECT).await.unwrap();
    assert_eq!(node_state(&harness, spec::ARCHITECT), "RUNNING");
    assert_eq!(
        architect_history(&harness),
        (vec!["FAILED".into(), "RUNNING".into()], vec!["exited".into(), "running".into()])
    );
    assert!(closed_tabs(&harness).is_empty());
}

#[tokio::test]
async fn test_retry_after_failed_launch_stops_the_tab_it_actually_opened() {
    let harness = engine().await;
    harness.launchable();
    fail_architect_launch(&harness).await;
    harness.terminals.add_pane(
        "agentflow-TEST-1",
        PaneInfo {
            pane_id: "terminal_4".into(),
            tab_id: 4,
            tab_name: "Architect".into(),
            exited: false,
            exit_status: None,
            command: None,
        },
    );
    run(&harness, spec::ARCHITECT).await.unwrap();
    assert_eq!(closed_tabs(&harness), ["close_tab agentflow-TEST-1 4"]);
    assert_eq!(
        architect_history(&harness),
        (vec!["FAILED".into(), "RUNNING".into()], vec!["stopped".into(), "running".into()])
    );
}

#[tokio::test]
async fn test_retry_refused_without_orphan_run_while_zellij_is_unreachable() {
    let harness = engine().await;
    harness.launchable();
    fail_architect_launch(&harness).await;
    harness.terminals.fail("list_panes", "list-panes failed");
    let error = run(&harness, spec::ARCHITECT).await.unwrap_err();
    assert!(error.message.contains("Cannot establish the existing agent's pane identity"), "{error}");
    assert_eq!(architect_history(&harness), (vec!["FAILED".into()], vec!["running".into()]));
    let tabs = harness.terminals.calls().into_iter().filter(|c| c.starts_with("new_tab")).count();
    assert_eq!(tabs, 1, "only the failed launch opened a tab");
}

/// The broadcast and the HTTP response share one snapshot build (it reads requirement files,
/// hashes them and runs several queries) rather than each computing their own.
#[tokio::test]
async fn test_mutations_build_the_snapshot_once() {
    let harness = engine().await;
    let broadcasts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = broadcasts.clone();
    harness.engine.broadcaster.subscribe(move |message| seen.lock().unwrap().push(message.clone()));
    let layer = agentflow_core::engine::lifecycle::AgentOverride { model: Some("x".into()), ..Default::default() };
    let returned = harness.engine.set_node_override("TEST-1", spec::IMPLEMENTER, &layer).await.unwrap();
    let broadcasts = broadcasts.lock().unwrap();
    assert_eq!(broadcasts.len(), 1);
    assert_eq!(broadcasts[0]["task"], returned);
    let message = returned["events"].as_array().unwrap().last().unwrap()["message"].clone();
    assert_eq!(message, "Implementer configuration override set: {'model': 'x'}");
}

#[test]
fn python_repr_of_strings() {
    use agentflow_core::engine::launch::py_repr;
    assert_eq!(py_repr("x"), "'x'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("both ' and \""), "'both \\' and \"'");
    assert_eq!(py_repr("a\\b\n\u{7}é"), "'a\\\\b\\n\\x07é'");
}

// ------------------------------------------------------------------------------ request contracts
// test_contracts.py and test_questions_terminal.py. The result-model tests of test_contracts.py
// (test_review_result_camel_case, test_final_assessment_risk_normalised) are in engine_review_loop.rs.

#[test]
fn test_finding_blocking_defaults_from_severity() {
    use agentflow_core::engine::agents::{finding_dump, finding_is_blocking};
    let finding = |value: Value| finding_dump(&value).unwrap();
    assert!(finding_is_blocking(&finding(json!({"severity": "high", "title": "x"}))));
    assert!(!finding_is_blocking(&finding(json!({"severity": "low", "title": "x"}))));
    assert!(!finding_is_blocking(&finding(json!({"severity": "critical", "blocking": false, "title": "x"}))));
    assert_eq!(finding(json!({"severity": "Major", "title": "x"}))["severity"], "high");
}

#[test]
fn test_question_choices_and_single_line_reply_validation() {
    use agentflow_core::engine::agents::{clean_answer, clean_question};
    let (message, choices) = clean_question("  Which rollout?  ", Some(&json!([" Gradual ", "All at once"]))).unwrap();
    assert_eq!((message.as_str(), choices), ("Which rollout?", vec!["Gradual".to_string(), "All at once".to_string()]));
    assert_eq!(clean_answer("  Gradual ").unwrap(), "Gradual");
    assert!(clean_answer("one line\nanother line").is_err());
    assert!(clean_question("Pick", Some(&json!(["Same", "same"]))).is_err());
}

// ------------------------------------------------------------------------------ recovery

fn load(engine: &agentflow_core::engine::Engine) -> agentflow_core::engine::store::TaskState {
    engine.db.session(|tx| agentflow_core::engine::store::TaskState::load(tx, "TEST-1")).unwrap()
}

#[tokio::test]
async fn test_restart_requires_explicit_resume_without_new_run() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    harness.engine.recover().await.unwrap();
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "INTERRUPTED");
    let complete = harness.engine.agent_complete("TEST-1", "implementer", Some(run_id), Default::default()).await;
    assert!(complete.is_err());
    harness.engine.recover_node("TEST-1", spec::IMPLEMENTER, false).await.unwrap();
    assert_eq!(node_state(&harness, spec::IMPLEMENTER), "RUNNING");
    let snapshot = harness.engine.snapshot("TEST-1").unwrap();
    let node = snapshot["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::IMPLEMENTER).unwrap();
    assert_eq!(node["lastRun"]["id"], run_id);
    let resumed = writes(&harness);
    assert_eq!(resumed.len(), 1);
    let expected = format!("write_line agentflow-TEST-1 terminal_7 AgentFlow recovered. Continue run {run_id}");
    assert!(resumed[0].starts_with(&expected), "{resumed:?}");
}

/// flowctl saves the result beside credential.json before posting it; when the backend never
/// confirmed receipt, recover() must not apply it (that could run a checkpoint or verification at
/// startup) but lets a human Resume to submit it. Cover discovery and final verification.
#[tokio::test]
async fn test_recover_surfaces_a_completion_saved_to_disk_before_restart() {
    for key in [spec::ARCHITECT, spec::FINAL_ARCHITECT] {
        let harness = engine().await;
        let run_id = harness.add_run(key, "RUNNING", None);
        let run_dir = harness.tmp().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let saved = json!({"role": "architect", "runId": run_id, "result": {"status": "completed", "summary": "Done"}});
        std::fs::write(run_dir.join("pending-complete.json"), saved.to_string()).unwrap();
        harness.with_task(|state| state.runs[0].run_dir = Some(run_dir.to_string_lossy().into_owned()));

        harness.engine.recover().await.unwrap();

        assert_eq!(node_state(&harness, key), "INTERRUPTED");
        let message = harness.load().node(key).unwrap().message.clone().unwrap();
        assert!(message.contains("saved to disk"), "{message}");
        let probes = harness.terminals.calls().into_iter().filter(|c| c.starts_with("list_panes")).count();
        assert_eq!(probes, 0, "no live-session probing needed once a result is on disk");

        harness.engine.recover_node("TEST-1", key, false).await.unwrap();

        assert_eq!(node_state(&harness, key), "COMPLETED");
        assert!(!run_dir.join("pending-complete.json").exists());
    }
}

#[tokio::test]
async fn test_recovery_from_reopened_database_preserves_run_identity() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run_id);
    tab_seven(&harness);
    let restarted = harness.restarted();
    restarted.recover().await.unwrap();
    assert_eq!(load(&restarted).node(spec::IMPLEMENTER).unwrap().state, "INTERRUPTED");
    restarted.recover_node("TEST-1", spec::IMPLEMENTER, false).await.unwrap();
    assert_eq!(load(&restarted).runs.iter().find(|r| r.id == run_id).unwrap().state, "RUNNING");
    assert_eq!(harness.count("node_runs"), 1);
}

#[tokio::test]
async fn retry_after_interruption_fails_the_old_run_and_launches_a_new_one() {
    let harness = engine().await;
    harness.launchable();
    let run_id = harness.add_run(spec::ARCHITECT, "RUNNING", None);
    harness.engine.recover().await.unwrap();
    assert_eq!(node_state(&harness, spec::ARCHITECT), "INTERRUPTED");
    harness.engine.recover_node("TEST-1", spec::ARCHITECT, true).await.unwrap();
    let state = harness.load();
    let old = state.runs.iter().find(|r| r.id == run_id).unwrap();
    assert_eq!((old.state.as_str(), old.error.as_deref()), ("FAILED", Some("Human retried after interruption")));
    assert_eq!(state.runs.len(), 2);
    assert_eq!(node_state(&harness, spec::ARCHITECT), "RUNNING");
}
