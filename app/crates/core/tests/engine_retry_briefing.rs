//! A retry's briefing tells the fresh conversation what the unfinished attempts at the stage did,
//! ported by name from backend/tests/test_retry_briefing.py (the renderer itself is checked
//! against Python's output in prompts.rs, golden prompts-unit `attempt-*`).

mod engine_support;

use agentflow_core::db::table::{self, Tracked};
use agentflow_core::db::{AgentQuestionRow, AgentSessionRow, DbTime, Json, NodeRunRow, new_uid};
use agentflow_core::engine::launch::RunNodeRequest;
use agentflow_core::engine::spec;
use agentflow_core::vcs::Outcome;
use engine_support::{Harness, engine};
use serde_json::{Value, json};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const START: &str = "cccccccccccccccccccccccccccccccccccccccc";
const HEAD: &str = "dddddddddddddddddddddddddddddddddddddddd";
const HEADING: &str = "## Previous attempt at this stage";

fn time(text: &str) -> DbTime {
    DbTime::parse(text).unwrap()
}

async fn launching() -> Harness {
    let h = engine().await;
    h.launchable();
    h.git.returns("changed_paths", Outcome::Ok(json!(["src/app.py", "tests/test_app.py"])));
    h.git.returns("dirty_paths", Outcome::Ok(json!([" M src/app.py", "?? notes.txt"])));
    h
}

struct Attempt<'a> {
    key: &'a str,
    state: &'a str,
    error: Option<&'a str>,
    exit_code: Option<i64>,
    start_sha: Option<&'a str>,
    launched: bool,
}

impl Default for Attempt<'_> {
    fn default() -> Self {
        Attempt {
            key: spec::IMPLEMENTER,
            state: "FAILED",
            error: Some("Harness exited with code 1 before reporting completion"),
            exit_code: Some(1),
            start_sha: Some(START),
            launched: true,
        }
    }
}

/// A finished-but-unfinished run of `key`, as a crashed harness leaves it (Python's `failed_attempt`).
fn failed_attempt(h: &Harness, attempt: Attempt) -> i64 {
    h.with_task(|state| {
        let index = state.node_index(attempt.key).unwrap();
        state.nodes[index].state = attempt.state.into();
        let node_id = state.nodes[index].id;
        let role = state.nodes[index].role.clone().unwrap();
        let count = state.runs.iter().filter(|r| r.node_id == node_id).count() as i64 + 1;
        let run_dir = std::path::Path::new(&state.task.task_dir).join("runs").join(format!("{}-{count}", attempt.key));
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("config.json"),
            json!({"node": attempt.key, "startSha": attempt.start_sha}).to_string(),
        )
        .unwrap();
        let mut run = Tracked::new(NodeRunRow {
            id: 0,
            task_id: state.task.id,
            node_id,
            run_number: count,
            iteration: state.task.review_iteration,
            state: attempt.state.into(),
            harness: Some("stub".into()),
            model: Some("big".into()),
            reasoning: None,
            run_dir: Some(run_dir.to_string_lossy().into_owned()),
            prompt_path: None,
            result_path: None,
            expected_sha: Some(SHA.into()),
            result: Json(Value::Null),
            exit_code: attempt.exit_code,
            error: attempt.error.map(String::from),
            started_at: time("2026-11-01 10:04:00"),
            completed_at: Some(time("2026-11-01 10:31:59")),
        });
        h.engine.db.session(|tx| run.flush(tx)).unwrap();
        let id = run.id;
        state.runs.push(run);
        if attempt.launched {
            state.agents.push(Tracked::new(AgentSessionRow {
                id: 0,
                task_id: state.task.id,
                role,
                harness: "stub".into(),
                model: None,
                reasoning: None,
                zellij_tab_name: "Implementer".into(),
                zellij_tab_id: Some(3),
                zellij_pane_id: Some("terminal_3".into()),
                harness_session_id: None,
                status: "exited".into(),
                screen_state: None,
                screen_state_at: None,
                current_run_id: Some(id),
                started_at: DbTime::now(),
                ended_at: None,
            }));
        }
        id
    })
}

fn question(h: &Harness, run_id: i64, message: &str, answer: Option<&str>) {
    let task_id = h.load().task.id;
    h.engine
        .db
        .session(|tx| {
            table::insert(
                tx,
                &mut AgentQuestionRow {
                    id: 0,
                    uid: new_uid(),
                    task_id,
                    node_run_id: Some(run_id),
                    role: "implementer".into(),
                    message: message.into(),
                    choices: Json(json!([])),
                    answered: answer.is_some(),
                    answer: answer.map(String::from),
                    created_at: DbTime::now(),
                    answered_at: None,
                    decision: Json(Value::Null),
                },
            )
        })
        .unwrap();
}

fn latest_run(h: &Harness, key: &str) -> NodeRunRow {
    let state = h.load();
    let node_id = state.node(key).unwrap().id;
    state.runs.iter().rev().find(|r| r.node_id == node_id).unwrap().row.clone()
}

fn prompt_of_latest_run(h: &Harness, key: &str) -> String {
    std::fs::read_to_string(latest_run(h, key).prompt_path.unwrap()).unwrap()
}

fn section(h: &Harness, key: &str) -> String {
    let prompt = prompt_of_latest_run(h, key);
    prompt.split(HEADING).nth(1).unwrap_or_else(|| panic!("no retry section in\n{prompt}")).to_string()
}

fn recorded_start(h: &Harness, key: &str) -> Value {
    let run_dir = latest_run(h, key).run_dir.unwrap();
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(&run_dir).join("config.json")).unwrap())
            .unwrap();
    config["startSha"].clone()
}

async fn run(h: &Harness, key: &str) {
    h.engine.run_node("TEST-1", key, RunNodeRequest::default(), false).await.unwrap();
}

fn worktree(h: &Harness) -> String {
    h.load().task.worktree_path.clone()
}

#[tokio::test]
async fn test_retry_after_a_harness_exit_briefs_the_previous_attempt() {
    let h = launching().await;
    h.git.returns("head_sha", Outcome::Ok(json!(HEAD)));
    let run_id = failed_attempt(&h, Attempt::default());
    h.engine
        .db
        .session(|tx| {
            tx.execute(
                "INSERT INTO run_heartbeats (uid, run_id, received_at, phase) VALUES (?1, ?2, ?3, ?4)",
                (new_uid(), run_id, &time("2026-11-01 10:27:00"), "testing"),
            )
        })
        .unwrap();
    question(&h, run_id, "Keep the v1 endpoint?", Some("Yes, deprecate it"));
    question(&h, run_id, "Unanswered one", None);

    run(&h, spec::IMPLEMENTER).await;

    let section = section(&h, spec::IMPLEMENTER);
    assert!(section.contains("already ran once in this review round"), "{section}");
    assert!(section.contains("run #1 (stub, model big), started 2026-11-01 10:04 UTC, ended 2026-11-01 10:31 UTC"));
    assert!(section.contains("Harness exited with code 1 before reporting completion"));
    assert!(!section.contains("exited with code 1."), "already said by the error itself");
    assert!(section.contains("phase `testing` at 2026-11-01 10:27 UTC"));
    assert!(section.contains("Keep the v1 endpoint?") && section.contains("Yes, deprecate it"));
    assert!(!section.contains("Unanswered one"));
    assert!(section.contains(&format!("HEAD moved from `{START}`")) && section.contains(&format!("to `{HEAD}`")));
    assert!(section.contains("tests/test_app.py") && section.contains("?? notes.txt"));
    assert_eq!(h.git.calls_of("changed_paths"), vec![vec![worktree(&h), START.to_string(), HEAD.to_string()]]);
    // This run records its own start for any later retry.
    assert_eq!(recorded_start(&h, spec::IMPLEMENTER), json!(HEAD));
}

#[tokio::test]
async fn test_no_previous_attempt_section_after_a_completed_run_or_an_older_round() {
    let h = launching().await;
    h.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    h.with_task(|state| {
        let index = state.node_index(spec::IMPLEMENTER).unwrap();
        state.nodes[index].state = "READY".into();
    });
    run(&h, spec::IMPLEMENTER).await;
    assert!(!prompt_of_latest_run(&h, spec::IMPLEMENTER).contains(HEADING));

    let latest = latest_run(&h, spec::IMPLEMENTER).id;
    h.engine.agent_fail("TEST-1", "implementer", Some(latest), "Gave up").await.unwrap();
    h.with_task(|state| state.task.review_iteration = 2); // the failure belongs to round 1
    run(&h, spec::IMPLEMENTER).await;
    assert!(!prompt_of_latest_run(&h, spec::IMPLEMENTER).contains(HEADING));
}

#[tokio::test]
async fn test_consecutive_failures_count_and_measure_from_the_first_attempt() {
    let h = launching().await;
    h.git.returns("head_sha", Outcome::Ok(json!(HEAD)));
    failed_attempt(&h, Attempt::default());
    let second = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    failed_attempt(
        &h,
        Attempt {
            error: Some("Tests keep timing out"),
            exit_code: None,
            start_sha: Some(second),
            ..Default::default()
        },
    );
    run(&h, spec::IMPLEMENTER).await;
    let section = section(&h, spec::IMPLEMENTER);
    assert!(section.contains("already ran 2 times") && section.contains("run #2"), "{section}");
    assert!(section.contains("Tests keep timing out"));
    assert_eq!(h.git.calls_of("changed_paths"), vec![vec![worktree(&h), START.to_string(), HEAD.to_string()]]);
}

#[tokio::test]
async fn test_stop_and_retry_after_an_interruption_briefs_the_previous_attempt() {
    let h = launching().await;
    failed_attempt(&h, Attempt { state: "INTERRUPTED", error: None, exit_code: None, ..Default::default() });
    h.engine.recover_node("TEST-1", spec::IMPLEMENTER, true).await.unwrap();
    let section = section(&h, spec::IMPLEMENTER);
    assert!(section.contains("Human retried after interruption"), "{section}");
    assert!(section.contains("It sent no progress report."));
    assert!(!section.contains("never started"));
    assert!(section.contains(&format!("HEAD moved from `{START}`")) && section.contains(&format!("to `{SHA}`")));
}

#[tokio::test]
async fn test_a_run_without_a_recorded_start_lists_only_uncommitted_work() {
    let h = launching().await;
    failed_attempt(&h, Attempt { start_sha: None, ..Default::default() });
    run(&h, spec::IMPLEMENTER).await;
    let section = section(&h, spec::IMPLEMENTER);
    assert!(section.contains("did not record where the attempts started") && section.contains("?? notes.txt"));
    assert!(h.git.calls_of("changed_paths").is_empty());
}

#[tokio::test]
async fn test_launch_failure_is_reported_as_never_started() {
    let h = launching().await;
    failed_attempt(
        &h,
        Attempt { error: Some("Could not open zellij tab"), exit_code: None, launched: false, ..Default::default() },
    );
    run(&h, spec::IMPLEMENTER).await;
    let section = section(&h, spec::IMPLEMENTER);
    assert!(section.contains("It never started") && section.contains("Could not open zellij tab"), "{section}");
    assert!(!section.contains("progress report"));
}

#[tokio::test]
async fn test_unreadable_uncommitted_changes_never_block_the_retry() {
    let h = launching().await;
    h.git.returns("head_sha", Outcome::Ok(json!(HEAD)));
    h.git.returns("dirty_paths", Outcome::Failed("disk gone".into()));
    failed_attempt(&h, Attempt::default());
    run(&h, spec::IMPLEMENTER).await;
    assert_eq!(h.load().node(spec::IMPLEMENTER).unwrap().state, "RUNNING");
    let section = section(&h, spec::IMPLEMENTER);
    assert!(section.contains("could not list the uncommitted changes: check `git status` yourself"), "{section}");
    assert!(section.contains("tests/test_app.py"), "the committed files are still listed");
}

#[tokio::test]
async fn test_unlistable_commits_still_list_the_uncommitted_changes() {
    let h = launching().await;
    h.git.returns("head_sha", Outcome::Ok(json!(HEAD)));
    h.git.returns("changed_paths", Outcome::Failed("Cannot verify worker file ownership".into()));
    failed_attempt(&h, Attempt::default());
    run(&h, spec::IMPLEMENTER).await;
    let section = section(&h, spec::IMPLEMENTER);
    assert!(
        section.contains(&format!(
            "could not list the files those commits change: run `git diff --name-only {START} HEAD`"
        )),
        "{section}"
    );
    assert!(section.contains("?? notes.txt"));
    assert!(!section.contains("worker file ownership"));
}

#[tokio::test]
async fn test_agent_written_flowctl_commands_are_quoted_verbatim() {
    let h = launching().await;
    failed_attempt(
        &h,
        Attempt { error: Some("flowctl complete --file result.json was rejected"), ..Default::default() },
    );
    run(&h, spec::IMPLEMENTER).await;
    let prompt = prompt_of_latest_run(&h, spec::IMPLEMENTER);
    let (briefing, section) = prompt.split_once(HEADING).unwrap();
    assert!(section.contains("  flowctl complete --file result.json was rejected"), "{section}");
    assert!(!section.contains("flowctl complete --run"));
    assert!(briefing.contains("flowctl complete --run"), "the briefing's own examples still carry the run id");
}

#[tokio::test]
async fn test_reviewer_retry_lists_no_worktree_and_records_no_start() {
    let h = launching().await;
    failed_attempt(&h, Attempt { key: spec::CODE_REVIEW, error: Some("Reviewer crashed"), ..Default::default() });
    run(&h, spec::CODE_REVIEW).await;
    let section = section(&h, spec::CODE_REVIEW);
    assert!(section.contains("Reviewer crashed"));
    assert!(!section.contains("What it left in the worktree"));
    assert!(section.contains("Findings it reported were discarded with it"), "{section}");
    assert!(!section.contains("Task documents"));
    assert!(h.git.calls_of("changed_paths").is_empty());
    assert_eq!(recorded_start(&h, spec::CODE_REVIEW), Value::Null);
}
