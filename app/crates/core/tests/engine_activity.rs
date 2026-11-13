//! Live activity of running agents: the trackers, the `activity` websocket message and the GET.

mod engine_support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentflow_core::engine::activity_trackers::TrackerSpec;
use agentflow_core::engine::launch::RunNodeRequest;
use agentflow_core::engine::spec;
use chrono::{SecondsFormat, TimeZone, Utc};
use engine_support::{Harness, engine};
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;

fn stamp(ms: i64) -> String {
    (Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + chrono::Duration::milliseconds(ms))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn prompt_line(ms: i64) -> String {
    json!({"type": "user", "timestamp": stamp(ms), "uuid": "u1", "origin": {"kind": "human"},
        "message": {"content": "do it"}})
    .to_string()
}

fn call_line(ms: i64, message_id: &str, tool_use_id: &str, command: &str) -> String {
    json!({"type": "assistant", "timestamp": stamp(ms), "message": {
        "id": message_id, "model": "claude-test",
        "usage": {"input_tokens": 1000, "output_tokens": 100},
        "content": [{"type": "tool_use", "id": tool_use_id, "name": "Bash", "input": {"command": command}}]}})
    .to_string()
}

fn result_line(ms: i64, tool_use_id: &str) -> String {
    json!({"type": "user", "timestamp": stamp(ms), "message": {"content": [
        {"type": "tool_result", "tool_use_id": tool_use_id, "is_error": false}]}})
    .to_string()
}

fn append(path: &Path, lines: &[String]) {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

fn fast(harness: &Harness) {
    harness.engine.activity_trackers.set_timing(Duration::from_millis(20), Duration::from_secs(60));
    harness.engine.activity_trackers.set_catalog_source(Arc::new(|| {
        let catalog = json!({"anthropic": {"models": {"claude-test": {
            "cost": {"input": 3, "output": 15}, "limit": {"context": 200000}}}}});
        Some(Arc::new(catalog.as_object().unwrap().clone()))
    }));
}

/// The next `activity` message that satisfies `wanted`; other messages are skipped.
async fn activity(client: &mut Receiver<Arc<str>>, wanted: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let text = client.recv().await.unwrap();
            let message: Value = serde_json::from_str(&text).unwrap();
            if message["type"] == "activity" && wanted(&message) {
                return message;
            }
        }
    })
    .await
    .expect("no matching activity message arrived")
}

fn has(message: &Value, id: &str, status: &str) -> bool {
    message["entries"].as_array().unwrap().iter().any(|entry| entry["id"] == id && entry["status"] == status)
}

#[tokio::test]
async fn a_running_agents_transcript_reaches_the_websocket_and_the_get() {
    let harness = engine().await;
    let home = harness.launchable();
    fast(&harness);
    let mut client = harness.engine.broadcaster.connect();
    let heard = Arc::new(Mutex::new(Vec::<String>::new()));
    let listener_heard = heard.clone();
    harness.engine.broadcaster.subscribe(move |message| {
        listener_heard.lock().unwrap().push(message["type"].as_str().unwrap_or_default().to_string());
    });

    harness.with_task(|task| {
        let index = task.node_index(spec::IMPLEMENTER).unwrap();
        task.nodes[index].state = "READY".into();
    });
    let launched =
        harness.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
    let run_id =
        launched["nodes"].as_array().unwrap().iter().find(|node| node["key"] == spec::IMPLEMENTER).unwrap()["lastRun"]
            ["id"]
            .as_i64()
            .unwrap();
    let (session_id, claude_id) = {
        let state = harness.load();
        let agent = state.agents.last().unwrap();
        (agent.id, agent.harness_session_id.clone().unwrap())
    };
    assert!(harness.engine.activity_trackers.is_tracking(session_id));
    let transcript = home.join(".claude").join("projects").join("repo").join(format!("{claude_id}.jsonl"));

    // The first lines: a prompt, and a command that has started.
    append(&transcript, &[prompt_line(0), call_line(1_000, "m1", "t1", "git status")]);
    let first = activity(&mut client, |m| has(m, "t1", "run")).await;
    assert_eq!(first["jiraKey"], "TEST-1");
    assert_eq!(first["nodeKey"], spec::IMPLEMENTER);
    assert_eq!(first["runId"], run_id);
    assert_eq!(first["sessionId"], session_id);
    assert_eq!(first["harness"], "claude");
    assert_eq!(first["summary"]["available"], true);
    assert_eq!(first["summary"]["steps"], 1);
    assert_eq!(first["summary"]["counts"]["git"], 1);
    assert_eq!(first["summary"]["counts"]["you"], 1);
    assert!(first["summary"]["costUsd"].as_f64().unwrap() > 0.0, "{first}");
    assert!(first["summary"]["lastWriteAt"].is_string());

    // Later messages carry only what changed, and never the history.
    append(&transcript, &[result_line(2_000, "t1")]);
    let second = activity(&mut client, |m| has(m, "t1", "ok")).await;
    let ids: Vec<&str> = second["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert!(!ids.iter().any(|id| id.starts_with("prompt-")), "{ids:?}");
    assert!(second.get("spans").is_none() && second.get("contextSteps").is_none());

    // The GET serves everything held for the task, history included.
    let (status, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    assert_eq!(status, 200, "{body}");
    let runs = body["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["runId"], run_id);
    assert!(runs[0]["spans"].as_array().unwrap().len() >= 3);
    assert!(runs[0]["contextSteps"].as_array().unwrap().len() == 1);
    assert_eq!(runs[0]["summary"]["ended"], false);
    assert_eq!(harness.route("GET", "/api/tasks/NOPE-1/activity", None).await.0, 404);

    // A call still open when the agent stops ends ok, and the tracker is gone.
    append(&transcript, &[call_line(3_000, "m2", "t2", "cargo test")]);
    activity(&mut client, |m| has(m, "t2", "run")).await;
    harness.engine.activity_trackers.cancel(session_id);
    assert!(!harness.engine.activity_trackers.is_tracking(session_id));
    let last = activity(&mut client, |m| m["summary"]["ended"] == true).await;
    assert!(has(&last, "t2", "ok"), "{last}");
    let (_, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    assert_eq!(body["runs"][0]["summary"]["ended"], true);
    assert_eq!(body["runs"][0]["summary"]["subagentsInFlight"], 0);

    // Nothing follows the file any more, and a second cancel changes nothing.
    append(&transcript, &[result_line(4_000, "t2")]);
    harness.engine.activity_trackers.cancel(session_id);
    tokio::time::sleep(Duration::from_millis(150)).await;
    while let Ok(text) = client.try_recv() {
        let message: Value = serde_json::from_str(&text).unwrap();
        assert_ne!(message["type"], "activity", "{message}");
    }

    // Activity goes to websocket clients only, never to in-process listeners.
    assert!(!heard.lock().unwrap().iter().any(|kind| kind == "activity"));
}

fn spec_for(harness: &str, run_id: i64, session_id: i64, id: Option<&str>, home: &Path, run_dir: &Path) -> TrackerSpec {
    TrackerSpec {
        jira_key: "TEST-1".into(),
        node_key: "implementer".into(),
        session_id,
        run_id,
        run_number: 1,
        harness: harness.into(),
        harness_session_id: id.map(str::to_string),
        home: home.to_path_buf(),
        run_dir: Some(run_dir.to_path_buf()),
    }
}

/// A copilot run's `events.jsonl` is followed like Claude's, and its subagents' lines, written into
/// the same file, hop to the `task` call that started them (fixture lines from a real session).
#[tokio::test]
async fn a_copilots_events_reach_the_websocket_and_the_get_with_their_subagents() {
    let harness = engine().await;
    fast(&harness);
    let home = harness.tmp().join("copilot-home");
    let run_dir = harness.tmp().join("copilot-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let mut client = harness.engine.broadcaster.connect();
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/copilot/subagents.jsonl"),
    )
    .unwrap();
    let lines: Vec<String> = fixture.lines().map(str::to_string).collect();
    let events = home.join(".copilot").join("session-state").join("sess-c").join("events.jsonl");

    // The file is not there yet (a fresh session): the tracker waits for it.
    harness.engine.activity_trackers.start(spec_for("copilot", 31, 41, Some("sess-c"), &home, &run_dir));
    // The task starts, its subagent works: a message and two searches begin.
    append(&events, &lines[0..5]);
    let out = activity(&mut client, |m| {
        m["runId"] == 31 && m["entries"].as_array().unwrap().iter().any(|e| e["bySubagent"] == true)
    })
    .await;
    assert_eq!(out["harness"], "copilot");
    assert_eq!(out["summary"]["available"], true);
    assert_eq!(out["summary"]["subagentsInFlight"], 1);
    let entries = out["entries"].as_array().unwrap();
    let task = entries.iter().find(|e| e["station"] == "agents").unwrap();
    assert_eq!(task["label"], "Explore the code");
    let hops: Vec<_> = entries.iter().filter(|e| e["bySubagent"] == true && e["station"] == "files").collect();
    assert!(!hops.is_empty() && hops.iter().all(|e| e["parentId"] == task["id"]), "{entries:?}");

    // The calls end and the subagent completes.
    append(&events, &lines[5..9]);
    activity(&mut client, |m| {
        m["runId"] == 31 && m["summary"]["subagentsInFlight"] == 0 && has(m, task["id"].as_str().unwrap(), "ok")
    })
    .await;

    let (status, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    assert_eq!(status, 200, "{body}");
    let run = &body["runs"][0];
    assert_eq!(run["harness"], "copilot");
    assert_eq!(run["summary"]["available"], true);
    assert!(run["spans"].as_array().unwrap().len() >= 3);
    assert!(run["summary"]["costUsd"].is_null(), "Copilot has no dollar cost: {run}");
    assert!(
        run["entries"].as_array().unwrap().iter().any(|e| e["bySubagent"] == true && e["parentId"] == task["id"]),
        "{run}"
    );
    harness.engine.activity_trackers.stop_all();
}

/// A Codex run's rollout is found by its session id once it is written, then followed like the
/// others: a script is seen running before it ends (fixture lines from a real rollout).
#[tokio::test]
async fn a_codexs_rollout_is_found_by_its_session_id_and_reaches_the_websocket_and_the_get() {
    let harness = engine().await;
    fast(&harness);
    let home = harness.tmp().join("codex-home");
    let run_dir = harness.tmp().join("codex-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let mut client = harness.engine.broadcaster.connect();
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/codex/typed.jsonl"),
    )
    .unwrap();
    let lines: Vec<String> = fixture.lines().map(str::to_string).collect();
    let id = "00000000-0000-4000-8000-0000000000a1";
    let rollout = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("11")
        .join("12")
        .join(format!("rollout-2026-11-12T22-14-20-{id}.jsonl"));

    // The id is known at launch; the file is not there yet (Codex writes it with its first turn).
    harness.engine.activity_trackers.start(spec_for("codex", 51, 61, Some(id), &home, &run_dir));
    // The prompt is read, and the model makes its first call: the script runs.
    append(&rollout, &lines[..10]);
    let running = activity(&mut client, |m| {
        m["runId"] == 51
            && m["entries"].as_array().unwrap().iter().any(|e| e["station"] == "shell" && e["status"] == "run")
    })
    .await;
    assert_eq!(running["harness"], "codex");
    assert_eq!(running["summary"]["available"], true);
    let call = running["entries"].as_array().unwrap().iter().find(|e| e["station"] == "shell").unwrap();
    assert_eq!(call["label"], "Get-Location");

    // The rest of the session: results, two failed commands, tokens, and the end of the turn.
    append(&rollout, &lines[10..]);
    let done = activity(&mut client, |m| m["runId"] == 51 && m["summary"]["steps"] == 10).await;
    assert_eq!(done["summary"]["tokens"], 451_170);
    assert_eq!(done["summary"]["errors"], 2);
    assert_eq!(done["summary"]["model"], "gpt-6.1-sol");

    let (status, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    assert_eq!(status, 200, "{body}");
    let run = &body["runs"][0];
    assert_eq!(run["harness"], "codex");
    assert_eq!(run["summary"]["contextPercent"], 12.7);
    assert!(run["summary"]["costUsd"].is_null(), "the catalog does not know this model: {run}");
    assert!(run["spans"].as_array().unwrap().len() >= 8);
    assert_eq!(run["contextSteps"].as_array().unwrap().len(), 10);
    harness.engine.activity_trackers.stop_all();
}

#[tokio::test]
async fn a_resumed_run_starts_at_its_recorded_offset_and_a_missing_transcript_is_unavailable() {
    let harness = engine().await;
    fast(&harness);
    harness.engine.activity_trackers.set_timing(Duration::from_millis(20), Duration::from_millis(200));
    let home = harness.tmp().join("tracker-home");
    let run_dir = harness.tmp().join("run-dir");
    std::fs::create_dir_all(&run_dir).unwrap();

    let mut client = harness.engine.broadcaster.connect();

    // An earlier run's lines come before the launch offset.
    let transcript = home.join(".claude").join("projects").join("repo").join("sess-a.jsonl");
    append(&transcript, &[call_line(0, "old", "old-t", "cargo build"), result_line(10, "old-t")]);
    let launch = std::fs::metadata(&transcript).unwrap().len();
    std::fs::write(
        run_dir.join("activity-offset.json"),
        // Spelled differently from the path a search finds: it is still the same file.
        json!({"path": transcript.parent().unwrap().join(".").join("sess-a.jsonl").to_string_lossy(), "offset": launch})
            .to_string(),
    )
    .unwrap();
    append(&transcript, &[call_line(1_000, "new", "new-t", "git log"), result_line(1_100, "new-t")]);
    harness.engine.activity_trackers.start(spec_for("claude", 11, 21, Some("sess-a"), &home, &run_dir));

    // No transcript for this one, ever.
    harness.engine.activity_trackers.start(spec_for(
        "claude",
        12,
        22,
        Some("sess-missing"),
        &home,
        &harness.tmp().join("no-offset"),
    ));
    // A Codex run whose session id was never found has no rollout to follow.
    harness.engine.activity_trackers.start(spec_for("codex", 13, 23, None, &home, &harness.tmp().join("no-offset")));

    let seen = activity(&mut client, |m| m["runId"] == 11 && has(m, "new-t", "ok")).await;
    assert_eq!(seen["summary"]["counts"]["git"], 1);
    assert!(seen["summary"]["counts"].get("shell").is_none(), "the earlier run's build is not this run's");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let (_, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    let runs = body["runs"].as_array().unwrap();
    assert_eq!(runs.iter().map(|r| r["runId"].as_i64().unwrap()).collect::<Vec<_>>(), [11, 12, 13]);
    let available: Vec<bool> = runs.iter().map(|r| r["summary"]["available"].as_bool().unwrap()).collect();
    assert_eq!(available, [true, false, false]);
    assert!(runs[0]["entries"].as_array().unwrap().iter().all(|e| e["id"] != "old-t"));
    // "Last written" is the file's own time, not when it was read.
    let written: chrono::DateTime<Utc> = std::fs::metadata(&transcript).unwrap().modified().unwrap().into();
    let reported: chrono::DateTime<Utc> = runs[0]["summary"]["lastWriteAt"].as_str().unwrap().parse().unwrap();
    assert!((reported - written).num_milliseconds().abs() < 5, "{reported} vs {written}");

    // Starting a run that is already followed does nothing, and stopping them all stops them.
    harness.engine.activity_trackers.start(spec_for("claude", 11, 21, Some("sess-a"), &home, &run_dir));
    assert!(harness.engine.activity_trackers.is_tracking(21));
    harness.engine.activity_trackers.stop_all();
    assert!(!harness.engine.activity_trackers.is_tracking(21));

    // A deleted task's runs are forgotten.
    harness.engine.activity_trackers.forget_task("test-1");
    let (_, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    assert!(body["runs"].as_array().unwrap().is_empty());
}

/// A run closed long after its transcript last moved ends its open calls when it last moved, not
/// when it was closed (a pane found stopped days later).
#[tokio::test]
async fn a_closed_runs_open_calls_end_when_its_transcript_last_moved() {
    let harness = engine().await;
    fast(&harness);
    let home = harness.tmp().join("late-home");
    let run_dir = harness.tmp().join("late-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let transcript = home.join(".claude").join("projects").join("repo").join("sess-late.jsonl");
    append(&transcript, &[call_line(0, "m1", "t1", "cargo build")]);
    // Its file time is old too: only the line's own time says when it moved.
    let old = std::time::UNIX_EPOCH + Duration::from_secs(946_684_800);
    std::fs::File::options().write(true).open(&transcript).unwrap().set_modified(old).unwrap();
    let mut client = harness.engine.broadcaster.connect();
    harness.engine.activity_trackers.start(spec_for("claude", 71, 81, Some("sess-late"), &home, &run_dir));
    activity(&mut client, |m| m["runId"] == 71 && has(m, "t1", "run")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    harness.engine.activity_trackers.cancel(81);
    let done = activity(&mut client, |m| m["runId"] == 71 && m["summary"]["ended"] == true).await;
    let call = done["entries"].as_array().unwrap().iter().find(|e| e["id"] == "t1").unwrap();
    assert_eq!(call["status"], "ok");
    assert_eq!(call["endedAt"], stamp(0), "when the transcript last moved, not now");
    harness.engine.activity_trackers.stop_all();
}

/// Only the newest 200 finished runs are kept.
#[tokio::test]
async fn the_oldest_finished_runs_are_forgotten() {
    let harness = engine().await;
    fast(&harness);
    let home = harness.tmp().join("many-home");
    let run_dir = harness.tmp().join("many-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    for n in 1..=205_i64 {
        harness.engine.activity_trackers.start(spec_for("claude", n, 1000 + n, None, &home, &run_dir));
        harness.engine.activity_trackers.cancel(1000 + n);
    }
    // Closing happens on blocking threads: wait until every run has ended.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (_, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
            let runs = body["runs"].as_array().unwrap();
            // (Starting the later runs already forgot the oldest ended ones.)
            if runs.len() >= 200 && runs.iter().all(|r| r["summary"]["ended"] == true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the runs did not all end");
    harness.engine.activity_trackers.start(spec_for("claude", 999, 2000, None, &home, &run_dir));
    let (_, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    let ids: Vec<i64> = body["runs"].as_array().unwrap().iter().map(|r| r["runId"].as_i64().unwrap()).collect();
    assert_eq!(ids.len(), 201, "200 finished runs and the new one");
    assert!(!ids.contains(&1) && !ids.contains(&5), "the oldest five are gone: {ids:?}");
    assert!(ids.contains(&205) && ids.contains(&999));
    harness.engine.activity_trackers.stop_all();
}
