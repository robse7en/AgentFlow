//! Human gates, verification cancellation and persistence boundaries from test_workflow_safety.
mod engine_support;
use agentflow_core::engine::approvals::ApprovalRequest;
use agentflow_core::engine::verification::verify_commands;
use agentflow_core::engine::{rules, spec};
use agentflow_core::vcs::Outcome;
use engine_support::{Harness, SHA, engine};
use serde_json::json;
use std::time::Duration;

fn waiting(h: &Harness) {
    h.with_task(|s| {
        let n = s.node_index(spec::READY_FOR_PR).unwrap();
        s.nodes[n].state = "WAITING_FOR_APPROVAL".into();
    });
}
fn request(reason: Option<&str>, sha: &str) -> ApprovalRequest {
    ApprovalRequest { reason: reason.map(String::from), expected_sha: Some(sha.into()), ..Default::default() }
}

#[tokio::test]
async fn test_dirty_worktree_requires_reason_then_can_be_overridden() {
    let h = engine().await;
    waiting(&h);
    h.git.returns("dirty_paths", Outcome::Ok(json!([" M generated.cs"])));
    let error = h.engine.approve("TEST-1", spec::READY_FOR_PR, request(None, SHA)).await.unwrap_err();
    assert!(error.message.contains("Worktree changed"));
    let snap = h.engine.approve("TEST-1", spec::READY_FOR_PR, request(Some("accept"), SHA)).await.unwrap();
    assert_eq!(snap["status"], "READY_FOR_PR");
}
#[tokio::test]
async fn test_new_commits_since_checkpoint_can_be_overridden_with_reason() {
    let h = engine().await;
    waiting(&h);
    h.git.returns("head_sha", Outcome::Ok(json!("c".repeat(40))));
    assert!(
        h.engine
            .approve("TEST-1", spec::READY_FOR_PR, request(None, SHA))
            .await
            .unwrap_err()
            .message
            .contains("new commit")
    );
    assert_eq!(
        h.engine.approve("TEST-1", spec::READY_FOR_PR, request(Some("accept"), SHA)).await.unwrap()["status"],
        "READY_FOR_PR"
    );
}
#[tokio::test]
async fn test_stale_checkpoint_reference_cannot_be_overridden() {
    let h = engine().await;
    waiting(&h);
    assert!(
        h.engine
            .approve("TEST-1", spec::READY_FOR_PR, request(Some("accept"), &"b".repeat(40)))
            .await
            .unwrap_err()
            .message
            .contains("Checkpoint changed")
    );
    assert_eq!(h.load().task.status, "ACTIVE");
}
#[tokio::test]
async fn test_drift_rejection_carries_a_machine_readable_code() {
    let h = engine().await;
    waiting(&h);
    h.git.returns("dirty_paths", Outcome::Ok(json!([" M generated.cs"])));
    let (status, body) =
        h.route("POST", "/api/tasks/TEST-1/nodes/ready_for_pr/approve", Some(json!({"expectedSha":SHA}))).await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "worktree_drift");
    h.git.returns("dirty_paths", Outcome::Ok(json!([])));
    let (status, body) =
        h.route("POST", "/api/tasks/TEST-1/nodes/ready_for_pr/approve", Some(json!({"expectedSha":SHA}))).await;
    assert_eq!(status, 409);
    assert!(body.get("code").is_none());
}
#[tokio::test]
async fn test_discard_worktree_drift_resets_tracked_changes_only() {
    let h = engine().await;
    h.git.returns("dirty_paths", Outcome::Ok(json!([" M generated.cs"])));
    h.engine.discard_worktree_drift("TEST-1", SHA).await.unwrap();
    assert_eq!(h.git.calls_of("reset_hard")[0][1], SHA);
}
#[tokio::test]
async fn test_discard_worktree_drift_refuses_untracked_files() {
    let h = engine().await;
    h.git.returns("dirty_paths", Outcome::Ok(json!(["?? new.txt"])));
    assert!(h.engine.discard_worktree_drift("TEST-1", SHA).await.unwrap_err().message.contains("Untracked"));
    assert!(h.git.calls_of("reset_hard").is_empty());
}
#[tokio::test]
async fn test_discard_worktree_drift_refuses_when_head_diverged() {
    let h = engine().await;
    h.git.returns("head_sha", Outcome::Ok(json!("c".repeat(40))));
    assert!(h.engine.discard_worktree_drift("TEST-1", SHA).await.unwrap_err().message.contains("new commits"));
    assert!(h.git.calls_of("reset_hard").is_empty());
}
#[tokio::test]
async fn requirements_approval_is_bound_to_the_displayed_digest() {
    let h = engine().await;
    let task_id = h.load().task.id;
    h.engine.db.session(|tx| tx.execute("DELETE FROM requirements_approvals WHERE task_id=?1", [task_id])).unwrap();
    h.with_task(|s| {
        let n = s.node_index(spec::REQUIREMENTS_GATE).unwrap();
        s.nodes[n].state = "WAITING_FOR_APPROVAL".into();
    });
    let docs = agentflow_core::engine::snapshot::workspace(&h.load().task).requirement_files().unwrap();
    let digest = rules::documents_digest(&docs);
    assert!(h.engine.approve("TEST-1", spec::REQUIREMENTS_GATE, ApprovalRequest::default()).await.is_err());
    assert!(
        h.engine
            .approve(
                "TEST-1",
                spec::REQUIREMENTS_GATE,
                ApprovalRequest { expected_requirements_digest: Some("0".repeat(64)), ..Default::default() }
            )
            .await
            .is_err()
    );
    h.engine
        .approve(
            "TEST-1",
            spec::REQUIREMENTS_GATE,
            ApprovalRequest { expected_requirements_digest: Some(digest), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(h.count("requirements_approvals"), 1);
    assert_eq!(h.load().node(spec::IMPLEMENTER).unwrap().state, "READY");
}

#[cfg(windows)]
fn command(script: &str) -> Vec<String> {
    vec!["pwsh".into(), "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script.into()]
}

#[cfg(windows)]
async fn wait_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn test_verification_does_not_hold_the_database_writer_lock() {
    let h = engine().await;
    h.launchable();
    h.git.returns("checkpoint", Outcome::Ok(json!([SHA, true])));
    let marker = h.tmp().join("verifying");
    let script = format!(
        "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 2; Write-Output done",
        marker.display()
    );
    h.configure(|c| c.workflow.verification_commands = vec![command(&script)]);
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let engine = h.engine.clone();
    let completion = tokio::spawn(async move {
        engine
            .agent_complete("TEST-1", "implementer", Some(run), json!({"summary":"done"}).as_object().unwrap().clone())
            .await
    });
    wait_file(&marker).await;
    // An independent writer can commit while verification is still running, and column-level
    // flushing preserves that writer's value when completion later commits.
    h.with_task(|s| s.task.jira_title = Some("Concurrent update".into()));
    assert!(!completion.is_finished());
    assert_eq!(completion.await.unwrap().unwrap()["ok"], true);
    assert_eq!(h.load().task.jira_title.as_deref(), Some("Concurrent update"));
}

#[cfg(windows)]
#[tokio::test]
async fn cancel_stops_verification_before_waiting_for_the_task_lock() {
    let h = engine().await;
    h.launchable();
    h.git.returns("checkpoint", Outcome::Ok(json!([SHA, true])));
    let marker = h.tmp().join("verifying-cancel");
    let script = format!("Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 60", marker.display());
    h.configure(|c| c.workflow.verification_commands = vec![command(&script)]);
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let engine = h.engine.clone();
    let completion =
        tokio::spawn(
            async move { engine.agent_complete("TEST-1", "implementer", Some(run), serde_json::Map::new()).await },
        );
    wait_file(&marker).await;
    let snap = tokio::time::timeout(Duration::from_secs(15), h.engine.cancel_task("TEST-1")).await.unwrap().unwrap();
    assert_eq!(snap["status"], "CANCELLED");
    let response = completion.await.unwrap().unwrap();
    assert_eq!(response["ok"], false);
    assert!(response["error"].as_str().unwrap().contains("verification was cancelled"));
}

#[cfg(windows)]
#[tokio::test]
async fn verification_captures_stderr_and_keeps_the_end_of_large_output() {
    let h = engine().await;
    let commands = vec![command("[Console]::Out.Write('x' * 40000); [Console]::Error.Write('stderr-summary'); exit 1")];
    let (_send, receive) = tokio::sync::watch::channel(false);
    let result = verify_commands(&commands, &h.tmp(), SHA, receive).await.unwrap();
    let output = result["commands"][0]["output"].as_str().unwrap();
    assert!(output.len() <= 16000);
    assert!(output.contains("output truncated"));
    assert!(output.ends_with("stderr-summary"));
    assert_eq!(result["passed"], false);
}

#[tokio::test]
async fn verification_checkpoint_mutation_fails_the_run() {
    let h = engine().await;
    h.git.returns("checkpoint", Outcome::Ok(json!([SHA, true])));
    h.git.returns("is_dirty", Outcome::Ok(json!(true)));
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    let result = h.engine.agent_complete("TEST-1", "implementer", Some(run), serde_json::Map::new()).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(result["error"].as_str().unwrap().contains("Verification changed the checkpoint"));
}

#[cfg(windows)]
#[tokio::test]
async fn cancelling_verification_kills_a_hidden_descendant() {
    let h = engine().await;
    let marker = h.tmp().join("descendant.pid");
    let script = format!(
        "$descendant = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep -Seconds 60' -WindowStyle Hidden -PassThru; Set-Content -LiteralPath '{}' -Value $descendant.Id; Start-Sleep -Seconds 60",
        marker.display()
    );
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let cwd = h.tmp();
    let verifying = tokio::spawn(async move { verify_commands(&[command(&script)], &cwd, SHA, receiver).await });
    // Set-Content creates the file before writing the PID. Wait for a complete value, not just
    // existence; otherwise a fast test reader can observe an empty file.
    let pid: u32 = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(contents) = std::fs::read_to_string(&marker)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    cancel.send_replace(true);
    assert!(tokio::time::timeout(Duration::from_secs(15), verifying).await.unwrap().unwrap().is_err());
    // Wait for the OS to finish terminating the descendant, rather than assuming a signalled
    // process has already disappeared from the process table.
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // SAFETY: the handle, if open, is queried and closed exactly once.
            let alive = unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if handle.is_null() {
                    false
                } else {
                    let mut code = 0;
                    let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == 259;
                    windows_sys::Win32::Foundation::CloseHandle(handle);
                    alive
                }
            };
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn test_full_workflow_with_retry_and_persistent_sessions() {
    let h = engine().await;
    h.launchable();
    h.git.returns("checkpoint", Outcome::Ok(json!([SHA, true])));
    h.configure(|c| c.workflow.verification_commands = vec![command("Write-Output verified")]);
    let task_id = h.load().task.id;
    h.engine.db.session(|tx| tx.execute("DELETE FROM requirements_approvals WHERE task_id=?1", [task_id])).unwrap();
    async fn start(h: &Harness, key: &str) -> i64 {
        h.engine.run_node("TEST-1", key, Default::default(), false).await.unwrap();
        h.load().runs.last().unwrap().id
    }
    async fn finish(h: &Harness, role: &str, run: i64, data: serde_json::Value) {
        let response =
            h.engine.agent_complete("TEST-1", role, Some(run), data.as_object().unwrap().clone()).await.unwrap();
        assert_eq!(response["ok"], true, "{response}");
    }
    let discovery = start(&h, spec::ARCHITECT).await;
    finish(&h, "architect", discovery, json!({"status":"completed"})).await;
    assert_eq!(h.load().node(spec::IMPLEMENTER).unwrap().state, "PENDING");
    let documents = agentflow_core::engine::snapshot::workspace(&h.load().task).requirement_files().unwrap();
    h.engine
        .approve(
            "TEST-1",
            spec::REQUIREMENTS_GATE,
            ApprovalRequest {
                expected_requirements_digest: Some(rules::documents_digest(&documents)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.with_task(|s| s.task.max_review_iterations = 3);
    for iteration in [1, 2] {
        let implementation = start(&h, spec::IMPLEMENTER).await;
        let mut result = json!({"status":"completed", "tests":{"executed":1,"passed":1,"failed":0}});
        if iteration == 2 {
            result["findingResponses"] = json!([
                {"id":"CODE-001","action":"fixed","rationale":"Guarded src/app.py"},
                {"id":"SEC-001","action":"fixed","rationale":"Guarded src/app.py"},
            ]);
        }
        finish(&h, "implementer", implementation, result).await;
        for (key, role, id) in
            [(spec::CODE_REVIEW, "code_reviewer", "CODE-001"), (spec::SECURITY_REVIEW, "security_reviewer", "SEC-001")]
        {
            let mut run = start(&h, key).await;
            if iteration == 1 && key == spec::CODE_REVIEW {
                finish(&h, role, run, json!({"reviewedSha":"a"})).await;
                assert_eq!(h.load().node(key).unwrap().state, "FAILED");
                run = start(&h, key).await;
            }
            let mut review =
                json!({"reviewedSha":SHA,"verdict":if iteration == 1 {"changes_required"} else {"approved"}});
            if iteration == 1 {
                review["findings"] = json!([{"title":"Bug","severity":"high","blocking":true}]);
            } else {
                review["priorFindings"] = json!([{"id":id,"status":"fixed","evidence":"src/app.py:12 guard"}]);
            }
            finish(&h, role, run, review).await;
        }
    }
    assert_eq!(h.load().node(spec::FINAL_ARCHITECT).unwrap().state, "READY");
    let final_run = start(&h, spec::FINAL_ARCHITECT).await;
    finish(&h,"architect",final_run,json!({"taskSummary":"Done","testsPerformed":"one passing test","requirementsVerified":true,"overallRisk":"LOW"})).await;
    assert_eq!(
        h.engine.approve("TEST-1", spec::READY_FOR_PR, request(None, SHA)).await.unwrap()["status"],
        "READY_FOR_PR"
    );
}
