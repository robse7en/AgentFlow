//! Worktree cleanup after merge, ported by name from backend/tests/test_worktree_cleanup.py.

mod engine_support;

use std::sync::Arc;

use agentflow_core::api::{self, App};
use agentflow_core::db::table::Tracked;
use agentflow_core::db::{AgentSessionRow, DbTime, ImplementationWorkItemRow, Json, TaskRow};
use agentflow_core::engine::WorkflowError;
use agentflow_core::process::worktree::WorktreeProcess;
use agentflow_core::terminal::PaneInfo;
use agentflow_core::terminal::testing::FakeTerminals;
use agentflow_core::vcs::Outcome;
use axum::body::Body;
use axum::http::Request;
use engine_support::{Harness, SHA, engine};
use serde_json::{Value, json};
use tower::ServiceExt;

const SESSION: &str = "agentflow-TEST-1";

/// Python's `ready` fixture: TEST-1 Merged with its worktree in the managed directory.
async fn ready() -> Harness {
    let harness = engine().await;
    let worktree = harness.worktree_dir().join("TEST-1").to_string_lossy().into_owned();
    harness.with_task(|state| {
        state.task.status = "MERGED".into();
        state.task.worktree_path = worktree;
    });
    harness.git.returns("cleanup_worktree", Outcome::Ok(json!([])));
    harness.git.returns("discard_worktree", Outcome::Ok(json!([])));
    harness
}

fn add_agent(harness: &Harness, tab_id: Option<i64>, status: &str) {
    harness.with_task(|state| {
        state.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: state.task.id,
            role: "architect".into(),
            harness: "claude".into(),
            model: None,
            reasoning: None,
            zellij_tab_name: "Architect".into(),
            zellij_tab_id: tab_id,
            zellij_pane_id: Some("terminal_7".into()),
            harness_session_id: None,
            status: status.into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: None,
            started_at: DbTime::now(),
            ended_at: None,
        }));
    });
}

fn live_pane(harness: &Harness, pane_id: &str, tab_id: i64, tab_name: &str) {
    harness.terminals.add_pane(
        SESSION,
        PaneInfo {
            pane_id: pane_id.into(),
            tab_id,
            tab_name: tab_name.into(),
            exited: false,
            exit_status: None,
            command: None,
        },
    );
}

fn add_worker_item(harness: &Harness, worktree_path: &str) {
    let state = harness.load();
    let mut item = ImplementationWorkItemRow {
        uid: agentflow_core::db::new_uid(),
        task_id: state.task.id,
        node_id: state.nodes[0].id,
        plan_key: "worker-a".into(),
        brief: "do the thing".into(),
        owned_paths: Json(json!([])),
        acceptance_criteria: Json(json!([])),
        dependencies: Json(json!([])),
        worktree_path: worktree_path.into(),
        branch_name: "agentflow-worker/a".into(),
        base_sha: SHA.into(),
        prepared: false,
    };
    harness.engine.db.session(|tx| agentflow_core::db::table::insert(tx, &mut item)).unwrap();
}

fn closed_tabs(harness: &Harness) -> Vec<String> {
    harness.terminals.calls().into_iter().filter(|c| c.starts_with("close_tab")).collect()
}

fn cleanups(harness: &Harness) -> usize {
    harness.git.calls_of("cleanup_worktree").len()
}

fn agent_status(harness: &Harness) -> String {
    harness.load().agents[0].status.clone()
}

async fn cleanup(harness: &Harness, sha: &str) -> Result<Value, String> {
    harness.engine.cleanup_worktree("TEST-1", sha, false).await.map_err(|e| e.message)
}

fn messages(harness: &Harness) -> Vec<Value> {
    harness.engine.snapshot("TEST-1").unwrap()["events"].as_array().unwrap().clone()
}

#[tokio::test]
async fn test_cleanup_requires_merged_status_and_current_checkpoint() {
    let harness = ready().await;
    assert!(cleanup(&harness, &"b".repeat(40)).await.unwrap_err().contains("checkpoint changed"));
    harness.set_status("READY_FOR_PR");
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("marked Merged"));
    assert_eq!(cleanups(&harness), 0);
}

#[tokio::test]
async fn test_cleanup_refuses_active_task_or_unmanaged_path() {
    let harness = ready().await;
    harness.set_status("ACTIVE");
    assert!(cleanup(&harness, SHA).await.is_err());
    let elsewhere = harness.task_dir().join("TEST-1").to_string_lossy().into_owned();
    harness.with_task(|state| {
        state.task.status = "MERGED".into();
        state.task.worktree_path = elsewhere;
    });
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("configured task location"));
    assert_eq!(cleanups(&harness), 0);
}

#[tokio::test]
async fn test_cleanup_force_closes_live_agent_pane() {
    let harness = ready().await;
    add_agent(&harness, Some(7), "running");
    live_pane(&harness, "terminal_7", 7, "Architect");
    cleanup(&harness, SHA).await.unwrap();
    assert_eq!(closed_tabs(&harness), ["close_tab agentflow-TEST-1 7"]);
    assert_eq!(cleanups(&harness), 1);
    assert_eq!(agent_status(&harness), "stopped");
}

/// DB status can drift from Zellij reality by the time a task reaches Merged (e.g. a crash
/// recovery that never confirmed exit). A pane that's actually still live must be force-closed,
/// not silently ignored because the row says 'stopped'.
#[tokio::test]
async fn test_cleanup_closes_pane_even_when_db_status_is_stale() {
    let harness = ready().await;
    add_agent(&harness, None, "stopped");
    live_pane(&harness, "terminal_7", 7, "Architect");
    cleanup(&harness, SHA).await.unwrap();
    assert_eq!(closed_tabs(&harness), ["close_tab agentflow-TEST-1 7"]);
    assert_eq!(cleanups(&harness), 1);
}

/// The inverse drift: DB still says 'running' but the pane is actually gone. It must not demand
/// a tab id `_stop_sessions` has no way to supply.
#[tokio::test]
async fn test_cleanup_does_not_block_on_a_running_row_whose_pane_already_exited() {
    let harness = ready().await;
    add_agent(&harness, None, "running");
    cleanup(&harness, SHA).await.unwrap();
    assert!(closed_tabs(&harness).is_empty());
    assert_eq!(cleanups(&harness), 1);
    assert_eq!(agent_status(&harness), "stopped");
}

#[tokio::test]
async fn test_cleanup_fails_when_pane_cannot_be_confirmed_stopped() {
    let harness = ready().await;
    add_agent(&harness, Some(7), "running");
    live_pane(&harness, "terminal_7", 7, "Architect");
    harness.terminals.fail("close_tab", "stop failed");
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("Could not confirm"));
    assert_eq!(cleanups(&harness), 0);
}

#[tokio::test]
async fn test_cleanup_rejects_unfinished_node_even_on_completed_task() {
    let harness = ready().await;
    harness.with_task(|state| state.nodes[0].state = "RUNNING".into());
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("still active"));
    assert_eq!(cleanups(&harness), 0);
}

#[tokio::test]
async fn test_cleanup_fails_closed_when_zellij_unavailable() {
    let harness = ready().await;
    add_agent(&harness, Some(7), "running");
    harness.terminals.fail("session_state", "unavailable");
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("Cannot verify agents"));
    assert_eq!(cleanups(&harness), 0);
}

#[tokio::test]
async fn test_git_refusal_is_actionable_and_does_not_record_success() {
    let harness = ready().await;
    harness.git.returns("cleanup_worktree", Outcome::Failed("dirty worktree".into()));
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("dirty worktree"));
    assert!(!messages(&harness).iter().any(|e| e["message"].as_str().unwrap().contains("Worktree cleaned up")));
}

fn kill_claude(harness: &Harness) {
    *harness.killer.kills.lock().unwrap() =
        vec![WorktreeProcess { pid: 27500, name: "claude.exe".into(), cwd: r"D:\worktrees\TEST-1\Admin\".into() }];
}

/// A terminal the human opened in the worktree (outside the task's session) keeps Windows from
/// deleting it; cleanup kills it and tries again, and says what it killed.
#[tokio::test]
async fn test_cleanup_kills_processes_holding_the_worktree_then_retries() {
    let harness = ready().await;
    kill_claude(&harness);
    harness.git.then("cleanup_worktree", Outcome::Busy("Could not remove leftover worktree directory".into()));
    cleanup(&harness, SHA).await.unwrap();
    assert_eq!(*harness.killer.calls.lock().unwrap(), [harness.worktree_dir().join("TEST-1")]);
    assert_eq!(cleanups(&harness), 2);
    assert!(messages(&harness).iter().any(|e| {
        let m = e["message"].as_str().unwrap();
        m.contains("Closed 1 process(es)") && m.contains("claude.exe (27500)")
    }));
}

#[tokio::test]
async fn test_cleanup_records_killed_processes_even_if_the_retry_fails() {
    let harness = ready().await;
    kill_claude(&harness);
    harness.git.returns("cleanup_worktree", Outcome::Busy("Could not remove leftover worktree directory".into()));
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("Could not remove"));
    assert!(messages(&harness).iter().any(|e| e["message"].as_str().unwrap().contains("claude.exe (27500)")));
}

/// Processes are only killed once the branch has passed cleanup's checks.
#[tokio::test]
async fn test_cleanup_kills_nothing_when_git_refuses_the_branch() {
    let harness = ready().await;
    kill_claude(&harness);
    harness.git.returns("cleanup_worktree", Outcome::Failed("Task branch changed since final review".into()));
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("changed"));
    assert!(harness.killer.calls.lock().unwrap().is_empty());
}

/// Merged into a local branch: the refusal carries a code the UI offers force for, and isn't
/// retried (waiting can't make the branch reviewed or merged).
#[tokio::test]
async fn test_cleanup_tags_an_unverified_branch_and_does_not_retry() {
    let harness = ready().await;
    harness.git.returns("cleanup_worktree", Outcome::Unverified("Task branch changed since final review".into()));
    let error = harness.engine.cleanup_worktree("TEST-1", SHA, false).await.unwrap_err();
    assert_eq!((error.status, error.code), (409, Some(WorkflowError::BRANCH_UNVERIFIED)));
    assert_eq!(cleanups(&harness), 1);
    assert!(harness.killer.calls.lock().unwrap().is_empty());
}

fn failure_logs(harness: &Harness) -> Vec<Value> {
    messages(harness)
        .into_iter()
        .filter(|e| e["message"].as_str().unwrap().starts_with("Worktree cleanup failed"))
        .collect()
}

/// A refused cleanup used to leave nothing but a toast: it is now on the task, as its status
/// message (which the task page shows) and a warning event with the same text.
#[tokio::test]
async fn test_a_failed_cleanup_is_logged_and_becomes_the_status_message() {
    let harness = ready().await;
    harness.git.returns("cleanup_worktree", Outcome::Failed("dirty worktree".into()));
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("dirty worktree"));
    let snapshot = harness.engine.snapshot("TEST-1").unwrap();
    let expected = "Worktree cleanup failed while removing the worktree: dirty worktree";
    assert_eq!(snapshot["statusMessage"], expected);
    let logged = failure_logs(&harness);
    assert_eq!(logged.len(), 1);
    assert_eq!((logged[0]["message"].as_str(), logged[0]["level"].as_str()), (Some(expected), Some("warning")));
    assert_eq!(logged[0]["data"]["step"], "removing the worktree");
    assert_eq!(logged[0]["data"]["force"], false);
}

/// Every attempt is recorded, and a later success replaces the failure as the status message.
#[tokio::test]
async fn test_each_failed_attempt_is_logged_and_a_success_replaces_the_message() {
    let harness = ready().await;
    harness.git.returns("cleanup_worktree", Outcome::Failed("dirty worktree".into()));
    cleanup(&harness, SHA).await.unwrap_err();
    cleanup(&harness, SHA).await.unwrap_err();
    assert_eq!(failure_logs(&harness).len(), 2);
    harness.git.returns("cleanup_worktree", Outcome::Ok(json!([])));
    let snapshot = cleanup(&harness, SHA).await.unwrap();
    assert!(snapshot["statusMessage"].as_str().unwrap().starts_with("Worktree cleaned up"));
}

#[tokio::test]
async fn test_failures_before_the_removal_are_logged_too() {
    let harness = ready().await;
    harness.with_task(|state| state.nodes[0].state = "RUNNING".into());
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("still active"));
    let logged = failure_logs(&harness);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0]["data"]["step"], "stopping agent tabs");

    let harness = ready().await;
    harness.terminals.fail("kill_session", "session stuck");
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("Zellij session"));
    assert_eq!(failure_logs(&harness)[0]["data"]["step"], "closing the terminal session");
}

/// The requests that never started a cleanup (not Merged, stale checkpoint) are refusals, not
/// failures, and leave the task alone.
#[tokio::test]
async fn test_a_rejected_request_is_not_logged_as_a_failure() {
    let harness = ready().await;
    cleanup(&harness, &"b".repeat(40)).await.unwrap_err();
    assert!(failure_logs(&harness).is_empty());
}

/// An editor with the folder open holds a directory without working in it, so killing by working
/// directory can't free it: the error names who to close instead.
#[tokio::test]
async fn test_a_busy_directory_names_what_still_holds_it() {
    let harness = ready().await;
    *harness.killer.holders.lock().unwrap() =
        vec![agentflow_core::process::worktree::WorktreeHolder { pid: 31944, name: "Code.exe".into() }];
    harness.git.returns("cleanup_worktree", Outcome::Busy("Could not remove leftover worktree directory".into()));
    let message = cleanup(&harness, SHA).await.unwrap_err();
    assert!(message.contains("still open in Code.exe (31944): close it and try again"), "{message}");
    let logged = failure_logs(&harness);
    assert!(logged[0]["message"].as_str().unwrap().contains("Code.exe (31944)"));
    assert_eq!(logged[0]["data"]["holders"], json!([{"pid": 31944, "name": "Code.exe"}]));
}

#[tokio::test]
async fn test_holders_are_only_looked_up_for_a_busy_directory() {
    let harness = ready().await;
    *harness.killer.holders.lock().unwrap() =
        vec![agentflow_core::process::worktree::WorktreeHolder { pid: 31944, name: "Code.exe".into() }];
    harness.git.returns("cleanup_worktree", Outcome::Unverified("Task branch changed since final review".into()));
    let error = harness.engine.cleanup_worktree("TEST-1", SHA, false).await.unwrap_err();
    assert!(!error.message.contains("Code.exe"));
    assert_eq!(error.code, Some(WorkflowError::BRANCH_UNVERIFIED));
    assert_eq!(failure_logs(&harness)[0]["data"]["code"], WorkflowError::BRANCH_UNVERIFIED);
}

#[tokio::test]
async fn test_forced_cleanup_passes_force_to_git_and_logs_the_unverified_tip() {
    let harness = ready().await;
    harness.git.returns("branch_sha", Outcome::Ok(json!("d".repeat(40))));
    harness.engine.cleanup_worktree("TEST-1", SHA, true).await.unwrap();
    assert_eq!(harness.git.calls_of("cleanup_worktree")[0][6], "force");
    let forced = messages(&harness)
        .into_iter()
        .find(|e| e["message"].as_str().unwrap().contains("forced"))
        .expect("a forced-cleanup log entry");
    assert_eq!(forced["level"], "warning");
    assert_eq!(forced["data"]["branchSha"], "d".repeat(40));
}

/// D2: the Tests/Shell tabs created at task creation also have their cwd inside the worktree and
/// keep it locked on Windows: cleanup kills the whole task session before calling git.
#[tokio::test]
async fn test_cleanup_kills_the_task_zellij_session_before_removing_the_worktree() {
    let harness = ready().await;
    cleanup(&harness, SHA).await.unwrap();
    let calls = harness.terminals.calls();
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("kill_session")).collect::<Vec<_>>(),
        ["kill_session agentflow-TEST-1"]
    );
    assert_eq!(cleanups(&harness), 1);
}

#[tokio::test]
async fn test_cleanup_fails_closed_when_session_cannot_be_killed() {
    let harness = ready().await;
    harness.terminals.fail("kill_session", "session stuck");
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("Zellij session"));
    assert_eq!(cleanups(&harness), 0);
}

/// A task that ran in zellij before agents moved to embedded terminals left its session there:
/// cleanup closes that one too, after the session agents run in now.
#[tokio::test]
async fn test_cleanup_also_closes_the_task_session_left_in_zellij() {
    let mut harness = ready().await;
    let zellij = Arc::new(FakeTerminals::default());
    zellij.set_exited(SESSION);
    harness.engine = harness.restarted_with_leftover_zellij(zellij.clone());
    cleanup(&harness, SHA).await.unwrap();
    assert!(harness.terminals.calls().contains(&format!("kill_session {SESSION}")));
    assert_eq!(
        zellij.calls().iter().filter(|c| c.starts_with("kill_session")).collect::<Vec<_>>(),
        [&format!("kill_session {SESSION}")]
    );
    assert_eq!(cleanups(&harness), 1);
}

/// The leftover session isn't where agents run, so failing to close it only warns.
#[tokio::test]
async fn test_cleanup_only_warns_when_the_leftover_zellij_session_cannot_be_closed() {
    let mut harness = ready().await;
    let zellij = Arc::new(FakeTerminals::default());
    zellij.fail("kill_session", "zellij is wedged");
    harness.engine = harness.restarted_with_leftover_zellij(zellij);
    cleanup(&harness, SHA).await.unwrap();
    assert_eq!(cleanups(&harness), 1);
    assert!(messages(&harness).iter().any(|e| e["level"] == "warning"
        && e["message"].as_str().unwrap().contains("leftover Zellij session: zellij is wedged")));
}

#[tokio::test]
async fn test_cleanup_passes_the_configured_worktree_dir_to_git() {
    let harness = ready().await;
    cleanup(&harness, SHA).await.unwrap();
    let args = &harness.git.calls_of("cleanup_worktree")[0];
    assert_eq!(args[4], harness.worktree_dir().to_string_lossy());
    assert_eq!(args[5], "", "no merged PR head");
}

#[tokio::test]
async fn test_cleanup_passes_the_merged_pr_head_to_git() {
    let harness = ready().await;
    harness.with_task(|state| state.task.merged_head_sha = Some("c".repeat(40)));
    cleanup(&harness, SHA).await.unwrap();
    assert_eq!(harness.git.calls_of("cleanup_worktree")[0][5], "c".repeat(40));
}

/// D4: closing a tab is irreversible, so the row it updates is committed before the git step.
#[tokio::test]
async fn test_cleanup_commits_agent_stopped_status_even_if_git_then_fails() {
    let harness = ready().await;
    add_agent(&harness, Some(7), "running");
    live_pane(&harness, "terminal_7", 7, "Architect");
    harness.git.returns("cleanup_worktree", Outcome::Failed("permission denied".into()));
    assert!(cleanup(&harness, SHA).await.unwrap_err().contains("permission denied"));
    assert_eq!(closed_tabs(&harness), ["close_tab agentflow-TEST-1 7"]);
    assert_eq!(agent_status(&harness), "stopped");
}

#[tokio::test]
async fn test_cleanup_warns_about_discarded_files_instead_of_blocking() {
    let harness = ready().await;
    harness.git.returns("cleanup_worktree", Outcome::Ok(json!([" M src/foo.py", "?? scratch.txt", "!! build/out.js"])));
    let result = cleanup(&harness, SHA).await.unwrap();
    let message = result["statusMessage"].as_str().unwrap();
    assert!(message.contains("discarded 3") && message.contains("src/foo.py"), "{message}");
    assert!(
        messages(&harness)
            .iter()
            .any(|e| e["level"] == "warning" && e["message"].as_str().unwrap().contains("discarded 3"))
    );
}

#[tokio::test]
async fn test_worktree_cleanup_preview_lists_dirt_and_open_panes() {
    let harness = ready().await;
    add_agent(&harness, Some(7), "running");
    harness.git.returns("dirty_paths", Outcome::Ok(json!([" M src/foo.py", "?? scratch.txt"])));
    live_pane(&harness, "terminal_7", 7, "Architect");
    std::fs::create_dir_all(harness.worktree_dir().join("TEST-1")).unwrap();
    std::fs::write(harness.worktree_dir().join("TEST-1").join(".git"), "gitdir: elsewhere\n").unwrap(); // a linked worktree's marker
    let preview = harness.engine.worktree_cleanup_preview("TEST-1").await.unwrap();
    assert_eq!(preview["dirtyPaths"], json!(["src/foo.py", "scratch.txt"]));
    assert_eq!(preview["openAgentPanes"], json!(["Architect"]));
    assert_eq!(preview["workerWorktrees"], json!([]));
    assert_eq!(preview["sessionWillBeClosed"], true);
    assert_eq!(preview["otherOpenTabs"], json!([]));
}

/// A directory whose `.git` is gone isn't a checkout: git would answer for whatever repository
/// encloses the data directory and the preview would list that repository's files as discarded.
#[tokio::test]
async fn test_worktree_cleanup_preview_does_not_ask_git_about_a_leftover_directory() {
    let harness = ready().await;
    harness.git.returns("dirty_paths", Outcome::Ok(json!([" M README.md"])));
    std::fs::create_dir_all(harness.worktree_dir().join("TEST-1")).unwrap();
    let preview = harness.engine.worktree_cleanup_preview("TEST-1").await.unwrap();
    assert_eq!(preview["dirtyPaths"], json!([]));
    assert!(harness.git.calls_of("dirty_paths").is_empty());
}

/// D2: the preview surfaces the non-agent tabs the whole-session close will take with it.
#[tokio::test]
async fn test_worktree_cleanup_preview_reports_non_agent_tabs_that_will_close() {
    let harness = ready().await;
    live_pane(&harness, "terminal_1", 1, "Tests");
    live_pane(&harness, "terminal_2", 2, "Shell");
    let preview = harness.engine.worktree_cleanup_preview("TEST-1").await.unwrap();
    assert_eq!(preview["sessionWillBeClosed"], true);
    assert_eq!(preview["otherOpenTabs"], json!(["Shell", "Tests"]));
    assert_eq!(preview["openAgentPanes"], json!([]));
}

#[tokio::test]
async fn test_worktree_cleanup_preview_no_session_reports_not_closing() {
    let harness = ready().await;
    harness.terminals.remove_session(SESSION);
    let preview = harness.engine.worktree_cleanup_preview("TEST-1").await.unwrap();
    assert_eq!(preview["sessionWillBeClosed"], false);
    assert_eq!(preview["otherOpenTabs"], json!([]));
}

#[tokio::test]
async fn test_cleanup_also_removes_worker_worktrees() {
    let harness = ready().await;
    let worker = harness.worktree_dir().join("TEST-1-worker-a").to_string_lossy().into_owned();
    add_worker_item(&harness, &worker);
    let result = cleanup(&harness, SHA).await.unwrap();
    let calls = harness.git.calls_of("discard_worktree");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0][1], worker);
    assert!(result["statusMessage"].as_str().unwrap().contains("1 worker worktree"));
}

#[tokio::test]
async fn test_cleanup_reports_worker_worktree_failure_without_blocking_main_cleanup() {
    let harness = ready().await;
    let worker = harness.worktree_dir().join("TEST-1-worker-a").to_string_lossy().into_owned();
    add_worker_item(&harness, &worker);
    harness.git.returns("discard_worktree", Outcome::Failed("Filename too long".into()));
    let result = cleanup(&harness, SHA).await.unwrap();
    assert_eq!(cleanups(&harness), 1, "the main worktree cleanup still ran and succeeded");
    let message = result["statusMessage"].as_str().unwrap();
    assert!(
        message.contains("Could not remove 1 worker worktree") && message.contains("Filename too long"),
        "{message}"
    );
}

#[tokio::test]
async fn test_worktree_cleanup_preview_lists_existing_worker_worktrees() {
    let harness = ready().await;
    let worker = harness.worktree_dir().join("TEST-1-worker-a");
    std::fs::create_dir_all(&worker).unwrap();
    add_worker_item(&harness, &worker.to_string_lossy());
    let preview = harness.engine.worktree_cleanup_preview("TEST-1").await.unwrap();
    assert_eq!(preview["workerWorktrees"], json!([worker.to_string_lossy()]));
}

#[tokio::test]
async fn test_snapshot_skips_worktree_existence_check_before_merged() {
    let harness = engine().await;
    let snapshot = harness.engine.snapshot("TEST-1").unwrap();
    assert_ne!(snapshot["status"], "MERGED");
    assert!(snapshot["worktreeExists"].is_null());
    assert!(snapshot["workerWorktreeCount"].is_null());
}

#[tokio::test]
async fn test_snapshot_reports_worktree_existence_once_merged() {
    let harness = ready().await;
    let snapshot = harness.engine.snapshot("TEST-1").unwrap();
    assert_eq!(snapshot["worktreeExists"], false);
    assert_eq!(snapshot["workerWorktreeCount"], 0);
    std::fs::create_dir_all(harness.worktree_dir().join("TEST-1")).unwrap();
    let worker = harness.worktree_dir().join("TEST-1-worker-a");
    std::fs::create_dir_all(&worker).unwrap();
    add_worker_item(&harness, &worker.to_string_lossy());
    let snapshot = harness.engine.snapshot("TEST-1").unwrap();
    assert_eq!(snapshot["worktreeExists"], true);
    assert_eq!(snapshot["workerWorktreeCount"], 1);
}

#[tokio::test]
async fn test_task_list_reports_worktree_existence_per_merged_task() {
    let harness = ready().await;
    let first = harness.load().task.row.clone();
    let mut second = TaskRow {
        id: 0,
        jira_key: "TEST-2".into(),
        jira_title: Some("Other".into()),
        branch_name: "agent/TEST-2".into(),
        worktree_path: harness.worktree_dir().join("TEST-2").to_string_lossy().into_owned(),
        zellij_session_name: "agentflow-TEST-2".into(),
        task_dir: harness.task_dir().join("TEST-2").to_string_lossy().into_owned(),
        status: "MERGED".into(),
        created_at: DbTime::now(),
        updated_at: DbTime::now(),
        ..first
    };
    harness.engine.db.session(|tx| agentflow_core::db::table::insert(tx, &mut second)).unwrap();
    std::fs::create_dir_all(harness.worktree_dir().join("TEST-2")).unwrap();
    let worker = harness.worktree_dir().join("TEST-1-worker-a");
    std::fs::create_dir_all(&worker).unwrap();
    add_worker_item(&harness, &worker.to_string_lossy());

    let rows = harness.engine.list_tasks("exclude").await.unwrap();
    let row = |key: &str| rows.iter().find(|r| r["jiraKey"] == key).unwrap().clone();
    assert_eq!(
        (row("TEST-1")["worktreeExists"].clone(), row("TEST-1")["workerWorktreeCount"].clone()),
        (json!(false), json!(1))
    );
    assert_eq!(
        (row("TEST-2")["worktreeExists"].clone(), row("TEST-2")["workerWorktreeCount"].clone()),
        (json!(true), json!(0))
    );

    harness.set_status("PR_CREATED");
    let rows = harness.engine.list_tasks("exclude").await.unwrap();
    let first = rows.iter().find(|r| r["jiraKey"] == "TEST-1").unwrap();
    assert!(first["worktreeExists"].is_null() && first["workerWorktreeCount"].is_null());
}

#[tokio::test]
async fn test_cleanup_endpoint_requires_human_auth_origin_and_merged_status() {
    let harness = ready().await;
    let app = App::around(harness.engine.clone(), harness.tmp().join("config.json"));
    let call = |body: Value, headers: Vec<(&'static str, String)>| {
        let app = app.clone();
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/tasks/TEST-1/worktree/cleanup")
                .header("host", "127.0.0.1")
                .header("content-type", "application/json");
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = api::router(app).oneshot(request.body(Body::from(body.to_string())).unwrap()).await.unwrap();
            let status = response.status().as_u16();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
        }
    };
    let payload = json!({"expectedSha": SHA});
    assert_eq!(call(payload.clone(), vec![]).await.0, 401);
    assert_eq!(call(payload.clone(), vec![("authorization", "Bearer any".into())]).await.0, 403);
    let code = app.human.lock().unwrap().pairing_code.clone();
    let pair = Request::builder()
        .method("POST")
        .uri("/api/auth/pair")
        .header("host", "127.0.0.1")
        .header("origin", "http://127.0.0.1")
        .body(Body::from(json!({"code": code}).to_string()))
        .unwrap();
    let paired = api::router(app.clone()).oneshot(pair).await.unwrap();
    assert_eq!(paired.status(), 200);
    let cookie = paired.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    let origin = |value: &str| vec![("cookie", cookie.clone()), ("origin", value.to_string())];
    assert_eq!(call(payload.clone(), origin("http://evil.invalid")).await.0, 403);
    assert_eq!(call(json!({}), origin("http://127.0.0.1")).await.0, 422);
    harness.set_status("READY_FOR_PR");
    assert_eq!(call(payload.clone(), origin("http://127.0.0.1")).await.0, 409);
    harness.set_status("MERGED");
    let (status, result) = call(payload.clone(), origin("http://127.0.0.1")).await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["status"], "MERGED");
    assert!(result["statusMessage"].as_str().unwrap().contains("Worktree cleaned up"));
    assert_eq!(cleanups(&harness), 1);
    assert_eq!(harness.git.calls_of("cleanup_worktree")[0][6], "", "force defaults off");
    harness.git.then("cleanup_worktree", Outcome::Unverified("Task branch changed since final review".into()));
    let (status, result) = call(payload, origin("http://127.0.0.1")).await;
    assert_eq!((status, result["code"].clone()), (409, json!("branch_unverified")), "{result}");
    let (status, result) = call(json!({"expectedSha": SHA, "force": true}), origin("http://127.0.0.1")).await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(harness.git.calls_of("cleanup_worktree")[2][6], "force");
}

/// A request the UI tagged with X-Operation-Id streams its steps to WebSocket clients, retry
/// included; an untagged one (chat, older UIs, the contract suite) stays silent.
#[tokio::test]
async fn test_tagged_cleanup_request_reports_its_steps() {
    let harness = ready().await;
    add_worker_item(&harness, &harness.tmp().join("worker-a").to_string_lossy());
    harness.git.then("cleanup_worktree", Outcome::Busy("Could not remove leftover worktree directory".into()));
    let app = App::around(harness.engine.clone(), harness.tmp().join("config.json"));
    let code = app.human.lock().unwrap().pairing_code.clone();
    let pair = Request::builder()
        .method("POST")
        .uri("/api/auth/pair")
        .header("host", "127.0.0.1")
        .header("origin", "http://127.0.0.1")
        .body(Body::from(json!({"code": code}).to_string()))
        .unwrap();
    let paired = api::router(app.clone()).oneshot(pair).await.unwrap();
    let cookie = paired.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    let mut client = harness.engine.broadcaster.connect();
    let call = |operation: Option<&'static str>| {
        let (app, cookie) = (app.clone(), cookie.clone());
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/tasks/TEST-1/worktree/cleanup")
                .header("host", "127.0.0.1")
                .header("origin", "http://127.0.0.1")
                .header("cookie", cookie)
                .header("content-type", "application/json");
            if let Some(id) = operation {
                request = request.header("x-operation-id", id);
            }
            let body = Body::from(json!({"expectedSha": SHA}).to_string());
            api::router(app).oneshot(request.body(body).unwrap()).await.unwrap().status().as_u16()
        }
    };
    assert_eq!(call(Some("op-7")).await, 200);
    let mut steps = Vec::new();
    while let Ok(raw) = client.try_recv() {
        let message: Value = serde_json::from_str(&raw).unwrap();
        if message["type"] == "progress" {
            assert_eq!((message["operation"].as_str(), message["jiraKey"].as_str()), (Some("op-7"), Some("TEST-1")));
            steps.push(message["message"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(
        steps,
        [
            "Stopping agent tabs",
            "Closing the terminal session",
            "Removing the worktree",
            "Worktree is in use; closing processes still running in it",
            "Retrying the worktree removal",
            "Removing 1 worker worktree(s)",
        ]
    );
    assert_eq!(call(None).await, 200);
    assert!(std::iter::from_fn(|| client.try_recv().ok()).all(|raw| !raw.contains("\"progress\"")));
}
