//! Codex rollout lines → activity events (`engine::activity`). The fixtures in
//! `tests/fixtures/activity/codex/` are cut from two real rollouts of Codex CLI 0.167.2 and scrubbed
//! down to the fields the parser reads, with stand-ins for every name, path, command, prompt and
//! output. `typed.jsonl` is a session typed in the terminal (two turns, eight scripts); `exec.jsonl`
//! is a `codex exec` run of one 20-second command.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use agentflow_core::collectors::{CollectorEnv, codex};
use agentflow_core::engine::activity::{
    CallStatus, Parsed, RunActivity, Station, Tail, launch_offset, parse_line, parses, transcript_start,
};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/codex").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect()
}

fn parse(line: &str) -> Vec<Parsed> {
    parse_line("codex", line)
}

fn at(second: u32, millis: u32) -> String {
    format!("2026-11-13T10:00:{second:02}.{millis:03}Z")
}

/// One line as Codex writes it.
fn line(kind: &str, payload: Value) -> String {
    json!({"timestamp": at(1, 0), "ordinal": 1, "type": kind, "payload": payload}).to_string()
}

fn call_line(name: &str, input: &str) -> String {
    line(
        "response_item",
        json!({"type": "custom_tool_call", "id": "ctc-1", "status": "completed",
                                  "call_id": "call-1", "name": name, "input": input}),
    )
}

fn output_line(output: Value) -> String {
    line(
        "response_item",
        json!({"type": "custom_tool_call_output", "id": "cto-1", "call_id": "call-1", "output": output}),
    )
}

fn item_line(item: Value) -> String {
    line("event_msg", json!({"type": "item_completed", "thread_id": "t", "turn_id": "u", "item": item}))
}

/// The station and label an `exec` script is given.
fn cell(script: &str) -> (Station, String) {
    match &parse(&call_line("exec", script))[..] {
        [Parsed::Start { id, station, label, inbound, .. }] => {
            assert_eq!(id, "call-1");
            assert!(!inbound);
            (*station, label.clone())
        }
        other => panic!("one Start, got {other:?}"),
    }
}

fn shell(command: &str) -> (Station, String) {
    cell(&format!("text(await tools.exec_command({{cmd:{},yield_time_ms:1000}}));", json!(command)))
}

fn run_of(lines: &[String]) -> RunActivity {
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", None);
    feed(&mut run, lines);
    run
}

fn feed(run: &mut RunActivity, lines: &[String]) {
    for line in lines {
        for event in parse(line) {
            run.apply(event);
        }
    }
}

// ---- the fixtures ----------------------------------------------------------------------------

#[test]
fn the_fixtures_are_the_shape_the_tests_rely_on() {
    assert_eq!(fixture("typed.jsonl").len(), 85);
    assert_eq!(fixture("exec.jsonl").len(), 17);
    // Nothing from the real sessions but their shape: no names, paths, prompts or accounts.
    for name in ["typed", "exec"] {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/activity/codex/{name}.jsonl")),
        )
        .unwrap();
        // (A drive path is written `C:\Users` inside JSON.)
        for word in ["robse7en", "Rob Rodolfo", "C:\\\\", "D:\\\\", "github.com", "creator", "account", "AgentFlow"] {
            assert!(!text.to_lowercase().contains(&word.to_lowercase()), "{name} holds {word:?}");
        }
    }
}

// ---- stations --------------------------------------------------------------------------------

#[test]
fn a_script_is_sorted_by_the_first_tool_it_calls() {
    let exec = |name: &str, arguments: &str| cell(&format!("text(await tools.{name}({arguments}));"));
    assert_eq!(exec("apply_patch", "\"patch\""), (Station::Files, "Patch".to_string()));
    assert_eq!(
        exec("mcp__serena__initial_instructions", "{}"),
        (Station::Network, "serena: initial instructions".to_string())
    );
    assert_eq!(exec("request_user_input", "{}"), (Station::You, "Question for you".to_string()));
    assert_eq!(exec("spawn_agent", "{}").0, Station::Agents);
    assert_eq!(exec("wait_agent", "{}").0, Station::Agents);
    assert_eq!(exec("followup_task", "{}").0, Station::Agents);
    assert_eq!(exec("send_message", "{}").0, Station::Agents);
    assert_eq!(exec("interrupt_agent", "{}").0, Station::Agents);
    assert_eq!(exec("list_agents", "{}").0, Station::Agents);
    // A tool nobody listed lands on Shell, named, as for Claude.
    assert_eq!(exec("some_new_tool", "{}"), (Station::Shell, "some_new_tool".to_string()));
    // Tools of a namespace are named by their own word.
    assert_eq!(exec("collaboration.spawn_agent", "{}").0, Station::Agents);

    // The first call decides, whatever follows.
    let mixed =
        "text(await tools.mcp__serena__find_symbol({}));\ntext(await tools.exec_command({cmd:\"git status\"}));";
    assert_eq!(cell(mixed).0, Station::Network);
    let mixed = "text(await tools.exec_command({cmd:\"git status\"}));\ntext(await tools.apply_patch(\"p\"));";
    assert_eq!(cell(mixed), (Station::Git, "git status".to_string()));

    // No tool at all: looking the tool list up is Skills, anything else a plain script.
    assert_eq!(cell("text(ALL_TOOLS.filter(x=>/server/.test(x.name)));"), (Station::Skills, "Loading tools".into()));
    assert_eq!(cell("const x = 1 + 1; text(x);"), (Station::Shell, "Script".to_string()));
    assert_eq!(cell(""), (Station::Shell, "Script".to_string()));
    // A tool call is found after the tool list lookup, too.
    assert_eq!(
        cell("text(ALL_TOOLS.length);\ntext(await tools.exec_command({cmd:\"cargo test\"}));").0,
        Station::Shell
    );
}

#[test]
fn a_command_is_sorted_by_its_name() {
    assert_eq!(shell("git status --short"), (Station::Git, "git status".to_string()));
    assert_eq!(shell("flowctl ask --message hello"), (Station::Agentflow, "flowctl ask".to_string()));
    assert_eq!(shell("rg -n term src"), (Station::Files, "rg".to_string()));
    assert_eq!(shell("cargo test"), (Station::Shell, "cargo".to_string()));
    assert_eq!(shell("Get-Content docs/a.md | Select-Object -First 80"), (Station::Shell, "Get-Content".to_string()));
}

#[test]
fn a_command_written_in_any_javascript_string_is_read() {
    let with = |literal: &str| cell(&format!("text(await tools.exec_command({{cmd:{literal},yield_time_ms:1}}));"));
    assert_eq!(with("\"git status\""), (Station::Git, "git status".to_string()));
    assert_eq!(with("'git status'"), (Station::Git, "git status".to_string()));
    assert_eq!(with("`git status`"), (Station::Git, "git status".to_string()));
    // Escapes are undone: a quote inside the command does not end it, and `\n` is a line break.
    assert_eq!(with(r#""echo \"a\"; git log""#), (Station::Git, "git log".to_string()));
    assert_eq!(with(r#""cd src\ngit diff""#), (Station::Git, "git diff".to_string()));
    // The property can be spelled with spaces, and come after others.
    let spaced = "text(await tools.exec_command({ yield_time_ms: 5, cmd : \"git status\" }));";
    assert_eq!(cell(spaced), (Station::Git, "git status".to_string()));
}

#[test]
fn a_command_that_is_not_plain_text_is_just_a_shell_call() {
    let plain = (Station::Shell, "shell".to_string());
    // Built from a variable, a template with a hole, a number, an unterminated string.
    assert_eq!(cell("const c = 'git status';\ntext(await tools.exec_command({cmd: c}));"), plain);
    assert_eq!(cell("text(await tools.exec_command({cmd: `git ${arg}`}));"), plain);
    assert_eq!(cell("text(await tools.exec_command({cmd: 5}));"), plain);
    assert_eq!(cell("text(await tools.exec_command({cmd: \"git status"), plain);
    assert_eq!(cell("text(await tools.exec_command({}));"), plain);
    assert_eq!(cell("text(await tools.exec_command("), plain);
}

#[test]
fn a_label_is_a_name_and_never_the_command_line() {
    let (_, label) = shell("cargo test --token SECRET-VALUE -p private-crate");
    assert_eq!(label, "cargo");
    let (_, label) = shell("git commit -m \"private words\"");
    assert_eq!(label, "git commit");
    let (_, label) = shell("flowctl ask --message \"private question\"");
    assert_eq!(label, "flowctl ask");
    // A patch names no file: its text, which holds paths and changes, is never read.
    let (_, label) = cell("text(await tools.apply_patch(\"*** Update File: secret/place.rs\"));");
    assert_eq!(label, "Patch");
    // A very long command name is cut, and control characters are gone.
    let (_, label) = cell(&format!("text(await tools.{}({{}}));", "x".repeat(300)));
    assert!(label.chars().count() <= 120);
    let (_, label) = shell("\u{1b}[31mred\u{7} --flag");
    assert!(!label.chars().any(char::is_control), "{label:?}");
}

#[test]
fn another_tool_of_the_model_is_named_and_a_line_without_an_id_gives_nothing() {
    assert_eq!(
        parse(&call_line("js", "code")),
        vec![Parsed::Start {
            id: "call-1".into(),
            station: Station::Shell,
            label: "js".into(),
            at: "2026-11-13T10:00:01.000Z".parse().unwrap(),
            inbound: false,
        }]
    );
    let no_id = line("response_item", json!({"type": "custom_tool_call", "name": "exec", "input": "text(1)"}));
    assert!(parse(&no_id).is_empty());
    let no_name = line("response_item", json!({"type": "custom_tool_call", "call_id": "c", "input": "text(1)"}));
    assert!(parse(&no_name).is_empty());
    let no_id = line("response_item", json!({"type": "custom_tool_call_output", "output": "x"}));
    assert!(parse(&no_id).is_empty());
}

// ---- results ---------------------------------------------------------------------------------

fn ends(output: Value) -> bool {
    match &parse(&output_line(output))[..] {
        [Parsed::End { id, is_error, .. }] => {
            assert_eq!(id, "call-1");
            *is_error
        }
        other => panic!("one End, got {other:?}"),
    }
}

fn result(exit_code: i64) -> String {
    json!({"chunk_id": "c", "wall_time_seconds": 0.5, "exit_code": exit_code, "original_token_count": 3,
           "output": "stand-in"})
    .to_string()
}

#[test]
fn a_script_fails_when_a_command_it_ran_exits_with_a_code() {
    let header = "Script completed\nWall time 1.4 seconds\nOutput:\n";
    assert!(!ends(json!(header)), "a header on its own");
    assert!(!ends(json!([{"type": "input_text", "text": header}])));
    assert!(!ends(json!([{"type": "input_text", "text": header}, {"type": "input_text", "text": result(0)}])));
    assert!(ends(json!([{"type": "input_text", "text": header}, {"type": "input_text", "text": result(1)}])));
    // Any of the results may be the one that failed, and a later success does not undo it.
    let both = json!([{"type": "input_text", "text": header}, {"type": "input_text", "text": result(2)},
                      {"type": "input_text", "text": result(0)}]);
    assert!(ends(both));
    // A result that is not a command's (an MCP tool's, a list, a note) is not an exit code.
    let mcp = json!({"content": [{"type": "text", "text": "x"}], "isError": false}).to_string();
    assert!(!ends(json!([{"type": "input_text", "text": header}, {"type": "input_text", "text": mcp}])));
    assert!(!ends(json!([{"type": "input_text", "text": "[1, 2]"}, {"type": "input_text", "text": "{}"}])));
    // A command still running has no exit code yet; nonsense is not an error either.
    let running = json!({"chunk_id": "c", "session_id": 4, "output": "x"}).to_string();
    assert!(!ends(json!([{"type": "input_text", "text": running}])));
    assert!(!ends(json!([{"type": "input_text", "text": "{\"exit_code\": \"1\"}"}])));
    assert!(!ends(json!([{"type": "input_text", "text": "{\"exit_code\": 1"}])));
    assert!(!ends(json!(null)));
    assert!(!ends(json!(7)));
    assert!(!ends(json!([7, null, {"text": 3}])));
}

#[test]
fn a_failed_command_marks_its_call_failed_in_the_run() {
    let lines = [
        call_line("exec", "text(await tools.exec_command({cmd:\"cargo test\"}));"),
        line(
            "response_item",
            json!({"type": "custom_tool_call_output", "call_id": "call-1",
                                      "output": [{"type": "input_text", "text": result(101)}]}),
        ),
    ];
    let run = run_of(&lines);
    let entry = run.entries.iter().find(|entry| entry.station == Station::Shell).unwrap();
    assert_eq!(entry.status, CallStatus::Err);
    assert_eq!(run.summary.errors, 1);
}

// ---- prompts and turns -----------------------------------------------------------------------

#[test]
fn a_user_message_item_is_a_prompt_and_never_shows_its_text() {
    let item = json!({"type": "UserMessage", "id": "item-9", "content": [{"type": "text", "text": "private words"}]});
    let events = parse(&item_line(item));
    assert_eq!(events.len(), 1);
    match &events[0] {
        Parsed::Start { id, station, label, inbound, .. } => {
            assert_eq!(id, "prompt-item-9");
            assert_eq!(*station, Station::You);
            assert_eq!(label, "Prompt");
            assert!(inbound);
        }
        other => panic!("{other:?}"),
    }
    // Without an id of its own the timestamp stands in, and the same line gives the same id.
    let item = json!({"type": "UserMessage", "content": []});
    assert_eq!(parse(&item_line(item.clone())), parse(&item_line(item)));
}

#[test]
fn the_other_lines_that_hold_the_prompt_are_not_read_again() {
    // The response item of role user repeats the prompt; the others hold the project's instructions.
    let repeat = line(
        "response_item",
        json!({"type": "message", "role": "user",
        "content": [{"type": "input_text", "text": "stand-in"}],
        "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["user.text"]}}),
    );
    let context = line(
        "response_item",
        json!({"type": "message", "role": "user",
        "content": [{"type": "input_text", "text": "stand-in"}],
        "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["agents_md.instructions"]}}),
    );
    assert!(parse(&repeat).is_empty());
    assert!(parse(&context).is_empty());
    let session = run_of(&fixture("typed.jsonl"));
    let prompts = session.entries.iter().filter(|entry| entry.inbound).count();
    assert_eq!(prompts, 2, "two turns, two prompts, not four");
}

#[test]
fn items_that_are_not_calls_or_prompts_give_nothing() {
    let items = [
        json!({"type": "AgentMessage", "id": "i", "content": [{"type": "Text", "text": "x"}], "phase": "final_answer"}),
        json!({"type": "Reasoning", "id": "i", "summary_text": [], "raw_content": []}),
        json!({"type": "CommandExecution", "id": "i", "command": ["pwsh"], "status": "failed", "exit_code": 1}),
        json!({"type": "McpToolCall", "id": "i", "server": "s", "tool": "t", "status": "completed"}),
        json!({"type": "FileChange", "id": "i", "changes": {}, "status": "completed"}),
        json!({"type": "SomethingNew", "id": "i"}),
    ];
    for item in items {
        assert!(parse(&item_line(item.clone())).is_empty(), "{item}");
    }
    for payload in [
        json!({"type": "additional_tools", "tools": []}),
        json!({"type": "reasoning", "summary": []}),
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "x"}]}),
        json!({"type": "token_count", "info": null}),
        json!({"type": "thread_settings_applied", "thread_settings": {"model": "m"}}),
        json!({"type": "function_call", "name": "shell", "call_id": "c", "arguments": "{}"}),
        json!({"type": "mystery"}),
        json!({}),
    ] {
        for kind in ["response_item", "event_msg", "world_state", "session_meta"] {
            assert!(parse(&line(kind, payload.clone())).is_empty(), "{kind} {payload}");
        }
    }
}

#[test]
fn a_task_ends_the_models_turn() {
    let lines = [
        item_line(json!({"type": "UserMessage", "id": "p", "content": []})),
        line("event_msg", json!({"type": "task_complete", "turn_id": "u", "duration_ms": 1})),
    ];
    assert_eq!(parse(&lines[1]), vec![Parsed::TurnEnd { at: "2026-11-13T10:00:01.000Z".parse().unwrap() }]);
    let run = run_of(&lines);
    let model = run.entries.iter().find(|entry| entry.station == Station::Model).unwrap();
    assert!(model.ended_at.is_some(), "the model is not still working after the task");
}

// ---- model, window and usage -----------------------------------------------------------------

#[test]
fn the_model_and_window_come_from_the_lines_that_name_them() {
    let context = line("turn_context", json!({"turn_id": "u", "model": "gpt-test", "effort": "high"}));
    assert_eq!(parse(&context), vec![Parsed::Setting { model: Some("gpt-test".into()), window: None }]);
    let started = line("event_msg", json!({"type": "task_started", "turn_id": "u", "model_context_window": 475000}));
    assert_eq!(parse(&started), vec![Parsed::Setting { model: None, window: Some(475000) }]);
    let bare = line("event_msg", json!({"type": "task_started", "turn_id": "u"}));
    assert_eq!(parse(&bare), vec![Parsed::Setting { model: None, window: None }]);
    let empty = line("turn_context", json!({"model": ""}));
    assert_eq!(parse(&empty), vec![Parsed::Setting { model: None, window: None }]);
}

fn record(response: &str, input: u64, cached: u64, written: u64, output: u64) -> String {
    line(
        "token_usage_record",
        json!({"response_id": response, "usage": {
        "input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": written,
        "output_tokens": output, "reasoning_output_tokens": 5, "total_tokens": input + output}}),
    )
}

#[test]
fn a_usage_record_splits_the_input_and_counts_the_output_once() {
    let events = parse(&record("r1", 1000, 600, 100, 50));
    assert_eq!(
        events,
        vec![Parsed::Request {
            message_id: Some("resp:r1".into()),
            at: "2026-11-13T10:00:01.000Z".parse().unwrap(),
            input_tokens: 300,
            output_tokens: 50,
            cache_read: 600,
            cache_write: 100,
        }]
    );
    // More cache than input (a bad record) cannot go below zero.
    match &parse(&record("r2", 10, 600, 100, 5))[..] {
        [Parsed::Request { input_tokens, .. }] => assert_eq!(*input_tokens, 0),
        other => panic!("{other:?}"),
    }
    // Nothing used, or no usage: nothing to count. The count the harness repeats in `token_count`
    // is not read at all.
    assert!(parse(&record("r3", 0, 0, 0, 0)).is_empty());
    assert!(parse(&line("token_usage_record", json!({"response_id": "r"}))).is_empty());
    let repeat = line(
        "event_msg",
        json!({"type": "token_count", "info": {"last_token_usage": {
        "input_tokens": 1000, "output_tokens": 50}, "model_context_window": 475000}}),
    );
    assert!(parse(&repeat).is_empty());
}

#[test]
fn a_request_is_counted_once_however_often_it_is_read() {
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", None);
    let record = record("r1", 1000, 600, 0, 50);
    feed(&mut run, &[record.clone(), record]);
    assert_eq!(run.summary.tokens, 1050, "input and output; the cached part is inside the input");
    assert_eq!(run.summary.steps, 1);
}

#[test]
fn the_window_the_rollout_names_sets_the_context_and_beats_the_catalog() {
    let catalog = json!({"openai": {"models": {"gpt-test": {
        "cost": {"input": 2, "output": 10}, "limit": {"context": 1000}}}}});
    let catalog = Arc::new(catalog.as_object().unwrap().clone());
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", Some(catalog));
    feed(
        &mut run,
        &[
            line("turn_context", json!({"model": "gpt-test"})),
            line("event_msg", json!({"type": "task_started", "model_context_window": 10000})),
            record("r1", 2500, 0, 0, 10),
        ],
    );
    assert_eq!(run.summary.context_percent, Some(25.0), "2500 of the rollout's 10000, not the catalog's 1000");
    assert_eq!(run.context_steps.len(), 1);
    assert_eq!(run.summary.model.as_deref(), Some("gpt-test"));

    // Without a catalog entry, the rollout's own window still gives the context.
    let mut bare = run_of(&[
        line("turn_context", json!({"model": "gpt-unknown"})),
        line("event_msg", json!({"type": "task_started", "model_context_window": 10000})),
        record("r1", 2500, 0, 0, 10),
    ]);
    assert_eq!(bare.summary.context_percent, Some(25.0));
    assert_eq!(bare.summary.cost_usd, None);
    assert_eq!(bare.summary.tokens, 2510);
    // No window and no catalog: unknown, never 0%. A step without a window leaves the old fill.
    bare = run_of(&[line("turn_context", json!({"model": "gpt-unknown"})), record("r1", 2500, 0, 0, 10)]);
    assert_eq!(bare.summary.context_percent, None);
}

// ---- the whole sessions ----------------------------------------------------------------------

#[test]
fn a_typed_session_gives_its_prompts_calls_tokens_and_failures() {
    let run = run_of(&fixture("typed.jsonl"));
    let count = |station: Station| run.summary.counts.get(&station).copied().unwrap_or(0);
    assert_eq!(count(Station::You), 2, "two prompts");
    assert_eq!(count(Station::Shell), 5, "Get-Location, Get-Content ×3, a script of its own");
    assert_eq!(count(Station::Files), 2, "rg, and a patch");
    assert_eq!(count(Station::Network), 1, "the MCP call");
    assert_eq!(count(Station::Git), 0);
    assert_eq!(run.summary.errors, 2, "two scripts ran a command that exited with 1");

    // The ten requests: tokens are input plus output of the last total, as the usage collector adds them.
    assert_eq!(run.summary.steps, 10);
    assert_eq!(run.summary.tokens, 447_729 + 3_441);
    assert_eq!(run.summary.model.as_deref(), Some("gpt-6.1-sol"));
    // The last request's input of 60523 against the rollout's 475000.
    assert_eq!(run.summary.context_percent, Some(12.7));
    assert_eq!(run.context_steps.len(), 10);
    assert_eq!(run.summary.cost_usd, None, "no prices were given");

    // Every script ended, the model is not left working, and its time is the time between its
    // prompt or a result and the next call or the end of the task.
    assert_eq!(run.summary.subagents_in_flight, 0);
    assert!(run.entries.iter().all(|entry| entry.ended_at.is_some()), "nothing is left open");
    assert_eq!(run.summary.busy_ms[&Station::Model], 95_861);
    let first_model = run.entries.iter().find(|entry| entry.station == Station::Model).unwrap();
    assert_eq!(first_model.label, "gpt-6.1-sol", "a span the model's own call closed is named by the model");
}

#[test]
fn a_slow_command_is_one_long_call_and_the_model_waits_for_it() {
    let run = run_of(&fixture("exec.jsonl"));
    let shell = run.entries.iter().find(|entry| entry.station == Station::Shell).unwrap();
    assert_eq!(shell.label, "Start-Sleep");
    assert_eq!((shell.ended_at.unwrap() - shell.started_at).num_milliseconds(), 25_148);
    assert_eq!(shell.status, CallStatus::Ok);
    // The model works before the call and after the result, and not while the script runs.
    assert_eq!(run.summary.busy_ms[&Station::Model], 3_019 + 2_692);
    assert_eq!(run.summary.busy_ms[&Station::Shell], 25_148);
    assert_eq!(run.summary.steps, 2);
}

#[test]
fn a_script_is_seen_running_before_it_ends() {
    // The call line is all there is while the script runs: it is open until its result.
    let lines = fixture("exec.jsonl");
    let call = lines.iter().position(|line| line.contains("custom_tool_call\"")).unwrap();
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", None);
    feed(&mut run, &lines[..=call]);
    let shell = run.entries.iter().find(|entry| entry.station == Station::Shell).unwrap();
    assert_eq!((shell.status, shell.ended_at), (CallStatus::Run, None));
    let thinking = run.entries.iter().find(|entry| entry.station == Station::Model).unwrap();
    assert!(thinking.ended_at.is_some(), "the model's turn closed when it made the call");
    feed(&mut run, &lines[call + 1..]);
    assert!(run.entries.iter().all(|entry| entry.ended_at.is_some()));
}

#[test]
fn the_live_totals_match_the_usage_collector_on_the_same_rollout() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = r#"{"openai": {"models": {"gpt-6.1-sol": {"cost": {
        "input": 2, "output": 10, "cache_read": 0.2, "cache_write": 2.5}, "limit": {"context": 200000}}}}}"#;
    let proxy = tmp.path().join(".local/share/copilot-api");
    std::fs::create_dir_all(&proxy).unwrap();
    std::fs::write(proxy.join("models-dev-api.json"), catalog).unwrap();
    let env = CollectorEnv::at(tmp.path().to_path_buf(), None, Arc::new(|| None));
    let rollout = tmp.path().join("rollout.jsonl");
    std::fs::write(&rollout, fixture("typed.jsonl").join("\n") + "\n").unwrap();
    let report = codex::collect(&env, &rollout).unwrap().unwrap();

    let prices = serde_json::from_str::<Value>(catalog).unwrap().as_object().unwrap().clone();
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", Some(Arc::new(prices)));
    feed(&mut run, &fixture("typed.jsonl"));
    let tokens = report["inputTokens"].as_u64().unwrap() + report["outputTokens"].as_u64().unwrap();
    assert_eq!(run.summary.tokens, tokens);
    let cost = report["costUsd"].as_f64().unwrap();
    assert!((run.summary.cost_usd.unwrap() - cost).abs() < 1e-9, "{:?} against {cost}", run.summary.cost_usd);
    assert_eq!(run.summary.model.as_deref(), report["model"].as_str());
}

#[test]
fn a_session_gives_the_same_summary_however_it_is_read() {
    let lines = fixture("typed.jsonl");
    let whole = run_of(&lines);
    let text = lines.join("\n") + "\n";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let cut = text.len() / 2;
    std::fs::write(&path, &text.as_bytes()[..cut]).unwrap();
    let mut tail = Tail::new(&path, 0);
    let mut piecewise = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", None);
    let read = |tail: &mut Tail, run: &mut RunActivity| {
        tail.poll(|line| {
            for event in parse_line("codex", line) {
                run.apply(event);
            }
        })
        .unwrap();
    };
    read(&mut tail, &mut piecewise);
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&text.as_bytes()[cut..]).unwrap();
    read(&mut tail, &mut piecewise);
    assert_eq!(piecewise.summary.tokens, whole.summary.tokens);
    assert_eq!(piecewise.summary.counts, whole.summary.counts);
    assert_eq!(piecewise.summary.busy_ms, whole.summary.busy_ms);
    assert_eq!(piecewise.summary.errors, whole.summary.errors);
}

#[test]
fn every_event_knows_the_time_of_its_line_and_a_setting_has_none() {
    // The run's "last written" is taken from these, since the rollout's own modified time stands
    // still on Windows while Codex has the file open.
    let lines = fixture("typed.jsonl");
    let mut newest = None;
    for line in &lines {
        for event in parse(line) {
            match event.at() {
                Some(at) => newest = newest.max(Some(at)),
                None => assert!(matches!(event, Parsed::Setting { .. }), "{event:?}"),
            }
        }
    }
    assert_eq!(newest, Some("2026-11-12T22:19:33.809Z".parse().unwrap()), "the last task_complete");
    let mut run = RunActivity::new("PROJ-1", "architect", 1, 1, 1, "codex", None);
    for line in &lines {
        for event in parse(line) {
            if let Some(at) = event.at() {
                run.note_write(at);
            }
            run.apply(event);
        }
    }
    assert_eq!(run.summary.last_write_at, newest);
}

#[test]
fn malformed_lines_give_nothing_and_do_no_harm() {
    for text in [
        "",
        "{",
        "[]",
        "null",
        "{\"type\": \"event_msg\"}",
        "{\"timestamp\": \"not a time\", \"type\": \"event_msg\", \"payload\": {\"type\": \"task_complete\"}}",
        "{\"timestamp\": \"2026-11-13T10:00:00Z\", \"type\": \"event_msg\", \"payload\": 5}",
        "{\"timestamp\": \"2026-11-13T10:00:00Z\", \"type\": 5, \"payload\": {\"type\": \"task_complete\"}}",
    ] {
        assert!(parse(text).is_empty(), "{text}");
    }
}

#[test]
fn codex_is_understood_and_an_unknown_harness_is_not() {
    assert!(parses("codex"));
    assert!(!parses("gemini"));
    assert!(parse_line("gemini", &call_line("exec", "text(1)")).is_empty());
}

// ---- finding the rollout ---------------------------------------------------------------------

fn write_rollout(home: &std::path::Path, day: &str, name: &str, meta_id: &str, extra: &str) -> PathBuf {
    let dir = home.join(".codex/sessions").join(day);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let meta = json!({"timestamp": "2026-11-13T10:00:00.000Z", "type": "session_meta",
                      "payload": {"id": meta_id, "session_id": meta_id, "source": "cli"}});
    std::fs::write(&path, format!("{meta}\n{extra}")).unwrap();
    path
}

#[test]
fn a_resumed_codex_run_finds_its_rollout_by_session_id_and_starts_at_its_length() {
    let home = tempfile::tempdir().unwrap();
    let id = "01a1d1d3-798c-74f3-8d04-c4e063a9ac34";
    let file = write_rollout(home.path(), "2026/11/13", &format!("rollout-2026-11-13T11-14-20-{id}.jsonl"), id, "x\n");
    // Others that must not be taken: another session's, the same name with another id inside, and a
    // file that is not a rollout.
    write_rollout(home.path(), "2026/11/13", "rollout-2026-11-13T12-00-00-other-id.jsonl", "other-id", "");
    write_rollout(home.path(), "2026/11/12", &format!("rollout-2026-11-12T09-00-00-{id}.jsonl"), "another-id", "");
    write_rollout(home.path(), "2026/11/12", &format!("notes-{id}.jsonl"), id, "");

    let start = transcript_start(home.path(), "codex", Some(id));
    assert_eq!(start.path.as_deref(), Some(file.as_path()));
    assert_eq!(start.offset, std::fs::metadata(&file).unwrap().len());
    assert_eq!(launch_offset(&start)["offset"], start.offset);
    assert!(launch_offset(&start).get("subagents").is_none(), "only Claude has subagent transcripts");

    // No id yet (a fresh run), an id with no rollout, an unsafe id: nothing named.
    assert_eq!(transcript_start(home.path(), "codex", None).path, None);
    assert_eq!(transcript_start(home.path(), "codex", Some("no-such-session")).path, None);
    assert_eq!(transcript_start(home.path(), "codex", Some("../x")).path, None);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(transcript_start(empty.path(), "codex", Some(id)).path, None);
}

#[test]
fn a_rollout_in_an_older_day_folder_is_still_found() {
    let home = tempfile::tempdir().unwrap();
    let id = "id-from-last-week";
    let file = write_rollout(home.path(), "2026/11/02", &format!("rollout-2026-11-02T08-00-00-{id}.jsonl"), id, "");
    for day in ["2026/11/13", "2026/11/12", "2026/11/10"] {
        write_rollout(home.path(), day, "rollout-2026-x-someone-else.jsonl", "someone-else", "");
    }
    assert_eq!(transcript_start(home.path(), "codex", Some(id)).path.as_deref(), Some(file.as_path()));
}
