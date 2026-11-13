//! Claude transcript lines → activity events (`engine::activity`). The fixtures in
//! `tests/fixtures/activity/claude/` are cut from real transcripts and scrubbed down to the
//! fields the parser reads; the per-tool and label tests build their lines inline, because the
//! fixtures hold no tool input beyond a stand-in Bash command.

use std::path::PathBuf;

use agentflow_core::engine::activity::{
    CallStatus, Parsed, Station, StepDedup, classify_shell, message_key, parse_line,
};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activity/claude").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect()
}

fn parse(line: &str) -> Vec<Parsed> {
    parse_line("claude", line)
}

const NOW: &str = "2026-11-12T10:00:00.000Z";

fn usage() -> Value {
    json!({"input_tokens": 3, "output_tokens": 5, "cache_read_input_tokens": 7, "cache_creation_input_tokens": 11})
}

/// An assistant line holding one tool call.
fn tool_line(name: &str, input: Value) -> String {
    json!({
        "type": "assistant", "timestamp": NOW,
        "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": usage(),
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": name, "input": input}]}
    })
    .to_string()
}

/// The station and label a tool call is given.
fn call(name: &str, input: Value) -> (Station, String) {
    let events = parse(&tool_line(name, input));
    let starts: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            Parsed::Start { station, label, id, inbound, .. } => {
                assert_eq!(id, "toolu_1");
                assert!(!inbound);
                Some((station, label))
            }
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "one Start for {name}");
    starts.into_iter().next().unwrap()
}

fn bash(command: &str) -> (Station, String) {
    call("Bash", json!({"command": command}))
}

fn user_line(extra: Value, content: Value) -> String {
    let mut line = json!({"type": "user", "timestamp": NOW, "message": {"content": content}});
    for (key, value) in extra.as_object().unwrap() {
        line[key] = value.clone();
    }
    line.to_string()
}

fn inbound(events: &[Parsed]) -> bool {
    matches!(events, [Parsed::Start { station: Station::You, inbound: true, .. }])
}

// ---- stations --------------------------------------------------------------------------------

#[test]
fn every_tool_in_the_table_lands_on_its_station() {
    let cases: &[(&str, Value, Station)] = &[
        ("Read", json!({"file_path": "/a/b.rs"}), Station::Files),
        ("Write", json!({"file_path": "/a/b.rs"}), Station::Files),
        ("Edit", json!({"file_path": "/a/b.rs"}), Station::Files),
        ("MultiEdit", json!({"file_path": "/a/b.rs"}), Station::Files),
        ("NotebookEdit", json!({"notebook_path": "/a/b.ipynb"}), Station::Files),
        ("Grep", json!({"pattern": "secret"}), Station::Files),
        ("Glob", json!({"pattern": "**/*.rs"}), Station::Files),
        ("WebFetch", json!({"url": "https://example.com/x"}), Station::Network),
        ("WebSearch", json!({"query": "secret"}), Station::Network),
        ("Artifact", json!({}), Station::Network),
        ("mcp__atlassian__getJiraIssue", json!({"issueIdOrKey": "X-1"}), Station::Network),
        ("Agent", json!({"description": "Explore the repo"}), Station::Agents),
        ("Task", json!({"description": "Explore the repo"}), Station::Agents),
        ("Workflow", json!({}), Station::Agents),
        ("SendMessage", json!({}), Station::Agents),
        ("TaskStop", json!({}), Station::Agents),
        ("Skill", json!({"skill": "simplify"}), Station::Skills),
        ("ToolSearch", json!({"query": "select:Read"}), Station::Skills),
        ("AskUserQuestion", json!({"questions": []}), Station::You),
        ("SendUserFile", json!({}), Station::You),
        ("Bash", json!({"command": "cargo test"}), Station::Shell),
        ("PowerShell", json!({"command": "Get-Date"}), Station::Shell),
        // A tool nobody listed falls to Shell, named after itself.
        ("TodoWrite", json!({"todos": []}), Station::Shell),
    ];
    for (name, input, station) in cases {
        assert_eq!(call(name, input.clone()).0, *station, "{name}");
    }
    assert_eq!(call("TodoWrite", json!({})).1, "TodoWrite");
}

#[test]
fn an_assistant_line_gives_a_step_then_a_start_per_tool_use() {
    let line = json!({
        "type": "assistant", "timestamp": NOW,
        "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": usage(),
                    "content": [{"type": "text"},
                                {"type": "tool_use", "id": "toolu_a", "name": "Read", "input": {"file_path": "/x/y.rs"}},
                                {"type": "tool_use", "id": "toolu_b", "name": "Grep", "input": {}}]}
    })
    .to_string();
    let events = parse(&line);
    assert_eq!(events.len(), 3);
    match &events[0] {
        Parsed::Step {
            message_id,
            model,
            input_tokens,
            output_tokens,
            cache_read,
            cache_write,
            cache_write_1h,
            ..
        } => {
            assert_eq!(message_id.as_deref(), Some("s:msg_1"));
            assert_eq!(model.as_deref(), Some("claude-opus-5-5"));
            assert_eq!((*input_tokens, *output_tokens, *cache_read, *cache_write, *cache_write_1h), (3, 5, 7, 11, 0));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&events[1], Parsed::Start { id, .. } if id == "toolu_a"));
    assert!(matches!(&events[2], Parsed::Start { id, .. } if id == "toolu_b"));
}

#[test]
fn only_plain_tool_use_blocks_are_calls() {
    // These never get a tool_result, so a Start would stay open for good.
    let line = json!({
        "type": "assistant", "timestamp": NOW,
        "message": {"id": "m", "model": "claude-opus-5-5", "usage": usage(),
                    "content": [{"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search"},
                                {"type": "advisor_tool_result", "tool_use_id": "srvtoolu_1"},
                                {"type": "tool_use", "name": "Read"},
                                {"type": "tool_use", "id": "", "name": "Read"}]}
    })
    .to_string();
    let events = parse(&line);
    assert!(events.iter().all(|event| matches!(event, Parsed::Step { .. })), "{events:?}");
}

#[test]
fn the_shell_decides_by_command_name() {
    assert_eq!(bash("flowctl heartbeat --phase x").0, Station::Agentflow);
    assert_eq!(bash("/mingw64/bin/flowctl.exe complete").0, Station::Agentflow);
    assert_eq!(bash("cd /repo && flowctl ask \"which one\"").0, Station::Agentflow);
    assert_eq!(bash("git status").0, Station::Git);
    assert_eq!(bash("git push origin main").0, Station::Git, "push is Git, not Network");
    assert_eq!(bash("git fetch").0, Station::Git);
    assert_eq!(bash("/mingw64/bin/git -C \"D:/some dir\" commit -m x").0, Station::Git);
    assert_eq!(bash("rtk git diff").0, Station::Git, "a wrapper word is not the command");
    assert_eq!(bash("GIT_PAGER=cat git log").0, Station::Git);
    assert_eq!(bash("curl -s https://example.com").0, Station::Network);
    assert_eq!(bash("gh pr view 3").0, Station::Network);
    assert_eq!(bash("npm install").0, Station::Network);
    assert_eq!(bash("npm test").0, Station::Shell);
    assert_eq!(bash("cat notes.md").0, Station::Files);
    assert_eq!(bash("cat a.txt | grep b | head -3").0, Station::Files);
    assert_eq!(bash("cd /repo && ls -la 2>&1").0, Station::Files, "cd says nothing either way");
    assert_eq!(bash("cat a.txt && cargo build").0, Station::Shell);
    assert_eq!(bash("cargo test").0, Station::Shell);
    assert_eq!(bash("").0, Station::Shell);
}

#[test]
fn a_command_inside_a_substitution_counts() {
    assert_eq!(bash("echo $(curl -s https://example.com)").0, Station::Network);
    assert_eq!(bash("echo \"$(curl -s https://example.com) done\"").0, Station::Network);
    assert_eq!(bash("x=`curl https://example.com`; echo $x").0, Station::Network);
    assert_eq!(bash("echo $(git rev-parse HEAD)").0, Station::Git);
    assert_eq!(bash("(cd sub && curl https://example.com)").0, Station::Network);
}

#[test]
fn a_command_name_inside_quotes_is_only_an_argument() {
    assert_eq!(bash("echo \"git push; curl x\"").0, Station::Shell);
    assert_eq!(bash("echo 'flowctl ask'").0, Station::Shell);
    assert_eq!(bash("cargo test -- --name \"a | b\"").0, Station::Shell);
}

#[test]
fn the_lines_of_a_heredoc_are_text_not_commands() {
    // Claude Code's usual commit: the body is the commit message, whatever words it holds.
    let commit = "git commit -m \"$(cat <<'EOF'\nFix the thing\n\nflowctl ask now waits, and don't curl it\ngit push is next\nEOF\n)\"";
    assert_eq!(bash(commit), (Station::Git, "git commit".to_string()));
    let pr = "gh pr create --title x --body \"$(cat <<'EOF'\n## Summary\ngit push --force\nflowctl ask\nEOF\n)\"";
    assert_eq!(bash(pr).0, Station::Network);
    assert_eq!(bash("cat <<'EOF' > notes.txt\nsome text\nEOF").0, Station::Files);
    assert_eq!(bash("cat <<EOF\ncurl x\nEOF").0, Station::Files);
    // A tab-indented closing word for `<<-`, Windows line ends, and a command after the heredoc.
    assert_eq!(bash("cat <<-EOF\n\tcurl x\n\tEOF\ncargo test").0, Station::Shell);
    assert_eq!(bash("cat <<EOF\r\ncurl x\r\nEOF\r\nflowctl ask").0, Station::Agentflow);
    assert_eq!(bash("cat <<'EOF'\ncurl x\nEOF\ngit status").0, Station::Git, "the line after the closing word counts");
    // Two heredocs on one line close in order.
    assert_eq!(bash("cat <<A <<B\ncurl x\nA\nflowctl y\nB\ncargo test").0, Station::Shell);
    // One that never closes swallows the rest, and nothing panics.
    assert_eq!(bash("cat <<EOF\ncurl x").0, Station::Files);
    assert_eq!(bash("cat <<").0, Station::Files);
    assert_eq!(bash("cat <<-").0, Station::Files);
    // `<<<` is a here-string, and `<<` before a number is a shift: neither hides what follows.
    assert_eq!(bash("grep x <<< \"$y\"\ncurl z").0, Station::Network);
    assert_eq!(bash("echo $((1 << 3))\ncurl z").0, Station::Network);
}

#[test]
fn a_wrapper_option_that_takes_a_value_does_not_become_the_command() {
    assert_eq!(bash("env -u PSModulePath git status"), (Station::Git, "git status".to_string()));
    assert_eq!(bash("env --unset X -C /tmp flowctl ask").0, Station::Agentflow);
    assert_eq!(bash("sudo -u root curl https://example.com").0, Station::Network);
    assert_eq!(bash("nice -n 5 git log").0, Station::Git);
    assert_eq!(bash("time -p git log").0, Station::Git);
    assert_eq!(bash("env -i FOO=1 git log").0, Station::Git);
    // A wrapper alone is itself the command.
    assert_eq!(bash("env").0, Station::Shell);
    assert_eq!(bash("env -u").0, Station::Shell);
}

#[test]
fn classify_shell_is_public_and_takes_a_description() {
    assert_eq!(
        classify_shell("curl https://example.com", "Fetch the page"),
        (Station::Network, "Fetch the page".to_string())
    );
}

// ---- labels ----------------------------------------------------------------------------------

#[test]
fn a_file_shows_the_last_two_parts_of_its_path() {
    assert_eq!(call("Read", json!({"file_path": "/home/me/project/src/main.rs"})).1, "src/main.rs");
    assert_eq!(call("Edit", json!({"file_path": "D:\\Dev\\Project\\src\\lib.rs"})).1, "src/lib.rs");
    assert_eq!(call("Write", json!({"file_path": "main.rs"})).1, "main.rs");
    assert_eq!(call("NotebookEdit", json!({"notebook_path": "/a/b/c.ipynb"})).1, "b/c.ipynb");
    assert_eq!(call("Read", json!({})).1, "Read", "no path: the tool's name");
    assert_eq!(call("Grep", json!({"pattern": "secret", "path": "/x"})).1, "Grep");
    assert_eq!(call("Glob", json!({"pattern": "secret"})).1, "Glob");
}

#[test]
fn a_web_address_shows_the_host_only() {
    assert_eq!(call("WebFetch", json!({"url": "https://docs.rs/serde/latest/serde?token=abc#frag"})).1, "docs.rs");
    assert_eq!(call("WebFetch", json!({"url": "example.com/path"})).1, "example.com");
    assert_eq!(call("WebFetch", json!({"url": "http://localhost:8765/api/tasks"})).1, "localhost:8765");
    // A login and a password never reach the label, even when the password holds a slash.
    let (_, label) = call("WebFetch", json!({"url": "https://user:hunter2@example.com:8080/secret?token=1"}));
    assert_eq!(label, "example.com:8080");
    assert_eq!(call("WebFetch", json!({"url": "https://user:pa/ss@evil.com/x"})).1, "evil.com");
    // An address that reads two ways gives its scheme alone.
    assert_eq!(call("WebFetch", json!({"url": "https://host:8080/a@b"})).1, "https");
    assert_eq!(call("WebFetch", json!({})).1, "Web page");
    assert_eq!(call("WebSearch", json!({"query": "my password is hunter2"})).1, "Web search");
    assert_eq!(call("mcp__atlassian__getJiraIssue", json!({"issueIdOrKey": "X-1"})).1, "atlassian: getJiraIssue");
    assert_eq!(call("mcp__claude_ai_Claude_Docs__update", json!({})).1, "claude_ai_Claude_Docs: update");
    assert_eq!(call("mcp__plugin_x__list_pages", json!({})).1, "plugin_x: list pages");
}

#[test]
fn a_shell_command_shows_the_description_not_the_command_line() {
    let (_, label) = call("Bash", json!({"command": "cargo test --token hunter2", "description": "Run the tests"}));
    assert_eq!(label, "Run the tests");
    let (_, label) = call("Bash", json!({"command": "cargo test --token hunter2"}));
    assert_eq!(label, "cargo", "no description: the command's name");
    let (_, label) = call("Bash", json!({"command": "cd /repo && /usr/bin/cargo.exe test --token hunter2"}));
    assert_eq!(label, "cargo", "the first command that is not a cd");
    let (_, label) = call("Bash", json!({"command": "   ", "description": "  "}));
    assert_eq!(label, "shell");
    let (_, label) = call("Bash", json!({"command": 5, "description": null}));
    assert_eq!(label, "shell");
}

#[test]
fn flowctl_and_git_show_their_subcommand_never_its_arguments() {
    assert_eq!(bash("flowctl ask \"what is the password\" --options a,b").1, "flowctl ask");
    assert_eq!(bash("flowctl heartbeat --phase x").1, "flowctl heartbeat");
    assert_eq!(bash("git commit -m \"fix the secret thing\"").1, "git commit");
    assert_eq!(bash("git -C \"D:/some dir\" -c user.name=x push origin main").1, "git push");
    assert_eq!(bash("git --no-pager log").1, "git log");
    assert_eq!(bash("git").1, "git");
    assert_eq!(bash("git \"Secret Words\"").1, "git", "a subcommand that is not a name is left out");
    // Claude's description does not replace the subcommand.
    assert_eq!(call("Bash", json!({"command": "git status", "description": "Show status"})).1, "git status");
}

#[test]
fn an_agent_shows_its_description_and_a_skill_its_name() {
    assert_eq!(
        call("Agent", json!({"description": "Explore the repo", "prompt": "secret instructions"})).1,
        "Explore the repo"
    );
    assert_eq!(call("Agent", json!({"prompt": "secret instructions"})).1, "Subagent");
    assert_eq!(call("Skill", json!({"skill": "simplify", "args": "secret"})).1, "simplify");
    assert_eq!(call("Skill", json!({})).1, "Skill");
    assert_eq!(call("ToolSearch", json!({"query": "select:Read"})).1, "Loading tools");
    assert_eq!(call("AskUserQuestion", json!({"questions": [{"question": "secret?"}]})).1, "Question for you");
}

#[test]
fn labels_are_cleaned_and_cut() {
    let (_, label) = call("Bash", json!({"command": "x", "description": "a\u{1b}[31m  b\u{202e}c\td\ne"}));
    assert_eq!(label, "a [31m b c d e");
    // 120 characters, cut on a character boundary: multi-byte text must not panic.
    for unit in ["é", "日", "😀"] {
        let (_, label) = call("Bash", json!({"command": "x", "description": unit.repeat(300)}));
        assert_eq!(label.chars().count(), 120, "{unit}");
        assert!(label.ends_with('…'));
        assert!(label.starts_with(unit));
    }
    let (_, label) = call("Bash", json!({"command": "x", "description": "y".repeat(120)}));
    assert_eq!(label.chars().count(), 120);
    assert!(!label.ends_with('…'), "120 characters fit");
    // A command name or path from a long script is cut too.
    let (_, label) = call("Read", json!({"file_path": format!("/a/{}", "z".repeat(500))}));
    assert!(label.chars().count() <= 120);
}

// ---- the person ------------------------------------------------------------------------------

#[test]
fn a_prompt_from_the_person_is_an_inbound_start_labelled_prompt() {
    let lines = fixture("prompts.jsonl");
    let events = parse(&lines[0]);
    assert!(inbound(&events), "{events:?}");
    let Parsed::Start { id, label, at, .. } = &events[0] else { unreachable!() };
    assert_eq!(label, "Prompt");
    assert_eq!(at.to_rfc3339(), "2026-10-20T21:30:47.362+00:00");
    // No uuid in the scrubbed fixture: the timestamp stands in, and reading the line again gives
    // the same id.
    assert!(id.starts_with("prompt-"));
    assert_eq!(parse(&lines[0]), events);
}

#[test]
fn a_prompt_never_shows_its_text() {
    let events = parse(&user_line(json!({"origin": {"kind": "human"}}), json!("my password is hunter2")));
    assert!(inbound(&events));
    assert!(!format!("{events:?}").contains("hunter2"));
}

#[test]
fn the_same_line_gives_the_same_id_and_a_uuid_is_preferred() {
    let line = user_line(json!({"uuid": "abc-123"}), json!("hello"));
    let events = parse(&line);
    assert!(matches!(&events[..], [Parsed::Start { id, .. }] if id == "prompt-abc-123"));
    assert_eq!(parse(&line), events);
}

#[test]
fn a_user_line_with_text_blocks_and_no_origin_is_a_prompt() {
    // Older CLIs write no origin, and content may be a list of blocks.
    assert!(inbound(&parse(&user_line(json!({}), json!([{"type": "text", "text": "hello"}])))));
    assert!(inbound(&parse(&user_line(json!({}), json!("hello")))));
}

#[test]
fn lines_that_are_not_the_person_are_not_inbound() {
    let lines = fixture("prompts.jsonl");
    // human, meta, system-reminder, task-notification, command, peer, compaction summary,
    // auto-continuation (in the order the fixture lists them)
    assert_eq!(lines.len(), 8);
    for (index, line) in lines.iter().enumerate().skip(1) {
        assert!(parse(line).is_empty(), "fixture line {index}: {line}");
    }
}

#[test]
fn each_exclusion_rule_holds_on_its_own() {
    let human = json!({"origin": {"kind": "human"}});
    for (name, extra, content) in [
        ("isMeta", json!({"isMeta": true}), json!("hello")),
        ("isCompactSummary", json!({"isCompactSummary": true}), json!("hello")),
        ("isVisibleInTranscriptOnly", json!({"isVisibleInTranscriptOnly": true}), json!("hello")),
        ("subagent prompt", json!({"isSidechain": true}), json!("hello")),
        ("peer", json!({"origin": {"kind": "peer"}}), json!("hello")),
        ("auto-continuation", json!({"origin": {"kind": "auto-continuation"}}), json!("hello")),
        ("task-notification", json!({"origin": {"kind": "task-notification"}}), json!("hello")),
        ("origin without a kind", json!({"origin": {}}), json!("hello")),
        ("origin not an object", json!({"origin": "human"}), json!("hello")),
        ("system-reminder", human.clone(), json!("<system-reminder>x</system-reminder>")),
        ("leading space then tag", human.clone(), json!("  \n<command-name>x</command-name>")),
        ("local-command", json!({}), json!("<local-command-stdout>x")),
        ("bracketed notice", human.clone(), json!("[Image #1]")),
        ("bracket in a text block", json!({}), json!([{"type": "text", "text": " [Image #1]"}])),
        ("empty", human.clone(), json!("   ")),
        ("no text block", json!({}), json!([{"type": "image"}])),
        ("content a number", json!({}), json!(5)),
    ] {
        assert!(parse(&user_line(extra, content)).is_empty(), "{name}");
    }
    // A false flag is no flag.
    assert!(inbound(&parse(&user_line(json!({"isMeta": false, "origin": {"kind": "human"}}), json!("hello")))));
}

#[test]
fn turns_end_on_a_turn_duration_line_an_interruption_or_an_error_message() {
    let ended = |events: Vec<Parsed>| matches!(&events[..], [Parsed::TurnEnd { .. }]);
    assert!(ended(parse(&json!({"type": "system", "subtype": "turn_duration", "timestamp": NOW}).to_string())));
    assert!(ended(parse(&user_line(json!({"origin": {"kind": "human"}}), json!("[Request interrupted by user]")))));
    assert!(ended(parse(
        &json!({"type": "assistant", "timestamp": NOW, "message": {"id": "a", "model": "<synthetic>", "content": [{"type": "text"}]}})
            .to_string()
    )));
    // Other system lines say nothing about the turn.
    for subtype in ["informational", "local_command", "compact_boundary"] {
        assert!(parse(&json!({"type": "system", "subtype": subtype, "timestamp": NOW}).to_string()).is_empty());
    }
    assert!(parse(&json!({"type": "system", "subtype": "turn_duration"}).to_string()).is_empty(), "no timestamp");
}

// ---- results and usage -----------------------------------------------------------------------

#[test]
fn a_tool_result_ends_its_call_and_is_error_marks_it_failed() {
    let lines = fixture("calls.jsonl");
    let mut ends = Vec::new();
    for line in &lines {
        for event in parse(line) {
            if let Parsed::End { id, is_error, .. } = event {
                ends.push((id, is_error));
            }
        }
    }
    assert_eq!(ends.len(), 4);
    let flags: Vec<bool> = ends.iter().map(|(_, is_error)| *is_error).collect();
    assert_eq!(flags, [false, false, true, false], "the third call, `cargo test`, failed");
    // Every End matches a Start with the same id.
    let starts: Vec<String> = lines
        .iter()
        .flat_map(|line| parse(line))
        .filter_map(|event| match event {
            Parsed::Start { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 4);
    for (id, _) in &ends {
        assert!(starts.contains(id), "{id}");
    }
    // The statuses a run keeps are `run`, `ok`, `err`.
    assert_eq!(serde_json::to_string(&CallStatus::Err).unwrap(), "\"err\"");
}

#[test]
fn a_tool_result_with_several_blocks_ends_each_call() {
    let line = user_line(
        json!({}),
        json!([{"type": "tool_result", "tool_use_id": "a", "is_error": true},
               {"type": "tool_result", "tool_use_id": "b"},
               {"type": "tool_result"},
               {"type": "text", "text": "hello"}]),
    );
    let events = parse(&line);
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], Parsed::End { id, is_error: true, .. } if id == "a"));
    assert!(matches!(&events[1], Parsed::End { id, is_error: false, .. } if id == "b"));
}

#[test]
fn the_fixture_commands_classify_as_the_stand_ins_say() {
    let lines = fixture("calls.jsonl");
    let starts: Vec<(Station, String)> = lines
        .iter()
        .flat_map(|line| parse(line))
        .filter_map(|event| match event {
            Parsed::Start { station, label, .. } => Some((station, label)),
            _ => None,
        })
        .collect();
    assert_eq!(starts[0], (Station::Git, "git status".to_string()));
    assert_eq!(starts[1], (Station::Agentflow, "flowctl heartbeat".to_string()));
    assert_eq!(starts[2], (Station::Shell, "Run the tests".to_string()));
}

#[test]
fn usage_repeated_across_the_lines_of_one_message_counts_once() {
    let lines = fixture("calls.jsonl");
    let mut dedup = StepDedup::default();
    let (mut all, mut counted) = (0u64, 0u64);
    let mut steps = 0;
    for line in &lines {
        for event in parse(line) {
            if let Parsed::Step { message_id, output_tokens, .. } = event {
                steps += 1;
                all += output_tokens;
                if dedup.counts(message_id.as_deref()) {
                    counted += output_tokens;
                }
            }
        }
    }
    // Three calls, each one message, then one message written as three lines (thinking, text,
    // tool_use) that repeat the same usage: 543 tokens counted once, not three times.
    assert_eq!(steps, 6);
    assert_eq!(all, 196 + 329 + 3092 + 3 * 543);
    assert_eq!(counted, 196 + 329 + 3092 + 543);
}

#[test]
fn a_step_without_a_usable_id_always_counts() {
    let mut dedup = StepDedup::default();
    assert!(dedup.counts(None));
    assert!(dedup.counts(None));
    assert!(dedup.counts(Some("s:a")));
    assert!(!dedup.counts(Some("s:a")));
    assert!(dedup.counts(Some("s:b")));
}

#[test]
fn the_message_id_key_follows_the_usage_collector() {
    assert_eq!(message_key(&json!("msg_1")).as_deref(), Some("s:msg_1"));
    assert_eq!(message_key(&json!(7)).as_deref(), Some("n:7"));
    assert_eq!(message_key(&json!(7.0)).as_deref(), Some("n:7"));
    assert_eq!(message_key(&json!(true)).as_deref(), Some("n:1"));
    for falsy in [json!(""), json!(0), json!(false), json!(null), json!([1]), json!({"a": 1})] {
        assert_eq!(message_key(&falsy), None, "{falsy}");
    }
}

#[test]
fn a_step_needs_usage_and_a_real_model() {
    let step = |message: Value| parse(&json!({"type": "assistant", "timestamp": NOW, "message": message}).to_string());
    assert!(step(json!({"id": "a", "model": "claude-opus-5-5"})).is_empty(), "no usage");
    assert!(step(json!({"id": "a", "model": "claude-opus-5-5", "usage": {}})).is_empty(), "empty usage");
    assert!(step(json!({"id": "a", "usage": usage()})).is_empty(), "no model");
    let synthetic = step(json!({"id": "a", "model": "<synthetic>", "usage": usage()}));
    assert!(matches!(&synthetic[..], [Parsed::TurnEnd { .. }]), "synthetic: no step, but the turn is over");
    assert_eq!(step(json!({"id": "a", "model": "claude-opus-5-5", "usage": usage()})).len(), 1);
    // The one-hour cache write is part of the cache write.
    let events = step(json!({"id": "a", "model": "m", "usage": {
        "cache_creation_input_tokens": 100, "cache_creation": {"ephemeral_1h_input_tokens": 400}}}));
    assert!(matches!(&events[..], [Parsed::Step { cache_write: 100, cache_write_1h: 100, .. }]));
}

// ---- bad input -------------------------------------------------------------------------------

#[test]
fn a_malformed_line_gives_nothing_and_does_not_panic() {
    for line in [
        "",
        " ",
        "{",
        "}",
        "null",
        "[]",
        "5",
        "\"text\"",
        "{\"type\":\"assistant\"}",
        "{\"type\":\"assistant\",\"timestamp\":\"yesterday\",\"message\":{}}",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-12T10:00:00Z\",\"message\":[]}",
        "{\"type\":\"user\",\"timestamp\":\"2026-11-12T10:00:00Z\"}",
        "{\"type\":\"user\",\"timestamp\":5,\"message\":{\"content\":\"hello\"}}",
        "{\"type\":7,\"timestamp\":\"2026-11-12T10:00:00Z\",\"message\":{}}",
        "{\"type\":\"attachment\",\"timestamp\":\"2026-11-12T10:00:00Z\"}",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-12T10:00:00Z\",\"message\":{\"content\":5}}",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-12T10:00:00Z\",\"message\":{\"content\":[1,null,\"x\",[]]}}",
        "\u{feff}{",
        "{\"type\":\"user\",\"timestamp\":\"2026-11-12T10:00:00Z\",\"message\":{\"content\":[1,null,\"x\"]}}",
    ] {
        assert!(parse(line).is_empty(), "{line:?}");
    }
    // Content that is half-shaped: nothing panics and nothing is invented.
    for input in
        [json!(null), json!(5), json!("x"), json!([]), json!({"command": []}), json!({"file_path": 5, "url": 5})]
    {
        for name in ["Bash", "Read", "WebFetch", "Agent", "Skill", "mcp__", "mcp__a", "mcp____b", ""] {
            let events = parse(&tool_line(name, input.clone()));
            assert!(events.len() <= 2, "{name}: {events:?}");
        }
    }
}

#[test]
fn odd_token_counts_are_zero_not_errors() {
    let line = json!({"type": "assistant", "timestamp": NOW, "message": {"id": "a", "model": "m", "usage": {
        "input_tokens": "12", "output_tokens": -5, "cache_read_input_tokens": null, "cache_creation_input_tokens": 1.0e30}}})
    .to_string();
    let events = parse(&line);
    let [Parsed::Step { input_tokens, output_tokens, cache_read, cache_write, .. }] = &events[..] else {
        panic!("{events:?}")
    };
    assert_eq!((*input_tokens, *output_tokens, *cache_read), (0, 0, 0));
    assert!(*cache_write > 0, "a huge float saturates instead of failing");
}

#[test]
fn a_claude_line_means_nothing_to_the_other_harnesses() {
    let line = tool_line("Read", json!({"file_path": "/a/b.rs"}));
    assert!(!parse(&line).is_empty());
    for harness in ["copilot", "codex", "", "Claude", "gpt"] {
        assert!(parse_line(harness, &line).is_empty(), "{harness:?}");
    }
    // Claude and Copilot are understood (see activity_copilot.rs); Codex is not yet.
    assert!(agentflow_core::engine::activity::parses("claude"));
    assert!(agentflow_core::engine::activity::parses("copilot"));
    assert!(!agentflow_core::engine::activity::parses("codex"));
}

#[test]
fn events_are_cloneable_and_the_wire_types_serialise_as_the_plan_says() {
    use agentflow_core::engine::activity::{ActivityEntry, ContextStep, Span};
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-12T10:00:00.250Z").unwrap().with_timezone(&chrono::Utc);
    let entry = ActivityEntry {
        id: "toolu_1".into(),
        station: Station::Agentflow,
        label: "flowctl ask".into(),
        started_at: at,
        ended_at: None,
        status: CallStatus::Run,
        by_subagent: false,
        parent_id: None,
        inbound: false,
    };
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        json!({"id": "toolu_1", "station": "agentflow", "label": "flowctl ask", "startedAt": "2026-11-12T10:00:00.250Z",
               "endedAt": null, "status": "run", "bySubagent": false, "parentId": null, "inbound": false})
    );
    let span = Span {
        station: Station::Git,
        started_at: at,
        ended_at: Some(at),
        status: CallStatus::Ok,
        by_subagent: true,
        inbound: false,
    };
    assert_eq!(
        serde_json::to_value(&span).unwrap(),
        json!({"station": "git", "startedAt": "2026-11-12T10:00:00.250Z", "endedAt": "2026-11-12T10:00:00.250Z",
               "status": "ok", "bySubagent": true, "inbound": false})
    );
    assert_eq!(
        serde_json::to_value(ContextStep { at, percent: 12.5 }).unwrap(),
        json!({"at": "2026-11-12T10:00:00.250Z", "percent": 12.5})
    );
}
