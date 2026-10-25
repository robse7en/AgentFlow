//! Starting the `agentflow` binary, ported from backend/tests/test_startup_jira.py (the uvicorn
//! and --reload cases have no counterpart: there is one server and no reload worker).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CLOUD: &str = r#"{"enabled": true, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t"}"#;

fn config_file(dir: &Path, jira: Option<&str>) -> std::path::PathBuf {
    let data = dir.join("data");
    // The server runs every configured harness's --version at startup: a harness that is never
    // installed keeps the user's real CLIs out of the test.
    let agent = serde_json::json!({"harness": "stub"});
    let mut raw = serde_json::json!({
        "app": {"dataDirectory": data, "databasePath": data.join("agentflow.db")},
        "workspace": {"worktreeDirectory": data.join("worktrees"), "taskDirectory": data.join("tasks")},
        "harnesses": {"stub": {"type": "generic-cli", "command": "agentflow-test-not-installed"}},
        "agents": {"architect": agent, "implementer": agent, "codeReviewer": agent, "securityReviewer": agent},
    });
    if let Some(jira) = jira {
        raw["jira"] = serde_json::from_str(jira).unwrap();
    }
    let path = dir.join("config.json");
    std::fs::write(&path, raw.to_string()).unwrap();
    path
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn agentflow(config: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentflow"));
    command.arg("--config").arg(config).args(extra).env_remove("AGENTFLOW_SKIP_JIRA_CHECK");
    command
}

/// Start the binary and wait for its pairing line; true when it started serving.
fn starts(mut command: Command) -> bool {
    let port = free_port();
    let mut child =
        command.args(["--port", &port.to_string()]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.starts_with("AgentFlow browser pairing code: ") {
                let _ = sender.send(());
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut started = false;
    while Instant::now() < deadline {
        if receiver.try_recv().is_ok() {
            started = true;
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    started
}

#[test]
fn test_backend_starts_without_jira() {
    let dir = tempfile::tempdir().unwrap();
    assert!(starts(agentflow(&config_file(dir.path(), None), &[])));
}

#[test]
fn test_backend_starts_with_jira_configured() {
    let dir = tempfile::tempdir().unwrap();
    assert!(starts(agentflow(&config_file(dir.path(), Some(CLOUD)), &[])));
}

#[test]
fn test_skip_flag_starts() {
    let dir = tempfile::tempdir().unwrap();
    assert!(starts(agentflow(&config_file(dir.path(), None), &["--skip-jira-check"])));
}

#[test]
fn test_the_environment_variable_skips_the_check() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = agentflow(&config_file(dir.path(), None), &[]);
    command.env("AGENTFLOW_SKIP_JIRA_CHECK", "1");
    assert!(starts(command));
}

#[test]
fn an_invalid_config_is_a_configuration_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"server": {"host": "0.0.0.0"}}"#).unwrap();
    let output = agentflow(&path, &["--skip-jira-check"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("Configuration error:"));
}
