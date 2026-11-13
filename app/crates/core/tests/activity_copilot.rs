//! Copilot `events.jsonl` lines → activity events (`engine::activity`). The fixtures in
//! `tests/fixtures/activity/copilot/` are cut from real sessions (Copilot CLI 1.0.5 to 1.0.105) and
//! scrubbed down to the fields the parser reads, with stand-ins for every name, path, command and
//! text. `current.jsonl` is one whole 1.0.105 session; the others group lines by kind (see the
//! order each test relies on, checked in `the_fixtures_are_the_shape_the_tests_rely_on`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use agentflow_core::engine::activity::{
    CallStatus, Parsed, RunActivity, Station, Tail, parse_line, parse_routed, parses,
};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/copilot").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect()
}

fn routed(line: &str) -> Vec<(Option<String>, Parsed)> {
    parse_routed("copilot", line)
}

fn parse(line: &str) -> Vec<Parsed> {
    parse_line("copilot", line)
}

fn at(second: u32, millis: u32) -> String {
    format!("2026-11-13T10:00:{second:02}.{millis:03}Z")
}

/// One line as Copilot writes it.
fn event(kind: &str, stamp: &str, data: Value) -> String {
    json!({"type": kind, "data": data, "id": format!("id-{kind}-{stamp}"), "timestamp": stamp}).to_string()
}

fn start_line(name: &str, arguments: Value) -> String {
    event("tool.execution_start", &at(0, 0), json!({"toolCallId": "call-1", "toolName": name, "arguments": arguments}))
}

fn mcp_start_line(server: &str, tool: &str) -> String {
    event(
        "tool.execution_start",
        &at(0, 0),
        json!({"toolCallId": "call-1", "toolName": format!("{server}-{tool}"), "mcpServerName": server,
               "mcpToolName": tool, "arguments": {}}),
    )
}

/// The station and label a tool call is given, or None for a call that is not listed.
fn call(line: &str) -> Option<(Station, String)> {
    let events = parse(line);
    match &events[..] {
        [] => None,
        [Parsed::Start { id, station, label, inbound, .. }] => {
            assert_eq!(id, "call-1");
            assert!(!inbound);
            Some((*station, label.clone()))
        }
        other => panic!("one Start or nothing, got {other:?}"),
    }
}

fn tool(name: &str, arguments: Value) -> Option<(Station, String)> {
    call(&start_line(name, arguments))
}

fn shell(command: &str) -> (Station, String) {
    tool("powershell", json!({"command": command})).unwrap()
}

fn run_of(lines: &[String]) -> RunActivity {
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "copilot", None);
    feed(&mut run, lines);
    run
}

fn feed(run: &mut RunActivity, lines: &[String]) {
    for line in lines {
        for (parent, event) in routed(line) {
            run.apply_routed(parent.as_deref(), event);
        }
    }
}

fn started(entries: &RunActivity, label: &str) -> agentflow_core::engine::activity::ActivityEntry {
    entries.entries.iter().find(|entry| entry.label == label).unwrap_or_else(|| panic!("no entry {label:?}")).clone()
}

// ---- the fixtures ----------------------------------------------------------------------------

#[test]
fn the_fixtures_are_the_shape_the_tests_rely_on() {
    assert_eq!(fixture("subagents.jsonl").len(), 22, "sync 9, background 9, failed 4");
    assert_eq!(fixture("permissions.jsonl").len(), 10, "shell 4, read 4, extension 2");
    assert_eq!(fixture("messages.jsonl").len(), 8);
    assert_eq!(fixture("tools.jsonl").len(), 15);
    assert_eq!(fixture("current.jsonl").len(), 23);
    // Nothing from the real sessions but their shape: no names, paths or text.
    for name in ["subagents", "permissions", "messages", "tools", "misc", "current"] {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/activity/copilot/{name}.jsonl")),
        )
        .unwrap();
        for word in ["robse7en", "Rob Rodolfo", "C:\\", "D:\\", "github.com"] {
            assert!(!text.to_lowercase().contains(&word.to_lowercase()), "{name} holds {word:?}");
        }
    }
}

// ---- stations --------------------------------------------------------------------------------

#[test]
fn every_tool_in_the_table_lands_on_its_station() {
    let cases: &[(&str, Value, Station)] = &[
        ("view", json!({"path": "/a/b.rs"}), Station::Files),
        ("edit", json!({"path": "/a/b.rs"}), Station::Files),
        ("create", json!({"path": "/a/b.rs"}), Station::Files),
        ("grep", json!({"pattern": "x"}), Station::Files),
        ("rg", json!({"pattern": "x"}), Station::Files),
        ("glob", json!({"pattern": "x"}), Station::Files),
        ("lsp", json!({"operation": "hover"}), Station::Files),
        ("apply_patch", json!({"0": "x"}), Station::Files),
        ("store_memory", json!({"fact": "x"}), Station::Files),
        ("powershell", json!({"command": "cargo test"}), Station::Shell),
        ("bash", json!({"command": "cargo test"}), Station::Shell),
        ("read_powershell", json!({"shellId": "x"}), Station::Shell),
        ("write_bash", json!({"shellId": "x"}), Station::Shell),
        ("stop_powershell", json!({"shellId": "x"}), Station::Shell),
        ("list_powershell", json!({}), Station::Shell),
        ("sql", json!({"query": "x"}), Station::Shell),
        ("web_fetch", json!({"url": "https://example.com/a"}), Station::Network),
        ("web_search", json!({"query": "x"}), Station::Network),
        ("task", json!({"description": "x"}), Station::Agents),
        ("read_agent", json!({"agent_id": "x"}), Station::Agents),
        ("write_agent", json!({"agent_id": "x"}), Station::Agents),
        ("list_agents", json!({}), Station::Agents),
        ("skill", json!({"skill": "x"}), Station::Skills),
        ("tool_search_tool", json!({"pattern": "x"}), Station::Skills),
        ("ask_user", json!({"question": "x"}), Station::You),
        ("exit_plan_mode", json!({"summary": "x"}), Station::You),
        // A tool nobody listed lands on Shell, labelled with its name, as for Claude.
        ("some_new_tool", json!({}), Station::Shell),
    ];
    for (name, arguments, station) in cases {
        let (got, label) = tool(name, arguments.clone()).unwrap_or_else(|| panic!("{name} gave nothing"));
        assert_eq!(got, *station, "{name}");
        assert!(!label.is_empty(), "{name} has a label");
    }
    assert_eq!(tool("some_new_tool", json!({})).unwrap().1, "some_new_tool");
}

#[test]
fn mcp_tools_are_network_named_by_server_and_tool() {
    assert_eq!(
        call(&mcp_start_line("example-server", "get_item")),
        Some((Station::Network, "example-server: get item".into()))
    );
    // Not split from the joined tool name: the harness gives the parts.
    assert_eq!(call(&mcp_start_line("a-b", "c_d")), Some((Station::Network, "a-b: c d".into())));
}

#[test]
fn the_harness_bookkeeping_is_not_a_call() {
    for name in ["report_intent", "task_complete", "update_todo"] {
        assert_eq!(tool(name, json!({"intent": "x"})), None, "{name}");
    }
    // Its result ends nothing (an unknown id is ignored), so it cannot disturb a real call.
    let mut run = run_of(&[start_line("view", json!({"path": "src/a.rs"}))]);
    let complete = event("tool.execution_complete", &at(1, 0), json!({"toolCallId": "other", "success": true}));
    feed(&mut run, &[complete]);
    assert_eq!(run.open_calls(), 1);
}

#[test]
fn the_shell_decides_by_command_name() {
    assert_eq!(shell("flowctl heartbeat --phase x").0, Station::Agentflow);
    assert_eq!(shell("git push origin main").0, Station::Git);
    assert_eq!(shell("curl https://example.com").0, Station::Network);
    assert_eq!(shell("cat notes.txt").0, Station::Files);
    assert_eq!(shell("$(curl https://example.com/x)").0, Station::Network);
    assert_eq!(shell("cargo test").0, Station::Shell);
}

#[test]
fn labels_carry_names_never_content() {
    assert_eq!(tool("view", json!({"path": "C:\\Dev\\project\\src\\app\\main.rs"})).unwrap().1, "app/main.rs");
    assert_eq!(
        tool("edit", json!({"path": "/a/b/c.rs", "old_str": "SECRET", "new_str": "SECRET"})).unwrap().1,
        "b/c.rs"
    );
    assert_eq!(
        tool("web_fetch", json!({"url": "https://user:pw@example.com/secret/path?token=SECRET"})).unwrap().1,
        "example.com"
    );
    // A shell shows Copilot's description over the command, else the command's name.
    assert_eq!(
        tool("powershell", json!({"command": "cargo test --secret SECRET", "description": "Run the tests"})).unwrap().1,
        "Run the tests"
    );
    assert_eq!(shell("cargo test --secret SECRET").1, "cargo");
    assert_eq!(shell("flowctl ask \"is SECRET ok?\"").1, "flowctl ask");
    assert_eq!(shell("git commit -m SECRET").1, "git commit");
    // Patches, queries, search patterns and prompts never become labels.
    assert_eq!(tool("apply_patch", json!({"0": "SECRET", "1": "SECRET"})).unwrap().1, "Patch");
    assert_eq!(tool("sql", json!({"query": "SELECT SECRET", "description": "SECRET"})).unwrap().1, "SQL");
    assert_eq!(tool("grep", json!({"pattern": "SECRET"})).unwrap().1, "Grep");
    assert_eq!(tool("rg", json!({"pattern": "SECRET"})).unwrap().1, "Grep");
    assert_eq!(tool("glob", json!({"pattern": "SECRET"})).unwrap().1, "Glob");
    assert_eq!(tool("web_search", json!({"query": "SECRET"})).unwrap().1, "Web search");
    assert_eq!(tool("read_agent", json!({"agent_id": "SECRET"})).unwrap().1, "Read agent");
    assert_eq!(tool("ask_user", json!({"question": "SECRET"})).unwrap().1, "Question for you");
    assert_eq!(tool("skill", json!({"skill": "example-skill"})).unwrap().1, "example-skill");
}

#[test]
fn a_subagent_shows_its_description_else_its_name_else_subagent() {
    let label = |arguments| tool("task", arguments).unwrap().1;
    assert_eq!(label(json!({"description": "Explore the code", "prompt": "SECRET"})), "Explore the code");
    assert_eq!(label(json!({"name": "explorer", "prompt": "SECRET"})), "explorer");
    assert_eq!(label(json!({"prompt": "SECRET"})), "Subagent");
}

#[test]
fn labels_are_cleaned_and_cut() {
    let long = "a".repeat(300);
    let label = tool("task", json!({"description": format!("{long}\u{7}\u{202e}")})).unwrap().1;
    assert!(label.chars().count() <= 120, "{}", label.chars().count());
    assert!(!label.chars().any(|c| c.is_control() || c == '\u{202e}'));
}

#[test]
fn the_tool_fixtures_classify_as_the_stand_ins_say() {
    let lines = fixture("tools.jsonl");
    let starts: Vec<(Station, String)> = lines
        .iter()
        .flat_map(|line| parse(line))
        .filter_map(|event| match event {
            Parsed::Start { station, label, .. } => Some((station, label)),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![
            (Station::Shell, "Run the tests".to_string()),
            (Station::Files, "app/main.rs".to_string()),
            (Station::Network, "example-server: get item".to_string()),
            (Station::Network, "example.com".to_string()),
            (Station::Skills, "example-skill".to_string()),
            (Station::You, "Question for you".to_string()),
        ],
        "report_intent is not listed"
    );
}

// ---- people and permissions ------------------------------------------------------------------

#[test]
fn a_message_from_the_person_is_an_inbound_prompt() {
    let lines = fixture("messages.jsonl");
    for index in [0, 1] {
        // A plain message, and one typed while the agent works (`delivery: steering`).
        match &parse(&lines[index])[..] {
            [Parsed::Start { id, station: Station::You, label, inbound: true, .. }] => {
                assert_eq!(label, "Prompt");
                assert!(id.starts_with("prompt-"), "{id}");
            }
            other => panic!("line {index}: {other:?}"),
        }
    }
    // The id comes from the line, so reading the file again gives the same one.
    assert_eq!(parse(&lines[0]), parse(&lines[0]));
}

#[test]
fn a_prompt_never_shows_its_text() {
    let line = event("user.message", &at(0, 0), json!({"content": "SECRET plan", "transformedContent": "SECRET"}));
    let events = parse(&line);
    assert!(!format!("{events:?}").contains("SECRET"), "{events:?}");
}

#[test]
fn lines_that_are_not_the_person_are_not_inbound() {
    // skill text, system notes, another agent's message, an autopilot continuation, memory notes and
    // instruction discovery (messages.jsonl, in that order after the two from the person).
    for (index, line) in fixture("messages.jsonl").iter().enumerate().skip(2) {
        assert!(parse(line).is_empty(), "line {index}: {line}");
    }
}

#[test]
fn each_exclusion_rule_holds_on_its_own() {
    let message = |data: Value| parse(&event("user.message", &at(0, 0), data));
    assert_eq!(message(json!({"content": "hello"})).len(), 1);
    assert_eq!(message(json!({"content": "hello", "source": "user"})).len(), 1);
    assert_eq!(message(json!({"content": "hello", "source": null})).len(), 1);
    assert!(message(json!({"content": "hello", "source": "agent-1234"})).is_empty());
    assert!(message(json!({"content": "hello", "source": "skill-x"})).is_empty());
    assert!(message(json!({"content": "hello", "source": "system"})).is_empty());
    assert!(message(json!({"content": "hello", "source": "autopilot"})).is_empty());
    assert!(message(json!({"content": "hello", "source": "memory"})).is_empty());
    assert!(message(json!({"content": "hello", "isAutopilotContinuation": true})).is_empty());
    assert!(message(json!({"content": "<system_notification>x</system_notification>"})).is_empty());
    assert!(message(json!({"content": "  [SYSTEM] something happened"})).is_empty());
    assert!(message(json!({"content": ""})).is_empty());
    assert!(message(json!({"content": "   "})).is_empty());
    assert!(message(json!({})).is_empty());
}

#[test]
fn a_permission_request_waits_on_you_until_it_is_answered() {
    let lines = fixture("permissions.jsonl");
    // shell: requested, completed, then the tool
    match &parse(&lines[0])[..] {
        [Parsed::Start { id, station: Station::You, label, inbound: false, .. }] => {
            assert!(id.starts_with("permission-"), "{id}");
            assert_eq!(label, "Permission: shell");
        }
        other => panic!("{other:?}"),
    }
    match (&parse(&lines[0])[0], &parse(&lines[1])[..]) {
        (Parsed::Start { id: asked, .. }, [Parsed::Done { id, is_error: false, .. }]) => assert_eq!(asked, id),
        other => panic!("{other:?}"),
    }
    // read has its own word; an extension's request has none; neither shows the request itself
    assert!(matches!(&parse(&lines[4])[..], [Parsed::Start { label, .. }] if label == "Permission: read"));
    assert!(matches!(&parse(&lines[8])[..], [Parsed::Start { label, .. }] if label == "Permission"));
    // A denial is an answer, not a failed call: the tool's own result carries the failure.
    assert!(matches!(&parse(&lines[9])[..], [Parsed::Done { is_error: false, .. }]));
    let unknown = event(
        "permission.requested",
        &at(0, 0),
        json!({"requestId": "r", "permissionRequest": {"kind": "SECRET", "commands": ["SECRET"]}}),
    );
    assert!(matches!(&parse(&unknown)[..], [Parsed::Start { label, .. }] if label == "Permission"));
}

#[test]
fn an_answered_permission_does_not_hand_the_turn_back_to_the_model() {
    let lines = fixture("permissions.jsonl");
    // requested, completed, the tool starting
    let run = run_of(&lines[0..3]);
    let stations: Vec<_> = run.entries.iter().map(|entry| entry.station).collect();
    assert_eq!(stations, vec![Station::You, Station::Git], "no Model entry between the answer and the tool");
    let ask = &run.entries[0];
    assert_eq!(ask.status, CallStatus::Ok);
    assert!(ask.ended_at.is_some());
}

// ---- results, errors and turns ---------------------------------------------------------------

#[test]
fn a_failed_result_marks_its_call_failed() {
    let complete = |data: Value| parse(&event("tool.execution_complete", &at(1, 0), data));
    assert!(matches!(
        &complete(json!({"toolCallId": "c", "success": false}))[..],
        [Parsed::End { is_error: true, .. }]
    ));
    assert!(matches!(
        &complete(json!({"toolCallId": "c", "success": true}))[..],
        [Parsed::End { is_error: false, .. }]
    ));
    assert!(matches!(&complete(json!({"toolCallId": "c"}))[..], [Parsed::End { is_error: false, .. }]));
    assert!(complete(json!({"success": false})).is_empty(), "no id, nothing to end");
    // the fixture's failed shell call
    let lines = fixture("tools.jsonl");
    let run = run_of(&lines[0..2]);
    assert_eq!(run.entries[0].status, CallStatus::Err);
    assert_eq!(run.summary.errors, 1);
}

#[test]
fn real_stops_end_the_turn_but_a_round_ending_does_not() {
    for kind in ["abort", "session.error", "session.task_complete", "session.shutdown"] {
        let events = parse(&event(kind, &at(1, 0), json!({})));
        assert!(matches!(&events[..], [Parsed::TurnEnd { .. }]), "{kind}: {events:?}");
    }
    // `assistant.turn_end` ends one request round after its tools; the model works on after it.
    for kind in [
        "assistant.turn_end",
        "assistant.turn_start",
        "hook.start",
        "hook.end",
        "skill.invoked",
        "session.start",
        "session.usage_checkpoint",
        "model.message",
        "model.turn_started",
        "system.message",
        "session.compaction_start",
    ] {
        assert!(parse(&event(kind, &at(1, 0), json!({"name": "x", "hookType": "x"}))).is_empty(), "{kind}");
    }
}

#[test]
fn the_model_keeps_working_across_a_round_ending() {
    // prompt, 5 s of thinking, a tool call, then 52 s of thinking across turn_end / turn_start.
    let lines = vec![
        event("user.message", &at(0, 0), json!({"content": "hello"})),
        event("assistant.message", &at(5, 0), json!({"messageId": "m1", "model": "model-a"})),
        event(
            "tool.execution_start",
            &at(5, 10),
            json!({"toolCallId": "t1", "toolName": "view", "arguments": {"path": "src/a.rs"}}),
        ),
        event("tool.execution_complete", &at(5, 50), json!({"toolCallId": "t1", "success": true})),
        event("assistant.turn_end", &at(5, 60), json!({"turnId": "0"})),
        event("assistant.turn_start", &at(5, 62), json!({"turnId": "1"})),
        event("assistant.message", &at(57, 0), json!({"messageId": "m2", "model": "model-a"})),
    ];
    let run = run_of(&lines);
    let model: Vec<_> = run.entries.iter().filter(|entry| entry.station == Station::Model).collect();
    assert_eq!(model.len(), 2, "{model:?}");
    assert_eq!(model[0].label, "model-a");
    let span = |entry: &agentflow_core::engine::activity::ActivityEntry| {
        (entry.ended_at.unwrap() - entry.started_at).num_milliseconds()
    };
    assert_eq!(span(model[0]), 5000);
    assert_eq!(span(model[1]), 51_950, "from the tool's end to the next message, not cut at turn_end");
    assert_eq!(run.summary.busy_ms[&Station::Model], 56_950);
    assert_eq!(run.summary.steps, 2);
    assert_eq!(run.summary.model.as_deref(), Some("model-a"));
}

#[test]
fn an_abort_stops_the_model_working() {
    let lines = vec![
        event("user.message", &at(0, 0), json!({"content": "hello"})),
        event("abort", &at(3, 0), json!({"reason": "user initiated"})),
    ];
    let run = run_of(&lines);
    assert_eq!(run.open_calls(), 0);
    assert_eq!(run.summary.busy_ms[&Station::Model], 3000);
}

// ---- subagents -------------------------------------------------------------------------------

#[test]
fn a_subagents_lines_are_routed_by_the_task_call_that_started_it() {
    let lines = fixture("subagents.jsonl");
    let parents: Vec<Option<String>> =
        lines[0..9].iter().flat_map(|line| routed(line)).map(|(parent, _)| parent).collect();
    // task start, subagent.started (nothing), message, two calls starting, two ending, subagent.completed, task end
    let task_id = match &parse(&lines[0])[..] {
        [Parsed::Start { id, station: Station::Agents, label, .. }] => {
            assert_eq!(label, "Explore the code");
            id.clone()
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(parents[0], None, "the task call itself is the run's own");
    for (index, parent) in parents.iter().enumerate().skip(1).take(5) {
        assert_eq!(parent.as_deref(), Some(task_id.as_str()), "event {index}");
    }
    assert_eq!(parents[6], None, "subagent.completed names the call, it is not the subagent's");
    assert_eq!(parents[7], None);
    // Claude keeps its subagents in files of their own: nothing is routed.
    let claude = json!({"type": "assistant", "timestamp": at(0, 0), "message": {"id": "m", "model": "x",
        "usage": {"input_tokens": 1}, "content": [{"type": "tool_use", "id": "t", "name": "Read", "input": {}}]}});
    assert!(parse_routed("claude", &claude.to_string()).iter().all(|(parent, _)| parent.is_none()));
}

#[test]
fn a_synchronous_subagents_calls_dock_under_its_agents_call() {
    let lines = fixture("subagents.jsonl");
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "copilot", None);
    feed(&mut run, &lines[0..1]);
    assert_eq!(run.summary.subagents_in_flight, 1, "the pod is out");
    let task = started(&run, "Explore the code");
    assert!(!task.by_subagent && task.parent_id.is_none());
    feed(&mut run, &lines[1..8]);
    let sub: Vec<_> = run.entries.iter().filter(|entry| entry.by_subagent).collect();
    assert!(sub.iter().any(|entry| entry.station == Station::Files && entry.label == "Grep"), "{sub:?}");
    assert!(sub.iter().all(|entry| entry.parent_id.as_deref() == Some(task.id.as_str())), "{sub:?}");
    assert_eq!(run.summary.subagents_in_flight, 0, "subagent.completed closes the Agents call");
    feed(&mut run, &lines[8..9]);
    assert_eq!(run.summary.subagents_in_flight, 0, "the task's own result finds it closed");
    assert_eq!(started(&run, "Explore the code").status, CallStatus::Ok);
    // The subagent's steps are not the run's own.
    assert_eq!(run.summary.steps, 0);
}

#[test]
fn a_background_task_stays_open_until_its_subagent_completes() {
    let lines = fixture("subagents.jsonl");
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "copilot", None);
    // task start, subagent.started, the task's result ("Agent started in background …")
    feed(&mut run, &lines[9..12]);
    assert!(matches!(&parse(&lines[11])[..], [Parsed::Detach { .. }]), "the early result ends nothing");
    assert_eq!(run.summary.subagents_in_flight, 1, "the pod stays out while the subagent works");
    let main_thinking = |run: &RunActivity| {
        run.entries.iter().filter(|e| e.station == Station::Model && !e.by_subagent && e.ended_at.is_none()).count()
    };
    assert_eq!(main_thinking(&run), 1, "the main agent is back with the model");
    // its message and calls, then subagent.completed
    feed(&mut run, &lines[12..17]);
    assert_eq!(run.summary.subagents_in_flight, 1);
    feed(&mut run, &lines[17..18]);
    assert_eq!(run.summary.subagents_in_flight, 0);
    let task = started(&run, "Explore the code");
    assert_eq!(task.status, CallStatus::Ok);
    let done = match &parse(&lines[17])[..] {
        [Parsed::Done { at, .. }] => *at,
        other => panic!("{other:?}"),
    };
    assert_eq!(task.ended_at, Some(done));
    // Finishing the call does not start the model working again: still the one entry from before.
    assert_eq!(main_thinking(&run), 1);
}

#[test]
fn the_main_agent_is_seen_thinking_while_a_background_subagent_runs() {
    let lines = vec![
        event("user.message", &at(0, 0), json!({"content": "hello"})),
        event("assistant.message", &at(1, 0), json!({"messageId": "m1", "model": "model-a"})),
        event(
            "tool.execution_start",
            &at(1, 10),
            json!({"toolCallId": "bg1", "toolName": "task", "arguments": {"description": "Look", "mode": "background"}}),
        ),
        event(
            "tool.execution_complete",
            &at(1, 20),
            json!({"toolCallId": "bg1", "success": true, "result": {"content": "Agent started in background with agent_id: x."}}),
        ),
        event(
            "tool.execution_start",
            &at(3, 0),
            json!({"toolCallId": "t1", "toolName": "view", "arguments": {"path": "src/a.rs"}}),
        ),
        event("tool.execution_complete", &at(3, 100), json!({"toolCallId": "t1", "success": true})),
        event("assistant.message", &at(6, 100), json!({"messageId": "m2", "model": "model-a"})),
        event("subagent.completed", &at(30, 0), json!({"toolCallId": "bg1"})),
    ];
    let run = run_of(&lines);
    // 0 to 1 s, 1.020 to 3 s (after the task returned) and 3.1 to 6.1 s (after the tool): the dock
    // is not dark for the subagent's whole life, and each step is not a zero-length tick.
    assert_eq!(run.summary.busy_ms[&Station::Model], 1000 + 1980 + 3000);
    assert_eq!(run.summary.subagents_in_flight, 0);
    assert_eq!(started(&run, "Look").ended_at.map(|t| t.timestamp_millis() % 60_000), Some(30_000));
}

#[test]
fn a_failed_subagent_ends_its_call_in_error() {
    let lines = fixture("subagents.jsonl");
    let run = run_of(&lines[18..22]);
    let task = started(&run, "Explore the code");
    assert_eq!(task.status, CallStatus::Err);
    assert_eq!(run.summary.errors, 1, "counted once, though the task's own result follows");
    assert_eq!(run.summary.subagents_in_flight, 0);
}

// ---- usage -----------------------------------------------------------------------------------

#[test]
fn a_usage_record_adds_tokens_and_sets_the_context_fill() {
    let lines = fixture("current.jsonl");
    let run = run_of(&lines);
    // (17966 + 203) + (18195 + 5): the input already holds what came from the cache
    assert_eq!(run.summary.tokens, 36_369);
    assert_eq!(run.summary.context_percent, Some(9.1), "18195 of the 200000-token prompt limit");
    let steps: Vec<f64> = run.context_steps.iter().map(|step| step.percent).collect();
    assert_eq!(steps, vec![9.0, 9.1]);
    assert_eq!(run.summary.steps, 2, "one per assistant message, not per usage record");
    assert_eq!(run.summary.model.as_deref(), Some("example-model"));
    assert_eq!(run.summary.cost_usd, None, "Copilot bills in premium requests");
    assert!(run.summary.available);
}

#[test]
fn a_usage_record_read_twice_counts_once() {
    let lines = fixture("current.jsonl");
    let usage = lines.iter().find(|line| line.contains("session.usage_record")).unwrap().clone();
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "copilot", None);
    feed(&mut run, &[usage.clone(), usage]);
    assert_eq!(run.summary.tokens, 18_169);
    assert_eq!(run.context_steps.len(), 1);
}

#[test]
fn a_message_without_usage_leaves_tokens_cost_and_context_unknown() {
    let line = event("assistant.message", &at(1, 0), json!({"messageId": "m1", "model": "model-a"}));
    let run = run_of(&[line]);
    assert_eq!(run.summary.steps, 1);
    assert_eq!(run.summary.tokens, 0);
    assert_eq!(run.summary.context_percent, None, "not 0%");
    assert!(run.context_steps.is_empty());
    assert_eq!(run.summary.cost_usd, None);
}

#[test]
fn a_copilot_run_has_no_dollar_cost_even_with_prices() {
    let catalog = Arc::new(json!({"x": {"models": {}}}).as_object().unwrap().clone());
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "copilot", Some(catalog.clone()));
    assert_eq!(run.summary.cost_usd, None);
    run.set_catalog(Some(catalog));
    feed(&mut run, &fixture("current.jsonl"));
    assert_eq!(run.summary.cost_usd, None);
}

#[test]
fn a_usage_record_with_nothing_usable_changes_nothing() {
    for usage in [json!({}), json!({"inputTokens": "many"}), json!({"model": "m"})] {
        let line = event("session.usage_record", &at(1, 0), json!({"usage": usage}));
        let run = run_of(&[line]);
        assert_eq!(run.summary.tokens, 0);
        assert_eq!(run.summary.context_percent, None);
    }
    let no_usage = event("session.usage_record", &at(1, 0), json!({}));
    assert!(parse(&no_usage).is_empty());
}

// ---- the whole file --------------------------------------------------------------------------

#[test]
fn a_whole_session_gives_the_same_summary_however_it_is_read() {
    let lines = fixture("current.jsonl");
    let whole = run_of(&lines);
    let text = lines.join("\n") + "\n";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    // written in two pieces, the first ending in the middle of a line
    let cut = text.len() / 2;
    std::fs::write(&path, &text.as_bytes()[..cut]).unwrap();
    let mut tail = Tail::new(&path, 0);
    let mut piecewise = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "copilot", None);
    let read = |tail: &mut Tail, run: &mut RunActivity| {
        tail.poll(|line| {
            for (parent, event) in parse_routed("copilot", line) {
                run.apply_routed(parent.as_deref(), event);
            }
        })
        .unwrap();
    };
    read(&mut tail, &mut piecewise);
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&text.as_bytes()[cut..]).unwrap();
    read(&mut tail, &mut piecewise);
    assert_eq!(piecewise.summary.tokens, whole.summary.tokens);
    assert_eq!(piecewise.summary.steps, whole.summary.steps);
    assert_eq!(piecewise.summary.counts, whole.summary.counts);
    assert_eq!(piecewise.summary.busy_ms, whole.summary.busy_ms);
    assert_eq!(piecewise.entries.len(), whole.entries.len());
    // the real 1.0.105 session: an extension's permission request and a prompt, two model steps and
    // thinking, a 25 s shell command
    assert_eq!(whole.summary.counts[&Station::You], 2);
    assert_eq!(whole.summary.counts[&Station::Shell], 1);
    assert_eq!(whole.summary.busy_ms[&Station::Shell], 25_530);
}

#[test]
fn a_malformed_line_gives_nothing_and_does_not_panic() {
    let long_nesting = format!("{}{}", "[".repeat(300), "]".repeat(300));
    let lines = [
        "",
        "not json",
        "{",
        "[]",
        "null",
        "{}",
        r#"{"type":"tool.execution_start"}"#,
        r#"{"type":"tool.execution_start","timestamp":"yesterday","data":{}}"#,
        r#"{"type":"tool.execution_start","timestamp":"2026-11-13T10:00:00Z","data":"x"}"#,
        r#"{"type":"tool.execution_start","timestamp":"2026-11-13T10:00:00Z","data":{"toolCallId":7,"toolName":[]}}"#,
        r#"{"type":"tool.execution_start","timestamp":"2026-11-13T10:00:00Z","data":{"toolCallId":"","toolName":"view"}}"#,
        r#"{"type":5,"timestamp":"2026-11-13T10:00:00Z","data":{}}"#,
        r#"{"type":"assistant.message","timestamp":"2026-11-13T10:00:00Z","data":{"messageId":{"a":1},"model":3}}"#,
        &long_nesting,
    ];
    for line in lines {
        let events = parse(line);
        // an assistant message with an odd id is still a step; everything else gives nothing
        assert!(events.len() <= 1, "{line}: {events:?}");
    }
    assert!(parse(r#"{"type":"tool.execution_start","timestamp":"2026-11-13T10:00:00Z","data":{"toolCallId":"","toolName":"view"}}"#).is_empty());
}

#[test]
fn copilot_is_understood_and_codex_is_not_yet() {
    assert!(parses("copilot"));
    assert!(parses("claude"));
    assert!(!parses("codex"));
    assert!(parse_line("codex", &fixture("current.jsonl")[0]).is_empty());
}
