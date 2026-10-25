//! Harness adapters against what the Python backend does (contract/golden/harness, recorded by
//! contract/record_python_goldens.py), plus the Python adapter tests ported by name.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agentflow_core::config::HarnessConfig;
use agentflow_core::harness::codex::{Polling, capture_session_id};
use agentflow_core::harness::{HarnessAdapter, HarnessKind, ScreenState, claude::trust_workspace};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};

fn golden(name: &str) -> Value {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/harness").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

fn config(fields: Value) -> HarnessConfig {
    serde_json::from_value(fields).unwrap()
}

fn adapter(kind: &str, command: &str) -> HarnessAdapter {
    HarnessAdapter::new("h", config(json!({"type": kind, "command": command})))
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

/// argv without the executable, with a generated session id replaced as the recorder did.
fn normalized(argv: &[String], session_id: Option<&str>) -> Vec<String> {
    argv[1..].iter().map(|a| if Some(a.as_str()) == session_id { "<SESSION>".into() } else { a.clone() }).collect()
}

#[test]
fn launch_and_resume_argv_match_python() {
    let cases = golden("launches.json");
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 25);
    for case in cases {
        let mut fields = case["extra"].as_object().unwrap().clone();
        fields.insert("type".into(), case["type"].clone());
        fields.insert("command".into(), json!("agentflow-golden-not-installed"));
        let adapter = HarnessAdapter::new("h", config(Value::Object(fields)));
        let what = format!("{} / {}", case["type"], case["config"]);
        assert_eq!(adapter.kind.type_name(), case["typeName"], "{what}");
        assert_eq!(adapter.kind.supports_resume(), case["supportsResume"], "{what}");
        assert_eq!(adapter.model_flag().is_some(), case["supportsModel"], "{what}");
        assert_eq!(adapter.reasoning_flag().is_some(), case["supportsReasoning"], "{what}");
        assert_eq!(adapter.kind.exit_command(), case["exitCommand"].as_str(), "{what}");
        for run in case["runs"].as_array().unwrap() {
            let (model, reasoning) = (run["model"].as_str(), run["reasoning"].as_str());
            let what = format!("{what} / {model:?} {reasoning:?}");
            let launch = adapter.build_launch(model, reasoning, "Read the brief");
            assert_eq!(launch.argv[0], "agentflow-golden-not-installed");
            assert_eq!(
                normalized(&launch.argv, launch.harness_session_id.as_deref()),
                strings(&run["launch"]),
                "{what}"
            );
            assert_eq!(serde_json::to_value(&launch.env).unwrap(), run["launchEnv"], "{what}");
            assert_eq!(launch.harness_session_id.is_some(), !run["launchSessionId"].is_null(), "{what}");
            let resume = adapter.build_resume(Some("sess-1"), model, reasoning, "Read the brief");
            match (&resume, &run["resume"]) {
                (None, Value::Null) => {}
                (Some(resume), expected) => {
                    assert_eq!(normalized(&resume.argv, None), strings(expected), "{what}");
                    assert_eq!(resume.harness_session_id.as_deref(), run["resumeSessionId"].as_str(), "{what}");
                }
                (None, expected) => panic!("{what}: no resume, Python resumed with {expected}"),
            }
            assert_eq!(adapter.build_resume(None, model, reasoning, "x"), None, "{what}");
            assert_eq!(adapter.build_resume(Some(""), model, reasoning, "x"), None, "{what}");
        }
    }
}

#[test]
fn screen_classification_matches_python() {
    let screens = golden("screens.json");
    for screen in screens.as_array().unwrap() {
        let rows = strings(&screen["rows"]);
        for (kind, expected) in screen["states"].as_object().unwrap() {
            let state = adapter(kind, "x").classify_screen(&rows);
            assert_eq!(state.as_str(), expected, "{} as {kind}", screen["name"]);
        }
    }
}

#[test]
fn prompt_references_match_python() {
    let misc = golden("misc.json");
    for (path, expected) in misc["promptReferences"].as_object().unwrap() {
        if cfg!(windows) || !path.contains('\\') {
            assert_eq!(HarnessAdapter::prompt_reference(Path::new(path)), *expected);
        }
    }
}

#[test]
fn trust_workspace_writes_what_python_writes() {
    let dir = tempfile::tempdir().unwrap();
    for case in golden("trust.json").as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let path = dir.path().join(name).join(".claude.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some(text) = case["input"].as_str() {
            std::fs::write(&path, text).unwrap();
        }
        let before = std::fs::read(&path).ok();
        trust_workspace(&path, Path::new(case["cwd"].as_str().unwrap()));
        let after = std::fs::read(&path).ok();
        assert!(!path.with_file_name(".claude.json.agentflow-tmp").exists(), "{name}");
        assert_eq!(after != before, case["written"].as_bool().unwrap(), "{name}");
        let Some(expected) = case["output"].as_str() else { continue };
        let written = String::from_utf8(after.unwrap()).unwrap();
        if name == "node-formatted" {
            // An integer beyond 64 bits is written as the same double in float notation.
            let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap();
            assert_eq!(parse(&written), parse(expected), "{name}");
            assert!(written.contains("\"big\": 1.2345678901234568e+20"), "{written}");
            assert_eq!(written.replace("1.2345678901234568e+20", "123456789012345680000"), expected);
        } else {
            assert_eq!(written, expected, "{name}");
        }
    }
}

/// On a copy of a real `~/.claude.json` (AGENTFLOW_CLAUDE_JSON_COPY; the file is changed in place,
/// so never point it at the real one): every value survives and only the one flag is added.
/// Prints nothing from the file.
#[test]
#[ignore = "needs AGENTFLOW_CLAUDE_JSON_COPY"]
fn trust_workspace_keeps_every_value_of_a_real_claude_json() {
    let path = PathBuf::from(std::env::var("AGENTFLOW_CLAUDE_JSON_COPY").expect("AGENTFLOW_CLAUDE_JSON_COPY"));
    let real = std::env::home_dir().unwrap().join(".claude.json");
    assert!(!agentflow_core::paths::same_path(&path, &real), "point this at a copy, not the real file");
    let before: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cwd = Path::new("D:/agentflow-real-data-check/never-a-real-worktree");

    trust_workspace(&path, cwd);

    let mut expected = before.clone();
    let projects = expected.as_object_mut().unwrap().entry("projects").or_insert_with(|| json!({}));
    projects.as_object_mut().unwrap().insert(as_posix_str(cwd), json!({"hasTrustDialogAccepted": true}));
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(after == expected, "values changed (contents not printed)");
    println!("{} top-level keys preserved", after.as_object().unwrap().len());
}

fn as_posix_str(path: &Path) -> String {
    agentflow_core::harness::as_posix(path)
}

// Ported from backend/tests/test_claude_adapter.py -------------------------------------------------

const WORKTREE: &str = "D:/agentflow/worktrees/DE-1";

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn test_trust_workspace_is_a_noop_when_no_claude_config_exists() {
    let dir = tempfile::tempdir().unwrap();
    trust_workspace(&dir.path().join("missing").join(".claude.json"), &dir.path().join("worktree"));
    assert!(!dir.path().join("missing").exists());
}

#[test]
fn test_trust_workspace_adds_a_new_project_entry() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join(".claude.json");
    std::fs::write(
        &config_path,
        json!({"projects": {"C:/Other/Project": {"hasTrustDialogAccepted": true}}}).to_string(),
    )
    .unwrap();

    trust_workspace(&config_path, Path::new(WORKTREE));

    let data = read_json(&config_path);
    assert_eq!(data["projects"][WORKTREE]["hasTrustDialogAccepted"], true);
    // The unrelated existing project entry must survive untouched.
    assert_eq!(data["projects"]["C:/Other/Project"]["hasTrustDialogAccepted"], true);
}

#[test]
fn test_trust_workspace_preserves_other_fields_on_an_existing_entry() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join(".claude.json");
    let existing = json!({"projects": {WORKTREE: {"hasTrustDialogAccepted": false, "allowedTools": ["Bash"]}}});
    std::fs::write(&config_path, existing.to_string()).unwrap();

    trust_workspace(&config_path, Path::new(WORKTREE));

    let entry = &read_json(&config_path)["projects"][WORKTREE];
    assert_eq!(entry["hasTrustDialogAccepted"], true);
    assert_eq!(entry["allowedTools"], json!(["Bash"]));
}

#[test]
fn test_trust_workspace_skips_writing_when_already_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join(".claude.json");
    std::fs::write(&config_path, json!({"projects": {WORKTREE: {"hasTrustDialogAccepted": true}}}).to_string())
        .unwrap();
    let original = std::fs::metadata(&config_path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));

    trust_workspace(&config_path, Path::new(WORKTREE));

    assert_eq!(std::fs::metadata(&config_path).unwrap().modified().unwrap(), original);
}

#[test]
fn test_trust_workspace_ignores_corrupt_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join(".claude.json");
    std::fs::write(&config_path, "not valid json").unwrap();
    trust_workspace(&config_path, &dir.path().join("worktree")); // must not panic
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "not valid json");
}

/// Python checked that prepare_workspace calls trust_workspace for a trusted repository; here
/// the adapter's home is a temp directory, so the effect is checked on its `.claude.json`.
#[test]
fn test_claude_adapter_prepare_workspace_delegates_to_trust_workspace() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".claude.json"), "{}").unwrap();
    let trusted = config(json!({"type": "claude", "command": "claude",
                                "trustedRepositories": [repo.path().to_string_lossy()]}));
    let adapter = HarnessAdapter::with_home("claude", trusted, home.path().to_path_buf());

    adapter.prepare_workspace(repo.path(), None);

    let key = agentflow_core::harness::as_posix(repo.path());
    assert_eq!(read_json(&home.path().join(".claude.json"))["projects"][key]["hasTrustDialogAccepted"], true);
}

/// A worktree of a trusted repository is trusted, matched however the repository path is written.
#[test]
fn prepare_workspace_trusts_a_worktree_of_a_trusted_repository() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".claude.json"), "{}").unwrap();
    let written = format!("{}{}", repo.path().display().to_string().to_uppercase(), std::path::MAIN_SEPARATOR);
    let listed = if cfg!(windows) { written } else { repo.path().to_string_lossy().into_owned() };
    let trusted = config(json!({"type": "claude", "command": "claude", "trustedRepositories": [listed]}));
    let adapter = HarnessAdapter::with_home("claude", trusted, home.path().to_path_buf());

    adapter.prepare_workspace(worktree.path(), Some(repo.path()));

    let data = read_json(&home.path().join(".claude.json"));
    assert_eq!(data["projects"][agentflow_core::harness::as_posix(worktree.path())]["hasTrustDialogAccepted"], true);
}

// Ported from backend/tests/test_security_improvements.py ------------------------------------------

#[test]
fn test_claude_does_not_implicitly_trust_workspace() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".claude.json"), "{}").unwrap();
    let adapter = HarnessAdapter::with_home(
        "claude",
        config(json!({"type": "claude", "command": "claude"})),
        home.path().to_path_buf(),
    );
    adapter.prepare_workspace(cwd.path(), None);
    assert_eq!(std::fs::read_to_string(home.path().join(".claude.json")).unwrap(), "{}");
}

#[test]
fn test_resume_requires_exact_session() {
    for kind in ["codex", "copilot"] {
        let adapter = adapter(kind, "test");
        assert_eq!(adapter.build_resume(None, None, None, "brief"), None, "{kind}");
    }
}

fn write_session_meta(path: &Path, session_id: &str, cwd: &Path, timestamp: chrono::DateTime<Utc>, source: &str) {
    let line = json!({"type": "session_meta", "payload": {
        "session_id": session_id, "cwd": cwd.to_string_lossy(), "source": source,
        "timestamp": timestamp.to_rfc3339(),
    }});
    std::fs::write(path, format!("{line}\n")).unwrap();
}

const FAST: Polling = Polling { interval: Duration::from_millis(5), attempts: 4 };

#[tokio::test]
async fn test_codex_capture_session_id_matches_cwd_and_launch_time() {
    let home = tempfile::tempdir().unwrap();
    let cwd = home.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    let sessions_dir = home.path().join(".codex").join("sessions").join("2026").join("01").join("01");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let since = Utc::now();
    // A pre-existing rollout for the same cwd (an earlier, unrelated run) must be ignored.
    write_session_meta(&sessions_dir.join("rollout-old.jsonl"), "old-id", &cwd, since - TimeDelta::minutes(5), "cli");
    // The file this exact launch just created.
    write_session_meta(&sessions_dir.join("rollout-new.jsonl"), "new-id", &cwd, since + TimeDelta::seconds(1), "cli");
    let adapter = HarnessAdapter::with_home(
        "codex",
        config(json!({"type": "codex", "command": "codex"})),
        home.path().to_path_buf(),
    );
    assert_eq!(adapter.capture_session_id(&cwd, since).await.as_deref(), Some("new-id"));
}

#[tokio::test]
async fn test_codex_capture_session_id_gives_up_on_concurrent_sibling() {
    // Code review and security review share the same task cwd and can launch close together; an
    // ambiguous match must not risk wiring resume to the wrong role's conversation.
    let home = tempfile::tempdir().unwrap();
    let cwd = home.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    let sessions_dir = home.path().join(".codex").join("sessions").join("2026").join("01").join("01");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let since = Utc::now();
    write_session_meta(&sessions_dir.join("rollout-a.jsonl"), "id-a", &cwd, since + TimeDelta::seconds(1), "cli");
    write_session_meta(&sessions_dir.join("rollout-b.jsonl"), "id-b", &cwd, since + TimeDelta::seconds(2), "cli");
    let root = home.path().join(".codex").join("sessions");
    assert_eq!(capture_session_id(&root, &cwd, since, FAST).await, None);
}

#[tokio::test]
async fn codex_capture_ignores_other_cwds_sources_and_old_days() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    let cwd = home.path().join("work");
    let since = Utc::now();
    let day = |date: &str| {
        let dir = date.split('/').fold(root.clone(), |dir, part| dir.join(part));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let later = since + TimeDelta::seconds(1);
    write_session_meta(&day("2026/02/01").join("other-cwd.jsonl"), "x", &home.path().join("elsewhere"), later, "cli");
    write_session_meta(&day("2026/01/31").join("exec.jsonl"), "y", &cwd, later, "exec");
    // A third day back is never scanned, as in Python (only the two newest day directories).
    write_session_meta(&day("2026/01/30").join("third-day.jsonl"), "z", &cwd, later, "cli");
    assert_eq!(capture_session_id(&root, &cwd, since, FAST).await, None);
    // Differently written, same directory: matched case- and separator-insensitively on Windows.
    let written = if cfg!(windows) {
        PathBuf::from(cwd.to_string_lossy().to_uppercase().replace('\\', "/"))
    } else {
        cwd.clone()
    };
    write_session_meta(&day("2026/01/31").join("match.jsonl"), "match", &written, later, "cli");
    assert_eq!(capture_session_id(&root, &cwd, since, FAST).await.as_deref(), Some("match"));
}

#[tokio::test]
async fn only_codex_captures_session_ids() {
    let home = tempfile::tempdir().unwrap();
    for kind in ["claude", "copilot", "generic-cli"] {
        let adapter =
            HarnessAdapter::with_home("h", config(json!({"type": kind, "command": "x"})), home.path().to_path_buf());
        assert_eq!(adapter.capture_session_id(home.path(), Utc::now()).await, None);
    }
}

#[test]
fn test_copilot_build_launch_sets_session_id_for_resume_and_usage_lookup() {
    // build_launch assigns --session-id both so a usage collector can find this run's session log
    // and so a later run can resume the same conversation with --resume.
    let adapter = adapter("copilot", "copilot");
    let launch = adapter.build_launch(None, None, "brief");
    let id = launch.harness_session_id.clone().expect("a session id");
    assert!(launch.argv.contains(&"--session-id".to_string()));
    let resumed = adapter.build_resume(Some(&id), None, None, "brief").expect("resumable");
    assert!(resumed.argv.contains(&"--resume".to_string()));
    assert!(resumed.argv.contains(&id));
    assert_eq!(resumed.harness_session_id, Some(id));
}

// Ported from backend/tests/test_usage_accounting.py -------------------------------------------------

#[test]
fn test_copilot_screen_states() {
    // Copilot CLI 1.0.89 screens, trimmed from a real pane.
    let idle = " ← open sidebar · / commands · ? help · tab next tab        GitHub Copilot • GPT-5.6 Luna";
    let cases = [
        (idle, ScreenState::Idle),
        (
            " ◉ Working · 141 B esc interrupt                                GitHub Copilot • GPT-5.6 Luna",
            ScreenState::Working,
        ),
        (
            " ◉ Loading: 11 instructions, 9 agents, 2 plugins                GitHub Copilot • GPT-5.6 Luna",
            ScreenState::Working,
        ),
        ("│ ❯ 1. Yes\n│   3. No (Esc)\n│ ↑/↓ to navigate · enter to select · esc to cancel", ScreenState::Blocked),
        // ask_user: the agent is waiting on a human inside the pane, so it must raise attention
        // rather than look idle (its empty input line alone matches the idle pattern).
        (
            "Copilot needs information.\nPaste prompt.md or provide an in-workspace copy path.\n>\n\
             enter accept · ctrl+d decline · esc cancel",
            ScreenState::Blocked,
        ),
    ];
    let copilot = adapter("copilot", "copilot");
    assert_eq!(copilot.kind, HarnessKind::Copilot);
    for (footer, state) in cases {
        let mut screen: Vec<String> = vec![
            r" D:\Dev\AgentFlow [⎇ main]                     Session: 0.63 AIC used".into(),
            "─".repeat(40),
            "❯".into(),
            "─".repeat(40),
        ];
        screen.extend(footer.lines().map(String::from));
        assert_eq!(copilot.classify_screen(&screen), state, "{footer}");
    }
}

#[tokio::test]
async fn detect_reports_a_missing_command_and_a_found_one() {
    let missing = adapter("codex", "agentflow-no-such-harness").detect().await;
    assert!(!missing.available);
    assert_eq!(missing.message.as_deref(), Some("'agentflow-no-such-harness' not found"));
    assert_eq!(missing.display_name, "h");
    // git stands in for a harness CLI: it is on PATH and answers --version.
    let found = adapter("generic-cli", "git").detect().await;
    assert!(found.available, "{found:?}");
    assert!(found.version.as_deref().is_some_and(|v| v.starts_with("git version")), "{found:?}");
    assert!(!found.supports_model && !found.supports_reasoning && !found.supports_resume);
}
