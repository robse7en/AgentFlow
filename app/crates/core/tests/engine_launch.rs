//! Launch policy, terminal windows and screen tracking, ported by name from test_budgets.py,
//! test_security_improvements.py and test_workflow_safety.py, plus the screen tracker's own tests
//! (Python had none for it).

mod engine_support;

use std::time::Duration;

use agentflow_core::engine::launch::{MeteredApproval, RunNodeRequest};
use agentflow_core::engine::lifecycle::AgentOverride;
use agentflow_core::engine::{rules, spec};
use agentflow_core::terminal::Terminals;
use axum::body::Body;
use axum::http::Request;
use engine_support::{Harness, add_session, engine};
use serde_json::{Value, json};
use tower::ServiceExt;

fn node(snapshot: &Value, key: &str) -> Value {
    snapshot["nodes"].as_array().unwrap().iter().find(|n| n["key"] == key).unwrap().clone()
}

fn set_node(harness: &Harness, key: &str, state: &str) {
    harness.with_task(|task| {
        let index = task.node_index(key).unwrap();
        task.nodes[index].state = state.into();
    });
}

async fn run_with(harness: &Harness, key: &str, request: RunNodeRequest) -> Value {
    harness.engine.run_node("TEST-1", key, request, false).await.unwrap()
}

#[test]
fn test_budget_blocks_next_launch_including_reservations() {
    let mut status = json!({"task": {"knownUsd": 2, "reservedUsd": 2, "unknownRuns": 0, "limitUsd": 5},
                            "daily": {"knownUsd": 0, "reservedUsd": 0, "unknownRuns": 0, "limitUsd": null},
                            "launchReserveUsd": 2});
    assert!(rules::launch_blocker(&status).unwrap().contains("Ticket"));
    status["task"]["limitUsd"] = Value::Null;
    assert_eq!(rules::launch_blocker(&status), None);
    status["daily"]["limitUsd"] = json!(10);
    status["daily"]["unknownRuns"] = json!(1);
    assert!(rules::launch_blocker(&status).unwrap().contains("incomplete"));
}

#[tokio::test]
async fn test_metered_approval_is_for_exact_model_and_does_not_bypass_budget() {
    let harness = engine().await;
    harness.launchable();
    harness.configure(|config| {
        config.workflow.cost_controls.require_metered_approval = true;
        config.harnesses.get_mut("claude").unwrap().billing_mode = agentflow_core::config::BillingMode::Metered;
    });
    set_node(&harness, spec::IMPLEMENTER, "READY");
    let runs = |harness: &Harness| harness.load().runs.len();

    let snapshot = run_with(&harness, spec::IMPLEMENTER, RunNodeRequest::default()).await;
    assert!(node(&snapshot, spec::IMPLEMENTER)["message"].as_str().unwrap().contains("approval"));
    let model = node(&snapshot, spec::IMPLEMENTER)["effectiveConfig"]["model"].as_str().map(String::from);
    let approval = |harness_key: &str| RunNodeRequest {
        metered_approval: Some(MeteredApproval {
            harness: harness_key.into(),
            model: model.clone(),
            reason: "Complex change".into(),
        }),
        ..Default::default()
    };
    run_with(&harness, spec::IMPLEMENTER, approval("wrong")).await;
    assert_eq!(runs(&harness), 0);
    run_with(&harness, spec::IMPLEMENTER, approval("claude")).await;
    assert_eq!(runs(&harness), 1);

    harness.with_task(|state| state.runs[0].state = "COMPLETED".into());
    set_node(&harness, spec::IMPLEMENTER, "READY");
    harness.configure(|config| config.workflow.cost_controls.task_budget_usd = Some(0.5));
    run_with(&harness, spec::IMPLEMENTER, approval("claude")).await;
    assert_eq!(runs(&harness), 1, "an approval does not bypass the budget");
}

#[tokio::test]
async fn test_agent_cannot_change_budgets_billing_or_approve_metered_launch() {
    let harness = engine().await;
    let run = harness.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&harness, run);
    let app = agentflow_core::api::App::around(harness.engine.clone(), harness.tmp().join("config.json"));
    for (method, path, body) in [
        ("PUT", "/api/config/cost-controls", json!({"controls": {}, "billing": {}})),
        ("PUT", "/api/tasks/TEST-1/budget", json!({"amount": 100})),
        (
            "POST",
            "/api/tasks/TEST-1/nodes/implementer/run",
            json!({"meteredApproval": {"harness": "codex", "reason": "self approval"}}),
        ),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let status = agentflow_core::api::router(app.clone()).oneshot(request).await.unwrap().status();
        assert_eq!(status, 403, "{path}");
    }
}

#[tokio::test]
async fn test_requirements_mutation_blocks_launch_and_final_override() {
    let harness = engine().await;
    let requirements = std::path::PathBuf::from(&harness.load().task.task_dir).join("requirements");
    std::fs::write(requirements.join("acceptance-criteria.md"), "Changed scope").unwrap();
    set_node(&harness, spec::IMPLEMENTER, "READY");
    let error =
        harness.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap_err();
    assert!(error.message.contains("requirements changed"), "{error}");
    set_node(&harness, spec::READY_FOR_PR, "WAITING_FOR_APPROVAL");
    let error = harness
        .engine
        .approve(
            "TEST-1",
            spec::READY_FOR_PR,
            agentflow_core::engine::approvals::ApprovalRequest {
                reason: Some("accept".into()),
                expected_sha: Some(engine_support::SHA.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("requirements changed"));
}

/// Existing installations keep their launch behaviour until cost controls are enabled.
#[tokio::test]
async fn test_usage_does_not_block_launch_when_cost_controls_are_disabled() {
    let harness = engine().await;
    harness.launchable();
    harness.add_run(spec::ARCHITECT, "COMPLETED", None);
    set_node(&harness, spec::IMPLEMENTER, "READY");
    let before = harness.engine.snapshot("TEST-1").unwrap();
    assert_eq!(
        (before["totalUsage"]["runs"].clone(), before["totalUsage"]["runsWithUsage"].clone()),
        (json!(1), json!(0))
    );
    let snapshot = run_with(&harness, spec::IMPLEMENTER, RunNodeRequest::default()).await;
    assert!(!node(&snapshot, spec::IMPLEMENTER)["lastRun"].is_null());
    assert_eq!(
        (snapshot["totalUsage"]["runs"].clone(), snapshot["totalUsage"]["runsWithUsage"].clone()),
        (json!(2), json!(0))
    );
}

#[tokio::test]
async fn test_global_capacity_pauses_launch() {
    let harness = engine().await;
    harness.launchable();
    harness.add_run(spec::CODE_REVIEW, "RUNNING", None);
    harness.configure(|config| config.workflow.max_concurrent_agents = 1);
    set_node(&harness, spec::IMPLEMENTER, "READY");
    let snapshot = run_with(&harness, spec::IMPLEMENTER, RunNodeRequest::default()).await;
    let implementer = node(&snapshot, spec::IMPLEMENTER);
    assert!(implementer["lastRun"].is_null());
    assert!(implementer["message"].as_str().unwrap().contains("Global agent capacity"));
}

#[tokio::test]
async fn test_new_configuration_gets_a_new_process() {
    let harness = engine().await;
    harness.launchable();
    set_node(&harness, spec::IMPLEMENTER, "READY");
    let first = run_with(&harness, spec::IMPLEMENTER, RunNodeRequest::default()).await;
    let run_id = node(&first, spec::IMPLEMENTER)["lastRun"]["id"].as_i64().unwrap();
    harness.with_task(|state| {
        state.runs.iter_mut().find(|r| r.id == run_id).unwrap().state = "COMPLETED".into();
    });
    set_node(&harness, spec::IMPLEMENTER, "READY");
    let changed =
        AgentOverride { model: Some("changed-model".into()), reasoning: Some("low".into()), ..Default::default() };
    run_with(&harness, spec::IMPLEMENTER, RunNodeRequest { r#override: Some(changed), ..Default::default() }).await;
    let calls = harness.terminals.calls();
    assert_eq!(calls.iter().filter(|c| c.starts_with("new_tab")).count(), 2);
    let state = harness.load();
    let last = state.runs.last().unwrap();
    assert_eq!((last.model.as_deref(), last.reasoning.as_deref()), (Some("changed-model"), Some("low")));
    assert_eq!(calls.iter().filter(|c| c.starts_with("write_line")).count(), 0);
}

#[tokio::test]
async fn test_collector_error_leaves_usage_unknown() {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    let script = if cfg!(windows) {
        let path = harness.tmp().join("collector.cmd");
        std::fs::write(&path, "@echo invalid json\r\n").unwrap();
        vec![path.to_string_lossy().into_owned()]
    } else {
        let path = harness.tmp().join("collector.sh");
        std::fs::write(&path, "echo invalid json\n").unwrap();
        vec!["sh".into(), path.to_string_lossy().into_owned()]
    };
    harness.configure(|config| config.harnesses.get_mut("codex").unwrap().usage_command = Some(script));
    harness.with_task(|state| state.runs[0].harness = Some("codex".into()));
    std::fs::create_dir_all(&harness.load().task.worktree_path).unwrap();
    harness.engine.agent_exited("TEST-1", "implementer", Some(run_id), 0).await.unwrap();
    let row = harness.engine.snapshot("TEST-1").unwrap()["executionEvidence"][0].clone();
    assert_eq!((row["usage"].clone(), row["confirmedModel"].clone()), (Value::Null, Value::Null));
}

/// Attaching resurrects an exited session; focusing a tab first would hang on it.
#[tokio::test]
async fn test_opening_a_window_on_an_exited_session_attaches_without_focusing_a_tab() {
    let harness = engine().await;
    let windows = |harness: &Harness| -> Vec<String> {
        harness.terminals.calls().into_iter().filter(|c| c.starts_with("open_window")).collect()
    };
    harness.terminals.set_exited("agentflow-TEST-1");
    let result = harness.engine.open_zellij_window("TEST-1", Some(spec::ARCHITECT)).await.unwrap();
    assert_eq!(windows(&harness), ["open_window agentflow-TEST-1"]);
    assert_eq!(result["tab"], Value::Null);

    harness.terminals.ensure_session("agentflow-TEST-1").await.unwrap();
    harness.engine.open_zellij_window("TEST-1", Some(spec::ARCHITECT)).await.unwrap();
    assert_eq!(windows(&harness).last().unwrap(), "open_window agentflow-TEST-1 Architect");

    harness.terminals.remove_session("agentflow-TEST-1");
    let error = harness.engine.open_zellij_window("TEST-1", Some(spec::ARCHITECT)).await.unwrap_err();
    assert!(error.message.contains("not running"), "{error}");
}

#[tokio::test]
async fn opening_the_embedded_view_focuses_the_agents_tab() {
    let harness = engine().await;
    let result = harness.engine.open_zellij("TEST-1", Some(spec::CODE_REVIEW)).await.unwrap();
    assert_eq!(result["session"], "agentflow-TEST-1");
    assert_eq!(result["attachCommand"], "zellij attach agentflow-TEST-1");
    assert!(harness.terminals.calls().contains(&"focus_tab agentflow-TEST-1 Code Review".to_string()));
}

#[tokio::test]
async fn revealing_refuses_unknown_targets_and_missing_paths() {
    let harness = engine().await;
    let unknown = harness.engine.reveal_path("TEST-1", "home").await.unwrap_err();
    assert_eq!((unknown.status, unknown.message.as_str()), (400, "Unknown reveal target: home"));
    let missing = harness.engine.reveal_path("TEST-1", "worktree").await.unwrap_err();
    assert_eq!(missing.status, 404);
    assert!(missing.message.starts_with("Path no longer exists: "), "{missing}");
}

// ------------------------------------------------------------------------------ screen tracking

async fn settle() {
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A launched agent's screen is classified as frames arrive; only a change is stored and
/// broadcast, and stopping the agent stops its tracker.
#[tokio::test(flavor = "multi_thread")]
async fn screen_states_are_recorded_on_change_and_tracking_stops_with_the_agent() {
    let harness = engine().await;
    harness.launchable();
    let broadcasts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = broadcasts.clone();
    harness.engine.broadcaster.subscribe(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    run_with(&harness, spec::ARCHITECT, RunNodeRequest::default()).await;
    settle().await;
    let agent = harness.load().agents[0].row.clone();
    let pane = agent.zellij_pane_id.clone().unwrap();
    assert!(harness.engine.screen_trackers.is_tracking(agent.id));
    // The first (empty) frame classifies as unknown.
    assert_eq!(harness.load().agents[0].screen_state.as_deref(), Some("unknown"));

    let working = ["✻ Thinking… (3s · esc to interrupt)"];
    harness.terminals.push_screen(&pane, &working);
    settle().await;
    assert_eq!(harness.load().agents[0].screen_state.as_deref(), Some("working"));
    let after_change = broadcasts.load(std::sync::atomic::Ordering::SeqCst);
    harness.terminals.push_screen(&pane, &working);
    settle().await;
    assert_eq!(
        broadcasts.load(std::sync::atomic::Ordering::SeqCst),
        after_change,
        "an unchanged state is not republished"
    );

    harness.engine.cancel_task("TEST-1").await.unwrap();
    assert!(!harness.engine.screen_trackers.is_tracking(agent.id));
}
