//! Archive, delete and cancel-from-post-ready, ported by name from
//! backend/tests/test_task_archive_delete.py.

mod engine_support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use agentflow_core::db::table::{self, Tracked};
use agentflow_core::db::{
    AgentAccessRow, AgentQuestionRow, AgentSessionRow, ChatThreadRow, DbTime, DeletedRunSpendRow,
    ImplementationWorkItemRow, Json, ProcessLeaseRow, ReviewFindingRow, RunEvidenceRow, TaskArtifactRow,
};
use agentflow_core::engine::lifecycle::CreateTaskRequest;
use agentflow_core::engine::spec;
use agentflow_core::vcs::Outcome;
use engine_support::{Harness, SHA, engine};
use serde_json::{Value, json};

const TABLES: [&str; 14] = [
    "tasks",
    "workflow_nodes",
    "node_runs",
    "agent_sessions",
    "process_leases",
    "agent_access",
    "run_evidence",
    "requirements_approvals",
    "implementation_work_items",
    "chat_threads",
    "review_findings",
    "agent_questions",
    "task_artifacts",
    "workflow_events",
];

// cancel from Ready for PR / PR Created ----------------------------------------------------------

#[tokio::test]
async fn test_cancel_retires_a_task_parked_before_merge() {
    for status in ["READY_FOR_PR", "PR_CREATED"] {
        let harness = engine().await;
        harness.set_status(status);
        let snapshot = harness.engine.cancel_task("TEST-1").await.unwrap();
        assert_eq!(snapshot["status"], "CANCELLED", "{status}");
    }
}

#[tokio::test]
async fn test_a_merged_task_cannot_be_cancelled() {
    let harness = engine().await;
    harness.set_status("MERGED");
    let error = harness.engine.cancel_task("TEST-1").await.unwrap_err();
    assert!(error.message.contains("already reached MERGED"), "{error}");
}

// archive ----------------------------------------------------------------------------------------

#[tokio::test]
async fn test_archive_requires_a_finished_task() {
    let harness = engine().await;
    assert!(harness.engine.archive_task("TEST-1").await.unwrap_err().message.contains("merged or cancelled"));
    harness.set_status("READY_FOR_PR");
    assert!(harness.engine.archive_task("TEST-1").await.unwrap_err().message.contains("merged or cancelled"));
}

fn keys(tasks: &[Value]) -> Vec<&str> {
    tasks.iter().map(|t| t["jiraKey"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn test_archived_tasks_leave_the_default_list_and_come_back() {
    let harness = engine().await;
    let engine = &harness.engine;
    harness.set_status("MERGED");
    let snapshot = engine.archive_task("TEST-1").await.unwrap();
    assert!(snapshot["archivedAt"].is_string());
    assert!(engine.list_tasks("exclude").await.unwrap().is_empty());
    assert_eq!(keys(&engine.list_tasks("only").await.unwrap()), ["TEST-1"]);
    assert_eq!(keys(&engine.list_tasks("include").await.unwrap()), ["TEST-1"]);

    let snapshot = engine.unarchive_task("TEST-1").await.unwrap();
    assert!(snapshot["archivedAt"].is_null());
    assert_eq!(keys(&engine.list_tasks("exclude").await.unwrap()), ["TEST-1"]);
    assert!(engine.list_tasks("only").await.unwrap().is_empty());
}

#[tokio::test]
async fn test_creating_an_archived_ticket_again_says_how_to_proceed() {
    let harness = engine().await;
    harness.set_status("CANCELLED");
    harness.engine.archive_task("TEST-1").await.unwrap();
    let request: CreateTaskRequest =
        serde_json::from_value(json!({"jiraKey": "TEST-1", "repositoryPath": ".", "baseBranch": "main"})).unwrap();
    let error = harness.engine.create_task(request.validated().unwrap()).await.unwrap_err();
    assert!(error.message.contains("archived. Unarchive or delete"), "{error}");
}

#[tokio::test]
async fn test_archive_routes_and_list_filter() {
    let harness = engine().await;
    harness.set_status("CANCELLED");
    assert!(harness.route("POST", "/api/tasks/TEST-1/archive", None).await.1["archivedAt"].is_string());
    assert_eq!(harness.route("GET", "/api/tasks", None).await.1, json!([]));
    assert_eq!(keys(harness.route("GET", "/api/tasks?archived=only", None).await.1.as_array().unwrap()), ["TEST-1"]);
    assert_eq!(harness.route("GET", "/api/tasks?archived=bogus", None).await.0, 422);
    assert!(harness.route("POST", "/api/tasks/TEST-1/unarchive", None).await.1["archivedAt"].is_null());
}

// spend ledger ----------------------------------------------------------------------------------

#[test]
fn test_deleted_runs_still_count_toward_today() {
    let now = DbTime::now();
    let two_days_ago = DbTime(now.0 - chrono::TimeDelta::days(2));
    let row = |charged_at, billing_mode: &str, cost_usd| DeletedRunSpendRow {
        id: 0,
        jira_key: "X".into(),
        run_id: 0,
        charged_at,
        billing_mode: billing_mode.into(),
        cost_usd,
    };
    let deleted = vec![
        row(now, "metered", Some(2.5)),
        row(now, "unknown", None),
        row(now, "subscription", None),
        row(two_days_ago, "metered", Some(100.0)),
    ];
    let controls: agentflow_core::config::CostControls = serde_json::from_value(json!({"dailyBudgetUsd": 10})).unwrap();
    let spending =
        agentflow_core::engine::rules::Spending { runs: vec![], evidence: Default::default(), deleted: &deleted };
    let status = agentflow_core::engine::rules::budget_status(&spending, None, &controls);
    let daily = &status["daily"];
    assert_eq!(daily["knownUsd"], 2.5);
    assert_eq!(daily["unknownRuns"], 1);
    assert_eq!(daily["limitUsd"], 10.0);
}

// delete ----------------------------------------------------------------------------------------

/// A cancelled task with a row in every table that references it (Python's `finished` fixture).
async fn finished() -> Harness {
    let harness = engine().await;
    let run_id = harness.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    let worktree_dir = harness.worktree_dir();
    harness.with_task(|state| {
        state.task.status = "CANCELLED".into();
        state.task.worktree_path = worktree_dir.join("TEST-1").to_string_lossy().into_owned();
        let run = state.runs.iter_mut().find(|r| r.id == run_id).unwrap();
        run.completed_at = Some(DbTime::now());
        state.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: state.task.id,
            role: "implementer".into(),
            harness: "codex".into(),
            model: None,
            reasoning: None,
            zellij_tab_name: "Implementer".into(),
            zellij_tab_id: None,
            zellij_pane_id: None,
            harness_session_id: None,
            status: "stopped".into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: Some(run_id),
            started_at: DbTime::now(),
            ended_at: None,
        }));
    });
    let state = harness.load();
    let (task_id, agent_id) = (state.task.id, state.agents[0].id);
    let node_id = state.node(spec::IMPLEMENTER).unwrap().id;
    let usage = json!({"inputTokens": 10, "outputTokens": 10, "cachedInputTokens": 0, "totalTokens": 20,
                       "model": "m", "source": "fixture", "costUsd": 1.25, "costKind": "billed"});
    harness
        .engine
        .db
        .session(|tx| -> rusqlite::Result<()> {
            let uid = agentflow_core::db::new_uid;
            table::insert(
                tx,
                &mut RunEvidenceRow {
                    uid: uid(),
                    run_id,
                    requirements_digest: None,
                    verification: Json(Value::Null),
                    usage: Json(usage.clone()),
                    confirmed_model: None,
                    billing_mode: "metered".into(),
                },
            )?;
            table::insert(tx, &mut ProcessLeaseRow { id: uid(), session_id: agent_id, launch_run_id: run_id })?;
            table::insert(
                tx,
                &mut AgentAccessRow {
                    id: uid(),
                    task_id,
                    run_id,
                    role: "implementer".into(),
                    token_hash: agentflow_core::api::auth::digest("t"),
                },
            )?;
            table::insert(
                tx,
                &mut ImplementationWorkItemRow {
                    uid: uid(),
                    task_id,
                    node_id,
                    plan_key: "a".into(),
                    brief: "b".into(),
                    owned_paths: Json(json!([])),
                    acceptance_criteria: Json(json!([])),
                    dependencies: Json(json!([])),
                    worktree_path: worktree_dir.join("TEST-1-worker-a").to_string_lossy().into_owned(),
                    branch_name: "agentflow-worker/a".into(),
                    base_sha: SHA.into(),
                    prepared: false,
                },
            )?;
            table::insert(
                tx,
                &mut ChatThreadRow {
                    id: 0,
                    task_id,
                    platform: "slack".into(),
                    channel: None,
                    thread_id: None,
                    title: None,
                    notified: Json(json!({})),
                    updated_at: DbTime::now(),
                },
            )?;
            table::insert(
                tx,
                &mut ReviewFindingRow {
                    id: 0,
                    task_id,
                    node_run_id: None,
                    finding_id: "CODE-001".into(),
                    kind: "code-review".into(),
                    iteration: 1,
                    reviewed_sha: None,
                    severity: "low".into(),
                    blocking: false,
                    file: None,
                    line: None,
                    title: "t".into(),
                    description: None,
                    recommendation: None,
                    status: "open".into(),
                    prior_finding_id: None,
                    occurrence: 1,
                    resolution: None,
                    response: None,
                    response_note: None,
                    created_at: DbTime::now(),
                    resolved_at: None,
                    end_line: None,
                },
            )?;
            table::insert(
                tx,
                &mut AgentQuestionRow {
                    id: 0,
                    uid: uid(),
                    task_id,
                    node_run_id: None,
                    role: "implementer".into(),
                    message: "q?".into(),
                    choices: Json(json!([])),
                    answered: false,
                    answer: None,
                    created_at: DbTime::now(),
                    answered_at: None,
                    decision: Json(Value::Null),
                },
            )?;
            table::insert(
                tx,
                &mut TaskArtifactRow {
                    id: 0,
                    task_id,
                    node_run_id: None,
                    kind: "k".into(),
                    path: "p".into(),
                    created_at: DbTime::now(),
                },
            )?;
            Ok(())
        })
        .unwrap();
    let git = &harness.git;
    git.returns("branch_sha", Outcome::Ok(json!(SHA)));
    git.returns("remove_task_worktree", Outcome::Ok(json!([" M changed.py"])));
    git.returns("discard_worktree", Outcome::Ok(json!([])));
    git.returns("delete_branch", Outcome::Ok(json!(true)));
    git.returns(
        "branch_report",
        Outcome::Ok(json!({"name": "agent/TEST-1", "exists": true, "sha": SHA, "unmergedCommits": 2,
                           "upstream": null, "unpushedCommits": null})),
    );
    harness
}

fn counts_all_zero(harness: &Harness) -> bool {
    TABLES.iter().all(|t| harness.count(t) == 0)
}

async fn delete(
    harness: &Harness,
    worktree: bool,
    branch: bool,
    folder: bool,
    sha: Option<&str>,
) -> Result<Value, String> {
    harness.engine.delete_task("TEST-1", worktree, branch, folder, sha).await.map_err(|e| e.message)
}

#[tokio::test]
async fn test_delete_removes_every_row_and_keeps_the_spend() {
    let harness = finished().await;
    let messages: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = messages.clone();
    harness.engine.broadcaster.subscribe(move |m| sink.lock().unwrap().push(m.clone()));
    let folder = harness.task_dir().join("TEST-1");
    let result = delete(&harness, false, false, false, None).await.unwrap();
    assert_eq!(result["deleted"], true);
    assert!(counts_all_zero(&harness), "{:?}", TABLES.map(|t| (t, harness.count(t))));
    let spend: Vec<DeletedRunSpendRow> = harness.engine.db.session(|tx| table::select(tx, "", [])).unwrap();
    assert_eq!(spend.len(), 1);
    assert_eq!(
        (spend[0].jira_key.as_str(), spend[0].billing_mode.as_str(), spend[0].cost_usd),
        ("TEST-1", "metered", Some(1.25))
    );
    assert_eq!(
        harness.terminals.calls().iter().filter(|c| c.starts_with("kill_session")).cloned().collect::<Vec<_>>(),
        ["kill_session agentflow-TEST-1"]
    );
    assert!(harness.git.calls_of("remove_task_worktree").is_empty());
    assert!(harness.git.calls_of("delete_branch").is_empty());
    // A kept folder is moved aside so the ticket can start again from an empty one.
    assert!(!folder.exists());
    assert!(Path::new(result["removed"]["taskFolderMovedTo"].as_str().unwrap()).is_dir());
    assert!(messages.lock().unwrap().contains(&json!({"type": "task.deleted", "jiraKey": "TEST-1"})));
    assert!(harness.engine.snapshot("TEST-1").is_err());
}

#[tokio::test]
async fn test_delete_also_closes_the_task_session_left_in_zellij() {
    let mut harness = finished().await;
    let zellij = Arc::new(agentflow_core::terminal::testing::FakeTerminals::default());
    zellij.set_exited("agentflow-TEST-1");
    harness.engine = harness.restarted_with_leftover_zellij(zellij.clone());
    delete(&harness, false, false, false, None).await.unwrap();
    assert_eq!(
        zellij.calls().iter().filter(|c| c.starts_with("kill_session")).cloned().collect::<Vec<_>>(),
        ["kill_session agentflow-TEST-1"]
    );
}

#[tokio::test]
async fn test_deleted_spend_keeps_the_daily_budget_whole() {
    let harness = finished().await;
    harness.configure(|c| c.workflow.cost_controls.daily_budget_usd = Some(100.0));
    let before = harness.engine.snapshot("TEST-1").unwrap()["budget"]["daily"]["knownUsd"].clone();
    delete(&harness, false, false, false, None).await.unwrap();
    let deleted: Vec<DeletedRunSpendRow> = harness.engine.db.session(|tx| table::select(tx, "", [])).unwrap();
    let spending =
        agentflow_core::engine::rules::Spending { runs: vec![], evidence: Default::default(), deleted: &deleted };
    let after = agentflow_core::engine::rules::budget_status(&spending, None, &harness.config().workflow.cost_controls);
    assert_eq!(after["daily"]["knownUsd"], before);
    assert_eq!(before, 1.25);
}

#[tokio::test]
async fn test_delete_everything() {
    let harness = finished().await;
    let result = delete(&harness, true, true, true, Some(SHA)).await.unwrap();
    assert_eq!(harness.git.calls_of("remove_task_worktree").len(), 1);
    assert_eq!(harness.git.calls_of("discard_worktree").len(), 1);
    let branches: Vec<String> = harness.git.calls_of("delete_branch").into_iter().map(|args| args[1].clone()).collect();
    assert_eq!(branches, ["agent/TEST-1", "agentflow-worker/a"]);
    assert_eq!(result["removed"]["discardedPaths"], json!(["changed.py"]));
    assert!(!harness.task_dir().join("TEST-1").exists());
    let leftovers = std::fs::read_dir(harness.task_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("TEST-1.deleted-"))
        .count();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn test_delete_refuses_an_unfinished_task() {
    let harness = finished().await;
    harness.set_status("PR_CREATED");
    assert!(delete(&harness, false, false, false, None).await.unwrap_err().contains("merged or cancelled"));
    assert!(harness.engine.delete_preview("TEST-1").await.unwrap_err().message.contains("merged or cancelled"));
    assert_eq!(harness.count("tasks"), 1);
}

#[tokio::test]
async fn test_delete_refuses_a_moved_branch() {
    let harness = finished().await;
    let error = delete(&harness, true, false, false, Some(&"b".repeat(40))).await.unwrap_err();
    assert!(error.contains("branch changed since the preview"), "{error}");
    assert!(harness.git.calls_of("remove_task_worktree").is_empty());
    assert!(!harness.terminals.calls().iter().any(|c| c.starts_with("kill_session")));
    assert_eq!(harness.count("tasks"), 1);
}

#[tokio::test]
async fn test_branch_removal_needs_the_worktree_removed() {
    let harness = finished().await;
    std::fs::create_dir_all(harness.worktree_dir().join("TEST-1")).unwrap();
    assert!(delete(&harness, false, true, false, Some(SHA)).await.unwrap_err().contains("remove the worktrees"));
    assert!(harness.git.calls_of("delete_branch").is_empty());
}

#[tokio::test]
async fn test_branch_removal_needs_worker_worktrees_removed_too() {
    let harness = finished().await;
    std::fs::create_dir_all(harness.worktree_dir().join("TEST-1-worker-a")).unwrap();
    assert!(delete(&harness, false, true, false, Some(SHA)).await.unwrap_err().contains("remove the worktrees"));
    assert!(harness.git.calls_of("delete_branch").is_empty());
}

#[tokio::test]
async fn test_a_failed_disk_step_leaves_the_task_to_retry() {
    let harness = finished().await;
    harness.git.returns("remove_task_worktree", Outcome::Failed("Permission denied".into()));
    let error = delete(&harness, true, false, true, Some(SHA)).await.unwrap_err();
    assert!(error.contains("Permission denied"), "{error}");
    assert_eq!(harness.git.calls_of("remove_task_worktree").len(), 2, "retried once");
    assert_eq!(harness.count("tasks"), 1);
    assert!(harness.task_dir().join("TEST-1").exists());

    harness.git.returns("remove_task_worktree", Outcome::Ok(json!([])));
    delete(&harness, true, false, true, Some(SHA)).await.unwrap();
    assert_eq!(harness.count("tasks"), 0);
}

#[tokio::test]
async fn test_unknown_cost_blocks_delete_only_under_a_daily_limit() {
    let harness = finished().await;
    harness.engine.db.session(|tx| tx.execute("UPDATE run_evidence SET billing_mode = 'unknown'", [])).unwrap();
    harness.configure(|c| c.workflow.cost_controls.daily_budget_usd = Some(50.0));
    assert_eq!(harness.engine.delete_preview("TEST-1").await.unwrap()["unknownCostRunsToday"], 1);
    assert!(delete(&harness, false, false, false, None).await.unwrap_err().contains("Import the missing run costs"));
    harness.configure(|c| c.workflow.cost_controls.daily_budget_usd = None);
    assert_eq!(harness.engine.delete_preview("TEST-1").await.unwrap()["unknownCostRunsToday"], 0);
    delete(&harness, false, false, false, None).await.unwrap();
    assert_eq!(harness.count("tasks"), 0);
}

#[tokio::test]
async fn test_delete_preview_reports_what_would_go() {
    let harness = finished().await;
    let preview = harness.engine.delete_preview("TEST-1").await.unwrap();
    assert_eq!(preview["branch"]["unmergedCommits"], 2);
    assert_eq!(preview["branch"]["sha"], SHA);
    let path = agentflow_core::paths::py_str(&harness.worktree_dir().join("TEST-1"));
    assert_eq!(preview["worktree"], json!({"path": path, "exists": false, "dirtyPaths": [], "error": null}));
    assert_eq!(preview["workerBranches"], json!(["agentflow-worker/a"]));
    assert_eq!(preview["taskFolder"]["exists"], true);
    assert_eq!(preview["sessionWillBeClosed"], true);
}

#[tokio::test]
async fn test_delete_routes() {
    let harness = finished().await;
    assert_eq!(harness.route("GET", "/api/tasks/TEST-1/delete-preview", None).await.1["branch"]["sha"], SHA);
    let body = json!({"removeWorktree": false, "removeBranch": false, "removeTaskFolder": true});
    let (status, response) = harness.route("POST", "/api/tasks/TEST-1/delete", Some(body)).await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(harness.route("GET", "/api/tasks/TEST-1", None).await.0, 404);
}
