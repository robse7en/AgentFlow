//! workflow.maxAutomaticRetries: a stage whose harness exited without reporting completion is
//! launched again by itself, a limited number of times, through the same checks as a human's
//! Retry. Ported by name from backend/tests/test_automatic_retry.py.

mod engine_support;

use std::time::Duration;

use agentflow_core::config::BillingMode;
use agentflow_core::db::NodeRunRow;
use agentflow_core::engine::launch::RunNodeRequest;
use agentflow_core::engine::spec;
use agentflow_core::vcs::Outcome;
use engine_support::{Harness, engine};
use serde_json::json;

const HEADING: &str = "## Previous attempt at this stage";

async fn launching() -> Harness {
    let h = engine().await;
    h.launchable();
    h.git.returns("changed_paths", Outcome::Ok(json!([])));
    h.git.returns("dirty_paths", Outcome::Ok(json!([])));
    h
}

async fn retrying() -> Harness {
    let h = launching().await;
    h.configure(|c| c.workflow.max_automatic_retries = 1);
    h.engine.set_automatic_retry_delay(Duration::ZERO);
    h
}

async fn start(h: &Harness, key: &str) -> i64 {
    h.with_task(|state| {
        let index = state.node_index(key).unwrap();
        state.nodes[index].state = "READY".into();
    });
    h.engine.run_node("TEST-1", key, RunNodeRequest::default(), false).await.unwrap();
    latest_run(h, key).id
}

fn latest_run(h: &Harness, key: &str) -> NodeRunRow {
    let state = h.load();
    let node_id = state.node(key).unwrap().id;
    state.runs.iter().rev().find(|r| r.node_id == node_id).unwrap().row.clone()
}

fn run_count(h: &Harness, key: &str) -> usize {
    let state = h.load();
    let node_id = state.node(key).unwrap().id;
    state.runs.iter().filter(|r| r.node_id == node_id).count()
}

fn node_field(h: &Harness, key: &str, field: &str) -> String {
    let state = h.load();
    let node = state.node(key).unwrap();
    match field {
        "state" => node.state.clone(),
        "message" => node.message.clone().unwrap_or_default(),
        "error" => node.error.clone().unwrap_or_default(),
        _ => unreachable!(),
    }
}

fn logs(h: &Harness) -> Vec<String> {
    h.engine
        .db
        .session(|tx| {
            let mut query = tx.prepare("SELECT message FROM workflow_events ORDER BY id")?;
            query.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
}

async fn exited(h: &Harness, run_id: i64, code: i64) {
    h.engine.agent_exited("TEST-1", "implementer", Some(run_id), code).await.unwrap();
}

fn prompt_of_latest_run(h: &Harness) -> String {
    std::fs::read_to_string(latest_run(h, spec::IMPLEMENTER).prompt_path.unwrap()).unwrap()
}

#[tokio::test]
async fn test_off_by_default_a_harness_exit_stays_failed() {
    let h = launching().await;
    let run_id = start(&h, spec::IMPLEMENTER).await;
    exited(&h, run_id, 1).await;
    assert!(!h.engine.automatic_retry_pending(run_id));
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "FAILED");
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "message"), "Harness exited (code 1)");
}

#[tokio::test]
async fn test_a_harness_exit_is_retried_once_with_a_fresh_briefing() {
    let h = retrying().await;
    let first = start(&h, spec::IMPLEMENTER).await;
    h.engine.set_automatic_retry_delay(Duration::from_secs(3600));
    exited(&h, first, 1).await;
    assert_eq!(
        node_field(&h, spec::IMPLEMENTER, "message"),
        "Harness exited (code 1); automatic retry 1 of 1 starts shortly"
    );
    assert!(h.engine.automatic_retry_pending(first));
    h.engine.shutdown();
    h.engine.automatic_retry("TEST-1", spec::IMPLEMENTER, first, 1).await;

    let second = latest_run(&h, spec::IMPLEMENTER);
    assert!(second.id != first && second.state == "RUNNING");
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "RUNNING");
    assert!(prompt_of_latest_run(&h).contains(HEADING), "the retry is told what the failed attempt did");
    assert!(logs(&h).contains(&"Implementer: automatic retry 1 of 1 after the harness exited".to_string()));

    exited(&h, second.id, 1).await;
    assert!(!h.engine.automatic_retry_pending(second.id));
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "FAILED");
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 2);
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "message"), "Harness exited (code 1)");
    assert!(logs(&h).iter().any(|l| l.contains("not retried automatically: it has failed 2 times in a row")));
}

#[tokio::test]
async fn test_the_background_retry_launches_after_the_delay() {
    let h = retrying().await;
    let first = start(&h, spec::IMPLEMENTER).await;
    exited(&h, first, 1).await;
    h.engine.join_automatic_retry(first).await;
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 2);
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "RUNNING");
}

#[tokio::test]
async fn test_a_reported_failure_or_a_later_exit_is_not_retried() {
    let h = retrying().await;
    let run_id = start(&h, spec::IMPLEMENTER).await;
    h.engine.agent_fail("TEST-1", "implementer", Some(run_id), "Cannot build the project").await.unwrap();
    // The launcher still reports its exit afterwards; the run is no longer active.
    exited(&h, run_id, 0).await;
    assert!(!h.engine.automatic_retry_pending(run_id));
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "FAILED");
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 1);
}

#[tokio::test]
async fn test_a_human_retry_or_cancel_during_the_delay_wins() {
    let h = retrying().await;
    h.engine.set_automatic_retry_delay(Duration::from_secs(3600));
    let first = start(&h, spec::IMPLEMENTER).await;
    exited(&h, first, 1).await;
    assert!(h.engine.automatic_retry_pending(first));
    // The human clicked Retry first.
    h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
    h.engine.automatic_retry("TEST-1", spec::IMPLEMENTER, first, 1).await;
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 2);

    let human = latest_run(&h, spec::IMPLEMENTER).id;
    h.configure(|c| c.workflow.max_automatic_retries = 3);
    exited(&h, human, 1).await;
    assert!(h.engine.automatic_retry_pending(human));
    h.engine.cancel_task("TEST-1").await.unwrap();
    h.engine.automatic_retry("TEST-1", spec::IMPLEMENTER, human, 2).await;
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 2, "a cancelled task keeps its failed stage, but nothing relaunches");
}

#[tokio::test]
async fn test_no_retry_once_the_task_needs_human_review() {
    let h = retrying().await;
    let run_id = start(&h, spec::IMPLEMENTER).await;
    h.set_status("NEEDS_HUMAN_REVIEW");
    exited(&h, run_id, 1).await;
    assert!(!h.engine.automatic_retry_pending(run_id));
}

#[tokio::test]
async fn test_metered_approval_keeps_the_stage_failed() {
    let h = retrying().await;
    let first = start(&h, spec::IMPLEMENTER).await;
    h.configure(|c| {
        c.workflow.cost_controls.require_metered_approval = true;
        c.harnesses.get_mut("claude").unwrap().billing_mode = BillingMode::Metered;
    });
    exited(&h, first, 1).await;
    h.engine.join_automatic_retry(first).await;
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "FAILED");
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 1);
    let message = node_field(&h, spec::IMPLEMENTER, "message");
    assert!(message.starts_with("Not retried automatically: API launch needs approval"), "{message}");
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "error"), "Harness exited with code 1 before reporting completion");
}

#[tokio::test]
async fn test_global_capacity_keeps_the_stage_failed() {
    let h = retrying().await;
    let first = start(&h, spec::IMPLEMENTER).await;
    h.configure(|c| c.workflow.max_concurrent_agents = 1);
    h.add_run(spec::CODE_REVIEW, "RUNNING", None); // another stage holds the only slot
    exited(&h, first, 1).await;
    h.engine.join_automatic_retry(first).await;
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "FAILED");
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 1);
    assert_eq!(
        node_field(&h, spec::IMPLEMENTER, "message"),
        "Not retried automatically: Global agent capacity reached; start again when a slot is free"
    );
}

#[tokio::test]
async fn test_a_failed_automatic_launch_is_not_retried_again() {
    let h = retrying().await;
    let first = start(&h, spec::IMPLEMENTER).await;
    h.terminals.fail("new_tab", "zellij is gone");
    h.configure(|c| c.workflow.max_automatic_retries = 3);
    exited(&h, first, 1).await;
    h.engine.join_automatic_retry(first).await;
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "state"), "FAILED");
    assert_eq!(node_field(&h, spec::IMPLEMENTER, "message"), "Automatic retry failed");
    assert!(node_field(&h, spec::IMPLEMENTER, "error").contains("zellij is gone"));
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 2);
    let second = latest_run(&h, spec::IMPLEMENTER).id;
    assert!(!h.engine.automatic_retry_pending(second));
}

#[tokio::test]
async fn test_shutdown_cancels_a_pending_retry() {
    let h = retrying().await;
    h.engine.set_automatic_retry_delay(Duration::from_millis(200));
    let first = start(&h, spec::IMPLEMENTER).await;
    exited(&h, first, 1).await;
    assert!(h.engine.automatic_retry_pending(first));
    h.engine.shutdown();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(run_count(&h, spec::IMPLEMENTER), 1);
}
