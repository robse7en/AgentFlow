//! `flowctl complete` and `question`, ported by name from backend/tests/test_flowctl_complete.py
//! and test_questions_terminal.py. Python called the command functions with `_post` mocked; these
//! run the real binary against a stub backend on a local port, which also checks what is sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// A backend that answers each request with the next canned (status, body) and records bodies.
struct Stub {
    api: String,
    received: Arc<Mutex<Vec<Value>>>,
}

fn stub(responses: Vec<(u16, Value)>) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = received.clone();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut request = vec![0u8; length];
            reader.read_exact(&mut request).unwrap();
            log.lock().unwrap().push(serde_json::from_slice(&request).unwrap_or(Value::Null));
            let text = body.to_string();
            let mut stream = reader.into_inner();
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                text.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(text.as_bytes());
        }
    });
    Stub { api, received }
}

fn accepted() -> (u16, Value) {
    (200, json!({"state": "COMPLETED", "taskStatus": "ACTIVE"}))
}

/// Run flowctl as an architect (or `agent`) on run 1, with a credential in `run_dir` when given.
fn flowctl(api: &str, agent: &str, run_dir: Option<&Path>, args: &[&str], stdin: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flowctl"));
    command
        .args(args)
        .env_remove("AGENTFLOW_CREDENTIAL_FILE")
        .env("AGENTFLOW_API", api)
        .env("AGENTFLOW_TASK", "DE-1")
        .env("AGENTFLOW_AGENT", agent)
        .env("AGENTFLOW_RUN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = run_dir {
        let credential = dir.join("credential.json");
        std::fs::write(&credential, r#"{"token": "t", "runId": 1}"#).unwrap();
        command.env("AGENTFLOW_CREDENTIAL_FILE", &credential);
    }
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn sent_result(stub: &Stub) -> Value {
    stub.received.lock().unwrap()[0]["result"].clone()
}

#[test]
fn test_flowctl_finding_can_reraise_a_prior_finding() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![(200, json!({"count":1}))]);
    let output = flowctl(
        &backend.api,
        "code_reviewer",
        Some(dir.path()),
        &["finding", "--title", "Null deref", "--blocking", "--prior-id", "CODE-001"],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let sent = backend.received.lock().unwrap();
    assert_eq!(sent[0]["finding"]["priorId"], "CODE-001");
    assert_eq!(sent[0]["finding"]["blocking"], true);
}

/// The usual path for a short result: piped in with a heredoc, never written to disk.
#[test]
fn test_complete_reads_json_from_stdin_when_file_is_dash() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![accepted()]);
    let output = flowctl(
        &backend.api,
        "architect",
        Some(dir.path()),
        &["complete", "--file", "-"],
        r#"{"status": "completed", "summary": "done"}"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sent_result(&backend), json!({"status": "completed", "summary": "done"}));
}

#[test]
fn test_complete_exits_cleanly_on_invalid_json_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let output = flowctl("http://127.0.0.1:9", "architect", Some(dir.path()), &["complete", "--file", "-"], "not json");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("not valid JSON"), "{}", stderr(&output));
}

/// A work plan has no status field and the backend forbids extra fields.
#[test]
fn test_complete_does_not_inject_status_for_orchestrator_work_plans() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![accepted()]);
    let output = flowctl(
        &backend.api,
        "orchestrator",
        Some(dir.path()),
        &["complete", "--file", "-"],
        r#"{"summary": "done", "subtasks": []}"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sent_result(&backend), json!({"summary": "done", "subtasks": []}));
}

#[test]
fn test_complete_file_path_still_works_alongside_the_new_dash_convention() {
    let dir = tempfile::tempdir().unwrap();
    let result = dir.path().join("result.json");
    std::fs::write(&result, r#"{"status": "completed"}"#).unwrap();
    let backend = stub(vec![accepted()]);
    let output =
        flowctl(&backend.api, "architect", Some(dir.path()), &["complete", "--file", result.to_str().unwrap()], "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sent_result(&backend), json!({"status": "completed"}));
}

/// A transport failure never reaches the response, so the durability file survives it: that is
/// the file AgentFlow offers to submit after its next restart.
#[test]
fn test_complete_keeps_pending_file_when_backend_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let output = flowctl(
        &format!("http://{closed}"),
        "architect",
        Some(dir.path()),
        &["complete", "--file", "-"],
        r#"{"status": "completed", "summary": "done"}"#,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("cannot reach"), "{}", stderr(&output));
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("pending-complete.json")).unwrap()).unwrap();
    assert_eq!(saved, json!({"role": "architect", "runId": 1, "result": {"status": "completed", "summary": "done"}}));
}

/// A rejection (e.g. a stale run) still means the backend decided; nothing is left to replay.
#[test]
fn test_complete_clears_pending_file_once_backend_responds_even_with_a_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![(409, json!({"detail": "Stale run; signal rejected"}))]);
    let output = flowctl(
        &backend.api,
        "architect",
        Some(dir.path()),
        &["complete", "--file", "-"],
        r#"{"status": "completed", "summary": "done"}"#,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("rejected"), "{}", stderr(&output));
    assert!(!dir.path().join("pending-complete.json").exists());
}

#[test]
fn test_complete_clears_pending_file_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![accepted()]);
    let output = flowctl(
        &backend.api,
        "architect",
        Some(dir.path()),
        &["complete", "--file", "-"],
        r#"{"status": "completed", "summary": "done"}"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!dir.path().join("pending-complete.json").exists());
}

/// Without a credential file there is no run directory to keep a result in. (Python's test mocked
/// the request; the real command also refuses to send without the run's credential.)
#[test]
fn test_complete_skips_durability_write_without_a_credential_file() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![accepted()]);
    let output = Command::new(env!("CARGO_BIN_EXE_flowctl"))
        .args(["complete", "--json", r#"{"summary": "done"}"#])
        .env_remove("AGENTFLOW_CREDENTIAL_FILE")
        .env("AGENTFLOW_API", &backend.api)
        .env("AGENTFLOW_TASK", "DE-1")
        .env("AGENTFLOW_AGENT", "architect")
        .env("AGENTFLOW_RUN", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("scoped credential file"), "{}", stderr(&output));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing written");
    assert!(backend.received.lock().unwrap().is_empty());
}

fn submission(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("submission.json");
    std::fs::write(&path, r#"{"summary": "plan", "subtasks": []}"#).unwrap();
    path
}

/// A result too long for a heredoc goes through submission.json; once AgentFlow has it, nothing
/// is left for the file to do.
#[test]
fn test_complete_deletes_the_run_dir_submission_file_once_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let file = submission(dir.path());
    let backend = stub(vec![accepted()]);
    let output =
        flowctl(&backend.api, "orchestrator", Some(dir.path()), &["complete", "--file", file.to_str().unwrap()], "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sent_result(&backend), json!({"summary": "plan", "subtasks": []}));
    assert!(!file.exists());
}

/// The agent fixes the file and runs the same command again.
#[test]
fn test_complete_keeps_the_submission_file_when_the_result_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = submission(dir.path());
    let backend = stub(vec![(
        200,
        json!({"error": "subtask api has no owned paths", "state": "RUNNING", "taskStatus": "ACTIVE"}),
    )]);
    let output =
        flowctl(&backend.api, "orchestrator", Some(dir.path()), &["complete", "--file", file.to_str().unwrap()], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("REJECTED"), "{}", stderr(&output));
    assert!(file.exists());
}

#[test]
fn test_complete_never_deletes_any_other_file_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    submission(dir.path());
    let other = dir.path().join("package.json");
    std::fs::write(&other, r#"{"name": "app"}"#).unwrap();
    let backend = stub(vec![accepted()]);
    let output =
        flowctl(&backend.api, "architect", Some(dir.path()), &["complete", "--file", other.to_str().unwrap()], "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(other.exists());
}

#[test]
fn test_flowctl_question_accepts_repeatable_choice_options() {
    let dir = tempfile::tempdir().unwrap();
    let backend = stub(vec![(200, json!({"ok": true, "state": "WAITING_FOR_USER", "runId": 1}))]);
    let args = ["question", "--message", "Which rollout?", "--choice", "Gradual", "--choice", "All at once"];
    let output = flowctl(&backend.api, "architect", Some(dir.path()), &args, "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(backend.received.lock().unwrap()[0]["choices"], json!(["Gradual", "All at once"]));
}
