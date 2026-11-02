//! Cost estimates from past runs in the engine: a stage's estimate in the task snapshot, and a new
//! task's route estimate for the New Task form. Ported by name from
//! backend/tests/test_cost_estimates.py (the estimator itself is checked in tests/estimates.rs).

mod engine_support;

use agentflow_core::config::BillingMode;
use agentflow_core::db::table::{self, Tracked};
use agentflow_core::db::{DbTime, Json, NodeRunRow, RunEvidenceRow, new_uid};
use agentflow_core::engine::lifecycle::AgentOverride;
use agentflow_core::engine::spec;
use engine_support::{Harness, engine};
use indexmap::IndexMap;
use serde_json::{Value, json};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn node(snapshot: &Value, key: &str) -> Value {
    snapshot["nodes"].as_array().unwrap().iter().find(|n| n["key"] == key).unwrap().clone()
}

/// The implementer's configured harness billed per use.
async fn metered() -> Harness {
    let h = engine().await;
    let harness = node(&h.engine.snapshot("TEST-1").unwrap(), spec::IMPLEMENTER)["effectiveConfig"]["harness"]
        .as_str()
        .unwrap()
        .to_string();
    h.configure(|c| c.harnesses.get_mut(&harness).unwrap().billing_mode = BillingMode::Metered);
    h
}

/// Finished runs of `key` on TEST-1 with the given costs, an hour apart on November 1, oldest first.
fn finished_runs(h: &Harness, key: &str, costs: &[f64], model: Option<&str>, state: &str) {
    finished_runs_on(h, 1, key, costs, model, state);
}

/// `finished_runs` on November `day`.
fn finished_runs_on(h: &Harness, day: u32, key: &str, costs: &[f64], model: Option<&str>, state: &str) {
    let effective = node(&h.engine.snapshot("TEST-1").unwrap(), key)["effectiveConfig"].clone();
    let harness = effective["harness"].as_str().map(String::from);
    let model = model.map(String::from).or_else(|| effective["model"].as_str().map(String::from));
    h.with_task(|state_| {
        let node_id = state_.node(key).unwrap().id;
        for (index, cost) in costs.iter().enumerate() {
            let hour = format!("2026-11-{day:02} {:02}:00:00", index);
            let done = format!("2026-11-{day:02} {:02}:30:00", index);
            let mut run = Tracked::new(NodeRunRow {
                id: 0,
                task_id: state_.task.id,
                node_id,
                run_number: index as i64 + 1,
                iteration: 1,
                state: state.into(),
                harness: harness.clone(),
                model: model.clone(),
                reasoning: None,
                run_dir: None,
                prompt_path: None,
                result_path: None,
                expected_sha: Some(SHA.into()),
                result: Json(Value::Null),
                exit_code: None,
                error: None,
                started_at: DbTime::parse(&hour).unwrap(),
                completed_at: Some(DbTime::parse(&done).unwrap()),
            });
            h.engine.db.session(|tx| run.flush(tx)).unwrap();
            let mut evidence = RunEvidenceRow {
                uid: new_uid(),
                run_id: run.id,
                requirements_digest: None,
                verification: Json(Value::Null),
                usage: Json(json!({"costUsd": cost, "totalTokens": null})),
                confirmed_model: None,
                billing_mode: "metered".into(),
            };
            h.engine.db.session(|tx| table::insert(tx, &mut evidence)).unwrap();
            state_.runs.push(run);
        }
    });
}

fn estimate(h: &Harness, key: &str) -> Value {
    node(&h.engine.snapshot("TEST-1").unwrap(), key)["costEstimate"].clone()
}

#[tokio::test]
async fn test_snapshot_estimates_each_agent_stage_from_its_past_runs() {
    let h = metered().await;
    assert_eq!(estimate(&h, spec::IMPLEMENTER)["enough"], false);
    assert_eq!(estimate(&h, spec::REQUIREMENTS_GATE), Value::Null);

    finished_runs(&h, spec::IMPLEMENTER, &[0.25, 0.75, 0.5], None, "COMPLETED");
    finished_runs(&h, spec::IMPLEMENTER, &[9.0, 9.0, 9.0], Some("another-model"), "COMPLETED");
    finished_runs(&h, spec::CODE_REVIEW, &[9.0, 9.0, 9.0], None, "COMPLETED");
    let result = estimate(&h, spec::IMPLEMENTER);
    assert_eq!(
        (result["typical"].as_f64(), result["high"].as_f64(), &result["runs"], &result["unit"]),
        (Some(0.5), Some(0.75), &json!(3), &json!("usd"))
    );
}

#[tokio::test]
async fn test_the_most_recent_runs_are_the_ones_counted() {
    let h = metered().await;
    finished_runs_on(&h, 1, spec::IMPLEMENTER, &[9.0; 3], None, "COMPLETED"); // older than the last 20
    finished_runs_on(&h, 2, spec::IMPLEMENTER, &[1.0; 20], None, "COMPLETED");
    assert_eq!(estimate(&h, spec::IMPLEMENTER)["high"].as_f64(), Some(1.0));
}

#[tokio::test]
async fn test_failed_runs_count_and_running_ones_do_not() {
    let h = metered().await;
    finished_runs(&h, spec::IMPLEMENTER, &[1.0, 2.0], None, "FAILED");
    finished_runs(&h, spec::IMPLEMENTER, &[3.0], None, "RUNNING");
    assert_eq!(estimate(&h, spec::IMPLEMENTER)["runs"], 2);
    finished_runs(&h, spec::IMPLEMENTER, &[3.0], None, "COMPLETED");
    assert_eq!(estimate(&h, spec::IMPLEMENTER)["typical"].as_f64(), Some(2.0));
}

#[tokio::test]
async fn test_an_estimate_never_changes_the_budget() {
    let h = metered().await;
    let before = h.engine.snapshot("TEST-1").unwrap()["budget"].clone();
    h.engine.cost_estimate(Some("direct"), &IndexMap::new()).unwrap();
    let after = h.engine.snapshot("TEST-1").unwrap()["budget"].clone();
    assert_eq!(before["task"], after["task"]);
    assert_eq!(before["daily"], after["daily"]);
}

#[tokio::test]
async fn test_route_estimate_lists_each_routes_stages_with_the_agents_they_would_get() {
    let h = metered().await;
    finished_runs(&h, spec::IMPLEMENTER, &[0.25, 0.5, 0.75], None, "COMPLETED");
    let direct = h.engine.cost_estimate(Some("direct"), &IndexMap::new()).unwrap();
    let keys: Vec<&str> = direct["stages"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["architect", "implementer", "code_review", "security_review", "final_architect"]);
    assert_eq!(direct["onePass"]["usd"], json!({"typical": 0.5, "high": 0.75}));
    assert_eq!(direct["perWorker"], Value::Null);
    assert_eq!(direct["perFixRound"]["usd"], json!({"typical": 0.5, "high": 0.75}));
    let missing = direct["missing"].as_array().unwrap();
    assert!(missing.contains(&json!("Architect")) && !missing.contains(&json!("Implementer")));

    let parallel = h.engine.cost_estimate(Some("parallel"), &IndexMap::new()).unwrap();
    let stages: Vec<(&str, &str)> = parallel["stages"].as_array().unwrap()[1..3]
        .iter()
        .map(|s| (s["key"].as_str().unwrap(), s["label"].as_str().unwrap()))
        .collect();
    assert_eq!(stages, [("orchestrator", "Architect Planning"), ("worker", "Worker")]);
    assert_eq!(parallel["perWorker"], json!({"usd": null, "tokens": null}), "no worker history yet");

    // A different implementer harness on the form has no history of its own.
    let mut overrides = IndexMap::new();
    overrides.insert("implementer".to_string(), AgentOverride { harness: Some("claude".into()), ..Default::default() });
    let other = h.engine.cost_estimate(Some("direct"), &overrides).unwrap();
    let stage = other["stages"].as_array().unwrap().iter().find(|s| s["key"] == "implementer").unwrap().clone();
    assert_eq!((&stage["harness"], &stage["estimate"]["enough"]), (&json!("claude"), &json!(false)));
    let configured = json!(h.config().workflow.implementation_mode);
    assert_eq!(h.engine.cost_estimate(None, &IndexMap::new()).unwrap()["implementationMode"], configured);
}
