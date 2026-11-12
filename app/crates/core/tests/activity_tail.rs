//! Tailing a transcript and keeping the per-run summary (`engine::activity`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use agentflow_core::engine::activity::{
    CallStatus, MAX_CONTEXT_STEPS, MAX_ENTRIES, MAX_SPANS, Parsed, RunActivity, Station, Tail, launch_offset,
    parse_line, transcript_start,
};
use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use serde_json::{Map, Value, json};

fn base() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

fn at(ms: i64) -> DateTime<Utc> {
    base() + Duration::milliseconds(ms)
}

fn stamp(ms: i64) -> String {
    at(ms).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn catalog() -> Arc<Map<String, Value>> {
    let value = json!({"anthropic": {"models": {"claude-test": {
        "cost": {"input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75},
        "limit": {"context": 200000, "output": 64000}
    }}}});
    Arc::new(value.as_object().unwrap().clone())
}

fn run(catalog: Option<Arc<Map<String, Value>>>) -> RunActivity {
    RunActivity::new("PROJ-1", "architect", 7, 3, 1, "claude", catalog)
}

fn start(id: &str, station: Station, ms: i64) -> Parsed {
    Parsed::Start { id: id.into(), station, label: id.into(), at: at(ms), inbound: false }
}

fn end(id: &str, ms: i64, is_error: bool) -> Parsed {
    Parsed::End { id: id.into(), at: at(ms), is_error }
}

fn step(id: Option<&str>, ms: i64, input: u64) -> Parsed {
    Parsed::Step {
        message_id: id.map(str::to_string),
        model: Some("claude-test".into()),
        at: at(ms),
        input_tokens: input,
        output_tokens: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
    }
}

// ---- Claude lines, as the transcript writes them ----

fn assistant_line(ms: i64, message_id: &str, tool: Option<(&str, &str)>) -> String {
    let mut content = vec![json!({"type": "text"})];
    if let Some((id, command)) = tool {
        content.push(json!({"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}));
    }
    json!({"type": "assistant", "timestamp": stamp(ms), "message": {
        "id": message_id, "model": "claude-test",
        "usage": {"input_tokens": 100, "output_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
        "content": content}})
    .to_string()
}

fn result_line(ms: i64, id: &str, is_error: bool) -> String {
    json!({"type": "user", "timestamp": stamp(ms), "message": {"content": [
        {"type": "tool_result", "tool_use_id": id, "is_error": is_error}]}})
    .to_string()
}

fn prompt_line(ms: i64) -> String {
    json!({"type": "user", "timestamp": stamp(ms), "uuid": format!("u{ms}"), "origin": {"kind": "human"},
        "message": {"content": "x"}})
    .to_string()
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn poll(tail: &mut Tail) -> Vec<String> {
    let mut lines = Vec::new();
    tail.poll(|line| lines.push(line.to_string())).unwrap();
    lines
}

fn feed(run: &mut RunActivity, lines: &[String]) {
    for line in lines {
        for event in parse_line("claude", line) {
            run.apply(event);
        }
    }
}

fn json_of(run: &RunActivity) -> Value {
    serde_json::to_value(run).unwrap()
}

// ---- Tail ----

#[test]
fn lines_arrive_in_pieces_and_a_split_line_waits_for_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let mut tail = Tail::new(&path, 0);
    assert!(poll(&mut tail).is_empty(), "a missing file is no lines, not an error");

    append(&path, "one\ntwo\nthr");
    assert_eq!(poll(&mut tail), ["one", "two"]);
    assert_eq!(tail.offset(), 8, "the partial line is not committed");

    append(&path, "ee\nfour");
    assert_eq!(poll(&mut tail), ["three"]);
    assert!(poll(&mut tail).is_empty());

    append(&path, "\n\n  \nfive\r\n");
    assert_eq!(poll(&mut tail), ["four", "five"], "blank lines are skipped and \\r\\n is one ending");
    assert_eq!(tail.offset(), std::fs::metadata(&path).unwrap().len());
}

#[test]
fn a_character_split_across_reads_stays_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let line = "caf\u{e9} \u{1f980}\n".as_bytes();
    let mut tail = Tail::new(&path, 0);
    // Cut inside the two-byte "é" and again inside the four-byte crab.
    for part in [&line[..4], &line[4..8], &line[8..]] {
        let mut file = OpenOptions::new().create(true).append(true).open(&path).unwrap();
        file.write_all(part).unwrap();
        drop(file);
        let lines = poll(&mut tail);
        assert!(lines.is_empty() || lines == ["caf\u{e9} \u{1f980}"], "{lines:?}");
    }
    assert_eq!(tail.offset(), line.len() as u64);
}

#[test]
fn a_resumed_file_is_read_from_the_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "before 1\nbefore 2\n");
    let launch = std::fs::metadata(&path).unwrap().len();
    append(&path, "after 1\nafter 2\n");
    let mut tail = Tail::new(&path, launch);
    assert_eq!(poll(&mut tail), ["after 1", "after 2"]);

    // A later reader that starts where the first one left off sees only what came since.
    append(&path, "after 3\npart");
    let mut later = Tail::new(&path, tail.offset());
    assert_eq!(poll(&mut later), ["after 3"]);
    assert_eq!(poll(&mut tail), ["after 3"]);
}

#[test]
fn a_file_that_shrinks_is_read_again_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "aaaa\nbbbb\n");
    let mut tail = Tail::new(&path, 0);
    assert_eq!(poll(&mut tail).len(), 2);
    std::fs::write(&path, "c\n").unwrap();
    let mut lines = Vec::new();
    let polled = tail.poll(|line| lines.push(line.to_string())).unwrap();
    assert!(polled.restarted && polled.grew);
    assert_eq!(lines, ["c"]);
}

// ---- RunActivity ----

#[test]
fn finish_closes_open_calls_as_ok() {
    let mut run = run(None);
    run.apply(start("shell", Station::Shell, 0));
    run.apply(start("agent", Station::Agents, 100));
    assert_eq!(run.summary.subagents_in_flight, 1);
    assert_eq!(run.open_calls(), 2);

    run.finish(at(1_000));
    assert_eq!(run.open_calls(), 0);
    assert!(run.entries.iter().all(|e| e.status == CallStatus::Ok && e.ended_at == Some(at(1_000))));
    assert!(run.spans.iter().all(|s| s.status == CallStatus::Ok && s.ended_at == Some(at(1_000))));
    assert_eq!(run.summary.subagents_in_flight, 0);
    assert!(run.summary.ended);
    assert_eq!(run.summary.busy_ms[&Station::Shell], 1_000);
    assert_eq!(run.summary.busy_ms[&Station::Agents], 900);
    assert_eq!(run.summary.errors, 0);
}

#[test]
fn busy_time_leaves_out_inbound_prompts() {
    let mut run = run(None);
    run.apply(Parsed::Start {
        id: "p".into(),
        station: Station::You,
        label: "Prompt".into(),
        at: at(0),
        inbound: true,
    });
    run.apply(start("f", Station::Files, 500));
    run.apply(end("f", 800, false));
    assert_eq!(run.summary.counts[&Station::You], 1);
    assert!(!run.summary.busy_ms.contains_key(&Station::You));
    assert_eq!(run.summary.busy_ms[&Station::Files], 300);
    let prompt = &run.entries[0];
    assert!(prompt.inbound && prompt.ended_at == Some(at(0)) && prompt.status == CallStatus::Ok);
    assert!(run.spans[0].inbound);
    // The prompt itself never waits for an End; the model's work on it does, and is not "busy" for You.
    assert_eq!(run.open_calls(), 1);
    assert_eq!(run.summary.busy_ms[&Station::Model], 500, "the model worked from the prompt to the file read");
}

#[test]
fn errors_are_counted_and_the_entry_shows_them() {
    let mut run = run(None);
    run.apply(start("a", Station::Shell, 0));
    run.apply(end("a", 10, true));
    run.apply(end("a", 20, true));
    run.apply(end("never-started", 30, true));
    assert_eq!(run.summary.errors, 1);
    assert_eq!(run.entries[0].status, CallStatus::Err);
    assert_eq!(run.spans[0].status, CallStatus::Err);
}

#[test]
fn spans_and_context_steps_stop_at_their_caps_and_drop_the_oldest() {
    let mut run = run(Some(catalog()));
    for n in 0..MAX_SPANS + 100 {
        run.apply(start(&format!("c{n}"), Station::Shell, n as i64));
    }
    assert_eq!(run.spans.len(), MAX_SPANS);
    assert_eq!(run.entries.len(), MAX_ENTRIES);
    assert_eq!(run.entries.front().unwrap().id, format!("c{}", MAX_SPANS + 100 - MAX_ENTRIES));
    assert_eq!(run.spans.front().unwrap().started_at, at(100), "the 100 oldest are gone");

    // Calls that end after they left the lists still count, and the ones still held are updated.
    let total = (MAX_SPANS + 100) as i64;
    run.apply(end("c0", total, true));
    for n in 1..MAX_SPANS + 100 {
        run.apply(end(&format!("c{n}"), total, false));
    }
    assert_eq!(run.open_calls(), 1, "the model is working again after the last call");
    run.finish(at(total));
    assert_eq!(run.summary.errors, 1);
    assert_eq!(run.summary.counts[&Station::Shell] as usize, MAX_SPANS + 100);
    assert_eq!(run.open_calls(), 0);
    assert!(run.spans.iter().all(|s| s.ended_at == Some(at(total))));
    let busy: i64 = (0..total).map(|n| total - n).sum();
    assert_eq!(run.summary.busy_ms[&Station::Shell] as i64, busy);

    for n in 0..MAX_CONTEXT_STEPS + 50 {
        run.apply(step(Some(&format!("s:m{n}")), 10_000 + n as i64, 2_000 + n as u64));
    }
    assert_eq!(run.context_steps.len(), MAX_CONTEXT_STEPS);
    assert_eq!(run.context_steps.front().unwrap().at, at(10_050), "the 50 oldest are gone");
}

#[test]
fn the_model_is_working_from_a_prompt_or_a_call_end_until_its_next_line() {
    let mut run = run(None);
    let model_run = |run: &RunActivity| {
        run.entries.iter().filter(|e| e.station == Station::Model && e.status == CallStatus::Run).count()
    };
    run.apply(Parsed::Start {
        id: "p1".into(),
        station: Station::You,
        label: "Prompt".into(),
        at: at(0),
        inbound: true,
    });
    assert_eq!(model_run(&run), 1, "after a prompt the model is thinking");
    run.apply(step(Some("s:m1"), 400, 10));
    assert_eq!(model_run(&run), 0);
    let thought = run.entries.iter().find(|e| e.station == Station::Model).unwrap();
    assert_eq!((thought.started_at, thought.ended_at, thought.label.as_str()), (at(0), Some(at(400)), "claude-test"));

    run.apply(start("a", Station::Files, 400));
    run.apply(start("b", Station::Shell, 400));
    run.apply(end("a", 500, false));
    assert_eq!(model_run(&run), 0, "a call is still running");
    run.apply(end("b", 600, false));
    assert_eq!(model_run(&run), 1, "every call is back");
    // A call (or the next prompt) from the model ends its work even without a step line.
    run.apply(start("c", Station::Network, 650));
    assert_eq!(model_run(&run), 0);
    assert_eq!(run.summary.busy_ms[&Station::Model], 400 + 50);

    // A turn that ends without an answer (an error, an interruption) stops the model's work too.
    run.apply(end("c", 700, false));
    assert_eq!(model_run(&run), 1);
    run.apply(Parsed::TurnEnd { at: at(705) });
    assert_eq!(model_run(&run), 0);
    assert_eq!(run.summary.busy_ms[&Station::Model], 400 + 50 + 5);
    run.apply(Parsed::TurnEnd { at: at(706) });

    // An agent waiting on a subagent is not thinking either.
    run.apply(start("agent", Station::Agents, 710));
    run.apply(end("agent", 900, false));
    assert_eq!(model_run(&run), 1);
    run.finish(at(1_000));
    assert_eq!(model_run(&run), 0);
    assert_eq!(run.open_calls(), 0);
}

#[test]
fn a_step_after_an_idle_gap_takes_no_time() {
    let mut run = run(None);
    run.apply(start("a", Station::Shell, 0));
    run.apply(end("a", 100, false));
    run.apply(step(Some("s:m1"), 600, 10));
    assert_eq!(run.summary.busy_ms[&Station::Model], 500);
    // The turn is over; a task notification (the parser skips it) wakes the agent an hour later.
    run.apply(step(Some("s:m2"), 3_600_000, 10));
    run.apply(step(None, 3_700_000, 10));
    assert_eq!(run.summary.busy_ms[&Station::Model], 500, "the idle hour is not model time");
    assert_eq!(run.summary.counts[&Station::Model], 3);
    assert_eq!(run.summary.steps, 3);
}

#[test]
fn model_entries_are_named_by_their_message_not_by_arrival_order() {
    let mut one = run(None);
    let mut other = run(None);
    one.apply(step(Some("s:main"), 10, 1));
    one.apply_subagent("agent-1", Some("agent"), step(Some("s:sub"), 20, 1));
    other.apply_subagent("agent-1", Some("agent"), step(Some("s:sub"), 20, 1));
    other.apply(step(Some("s:main"), 10, 1));
    let ids = |run: &RunActivity| {
        let mut ids: Vec<String> = run.entries.iter().map(|e| e.id.clone()).collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(&one), ids(&other));
    assert_eq!(ids(&one), ["model-s:main", "model-s:sub"]);
}

#[test]
fn a_message_written_as_several_lines_counts_once() {
    let mut run = run(Some(catalog()));
    feed(
        &mut run,
        &[
            assistant_line(0, "m1", None),
            assistant_line(10, "m1", None),
            assistant_line(20, "m1", Some(("t1", "git status"))),
            result_line(120, "t1", false),
            assistant_line(130, "m2", None),
        ],
    );
    let summary = &run.summary;
    assert_eq!(summary.steps, 2);
    assert_eq!(summary.tokens, 220, "two messages of 100 in and 10 out");
    assert_eq!(summary.counts[&Station::Model], 2);
    assert_eq!(run.context_steps.len(), 2);
    assert_eq!(summary.counts[&Station::Git], 1);
    assert_eq!(summary.model.as_deref(), Some("claude-test"));
    // A model step runs from the last thing that happened to the line that closes it.
    assert_eq!(run.entries[0].started_at, at(0));
    let second = run.entries.iter().filter(|e| e.station == Station::Model).nth(1).unwrap();
    assert_eq!((second.started_at, second.ended_at), (at(120), Some(at(130))));
}

#[test]
fn cost_and_context_follow_the_catalog() {
    let mut run = run(Some(catalog()));
    run.apply(Parsed::Step {
        message_id: Some("s:m1".into()),
        model: Some("claude-test".into()),
        at: at(0),
        input_tokens: 1_000,
        output_tokens: 500,
        cache_read: 2_000,
        cache_write: 4_000,
        cache_write_1h: 1_000,
    });
    // 3000 uncached-priced writes at 3.75, 1000 one-hour writes at twice the input price, not both.
    let expected = (1_000.0 * 3.0 + 2_000.0 * 0.3 + 3_000.0 * 3.75 + 1_000.0 * 3.0 * 2.0 + 500.0 * 15.0) / 1e6;
    assert!((run.summary.cost_usd.unwrap() - expected).abs() < 1e-12, "{:?}", run.summary.cost_usd);
    assert_eq!(run.summary.context_percent, Some(3.5));
    assert_eq!(run.summary.tokens, 7_500);

    // A model with no prices makes the cost unknown, and it stays unknown.
    run.apply(Parsed::Step {
        message_id: None,
        model: Some("mystery".into()),
        at: at(10),
        input_tokens: 1,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: 0,
    });
    assert_eq!(run.summary.cost_usd, None);
    assert_eq!(run.summary.context_percent, None);
    run.apply(step(None, 20, 5));
    assert_eq!(run.summary.cost_usd, None);

    let bare = {
        let mut bare = run_without_catalog();
        bare.apply(step(None, 0, 10));
        bare
    };
    assert_eq!(bare.summary.cost_usd, None);
    assert_eq!(bare.summary.context_percent, None);
    assert_eq!(bare.summary.tokens, 10);
}

fn run_without_catalog() -> RunActivity {
    run(None)
}

#[test]
fn subagent_calls_are_marked_and_their_steps_are_not_the_runs_own() {
    let mut run = run(Some(catalog()));
    run.apply(step(Some("s:main"), 0, 1_000));
    run.apply(start("agent", Station::Agents, 100));
    assert_eq!(run.summary.subagents_in_flight, 1);

    run.apply_subagent("agent-1", Some("agent"), step(Some("s:sub"), 150, 50_000));
    run.apply_subagent("agent-1", Some("agent"), start("sub-read", Station::Files, 160));
    run.apply_subagent("agent-1", Some("agent"), end("sub-read", 200, false));
    // A subagent's own `Agent` call is not one of the run's subagents.
    run.apply_subagent("agent-1", Some("agent"), start("nested", Station::Agents, 210));
    assert_eq!(run.summary.subagents_in_flight, 1);
    run.apply_subagent("agent-1", Some("agent"), end("nested", 220, false));
    assert_eq!(run.summary.subagents_in_flight, 1);

    let read = run.entries.iter().find(|e| e.id == "sub-read").unwrap();
    assert!(read.by_subagent);
    assert_eq!(read.parent_id.as_deref(), Some("agent"));
    let sub_step = run.entries.iter().find(|e| e.station == Station::Model && e.by_subagent).unwrap();
    assert_eq!(sub_step.parent_id.as_deref(), Some("agent"));
    assert_eq!(sub_step.started_at, at(150), "a subagent's first step has no earlier event of its own");
    assert!(run.spans.iter().filter(|s| s.by_subagent).count() >= 3);

    assert_eq!(run.summary.steps, 1);
    assert_eq!(run.summary.tokens, 1_000);
    assert_eq!(run.summary.context_percent, Some(0.5));
    assert_eq!(run.context_steps.len(), 1);
    // The run's step, the subagent's step, and the subagent's two stretches of work after a call.
    assert_eq!(run.summary.counts[&Station::Model], 4, "its calls still appear in the counts");

    run.apply(end("agent", 300, false));
    assert_eq!(run.summary.subagents_in_flight, 0);

    // The meta file may not be read yet: the calls have no parent for now.
    run.apply_subagent("agent-orphan", None, start("orphan", Station::Shell, 400));
    let orphan = run.entries.iter().find(|e| e.id == "orphan").unwrap();
    assert!(orphan.by_subagent && orphan.parent_id.is_none());
}

#[test]
fn last_write_only_moves_forward_and_unavailable_is_remembered() {
    let mut run = run(None);
    assert!(run.summary.available);
    assert_eq!(run.summary.last_write_at, None);
    run.note_write(at(500));
    run.note_write(at(100));
    assert_eq!(run.summary.last_write_at, Some(at(500)));
    run.mark_unavailable();
    assert!(!run.summary.available);
}

#[test]
fn a_rebuild_from_the_launch_offset_gives_the_same_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    // An earlier session's lines come before the launch offset and are never read.
    append(
        &path,
        &format!("{}\n{}\n", assistant_line(0, "old", Some(("old-t", "cargo build"))), result_line(5, "old-t", true)),
    );
    let launch = std::fs::metadata(&path).unwrap().len();

    let lines = [
        prompt_line(1_000),
        assistant_line(1_100, "m1", Some(("t1", "git status"))),
        result_line(1_400, "t1", false),
        assistant_line(1_500, "m2", Some(("t2", "cargo test"))),
        result_line(2_500, "t2", true),
        assistant_line(2_600, "m3", None),
    ];
    let mut live = run(Some(catalog()));
    let mut tail = Tail::new(&path, launch);
    for line in &lines {
        // One line at a time, the last one cut in two.
        let text = format!("{line}\n");
        let (first, second) = text.split_at(text.len() / 2);
        append(&path, first);
        let seen = poll(&mut tail);
        feed(&mut live, &seen);
        append(&path, second);
        let seen = poll(&mut tail);
        feed(&mut live, &seen);
    }
    assert_eq!(live.summary.errors, 1, "the earlier session's failure is not this run's");
    assert_eq!(live.summary.steps, 3);

    let mut rebuilt = run(Some(catalog()));
    let mut again = Tail::new(&path, launch);
    feed(&mut rebuilt, &poll(&mut again));
    assert_eq!(json_of(&live), json_of(&rebuilt));
    assert_eq!(again.offset(), tail.offset());

    // Closing it the same way ends them the same way too.
    live.finish(at(9_000));
    rebuilt.finish(at(9_000));
    assert_eq!(json_of(&live), json_of(&rebuilt));
}

#[test]
fn the_wire_shape_has_camel_case_names_and_leaves_out_empty_history() {
    let mut run = run(None);
    let before = json_of(&run);
    assert!(before.get("spans").is_none() && before.get("contextSteps").is_none());
    assert_eq!(before["summary"]["available"], json!(true));
    assert_eq!(before["summary"]["lastWriteAt"], Value::Null);
    assert!(before.get("open").is_none() && before.get("dedup").is_none());

    run.apply(start("a", Station::Git, 0));
    run.apply(end("a", 250, false));
    let value = json_of(&run);
    assert_eq!(value["jiraKey"], "PROJ-1");
    assert_eq!(value["nodeKey"], "architect");
    assert_eq!(value["runId"], 7);
    assert_eq!(value["summary"]["busyMs"]["git"], 250);
    assert_eq!(value["summary"]["counts"]["git"], 1);
    assert_eq!(value["summary"]["subagentsInFlight"], 0);
    assert_eq!(value["entries"][0]["startedAt"], "2026-01-01T00:00:00.000Z");
    assert_eq!(value["spans"][0]["status"], "ok");
}

// ---- the launch offset ----

#[test]
fn the_launch_offset_names_the_transcript_and_how_much_was_written() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join(".claude").join("projects").join("d--repo");
    std::fs::create_dir_all(&project).unwrap();

    // A fresh conversation has no file yet.
    let fresh = transcript_start(home.path(), "claude", Some("abc-123"));
    assert_eq!((fresh.path.clone(), fresh.offset), (None, 0));
    assert_eq!(launch_offset(&fresh), json!({"path": null, "offset": 0}));

    // A resumed one appends to the file it already has.
    let file = project.join("abc-123.jsonl");
    std::fs::write(&file, "12345\n").unwrap();
    let resumed = transcript_start(home.path(), "claude", Some("abc-123"));
    assert_eq!((resumed.path.as_deref(), resumed.offset), (Some(file.as_path()), 6));
    assert_eq!(launch_offset(&resumed)["offset"], 6);
    assert_eq!(launch_offset(&resumed)["path"], json!(file.to_string_lossy()));
    assert_eq!(launch_offset(&resumed)["subagents"], json!({}), "no subagent transcripts yet");

    // Subagent transcripts already there are recorded by name with their length; the meta files
    // and strays are not.
    let subagents = project.join("abc-123").join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(subagents.join("agent-a1.jsonl"), "1234567\n").unwrap();
    std::fs::write(subagents.join("agent-a1.meta.json"), "{}").unwrap();
    std::fs::write(subagents.join("notes.jsonl"), "x\n").unwrap();
    std::fs::write(subagents.join("agent-.jsonl"), "x\n").unwrap();
    let with_subagents = transcript_start(home.path(), "claude", Some("abc-123"));
    assert_eq!(launch_offset(&with_subagents)["subagents"], json!({"agent-a1.jsonl": 8}));

    // Copilot's path is known before it exists.
    let events = home.path().join(".copilot").join("session-state").join("sess-1").join("events.jsonl");
    let copilot = transcript_start(home.path(), "copilot", Some("sess-1"));
    assert_eq!((copilot.path.as_deref(), copilot.offset), (Some(events.as_path()), 0));
    assert!(launch_offset(&copilot).get("subagents").is_none(), "only Claude has subagent transcripts");

    // Codex has no path until the tracker finds the rollout; an unsafe id names nothing.
    assert_eq!(transcript_start(home.path(), "codex", Some("abc-123")).path, None);
    assert_eq!(transcript_start(home.path(), "claude", None).path, None);
    assert_eq!(transcript_start(home.path(), "claude", Some("../etc")).path, None);
}

// ---- subagents ----

#[test]
fn a_subagents_parent_can_arrive_after_its_calls() {
    let mut run = run(None);
    run.apply(start("agent-call", Station::Agents, 100));
    run.apply_subagent("agent-a", None, start("a-1", Station::Shell, 110));
    run.apply_subagent("agent-b", None, start("b-1", Station::Files, 120));
    assert_eq!(run.summary.subagents_in_flight, 1, "only the run's own Agent call is counted");
    run.take_delta();

    run.set_subagent_parent("agent-a", "agent-call");
    let delta = run.take_delta().expect("the backfilled entry is announced");
    let ids: Vec<&str> = delta.entries.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, ["a-1"]);
    let parent = |run: &RunActivity, id: &str| run.entries.iter().find(|e| e.id == id).unwrap().parent_id.clone();
    assert_eq!(parent(&run, "a-1").as_deref(), Some("agent-call"));
    assert_eq!(parent(&run, "b-1"), None, "another subagent's calls are left alone");
    // The same parent again changes nothing; later calls take it without being told again.
    run.set_subagent_parent("agent-a", "agent-call");
    assert!(run.take_delta().is_none());
    run.apply_subagent("agent-a", None, start("a-2", Station::Files, 130));
    assert_eq!(parent(&run, "a-2").as_deref(), Some("agent-call"));

    // A call that was open when its parent arrived still ends normally, and one subagent's turn
    // is not closed by another's.
    run.apply_subagent("agent-a", None, end("a-2", 135, false));
    run.apply_subagent("agent-a", None, end("a-1", 140, false));
    run.apply_subagent("agent-b", None, start("b-2", Station::Network, 150));
    let thinking = run.entries.iter().find(|e| e.id == "think-a-1").unwrap();
    assert_eq!(thinking.status, CallStatus::Run);
    assert!(thinking.by_subagent && thinking.parent_id.as_deref() == Some("agent-call"));
    let a1 = run.entries.iter().find(|e| e.id == "a-1").unwrap();
    assert_eq!((a1.status, a1.parent_id.as_deref()), (CallStatus::Ok, Some("agent-call")));

    // The Agent call ends: nothing of the run's own is in flight, and the totals are unchanged.
    run.apply(end("agent-call", 300, false));
    assert_eq!(run.summary.subagents_in_flight, 0);
    assert_eq!(run.summary.steps, 0);
}

// ---- updates ----

#[test]
fn a_delta_has_only_what_changed_since_the_last_one() {
    let mut run = run(None);
    let first = run.take_delta().expect("the first delta tells a client the run exists");
    assert!(first.entries.is_empty() && first.summary.available);
    assert!(run.take_delta().is_none(), "nothing changed");

    run.apply(start("a", Station::Shell, 0));
    run.apply(start("b", Station::Files, 10));
    let delta = run.take_delta().unwrap();
    assert_eq!(delta.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert!(run.take_delta().is_none());

    run.apply(end("a", 50, false));
    let delta = run.take_delta().unwrap();
    assert_eq!(delta.entries.len(), 1);
    assert_eq!((delta.entries[0].id.as_str(), delta.entries[0].status), ("a", CallStatus::Ok));

    // A change to the summary alone is still news.
    run.note_write(at(1_000));
    let delta = run.take_delta().unwrap();
    assert!(delta.entries.is_empty());
    assert_eq!(delta.summary.last_write_at, Some(at(1_000)));

    // The message names the run and carries no history.
    run.apply(start("c", Station::Git, 60));
    let delta = run.take_delta().unwrap();
    let message = run.message(&delta);
    assert_eq!(message["type"], "activity");
    assert_eq!(message["runNumber"], 1);
    assert_eq!(message["sessionId"], 3);
    assert_eq!(message["entries"][0]["id"], "c");
    assert!(message.get("spans").is_none() && message.get("contextSteps").is_none());
}

#[test]
fn a_backlog_longer_than_the_list_sends_only_the_newest_entries() {
    let mut run = run(None);
    for n in 0..1_000 {
        run.apply(start(&format!("c{n}"), Station::Shell, n));
    }
    let delta = run.take_delta().unwrap();
    assert_eq!(delta.entries.len(), MAX_ENTRIES);
    assert_eq!(delta.entries.last().unwrap().id, "c999");
}

#[test]
fn reset_forgets_the_history_but_not_who_the_run_is() {
    let mut run = run(Some(catalog()));
    run.apply(start("a", Station::Shell, 0));
    run.apply(step(Some("s:m1"), 10, 100));
    run.take_delta();
    run.reset();
    assert!(run.entries.is_empty() && run.spans.is_empty() && run.context_steps.is_empty());
    assert_eq!((run.run_id, run.node_key.as_str()), (7, "architect"));
    assert_eq!(run.summary.steps, 0);
    assert_eq!(run.summary.cost_usd, Some(0.0), "prices are kept");
    assert!(run.take_delta().is_some(), "a client is told again");
    // The same message counts again after a reset.
    run.apply(step(Some("s:m1"), 10, 100));
    assert_eq!(run.summary.steps, 1);
}

#[test]
fn a_shrunken_file_is_announced_before_its_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "aaaa\nbbbb\n");
    let mut tail = Tail::new(&path, 0);
    poll(&mut tail);
    std::fs::write(&path, "c\n").unwrap();
    let order = std::cell::RefCell::new(Vec::new());
    tail.poll_with(|| order.borrow_mut().push("restart".to_string()), |line| order.borrow_mut().push(line.to_string()))
        .unwrap();
    assert_eq!(*order.borrow(), ["restart", "c"]);
}
