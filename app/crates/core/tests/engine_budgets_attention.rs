//! Remaining named test_budgets and test_attention cases against the real engine and HTTP routes.
mod engine_support;
use agentflow_core::config::{BillingMode, Role};
use agentflow_core::db::table;
use agentflow_core::db::{Json, RunEvidenceRow, new_uid};
use agentflow_core::engine::{snapshot, spec};
use engine_support::{Harness, engine};
use serde_json::{Value, json};

fn evidence(h: &Harness, run: i64, mode: &str, usage: Value) {
    let mut row = RunEvidenceRow {
        uid: new_uid(),
        run_id: run,
        requirements_digest: None,
        verification: Json(Value::Null),
        usage: Json(usage),
        confirmed_model: None,
        billing_mode: mode.into(),
    };
    h.engine.db.session(|tx| table::insert(tx, &mut row)).unwrap();
}
fn spend(h: &Harness) -> Value {
    h.engine.snapshot("TEST-1").unwrap()["budget"]["task"].clone()
}

#[tokio::test]
async fn test_subscription_is_excluded_and_parallel_runs_reserve_capacity() {
    let h = engine().await;
    h.configure(|c| c.workflow.cost_controls.launch_reserve_usd = agentflow_core::config::Float::new(2.0));
    let a = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let b = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    h.add_run(spec::SECURITY_REVIEW, "RUNNING", None);
    evidence(&h, a, "subscription", json!({"costUsd":100}));
    evidence(&h, b, "metered", Value::Null);
    let s = spend(&h);
    assert_eq!(s["knownUsd"], 0.0);
    assert_eq!(s["reservedUsd"], 4.0);
    assert_eq!(s["unknownRuns"], 0);
}
#[tokio::test]
async fn test_unclassified_reported_cost_is_not_claimed_as_known_api_spend() {
    let h = engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    evidence(&h, run, "unknown", json!({"costUsd":100}));
    let s = spend(&h);
    assert_eq!(s["knownUsd"], 0.0);
    assert_eq!(s["reservedUsd"], 0.0);
    assert_eq!(s["unknownRuns"], 1);
}
#[tokio::test]
async fn test_list_price_estimate_is_not_known_api_spend() {
    let h = engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    evidence(&h, run, "metered", json!({"costUsd":5,"costKind":"estimated"}));
    assert_eq!(spend(&h)["knownUsd"], 0.0);
    assert_eq!(spend(&h)["unknownRuns"], 1);
    let report = agentflow_core::engine::usage::UsageReport::validate(
        &json!({"model":"measured","source":"provider","costUsd":5,"costKind":"billed"}),
    )
    .unwrap();
    h.engine.import_usage("TEST-1", run, &report, None).await.unwrap();
    assert_eq!(spend(&h)["knownUsd"], 5.0);
}
#[tokio::test]
async fn test_launch_pauses_without_creating_a_run_when_budget_exhausted() {
    let h = engine().await;
    h.launchable();
    h.configure(|c| c.workflow.cost_controls.task_budget_usd = Some(1.0));
    let run = h.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    evidence(&h, run, "metered", json!({"costUsd":1}));
    h.with_task(|s| {
        let n = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[n].state = "READY".into();
    });
    h.engine.run_node("TEST-1", spec::IMPLEMENTER, Default::default(), false).await.unwrap();
    assert_eq!(h.load().runs.len(), 1);
    assert!(h.load().node(spec::IMPLEMENTER).unwrap().message.as_ref().unwrap().contains("budget"));
}
#[tokio::test]
async fn test_copilot_routing_preserves_explicit_overrides_and_does_not_change_defaults() {
    let h = engine().await;
    h.usable_harnesses();
    h.configure(|c| {
        c.harnesses.get_mut("copilot").unwrap().default_model = Some("copilot-test-model".into());
        c.harnesses.get_mut("copilot").unwrap().billing_mode = BillingMode::Subscription;
        c.workflow.cost_controls.copilot_roles = vec![Role::Implementer];
    });
    let cfg = |h: &Harness| {
        let s = h.load();
        let refs = s.nodes.iter().map(|n| &n.row).collect::<Vec<_>>();
        snapshot::resolve_agent_config(
            &h.config(),
            &h.engine.harnesses(),
            &s.task,
            &refs,
            s.node(spec::IMPLEMENTER).unwrap(),
            None,
        )
    };
    assert_eq!(cfg(&h)["harness"], "copilot");
    h.with_task(|s| {
        let n = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[n].config_override = Json(json!({"harness":"codex"}));
    });
    assert_eq!(cfg(&h)["harness"], "codex");
    assert_ne!(cfg(&h)["model"], "copilot-test-model");
    h.with_task(|s| {
        let n = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[n].config_override = Json(json!({"harness":"codex","model":"explicit"}));
    });
    assert_eq!(cfg(&h)["model"], "explicit");
}
#[tokio::test]
async fn test_human_can_set_ticket_budget_and_correct_billing_without_session_import() {
    let h = engine().await;
    let (status, snap) = h.route("PUT", "/api/tasks/TEST-1/budget", Some(json!({"amount":5}))).await;
    assert_eq!(status, 200);
    assert_eq!(snap["budget"]["task"]["limitUsd"], 5.0);
    assert_eq!(h.route("PUT", "/api/tasks/MISSING-1/budget", Some(json!({"amount":5}))).await.0, 404);
    let run = h.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    let path = format!("/api/tasks/TEST-1/runs/{run}/usage");
    let mut report = json!({"inputTokens":100,"outputTokens":10,"model":"measured","source":"provider","costUsd":0.25,"billingMode":"metered","scope":"session"});
    assert_eq!(h.route("PUT", &path, Some(report.clone())).await.0, 422);
    report["scope"] = json!("run");
    let (status, snap) = h.route("PUT", &path, Some(report)).await;
    assert_eq!(status, 200);
    assert_eq!(snap["totalUsage"]["apiCostUsd"], 0.25);
    assert_eq!(snap["budget"]["task"]["knownUsd"], 0.25);
}
#[tokio::test]
async fn test_concurrent_tickets_cannot_both_take_the_last_daily_reservation() {
    let h = engine().await;
    h.launchable();
    h.configure(|c| {
        c.workflow.cost_controls.daily_budget_usd = Some(1.0);
        c.workflow.cost_controls.launch_reserve_usd = agentflow_core::config::Float::new(1.0);
        c.harnesses.get_mut("claude").unwrap().billing_mode = BillingMode::Metered;
    });
    let first = h.load();
    let mut task = first.task.row.clone();
    task.id = 0;
    task.jira_key = "TEST-2".into();
    task.branch_name = "TEST-2".into();
    task.zellij_session_name = "TEST-2".into();
    let mut node = first.node(spec::ARCHITECT).unwrap().clone();
    node.id = 0;
    h.engine
        .db
        .session(|tx| {
            table::insert(tx, &mut task)?;
            node.task_id = task.id;
            table::insert(tx, &mut node)
        })
        .unwrap();
    let (a, b) = tokio::join!(
        h.engine.run_node("TEST-1", spec::ARCHITECT, Default::default(), false),
        h.engine.run_node("TEST-2", spec::ARCHITECT, Default::default(), false)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(h.count("node_runs"), 1);
}
#[tokio::test]
async fn test_parallel_question_wins_over_running_agent_and_ignores_old_questions() {
    let h = engine().await;
    let old = h.add_run(spec::CODE_REVIEW, "WAITING_FOR_USER", None);
    h.engine.agent_question("TEST-1", "code_reviewer", Some(old), "Old question", &[]).await.unwrap();
    let run = h.add_run(spec::CODE_REVIEW, "WAITING_FOR_USER", None);
    h.engine.agent_question("TEST-1", "code_reviewer", Some(run), "Current question", &[]).await.unwrap();
    h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let snap = h.engine.snapshot("TEST-1").unwrap();
    let attention = snap["attention"].as_array().unwrap();
    assert_eq!(attention.len(), 1);
    assert_eq!(attention[0]["nodeKey"], spec::CODE_REVIEW);
    assert_eq!(attention[0]["questions"], 1);
    h.set_status("CANCELLED");
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["attention"], json!([]));
}
