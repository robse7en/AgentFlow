//! Fixes that came out of the full review of the activity map branch (2026-11-14): shell labels that
//! must never be command text, a subagent's work ending with its call, Claude's background agents,
//! calls that end after they left the newest entries, and hostile counts.

use std::io::Write;
use std::path::PathBuf;

use agentflow_core::engine::activity::{
    CallStatus, Parsed, RunActivity, Station, Tail, classify_shell, parse_line, parse_routed,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

fn base() -> DateTime<Utc> {
    "2026-11-14T10:00:00.000Z".parse().unwrap()
}

fn at(ms: i64) -> DateTime<Utc> {
    base() + Duration::milliseconds(ms)
}

fn stamp(ms: i64) -> String {
    at(ms).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn start(id: &str, station: Station, ms: i64) -> Parsed {
    Parsed::Start { id: id.into(), station, label: id.into(), at: at(ms), inbound: false }
}

fn end(id: &str, ms: i64) -> Parsed {
    Parsed::End { id: id.into(), at: at(ms), is_error: false }
}

fn run(harness: &str) -> RunActivity {
    RunActivity::new("PROJ-1", "architect", 1, 1, 1, harness, None)
}

fn open_entries(run: &RunActivity) -> Vec<&agentflow_core::engine::activity::ActivityEntry> {
    run.entries.iter().filter(|entry| entry.ended_at.is_none()).collect()
}

// ---- labels are never command text -----------------------------------------------------------

#[test]
fn a_command_that_starts_with_text_is_just_a_shell_call() {
    let commands = [
        "@'\nthe secret file contents\n'@ | Set-Content notes.md",
        "'secret text' | Out-File x",
        "\"secret text\" | Out-File x",
        "[IO.File]::WriteAllText(\"a.txt\",\"secret content\")",
        "$secret = 1; Write-Output $secret",
        "(Get-Content a.md) -join 'secret'",
        "\u{1b}[31msecret",
    ];
    for command in commands {
        let (_, label) = classify_shell(command, "");
        assert!(!label.contains("secret"), "{command:?} gave {label:?}");
        assert_eq!(label, "shell", "{command:?}");
    }
    // Real command names, paths to them, and Windows endings are still names.
    assert_eq!(classify_shell("cargo test", "").1, "cargo");
    assert_eq!(classify_shell("Get-Content a.md", "").1, "Get-Content");
    assert_eq!(classify_shell("/usr/local/bin/python3.11 -c x", "").1, "python3.11");
    assert_eq!(classify_shell("C:\\tools\\node.exe app.js", "").1, "node");
    assert_eq!(classify_shell("g++ main.cc", "").1, "g++");
    // A description is the agent's own short name for the call and is still used.
    assert_eq!(classify_shell("'text' | x", "Write the notes").1, "Write the notes");
}

// ---- a subagent's work ends with its call ----------------------------------------------------

fn copilot_fixture() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/copilot/subagents.jsonl");
    std::fs::read_to_string(path).unwrap().lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect()
}

fn feed_copilot(run: &mut RunActivity, lines: &[String]) {
    for line in lines {
        for (parent, event) in parse_routed("copilot", line) {
            run.apply_routed(parent.as_deref(), event);
        }
    }
}

#[test]
fn a_finished_subagent_leaves_nothing_of_its_own_open() {
    let lines = copilot_fixture();
    // The synchronous task: its calls, its completion, then the task call's own result.
    let mut sync = run("copilot");
    feed_copilot(&mut sync, &lines[..9]);
    let open = open_entries(&sync);
    assert!(open.iter().all(|entry| !entry.by_subagent), "a subagent entry is still running: {open:?}");
    // The main agent is back with the model, digesting what the subagent said.
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!((open[0].station, open[0].by_subagent), (Station::Model, false));
    assert_eq!(sync.summary.subagents_in_flight, 0);

    // The background task, and the one that failed, the same.
    let mut all = run("copilot");
    feed_copilot(&mut all, &lines);
    assert!(open_entries(&all).iter().all(|entry| !entry.by_subagent), "{:?}", open_entries(&all));
    assert_eq!(all.summary.subagents_in_flight, 0);
}

#[test]
fn the_model_resumes_after_a_subagent_that_finished_before_its_result() {
    // `Done` for the call first, then its own result: the result is where the model picks up.
    let mut run = run("copilot");
    run.apply(start("task", Station::Agents, 0));
    run.apply(Parsed::Done { id: "task".into(), at: at(1000), is_error: false });
    assert!(open_entries(&run).is_empty());
    run.apply(end("task", 1100));
    let open = open_entries(&run);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].station, Station::Model);
    // A second result for the same call changes nothing.
    run.apply(end("task", 1200));
    assert_eq!(open_entries(&run).len(), 1);
}

#[test]
fn a_stopped_subagents_calls_end_with_its_call() {
    let mut run = run("claude");
    run.apply(start("agent-call", Station::Agents, 0));
    run.apply_subagent("agent-a", Some("agent-call"), start("a-1", Station::Shell, 100));
    // The subagent was stopped: its call never reports back, and the main agent's call ends.
    run.apply(end("agent-call", 5_000));
    assert!(open_entries(&run).iter().all(|entry| !entry.by_subagent), "only the main model is left");
    let sub = run.entries.iter().find(|entry| entry.id == "a-1").unwrap();
    assert_eq!((sub.ended_at, sub.status), (Some(at(5_000)), CallStatus::Ok));
}

// ---- Claude's background agents --------------------------------------------------------------

fn claude(line: Value) -> Vec<Parsed> {
    parse_line("claude", &line.to_string())
}

fn launched_result(text: &str, is_error: bool) -> Value {
    json!({"type": "user", "timestamp": stamp(0), "message": {"content": [
        {"type": "tool_result", "tool_use_id": "toolu_bg", "is_error": is_error,
         "content": [{"type": "text", "text": text}]}]}})
}

fn notification(body: &str) -> Value {
    json!({"type": "user", "timestamp": stamp(60_000), "origin": {"kind": "task-notification"},
           "message": {"content": body}})
}

#[test]
fn a_background_agent_returns_at_once_and_ends_with_its_notification() {
    let launched = "Async agent launched successfully. It will notify you.";
    assert_eq!(claude(launched_result(launched, false)), vec![Parsed::Detach { id: "toolu_bg".into(), at: at(0) }]);
    // The same words as plain content, and as an ordinary result or an error, do not detach.
    let plain = json!({"type": "user", "timestamp": stamp(0), "message": {"content": [
        {"type": "tool_result", "tool_use_id": "toolu_bg", "content": launched}]}});
    assert_eq!(claude(plain), vec![Parsed::Detach { id: "toolu_bg".into(), at: at(0) }]);
    assert_eq!(claude(launched_result("Done: 3 files", false)), vec![end("toolu_bg", 0)]);
    assert_eq!(
        claude(launched_result(launched, true)),
        vec![Parsed::End { id: "toolu_bg".into(), at: at(0), is_error: true }]
    );

    let body = "<task-notification>\n<task-id>t1</task-id>\n<tool-use-id>toolu_bg</tool-use-id>\n\
                <status>completed</status>\n<summary>private words</summary>\n</task-notification>";
    assert_eq!(
        claude(notification(body)),
        vec![Parsed::Done { id: "toolu_bg".into(), at: at(60_000), is_error: false }]
    );
    let failed = body.replace("completed", "failed");
    assert_eq!(
        claude(notification(&failed)),
        vec![Parsed::Done { id: "toolu_bg".into(), at: at(60_000), is_error: true }]
    );
    // No id, or the same text from a line that is not a notification: nothing (and no prompt).
    assert!(claude(notification("<task-notification><status>completed</status></task-notification>")).is_empty());
    assert!(claude(notification("<tool-use-id></tool-use-id>")).is_empty());
    let human = json!({"type": "user", "timestamp": stamp(0), "message": {"content": body}});
    assert!(claude(human).is_empty(), "angle-bracket text is not a prompt");
}

#[test]
fn a_background_agent_stays_open_while_its_subagent_works() {
    let mut run = run("claude");
    run.apply(start("toolu_bg", Station::Agents, 0));
    run.apply(Parsed::Detach { id: "toolu_bg".into(), at: at(100) });
    assert_eq!(run.summary.subagents_in_flight, 1, "still going after the call returned");
    // Its subagent works on, and the main agent is seen with the model meanwhile.
    run.apply_subagent("agent-a", Some("toolu_bg"), start("a-1", Station::Shell, 1_000));
    run.apply_subagent("agent-a", Some("toolu_bg"), end("a-1", 2_000));
    assert!(open_entries(&run).iter().any(|entry| entry.station == Station::Model && !entry.by_subagent));
    run.apply(Parsed::Done { id: "toolu_bg".into(), at: at(60_000), is_error: false });
    assert_eq!(run.summary.subagents_in_flight, 0);
    assert!(open_entries(&run).iter().all(|entry| !entry.by_subagent), "its thinking ended with it");
    let agent = run.entries.iter().find(|entry| entry.id == "toolu_bg").unwrap();
    assert_eq!((agent.ended_at, agent.status), (Some(at(60_000)), CallStatus::Ok));
}

// ---- calls that end after they left the newest entries ---------------------------------------

#[test]
fn a_call_that_ends_after_leaving_the_newest_entries_is_announced_by_its_span() {
    let mut run = run("claude");
    run.apply(start("long", Station::Agents, 0));
    for i in 0..100 {
        run.apply(start(&format!("c{i}"), Station::Shell, 10 + i * 10));
        run.apply(end(&format!("c{i}"), 15 + i * 10));
    }
    assert!(run.entries.iter().all(|entry| entry.id != "long"), "it has left the newest 80");
    let first = run.take_delta().unwrap();
    assert!(first.span_ends.is_empty());

    run.apply(end("long", 5_000));
    let delta = run.take_delta().expect("the end of a call nobody lists is still news");
    assert_eq!(delta.span_ends.len(), 1);
    let span_end = &delta.span_ends[0];
    assert_eq!((span_end.started_at, span_end.ended_at, span_end.station), (at(0), at(5_000), Station::Agents));
    assert!(!span_end.by_subagent);
    assert_eq!(span_end.status, CallStatus::Ok);
    let message = run.message(&delta);
    assert_eq!(message["spanEnds"][0]["startedAt"], stamp(0));
    assert_eq!(message["spanEnds"][0]["endedAt"], stamp(5_000));
    assert_eq!(message["spanEnds"][0]["station"], "agents");
    assert_eq!(message["spanEnds"][0]["status"], "ok");
    // Told once.
    assert!(run.take_delta().is_none());

    // A call still listed is not announced this way, and a message without any has no such key.
    run.apply(start("short", Station::Files, 6_000));
    run.apply(end("short", 6_100));
    let delta = run.take_delta().unwrap();
    assert!(delta.span_ends.is_empty());
    assert!(run.message(&delta).get("spanEnds").is_none());
}

// ---- hostile input ---------------------------------------------------------------------------

#[test]
fn absurd_token_counts_neither_panic_nor_wrap() {
    let huge = "1e30";
    let claude_line = format!(
        r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"m1","model":"claude-test","usage":{{"input_tokens":{huge},"output_tokens":{huge},"cache_read_input_tokens":{huge},"cache_creation_input_tokens":{huge}}},"content":[]}}}}"#,
        stamp(0)
    );
    let mut run = run("claude");
    for event in parse_line("claude", &claude_line) {
        run.apply(event);
    }
    assert!(run.summary.tokens > 0 && run.summary.tokens < u64::MAX / 8, "{}", run.summary.tokens);

    let codex = format!(
        r#"{{"timestamp":"{}","type":"token_usage_record","payload":{{"response_id":"r","usage":{{"input_tokens":{huge},"cached_input_tokens":{huge},"cache_write_input_tokens":{huge},"output_tokens":{huge}}}}}}}"#,
        stamp(0)
    );
    let mut codex_run = self::run("codex");
    for event in parse_line("codex", &codex) {
        codex_run.apply(event);
    }
    assert!(codex_run.summary.tokens < u64::MAX / 8);

    let copilot = format!(
        r#"{{"type":"session.usage_record","timestamp":"{}","data":{{"usage":{{"inputTokens":{huge},"outputTokens":{huge},"maxPromptTokens":{huge},"apiCallId":"a"}}}}}}"#,
        stamp(0)
    );
    let mut copilot_run = self::run("copilot");
    for event in parse_line("copilot", &copilot) {
        copilot_run.apply(event);
    }
    assert!(copilot_run.summary.tokens < u64::MAX / 8);
}

#[test]
fn the_model_name_is_cleaned_like_a_label() {
    let mut run = run("claude");
    run.apply(Parsed::Usage {
        message_id: Some("a".into()),
        model: Some("gpt\u{1b}[31m-x\u{7}".into()),
        at: at(0),
        tokens: 1,
        context_percent: None,
    });
    let model = run.summary.model.clone().unwrap();
    assert!(!model.chars().any(char::is_control), "{model:?}");
    run.apply(Parsed::Step {
        message_id: Some("b".into()),
        model: Some(format!("m\u{1b}{}", "x".repeat(300))),
        at: at(10),
        input_tokens: 1,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
    });
    let model = run.summary.model.clone().unwrap();
    assert!(!model.chars().any(char::is_control) && model.chars().count() <= 120, "{model:?}");
}

#[test]
fn a_tail_never_prints_the_text_it_holds_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::File::create(&path).unwrap().write_all(b"{\"text\": \"secret half line").unwrap();
    let mut tail = Tail::new(&path, 0);
    tail.poll(|_| panic!("no whole line yet")).unwrap();
    let printed = format!("{tail:?}");
    assert!(!printed.contains("secret"), "{printed}");
    assert!(printed.contains("Tail"));
}

// ---- a replaced transcript, a restarted run, and a finished one ------------------------------

#[test]
fn a_transcript_replaced_by_a_longer_file_is_read_again_from_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, "{\"session\":\"first\"}\n{\"n\":1}\n").unwrap();
    let mut tail = Tail::new(&path, 0);
    let mut seen = Vec::new();
    let poll = tail.poll(|line| seen.push(line.to_string())).unwrap();
    assert!(!poll.restarted);
    assert_eq!(seen.len(), 2);

    // Growing is not replacing, also while the file is still shorter than the bytes remembered.
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"n\":2}\n").unwrap();
    let poll = tail.poll(|line| seen.push(line.to_string())).unwrap();
    assert!(!poll.restarted);
    assert_eq!(seen.len(), 3);

    // Another session's file, longer than the first, under the same name.
    let replacement = format!("{{\"session\":\"second\"}}\n{}", "{\"n\":9}\n".repeat(20));
    std::fs::write(&path, &replacement).unwrap();
    let mut again = Vec::new();
    let poll = tail.poll(|line| again.push(line.to_string())).unwrap();
    assert!(poll.restarted, "a longer file with other first bytes is another file");
    assert_eq!(again.len(), 21);
    assert_eq!(again[0], "{\"session\":\"second\"}");
    // And then it is followed as usual.
    let poll = tail.poll(|_| panic!("nothing new")).unwrap();
    assert!(!poll.restarted);
}

#[test]
fn a_tail_that_started_part_way_in_is_not_restarted_by_growth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let head = "{\"session\":\"s\"}\n".repeat(10);
    std::fs::write(&path, &head).unwrap();
    let mut tail = Tail::new(&path, head.len() as u64);
    assert!(!tail.poll(|_| panic!("nothing yet")).unwrap().restarted);
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"n\":1}\n").unwrap();
    let mut lines = 0;
    let poll = tail.poll(|_| lines += 1).unwrap();
    assert!(!poll.restarted);
    assert_eq!(lines, 1);
}

#[test]
fn a_rebuilt_run_tells_the_client_once() {
    let mut run = run("claude");
    run.apply(start("a", Station::Shell, 0));
    let first = run.take_delta().unwrap();
    assert!(!first.restarted);
    assert!(run.message(&first).get("restarted").is_none());
    run.reset();
    run.apply(start("b", Station::Files, 100));
    let delta = run.take_delta().unwrap();
    assert!(delta.restarted);
    assert_eq!(run.message(&delta)["restarted"], true);
    assert!(delta.entries.iter().all(|entry| entry.id != "a"));
    let next = {
        run.apply(end("b", 200));
        run.take_delta().unwrap()
    };
    assert!(!next.restarted, "told once");
}
