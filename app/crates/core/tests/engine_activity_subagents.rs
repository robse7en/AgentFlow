//! Claude subagents' transcripts: their calls reach the activity map with the `Agent` call that
//! started them. The fixtures in `tests/fixtures/activity/claude/subagent/` are a main transcript
//! with one `Agent` call and the matching `subagents/agent-x.jsonl` and `.meta.json`.

mod engine_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agentflow_core::engine::activity::{launch_offset, transcript_start};
use agentflow_core::engine::activity_trackers::TrackerSpec;
use chrono::{SecondsFormat, TimeZone, Utc};
use engine_support::{Harness, engine};
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;

fn stamp(ms: i64) -> String {
    (Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + chrono::Duration::milliseconds(ms))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
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

fn spec_for(run_id: i64, session_id: i64, id: &str, home: &Path, run_dir: &Path) -> TrackerSpec {
    TrackerSpec {
        jira_key: "TEST-1".into(),
        node_key: "implementer".into(),
        session_id,
        run_id,
        run_number: 1,
        harness: "claude".into(),
        harness_session_id: Some(id.to_string()),
        home: home.to_path_buf(),
        run_dir: Some(run_dir.to_path_buf()),
    }
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

/// The listed entry `id` of an update or a run, if it has one.
fn entry<'a>(message: &'a Value, id: &str) -> Option<&'a Value> {
    message["entries"].as_array()?.iter().find(|entry| entry["id"] == id)
}

fn has(message: &Value, id: &str, status: &str) -> bool {
    entry(message, id).is_some_and(|entry| entry["status"] == status)
}

/// The entry `id` of run `run_id` as the GET serves it.
async fn served(harness: &Harness, run_id: i64, id: &str) -> Option<Value> {
    let (_, body) = harness.route("GET", "/api/tasks/TEST-1/activity", None).await;
    let run = body["runs"].as_array()?.iter().find(|run| run["runId"] == run_id)?;
    entry(run, id).cloned()
}

/// The entry `id` once the GET serves it as finished.
async fn finished(harness: &Harness, run_id: i64, id: &str) -> Value {
    for _ in 0..500 {
        if let Some(found) = served(harness, run_id, id).await
            && found["status"] == "ok"
        {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{id} of run {run_id} never finished");
}

fn fixture(name: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/claude/subagent").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect()
}

fn write_meta(dir: &Path, key: &str, tool_use_id: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("{key}.meta.json")),
        json!({"agentType": "Explore", "description": "Look around", "toolUseId": tool_use_id}).to_string(),
    )
    .unwrap();
}

/// `(bySubagent, parentId)` of a listed entry.
fn lineage(entry: &Value) -> (Value, Value) {
    (entry["bySubagent"].clone(), entry["parentId"].clone())
}

#[tokio::test]
async fn a_subagents_calls_arrive_with_their_parent_and_the_run_counts_it_in_flight() {
    let harness = engine().await;
    fast(&harness);
    let home = harness.tmp().join("sub-home");
    let run_dir = harness.tmp().join("sub-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let transcript = home.join(".claude").join("projects").join("repo").join("sess-s.jsonl");
    let subagents = transcript.with_extension("").join("subagents");
    let (main, sub) = (fixture("main.jsonl"), fixture("agent-x.jsonl"));
    let mut client = harness.engine.broadcaster.connect();

    harness.engine.activity_trackers.start(spec_for(31, 41, "sess-s", &home, &run_dir));
    append(&transcript, &main[..2]);
    let started = activity(&mut client, |m| has(m, "toolu_agent1", "run")).await;
    assert_eq!(started["summary"]["subagentsInFlight"], 1, "rises with the Agent call");

    // The subagent's transcript appears, its meta file already beside it.
    write_meta(&subagents, "agent-x", "toolu_agent1");
    append(&subagents.join("agent-x.jsonl"), &sub[..3]);
    let seen = activity(&mut client, |m| has(m, "toolu_sub1", "ok")).await;
    let read = entry(&seen, "toolu_sub1").unwrap();
    assert_eq!(lineage(read), (json!(true), json!("toolu_agent1")));
    assert_eq!(read["station"], "files");
    assert!(seen["entries"].as_array().unwrap().iter().all(|e| e["inbound"] != true), "{seen}");
    assert_eq!(seen["summary"]["counts"]["you"], 1, "the prompt the parent wrote is not the person's");
    assert_eq!(seen["summary"]["subagentsInFlight"], 1);

    // The rest of its work, then the Agent call ends: in flight falls back to nothing.
    append(&subagents.join("agent-x.jsonl"), &sub[3..]);
    append(&transcript, &main[2..]);
    let done = activity(&mut client, |m| m["summary"]["subagentsInFlight"] == 0 && has(m, "toolu_agent1", "ok")).await;
    assert_eq!(done["summary"]["steps"], 1, "a subagent's steps are not the run's own");

    let bash = served(&harness, 31, "toolu_sub2").await.expect("served by the GET");
    assert_eq!(bash["status"], "ok");
    assert_eq!(lineage(&bash), (json!(true), json!("toolu_agent1")));
    assert_eq!(bash["station"], "git");
    let agent = served(&harness, 31, "toolu_agent1").await.unwrap();
    assert_eq!(lineage(&agent), (json!(false), Value::Null));
    harness.engine.activity_trackers.stop_all();
}

#[tokio::test]
async fn a_subagent_file_from_before_launch_is_read_only_from_its_launch_length() {
    let harness = engine().await;
    fast(&harness);
    let home = harness.tmp().join("resume-home");
    let run_dir = harness.tmp().join("resume-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let transcript = home.join(".claude").join("projects").join("repo").join("sess-r.jsonl");
    let subagents = transcript.with_extension("").join("subagents");
    let (main, sub) = (fixture("main.jsonl"), fixture("agent-x.jsonl"));

    // An earlier run left a transcript, and a subagent that had made a read.
    append(&transcript, &main[..2]);
    write_meta(&subagents, "agent-old", "toolu_agent1");
    append(&subagents.join("agent-old.jsonl"), &sub[..3]);
    let old_length = std::fs::metadata(subagents.join("agent-old.jsonl")).unwrap().len();

    // The launch records where everything stood.
    let recorded = launch_offset(&transcript_start(&home, "claude", Some("sess-r")));
    assert_eq!(recorded["subagents"], json!({"agent-old.jsonl": old_length}));
    std::fs::write(run_dir.join("activity-offset.json"), recorded.to_string()).unwrap();
    harness.engine.activity_trackers.start(spec_for(32, 42, "sess-r", &home, &run_dir));

    // The old subagent carries on, and a new one starts (its meta file never comes).
    append(&subagents.join("agent-old.jsonl"), &sub[3..]);
    append(
        &subagents.join("agent-new.jsonl"),
        &[call_line(9_000, "m9", "new-call", "ls"), result_line(9_500, "new-call")],
    );
    let carried_on = finished(&harness, 32, "toolu_sub2").await;
    assert_eq!(lineage(&carried_on), (json!(true), json!("toolu_agent1")));
    let new = finished(&harness, 32, "new-call").await;
    assert!(served(&harness, 32, "toolu_sub1").await.is_none(), "the read made before launch is not this run's");
    assert_eq!(lineage(&new), (json!(true), Value::Null));
    assert!(served(&harness, 32, "toolu_agent1").await.is_none(), "nor are the main transcript's earlier lines");
    harness.engine.activity_trackers.stop_all();
}

#[tokio::test]
async fn a_meta_file_that_comes_late_fills_in_the_parent_and_one_that_never_comes_is_given_up_on() {
    let harness = engine().await;
    fast(&harness);
    harness.engine.activity_trackers.set_timing(Duration::from_millis(20), Duration::from_millis(400));
    let home = harness.tmp().join("late-home");
    let run_dir = harness.tmp().join("late-run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let projects = home.join(".claude").join("projects").join("repo");
    let (main, sub) = (fixture("main.jsonl"), fixture("agent-x.jsonl"));
    let mut client = harness.engine.broadcaster.connect();

    // Run 33: the transcript comes first, the meta file after.
    let late = projects.join("sess-l.jsonl");
    let late_dir = late.with_extension("").join("subagents");
    harness.engine.activity_trackers.start(spec_for(33, 43, "sess-l", &home, &run_dir));
    append(&late, &main[..2]);
    append(&late_dir.join("agent-x.jsonl"), &sub[..3]);
    let first = activity(&mut client, |m| m["runId"] == 33 && has(m, "toolu_sub1", "ok")).await;
    assert_eq!(lineage(entry(&first, "toolu_sub1").unwrap()), (json!(true), Value::Null), "not known yet");

    write_meta(&late_dir, "agent-x", "toolu_agent1");
    let filled = activity(&mut client, |m| {
        m["runId"] == 33 && entry(m, "toolu_sub1").is_some_and(|e| e["parentId"] == "toolu_agent1")
    })
    .await;
    assert_eq!(entry(&filled, "toolu_sub1").unwrap()["status"], "ok");
    assert!(entry(&filled, "toolu_agent1").is_none(), "only the changed entry is sent");
    append(&late_dir.join("agent-x.jsonl"), &sub[3..4]);
    activity(&mut client, |m| {
        m["runId"] == 33 && entry(m, "toolu_sub2").is_some_and(|e| e["parentId"] == "toolu_agent1")
    })
    .await;

    // Run 34: the meta file does not come in time, so its calls stay without a parent for good.
    let missing = projects.join("sess-m.jsonl");
    let missing_dir = missing.with_extension("").join("subagents");
    harness.engine.activity_trackers.start(spec_for(34, 44, "sess-m", &home, &harness.tmp().join("none")));
    append(&missing, &main[..2]);
    append(
        &missing_dir.join("agent-y.jsonl"),
        &[call_line(1_000, "my1", "y-call", "ls"), result_line(1_100, "y-call")],
    );
    activity(&mut client, |m| m["runId"] == 34 && has(m, "y-call", "ok")).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    write_meta(&missing_dir, "agent-y", "toolu_agent1");
    append(&missing_dir.join("agent-y.jsonl"), &[call_line(2_000, "my2", "y-late", "ls")]);
    activity(&mut client, |m| m["runId"] == 34 && has(m, "y-late", "run")).await;
    for id in ["y-call", "y-late"] {
        let held = served(&harness, 34, id).await.unwrap();
        assert_eq!(lineage(&held), (json!(true), Value::Null), "{id}");
    }
    harness.engine.activity_trackers.stop_all();
}
