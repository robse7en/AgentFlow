//! Task creation surfaces a base branch that couldn't be refreshed from origin. Port of
//! backend/tests/test_task_base_warning.py: same test names, same assertions.

mod engine_support;

use agentflow_core::engine::lifecycle::CreateTaskRequest;
use agentflow_core::vcs::Outcome;
use serde_json::{Value, json};

const WARNING: &str = "Couldn't fetch main from origin, so this task branched from the last fetched copy of main, \
                       which may be missing newer commits.";

async fn create(h: &engine_support::Harness, created: Value) -> Value {
    h.git.returns("inspect_repository", Outcome::Ok(json!({"path": h.tmp(), "root": h.tmp(), "branches": ["main"]})));
    h.git.returns("create_worktree", Outcome::Ok(created));
    let request: CreateTaskRequest =
        serde_json::from_value(json!({"jiraKey": "NEW-1", "repositoryPath": h.tmp(), "baseBranch": "main"})).unwrap();
    h.engine.create_task(request.validated().unwrap()).await.unwrap()
}

fn warnings(snapshot: &Value) -> Vec<String> {
    snapshot["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["level"] == "warning")
        .map(|e| e["message"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn test_a_base_that_could_not_be_fetched_is_flagged_on_the_new_task() {
    let h = engine_support::engine().await;
    let snapshot = create(&h, json!({"branch": "agent/NEW-1", "base_warning": WARNING})).await;
    assert_eq!(snapshot["statusMessage"], WARNING);
    assert!(warnings(&snapshot).contains(&WARNING.to_string()), "{:?}", warnings(&snapshot));
}

#[tokio::test]
async fn test_a_fetched_base_leaves_the_new_task_without_a_warning() {
    let h = engine_support::engine().await;
    let snapshot = create(&h, json!({"branch": "agent/NEW-1"})).await;
    assert!(snapshot["statusMessage"].is_null());
    assert!(!warnings(&snapshot).contains(&WARNING.to_string()));
}
