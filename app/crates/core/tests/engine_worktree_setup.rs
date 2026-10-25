//! The engine around worktree setup: which launches wait for it, what starts when it finishes,
//! explicit retry, interruption and cancellation. Real commands, fake git and terminals; the
//! copying and checks themselves are in worktree_setup.rs.

mod engine_support;

use std::time::Duration;

use agentflow_core::config::WorktreeSetup;
use agentflow_core::db::{DbTime, Json, WorkflowNodeRow};
use agentflow_core::engine::launch::RunNodeRequest;
use agentflow_core::engine::{spec, worktree_setup};
use agentflow_core::vcs::Outcome;
use engine_support::{Harness, engine};
use serde_json::{Value, json};

const RUNNING: &str = "Waiting for worktree setup to finish";
const FAILED: &str = "Waiting for worktree setup: it failed. Retry setup from the task view";
const FINISHED: &str = "Worktree setup finished; start when ready";

fn shell(script: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".into(), "/c".into(), script.into()]
    } else {
        vec!["sh".into(), "-c".into(), script.into()]
    }
}

fn slow() -> Vec<String> {
    if cfg!(windows) { shell("ping -n 3 127.0.0.1 > nul") } else { shell("sleep 2") }
}

fn configure(harness: &Harness, commands: Vec<Vec<String>>) -> WorktreeSetup {
    let setup = WorktreeSetup { commands, ..WorktreeSetup::default() };
    let root = harness.load().task.repository_root.clone();
    let configured = setup.clone();
    harness.configure(move |config| {
        config.workspace.worktree_setup.insert(root, configured);
    });
    setup
}

fn set_node(harness: &Harness, key: &str, state: &str, mode: &str) {
    harness.with_task(|task| {
        let index = task.node_index(key).unwrap();
        task.nodes[index].state = state.into();
        task.nodes[index].execution_mode = mode.into();
    });
}

fn snapshot(harness: &Harness) -> Value {
    harness.engine.snapshot("TEST-1").unwrap()
}

fn node(snapshot: &Value, key: &str) -> Value {
    snapshot["nodes"].as_array().unwrap().iter().find(|n| n["key"] == key).unwrap().clone()
}

async fn run(harness: &Harness, key: &str) -> Value {
    harness.engine.run_node("TEST-1", key, RunNodeRequest::default(), false).await.unwrap()
}

async fn wait_for(harness: &Harness, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..400 {
        let current = snapshot(harness);
        if done(&current) {
            return current;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}: {:#}", snapshot(harness)["worktreeSetup"]);
}

fn setup_state(snapshot: &Value, name: &str) -> Option<String> {
    snapshot["worktreeSetup"].as_array()?.iter().find(|s| s["name"] == name)?["state"].as_str().map(String::from)
}

fn finished(name: &'static str) -> impl Fn(&Value) -> bool {
    move |s| setup_state(s, name).is_some_and(|state| state != "running")
}

#[tokio::test]
async fn later_agents_wait_for_setup_but_the_architect_does_not() {
    let harness = engine().await;
    harness.launchable();
    assert!(snapshot(&harness).get("worktreeSetup").is_none(), "no key without configuration (Python parity)");
    configure(&harness, vec![slow()]);
    assert_eq!(snapshot(&harness)["worktreeSetup"], json!([]));

    run(&harness, spec::ARCHITECT).await;
    assert_eq!(harness.load().runs.len(), 1, "the Architect launches during setup");

    set_node(&harness, spec::IMPLEMENTER, "READY", "manual");
    let held = run(&harness, spec::IMPLEMENTER).await;
    assert_eq!(node(&held, spec::IMPLEMENTER)["state"], "READY");
    assert_eq!(node(&held, spec::IMPLEMENTER)["message"], RUNNING);
    assert_eq!(setup_state(&held, "task").as_deref(), Some("running"));
    assert_eq!(harness.load().runs.len(), 1);
    // Asking again while it runs neither launches nor starts a second setup.
    run(&harness, spec::IMPLEMENTER).await;
    assert_eq!(harness.load().runs.len(), 1);

    let done = wait_for(&harness, "setup", finished("task")).await;
    assert_eq!(setup_state(&done, "task").as_deref(), Some("succeeded"));
    let done = wait_for(&harness, "the finished message", |s| node(s, spec::IMPLEMENTER)["message"] == FINISHED).await;
    let events = done["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["message"].as_str().unwrap().starts_with("Worktree setup finished")));
    let log = done["worktreeSetup"][0]["logPath"].as_str().unwrap();
    assert!(std::fs::read_to_string(log).unwrap().contains("$ "));

    run(&harness, spec::IMPLEMENTER).await;
    assert_eq!(harness.load().runs.len(), 2, "launches once setup succeeded");
}

#[tokio::test]
async fn automatic_nodes_start_when_setup_succeeds() {
    let harness = engine().await;
    harness.launchable();
    configure(&harness, vec![slow()]);
    set_node(&harness, spec::IMPLEMENTER, "READY", "automatic");

    let held = run(&harness, spec::IMPLEMENTER).await;
    assert_eq!(node(&held, spec::IMPLEMENTER)["message"], RUNNING);
    wait_for(&harness, "the automatic launch", |s| node(s, spec::IMPLEMENTER)["state"] == "RUNNING").await;
    assert_eq!(harness.load().runs.len(), 1);
}

#[tokio::test]
async fn a_failed_setup_waits_for_an_explicit_retry() {
    let harness = engine().await;
    harness.launchable();
    configure(&harness, vec![shell("exit 1")]);
    set_node(&harness, spec::IMPLEMENTER, "READY", "automatic");

    run(&harness, spec::IMPLEMENTER).await;
    let failed = wait_for(&harness, "the failure", |s| node(s, spec::IMPLEMENTER)["message"] == FAILED).await;
    let status = &failed["worktreeSetup"][0];
    assert_eq!(status["state"], "failed");
    assert!(status["error"].as_str().unwrap().ends_with("exited with code 1"), "{status}");
    assert!(
        failed["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["level"] == "error" && e["message"].as_str().unwrap().starts_with("Worktree setup failed"))
    );

    // Launching again does not re-run a failed setup by itself.
    let again = run(&harness, spec::IMPLEMENTER).await;
    assert_eq!(node(&again, spec::IMPLEMENTER)["message"], FAILED);
    assert_eq!(again["worktreeSetup"][0]["startedAt"], status["startedAt"]);

    configure(&harness, vec![shell("exit 0")]);
    let (code, retried) = harness.route("POST", "/api/tasks/TEST-1/worktree/setup/retry", None).await;
    assert_eq!(code, 200, "{retried}");
    assert_eq!(node(&retried, spec::IMPLEMENTER)["message"], RUNNING);
    wait_for(&harness, "the automatic launch", |s| node(s, spec::IMPLEMENTER)["state"] == "RUNNING").await;
    let events = snapshot(&harness)["events"].clone();
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"].as_str().unwrap().starts_with("Human retried worktree setup (task)"))
    );

    let (code, body) = harness.route("POST", "/api/tasks/TEST-1/worktree/setup/retry", None).await;
    assert_eq!((code, body["detail"].as_str()), (409, Some("No failed worktree setup to retry")));
}

#[tokio::test]
async fn setup_left_running_by_a_stopped_backend_reads_as_interrupted() {
    let harness = engine().await;
    harness.launchable();
    let setup = configure(&harness, vec![shell("exit 0")]);
    let task = harness.load().task.row.clone();
    let dir = std::path::Path::new(&task.task_dir).join("worktree-setup");
    std::fs::create_dir_all(&dir).unwrap();
    let status = json!({"name": "task", "worktree": task.worktree_path, "digest": worktree_setup::digest(&setup),
        "state": "running", "startedAt": DbTime::now().isoformat(), "completedAt": null, "error": null,
        "logPath": dir.join("task.log")});
    std::fs::write(dir.join("task.json"), status.to_string()).unwrap();

    let shown = snapshot(&harness)["worktreeSetup"][0].clone();
    assert_eq!(
        (shown["state"].as_str(), shown["error"].as_str()),
        (Some("failed"), Some("Interrupted: AgentFlow stopped during setup"))
    );
    set_node(&harness, spec::IMPLEMENTER, "READY", "manual");
    assert_eq!(node(&run(&harness, spec::IMPLEMENTER).await, spec::IMPLEMENTER)["message"], FAILED);
}

#[tokio::test]
async fn a_changed_configuration_runs_setup_again() {
    let harness = engine().await;
    harness.launchable();
    configure(&harness, vec![shell("exit 0")]);
    set_node(&harness, spec::IMPLEMENTER, "READY", "manual");
    run(&harness, spec::IMPLEMENTER).await;
    wait_for(&harness, "the finished message", |s| node(s, spec::IMPLEMENTER)["message"] == FINISHED).await;

    configure(&harness, vec![shell("exit 0"), shell("echo again")]);
    let held = run(&harness, spec::IMPLEMENTER).await;
    assert_eq!(node(&held, spec::IMPLEMENTER)["message"], RUNNING);
    assert_eq!(harness.load().runs.len(), 0);
}

#[tokio::test]
async fn cancelling_the_task_stops_its_setup() {
    let harness = engine().await;
    harness.launchable();
    let long = if cfg!(windows) { shell("ping -n 60 127.0.0.1 > nul") } else { shell("sleep 60") };
    configure(&harness, vec![long]);
    set_node(&harness, spec::IMPLEMENTER, "READY", "manual");
    run(&harness, spec::IMPLEMENTER).await;

    let started = std::time::Instant::now();
    harness.engine.cancel_task("TEST-1").await.unwrap();
    let stopped = wait_for(&harness, "the cancellation", finished("task")).await;
    assert_eq!(stopped["worktreeSetup"][0]["error"], "Cancelled");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(stopped["status"], "CANCELLED");
}

#[tokio::test]
async fn each_worker_waits_for_its_own_worktree_setup() {
    let harness = engine().await;
    harness.launchable();
    harness.with_task(|s| {
        s.add_node(WorkflowNodeRow {
            id: 0,
            task_id: s.task.id,
            key: spec::ORCHESTRATOR.into(),
            kind: "agent".into(),
            label: "Orchestrator".into(),
            role: Some("orchestrator".into()),
            state: "RUNNING".into(),
            execution_mode: "manual".into(),
            iteration: 1,
            checkpoint_sha: None,
            message: None,
            error: None,
            config_override: Json(json!({})),
            updated_at: DbTime::now(),
        });
    });
    harness.git.returns("is_ancestor", Outcome::Ok(json!(true)));
    // No commands: fake git creates no worker directories to run them in.
    configure(&harness, vec![]);
    let plan = json!({"summary": "Focused work", "subtasks": (0..4).map(|i| json!({"key": format!("part-{i}"),
        "title": format!("Part {i}"), "brief": "Build the owned change", "ownedPaths": [format!("src/part-{i}")],
        "acceptanceCriteria": ["Tests pass"], "dependencies": []})).collect::<Vec<_>>()});
    let run_id = harness.add_run(spec::ORCHESTRATOR, "RUNNING", None);
    harness
        .engine
        .agent_complete("TEST-1", "orchestrator", Some(run_id), plan.as_object().unwrap().clone())
        .await
        .unwrap();

    let workers = |s: &Value| -> Vec<Value> {
        s["nodes"].as_array().unwrap().iter().filter(|n| n["worker"].is_object()).cloned().collect()
    };
    let states = |s: &Value| -> Vec<(String, String)> {
        workers(s)
            .iter()
            .map(|w| (w["state"].as_str().unwrap().to_string(), w["message"].as_str().unwrap_or("").to_string()))
            .collect()
    };
    // Held for setup, a worker keeps its slot: the fourth stays PENDING, its worktree not created.
    let held = snapshot(&harness);
    let waiting = (String::from("READY"), String::from(RUNNING));
    let pending = (String::from("PENDING"), String::new());
    assert_eq!(states(&held), vec![waiting.clone(), waiting.clone(), waiting, pending.clone()]);
    assert_eq!(harness.git.calls_of("create_worker_worktree").len(), 3, "worktrees exist before their setup");

    let done =
        wait_for(&harness, "three workers", |s| workers(s).iter().filter(|w| w["state"] == "RUNNING").count() == 3)
            .await;
    assert_eq!(states(&done)[3], pending, "the fourth waits for a free slot, it does not fail");
    let mut names: Vec<String> =
        done["worktreeSetup"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
    names.sort();
    let mut expected: Vec<String> =
        workers(&done)[..3].iter().map(|w| format!("worker-{}", w["worker"]["uid"].as_str().unwrap())).collect();
    expected.sort();
    assert_eq!(names, expected, "one setup per started worker worktree, none for the task worktree yet");
}

#[tokio::test]
async fn a_new_task_starts_its_setup_while_the_architect_works() {
    let harness = engine().await;
    configure(&harness, vec![]);
    let root = harness.tmp();
    harness.git.returns("inspect_repository", Outcome::Ok(json!({"path": root, "root": root, "branches": ["main"]})));
    harness.git.returns("create_worktree", Outcome::Ok(json!({"branch": "agent/NEW-1"})));
    let request: agentflow_core::engine::lifecycle::CreateTaskRequest =
        serde_json::from_value(json!({"jiraKey": "NEW-1", "repositoryPath": root, "baseBranch": "main"})).unwrap();
    let created = harness.engine.create_task(request.validated().unwrap()).await.unwrap();
    assert!(created["events"].as_array().unwrap().iter().any(|e| e["message"] == "Worktree setup started"));
    assert_eq!(created["worktreeSetup"][0]["name"], "task");

    for _ in 0..400 {
        let current = harness.engine.snapshot("NEW-1").unwrap();
        if current["worktreeSetup"][0]["state"] == "succeeded" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("setup of the new task did not finish");
}
