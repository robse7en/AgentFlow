//! config.rs against what the Python backend accepts, rejects and writes
//! (contract/golden/config, recorded by contract/record_python_goldens.py).

use std::path::{Path, PathBuf};

use agentflow_core::config::{Config, Locations, Role, fresh_config, load_config, read_config};
use serde_json::Value;

fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/config").join(name)
}

/// Golden text with `\n` line ends (Python on Windows and git's autocrlf may have written `\r\n`).
fn read(name: &str) -> String {
    std::fs::read_to_string(golden(name)).unwrap_or_else(|e| panic!("{name}: {e}")).replace("\r\n", "\n")
}

/// "line N: python `...` / rust `...`" for the first line that differs.
fn first_difference(python: &str, rust: &str) -> Option<String> {
    let (mut p, mut r) = (python.lines(), rust.lines());
    for number in 1.. {
        match (p.next(), r.next()) {
            (None, None) => return None,
            (a, b) if a == b => continue,
            (a, b) => {
                return Some(format!(
                    "line {number}: python `{}` / rust `{}`",
                    a.unwrap_or("<end>"),
                    b.unwrap_or("<end>")
                ));
            }
        }
    }
    unreachable!()
}

fn parse(raw: Value) -> Result<Config, String> {
    let config = Config::from_value(raw).map_err(|problems| problems.join("; "))?;
    config.validate_references().map_err(|e| e.to_string())?;
    Ok(config)
}

#[test]
fn every_recorded_case_is_accepted_or_rejected_like_python() {
    let cases: Vec<Value> = serde_json::from_str(&read("cases.json")).unwrap();
    assert!(cases.len() >= 30, "expected the recorded cases, found {}", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let outcome = parse(case["input"].clone());
        match (case.get("dump"), outcome) {
            (Some(expected), Ok(config)) => {
                // Text comparison: key order and number formatting are part of what is written.
                let actual = pyjson::dumps(&serde_json::to_value(&config).unwrap());
                if let Some(difference) = first_difference(&pyjson::dumps(expected), &actual) {
                    failures.push(format!("{name}: dump differs at {difference}"));
                }
            }
            (Some(_), Err(error)) => failures.push(format!("{name}: Python accepts it, Rust rejects it: {error}")),
            (None, Ok(_)) => {
                failures.push(format!("{name}: Python rejects it ({}), Rust accepts it", case["pythonError"]))
            }
            (None, Err(_)) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn rejections_name_the_offending_field() {
    let cases: Vec<Value> = serde_json::from_str(&read("cases.json")).unwrap();
    for (name, field) in [
        ("reject-unknown-nested", "colour"),
        ("reject-range", "maxConcurrentAgents"),
        ("reject-slack-digest", "digestAt"),
        ("reject-env-name", "env"),
        ("reject-orchestrator-reference", "agents.orchestrator.harness"),
    ] {
        let case = cases.iter().find(|c| c["name"] == name).unwrap();
        let error = parse(case["input"].clone()).unwrap_err();
        assert!(error.contains(field), "{name}: {error}");
    }
}

#[test]
fn first_launch_writes_the_same_file_as_python() {
    assert_eq!(first_difference(&read("defaults.json"), &(Config::default().to_json() + "\n")), None);
    let checkout = Locations {
        home: PathBuf::from("/home/u/.agentflow"),
        home_overridden: false,
        checkout: Some(PathBuf::from("/src/AgentFlow")),
        config_override: None,
    };
    assert_eq!(fresh_config(&checkout).to_json() + "\n", read("fresh-checkout.json"));
    // A separate instance under $AGENTFLOW_HOME keeps the defaults, so it never shares checkout data.
    let separate = Locations { home_overridden: true, ..checkout };
    assert_eq!(fresh_config(&separate), Config::default());
}

#[test]
fn reads_the_example_config() {
    let config = read_config(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config.example.json")).unwrap();
    assert_eq!(config.agents.for_role(Role::Orchestrator), config.agents.for_role(Role::Architect));
}

#[test]
fn invalid_json_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, "{ nope").unwrap();
    let error = read_config(&path).unwrap_err().to_string();
    assert!(error.starts_with(&format!("{} is not valid JSON", path.display())), "{error}");
}

#[test]
fn config_path_prefers_override_then_home_then_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&checkout).unwrap();
    let at = Locations {
        home: home.clone(),
        home_overridden: false,
        checkout: Some(checkout.clone()),
        config_override: None,
    };

    // Neither file exists: a checkout keeps its config with the project.
    assert_eq!(at.config_path(), checkout.join("config.json"));
    // Only the old home config exists: it is still used.
    std::fs::write(home.join("config.json"), "{}").unwrap();
    assert_eq!(at.config_path(), home.join("config.json"));
    // Once the project has one, the project's wins.
    std::fs::write(checkout.join("config.json"), "{}").unwrap();
    assert_eq!(at.config_path(), checkout.join("config.json"));

    let overridden = Locations { home_overridden: true, ..at.clone() };
    assert_eq!(overridden.config_path(), home.join("config.json"));
    let explicit = Locations { config_override: Some(dir.path().join("x.json")), ..at };
    assert_eq!(explicit.config_path(), dir.path().join("x.json"));
}

/// A real config.json reads, and dumps to the same text Python's model_dump does. Compared by
/// hash, so secrets in the file never reach the test output:
///     python -c "import hashlib,json,sys; from agentflow.config import read_config; from pathlib import Path;
///       print(hashlib.sha256(json.dumps(read_config(Path(sys.argv[1])).model_dump(by_alias=True), indent=2).encode()).hexdigest())" <file>
///     AGENTFLOW_REAL_CONFIG=<file> AGENTFLOW_REAL_CONFIG_SHA256=<hash> cargo test -p agentflow-core --test config -- --ignored
#[test]
#[ignore = "needs AGENTFLOW_REAL_CONFIG and AGENTFLOW_REAL_CONFIG_SHA256"]
fn reads_a_real_config_like_python() {
    use sha2::{Digest, Sha256};
    let path = PathBuf::from(std::env::var("AGENTFLOW_REAL_CONFIG").expect("AGENTFLOW_REAL_CONFIG"));
    let config = read_config(&path).unwrap_or_else(|e| panic!("{}", e.to_string().lines().next().unwrap_or_default()));
    let digest: String = Sha256::digest(config.to_json().as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest, std::env::var("AGENTFLOW_REAL_CONFIG_SHA256").expect("AGENTFLOW_REAL_CONFIG_SHA256"));
}

/// A real config.json Python rejects is rejected at the same field. Only field paths are shown:
///     AGENTFLOW_REAL_CONFIG=<file> AGENTFLOW_REAL_CONFIG_REJECTED_AT=slack.command cargo test ... -- --ignored
#[test]
#[ignore = "needs AGENTFLOW_REAL_CONFIG and AGENTFLOW_REAL_CONFIG_REJECTED_AT"]
fn rejects_a_real_config_like_python() {
    let path = PathBuf::from(std::env::var("AGENTFLOW_REAL_CONFIG").expect("AGENTFLOW_REAL_CONFIG"));
    let field = std::env::var("AGENTFLOW_REAL_CONFIG_REJECTED_AT").expect("AGENTFLOW_REAL_CONFIG_REJECTED_AT");
    let error = read_config(&path).expect_err("Python rejects this file, so Rust must too").to_string();
    let fields: Vec<&str> = error.lines().skip(1).map(|line| line.split(':').next().unwrap_or_default()).collect();
    assert_eq!(fields, [field.as_str()]);
}

#[test]
fn relative_paths_resolve_against_the_checkout_or_home() {
    let at = Locations {
        home: PathBuf::from("/h/.agentflow"),
        home_overridden: false,
        checkout: Some(PathBuf::from("/src/AgentFlow")),
        config_override: None,
    };
    assert_eq!(at.resolve("data/.agentflow/tasks"), Path::new("/src/AgentFlow").join("data/.agentflow/tasks"));
    let installed = Locations { checkout: None, ..at };
    assert_eq!(installed.resolve("tasks"), Path::new("/h/.agentflow").join("tasks"));
    let absolute = std::env::temp_dir().join("abs");
    assert_eq!(installed.resolve(absolute.to_str().unwrap()), absolute);
    assert_eq!(installed.resolve("~/x"), std::env::home_dir().unwrap().join("x"));
}

// Ported from backend/tests/test_startup_jira.py (the environment-variable and binary cases are in
// crates/server/tests/startup.rs, which runs them in processes of their own).

fn jira(fields: Value) -> Config {
    Config::from_value(serde_json::json!({"jira": fields})).unwrap()
}

#[test]
fn test_configured_jira_passes() {
    use agentflow_core::config::jira_startup_problem;
    if std::env::var("AGENTFLOW_SKIP_JIRA_CHECK").is_ok() {
        return; // the check is skipped altogether
    }
    let cloud =
        serde_json::json!({"enabled": true, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t"});
    let server = serde_json::json!({"enabled": true, "baseUrl": "https://jira.example.com", "apiToken": "pat"});
    assert_eq!(jira_startup_problem(&jira(cloud)), None);
    assert_eq!(jira_startup_problem(&jira(server)), None);
}

#[test]
fn test_missing_jira_settings_are_named() {
    use agentflow_core::config::jira_startup_problem;
    if std::env::var("AGENTFLOW_SKIP_JIRA_CHECK").is_ok() {
        return;
    }
    let problem =
        jira_startup_problem(&jira(serde_json::json!({"enabled": true, "baseUrl": "https://x.atlassian.net"})))
            .unwrap();
    assert!(problem.contains("jira.apiToken") && problem.contains("agentflow setup"), "{problem}");
}

// ------------------------------------------------------------------------------ test_contracts.py
// Locations are built directly where Python patched the environment and the package path.

fn checkout_at(tmp: &Path) -> Locations {
    Locations {
        home: tmp.join("user").join(".agentflow"),
        home_overridden: false,
        checkout: Some(tmp.join("AgentFlow")),
        config_override: None,
    }
}

#[test]
fn test_example_config_is_valid() {
    let config = read_config(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config.example.json")).unwrap();
    config.validate_references().unwrap();
    assert_eq!(config.agents.implementer.harness, "codex");
}

#[test]
fn test_unknown_harness_reference_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"agents": {"architect": {"harness": "nope"}}}"#).unwrap();
    assert!(load_config(&path, &checkout_at(dir.path())).is_err());
}

#[test]
fn test_unknown_key_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"workflow": {"maxReviewIteration": 3}}"#).unwrap();
    assert!(load_config(&path, &checkout_at(dir.path())).is_err());
}

#[test]
fn test_default_config_written_on_first_launch_keeps_data_in_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let at = checkout_at(dir.path());
    let path = dir.path().join("config.json");
    let config = load_config(&path, &at).unwrap();
    assert!(path.exists() && config.workflow.max_review_iterations == 5);
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        raw["app"],
        serde_json::json!({"dataDirectory": "data/.agentflow", "databasePath": "data/.agentflow/agentflow.db"})
    );
    assert_eq!(raw["workspace"]["worktreeDirectory"], "data/.agentflow/worktrees");
    let data = dir.path().join("AgentFlow").join("data/.agentflow");
    assert_eq!((config.data_dir(&at), config.database_file(&at)), (data.clone(), data.join("agentflow.db")));
    assert_eq!((config.worktree_dir(&at), config.task_dir(&at)), (data.join("worktrees"), data.join("tasks")));
    assert!(config.worktree_dir(&at).is_dir() && config.task_dir(&at).is_dir());
}

#[test]
fn test_installed_copies_keep_data_in_the_home_directory() {
    let dir = tempfile::tempdir().unwrap();
    let installed = Locations { checkout: None, ..checkout_at(dir.path()) };
    assert_eq!(fresh_config(&installed).app.data_directory, "~/.agentflow");
}

#[test]
fn test_an_agentflow_home_instance_does_not_share_the_checkout_data() {
    let dir = tempfile::tempdir().unwrap();
    let other = Locations { home: dir.path().join("other"), home_overridden: true, ..checkout_at(dir.path()) };
    assert_eq!(fresh_config(&other).app.data_directory, "~/.agentflow");
}

#[test]
fn test_relative_paths_are_relative_to_the_agentflow_folder_not_the_current_directory() {
    let dir = tempfile::tempdir().unwrap();
    let at = checkout_at(dir.path());
    let config = Config::from_value(serde_json::json!({"app": {"dataDirectory": "data/x"}})).unwrap();
    assert_eq!(config.data_dir(&at), dir.path().join("AgentFlow").join("data/x"));
    // A file that omits it keeps the old default.
    assert_eq!(Config::default().data_dir(&at), std::env::home_dir().unwrap().join(".agentflow"));
}

#[test]
fn test_project_config_is_default_in_source_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let at = checkout_at(dir.path());
    std::fs::create_dir_all(at.checkout.as_ref().unwrap()).unwrap();
    std::fs::write(at.checkout.as_ref().unwrap().join("config.json"), "{}").unwrap();
    assert_eq!(at.config_path(), dir.path().join("AgentFlow").join("config.json"));
}

#[test]
fn test_a_fresh_checkout_puts_config_in_the_agentflow_folder() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(checkout_at(dir.path()).config_path(), dir.path().join("AgentFlow").join("config.json"));
}

#[test]
fn test_an_existing_home_config_is_still_used_until_the_checkout_has_one() {
    let dir = tempfile::tempdir().unwrap();
    let at = checkout_at(dir.path());
    let home_config = at.home.join("config.json");
    std::fs::create_dir_all(&at.home).unwrap();
    std::fs::write(&home_config, "{}").unwrap();
    std::fs::create_dir_all(at.checkout.as_ref().unwrap()).unwrap();
    assert_eq!(at.config_path(), home_config);
    std::fs::write(at.checkout.as_ref().unwrap().join("config.json"), "{}").unwrap();
    assert_eq!(at.config_path(), dir.path().join("AgentFlow").join("config.json"));
}

#[test]
fn test_installed_copies_keep_config_in_the_home_directory() {
    let dir = tempfile::tempdir().unwrap();
    let installed = Locations { checkout: None, ..checkout_at(dir.path()) };
    assert_eq!(installed.config_path(), dir.path().join("user").join(".agentflow").join("config.json"));
}

#[test]
fn test_explicit_config_environment_variable_wins() {
    let dir = tempfile::tempdir().unwrap();
    let expected = dir.path().join("custom.json");
    let at = Locations {
        home: dir.path().join("home"),
        home_overridden: true,
        config_override: Some(expected.clone()),
        ..checkout_at(dir.path())
    };
    assert_eq!(at.config_path(), expected);
}

#[test]
fn test_agentflow_home_overrides_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let at = Locations { home: home.clone(), home_overridden: true, ..checkout_at(dir.path()) };
    assert_eq!(at.config_path(), home.join("config.json"));
}

/// Python's test_questions_terminal.py: with no client zellij makes a session 50 columns wide, so
/// the headless width must stay above that for the sizing client to be seen.
#[test]
fn test_headless_width_must_stay_above_zellijs_clientless_default() {
    assert!(Config::from_value(serde_json::json!({"zellij": {"headlessCols": 50}})).is_err());
    assert!(Config::from_value(serde_json::json!({"zellij": {"headlessCols": 60}})).is_ok());
}

/// A configuration written out and read back is the same configuration (Python's model equality
/// compares 90 and 90.0 as equal), so a reload of an unchanged file never asks for a restart.
#[test]
fn a_written_configuration_reads_back_equal() {
    let config = Config::default();
    let again = Config::from_value(serde_json::from_str(&config.to_json()).unwrap()).unwrap();
    assert_eq!(config, again);
}

#[test]
fn disabled_jira_allows_local_use() {
    assert!(agentflow_core::config::jira_startup_problem(&Config::default()).is_none());
}
