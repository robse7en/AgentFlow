//! TaskWorkspace against the bytes the Python backend writes on this platform
//! (contract/golden/workspace/<platform>, recorded by contract/record_python_goldens.py).

use std::path::{Path, PathBuf};

use agentflow_core::workspace::{TaskWorkspace, write_credential};
use serde_json::json;

fn golden(name: &str) -> Vec<u8> {
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/workspace").join(platform).join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (record it on this platform)", path.display()))
}

#[test]
fn writes_the_same_bytes_as_python() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = TaskWorkspace::new(dir.path(), "WS-1");
    let meta =
        json!({"jiraKey": "WS-1", "branch": "agent/WS-1", "créé": "ünïcode", "nested": {"n": [1, 2.5, null, true]}});
    workspace.create(&meta, Some("  Some context\nwith two lines  ")).unwrap();
    write_credential(&workspace.root.join("credential.json"), &json!({"runId": 7, "token": "t0k€n"})).unwrap();

    assert_eq!(std::fs::read(workspace.root.join("task.json")).unwrap(), golden("task.json"));
    assert_eq!(
        std::fs::read(workspace.context_dir.join("additional-context.md")).unwrap(),
        golden("additional-context.md")
    );
    assert_eq!(std::fs::read(workspace.root.join("credential.json")).unwrap(), golden("credential.json"));
    for dir in [&workspace.requirements_dir, &workspace.runs_dir, &workspace.findings_dir, &workspace.final_dir] {
        assert!(dir.is_dir());
    }
}

#[test]
fn a_credential_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credential.json");
    write_credential(&path, &json!({"runId": 1, "token": "a"})).unwrap();
    assert!(write_credential(&path, &json!({"runId": 1, "token": "b"})).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"runId": 1, "token": "a"}"#);
}

#[test]
fn run_directories_are_numbered_with_two_digits() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = TaskWorkspace::new(dir.path(), "WS-1");
    let run = workspace.run_dir("code-review", 3).unwrap();
    assert!(run.is_dir());
    assert!(run.ends_with(Path::new("runs").join("code-review").join("03")));
}

#[test]
fn requirement_documents_read_with_universal_newlines_in_display_order() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = TaskWorkspace::new(dir.path(), "WS-1");
    workspace.create(&json!({}), None).unwrap();
    // An agent on Windows may write CRLF; the digest is computed over what Python read: LF.
    std::fs::write(workspace.requirements_dir.join("requirements.md"), "# R\r\n\r\n- one\r\n").unwrap();
    std::fs::write(workspace.requirements_dir.join("risks.md"), "none\n").unwrap();
    let files = workspace.requirement_files().unwrap();
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        ["requirements", "acceptanceCriteria", "assumptions", "implementationPlan", "risks"]
    );
    assert_eq!(files["requirements"], "# R\n\n- one\n");
    assert_eq!(files["acceptanceCriteria"], "");
    assert_eq!(files["risks"], "none\n");
}

#[test]
fn feedback_rounds_are_listed_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = TaskWorkspace::new(dir.path(), "WS-1");
    workspace.create(&json!({}), None).unwrap();
    std::fs::write(workspace.requirements_dir.join("feedback-2.md"), " second \n").unwrap();
    std::fs::write(workspace.requirements_dir.join("feedback-1.md"), "first").unwrap();
    std::fs::write(workspace.requirements_dir.join("feedback-x.md"), "ignored").unwrap();
    let rounds = workspace.requirements_feedback().unwrap();
    assert_eq!(
        rounds.iter().map(|r| (r["iteration"].as_i64().unwrap(), r["feedback"].as_str().unwrap())).collect::<Vec<_>>(),
        [(1, "first"), (2, "second")]
    );
    assert!(rounds[0]["at"].as_str().unwrap().ends_with("+00:00"));
}

#[test]
fn jira_issue_is_none_before_the_first_sync() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = TaskWorkspace::new(dir.path(), "WS-1");
    workspace.create(&json!({}), None).unwrap();
    assert_eq!(workspace.jira_issue().unwrap(), None);
    std::fs::write(workspace.context_dir.join("jira-issue.md"), "# WS-1\r\n").unwrap();
    assert_eq!(workspace.jira_issue().unwrap(), Some(json!({"markdown": "# WS-1\n", "syncedAt": null})));
    std::fs::write(workspace.context_dir.join("jira-manifest.json"), r#"{"syncedAt": "2026-10-02T00:00:00+00:00"}"#)
        .unwrap();
    assert_eq!(workspace.jira_issue().unwrap().unwrap()["syncedAt"], "2026-10-02T00:00:00+00:00");
}
