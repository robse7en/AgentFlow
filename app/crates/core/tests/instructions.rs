//! Repository instruction safety, actual engine launch branches, and operator API contracts.
mod engine_support;

use agentflow_core::{
    db::{DbTime, Json, WorkflowNodeRow},
    engine::{
        instruction_ui::{ApplyUpdate, PreviewQuery},
        launch::RunNodeRequest,
        spec,
    },
    instructions as guidance,
};
use engine_support::{Harness, engine};
use serde_json::json;
use std::{fs, path::Path};

fn sources(root: &Path) {
    for role in guidance::ROLES {
        guidance::save_source(root, role, &format!("Unique {role}\nflowctl complete\n<script>text</script>\n"), None)
            .unwrap();
    }
}

#[test]
fn optional_sources_exact_composition_and_role_separation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = root.join("task");
    let snapshot = guidance::capture(&task, root, "revision", false).unwrap();
    for role in guidance::ROLES {
        assert_eq!(
            guidance::compose("flowctl complete\n", &snapshot, role, 42).unwrap(),
            format!("flowctl complete --run 42\n{}", guidance::RUN_NOTICE)
        );
    }
    sources(root);
    let updated = guidance::capture(&task, root, "revision2", false).unwrap();
    let mut corrupted = updated.clone();
    corrupted.files.get_mut("architect").unwrap().content = Some("tampered".into());
    assert!(guidance::validate_snapshot(&corrupted).is_err());
    for role in guidance::ROLES {
        let text = guidance::compose("flowctl complete\n", &updated, role, 42).unwrap();
        assert!(text.ends_with(updated.files[role].content.as_deref().unwrap()));
        assert_eq!(text.matches("flowctl complete --run 42").count(), 1);
        assert_eq!(text.matches("Unique ").count(), 1);
    }
    assert_ne!(snapshot.version, updated.version);
    assert!(task.join("instructions").join(format!("{}.json", snapshot.version)).exists());
    assert_eq!(guidance::active_snapshot(&task).unwrap().unwrap().version, updated.version);
    fs::remove_file(task.join("instructions/active.json")).unwrap();
    assert!(guidance::active_snapshot(&task).is_err());
}

#[test]
fn invalid_sources_limits_and_concurrent_edits_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for remote in [r"\\server\share", r"\\?\UNC\server\share", r"\\.\C:\repo", r"\??\C:\repo"] {
        assert!(guidance::checked_path(Path::new(remote), "architect").unwrap_err().message.contains("UNC/device"));
    }
    for content in ["\u{feff}BOM".to_owned(), "NUL\0".to_owned(), "x".repeat(65537)] {
        assert!(guidance::save_source(root, "architect", &content, None).is_err());
    }
    for role in ["../escape", "C:/absolute", "\\\\server\\share", "architect.md", "\\\\?\\C:\\x"] {
        assert!(guidance::save_source(root, role, "bad", None).is_err());
    }
    let first = guidance::save_source(root, "architect", "one", None).unwrap();
    guidance::save_source(root, "architect", "external", first.hash.as_deref()).unwrap();
    assert!(guidance::save_source(root, "architect", "lost update", first.hash.as_deref()).is_err());
    for role in &guidance::ROLES[..4] {
        let source = guidance::read_source(root, role).unwrap();
        guidance::save_source(root, role, &"x".repeat(65536), source.hash.as_deref()).unwrap();
    }
    assert!(guidance::save_source(root, guidance::ROLES[4], "x", None).is_err());
    fs::write(root.join(guidance::role_path("architect").unwrap()), [0xff]).unwrap();
    assert!(guidance::read_set(root).is_err());
}

#[cfg(windows)]
#[test]
fn windows_junctions_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("other");
    fs::create_dir(&target).unwrap();
    let link = dir.path().join(".agentflow-instructions");
    let result =
        std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&link).arg(&target).output().unwrap();
    assert!(result.status.success());
    assert!(guidance::read_set(dir.path()).is_err());
}

fn add_orchestrator(h: &Harness) {
    h.with_task(|s| {
        s.add_node(WorkflowNodeRow {
            id: 0,
            task_id: s.task.id,
            key: spec::ORCHESTRATOR.into(),
            kind: "agent".into(),
            label: "Orchestrator".into(),
            role: Some("orchestrator".into()),
            state: "READY".into(),
            execution_mode: "manual".into(),
            iteration: 1,
            checkpoint_sha: None,
            message: None,
            error: None,
            config_override: Json(json!({})),
            updated_at: DbTime::now(),
        })
    });
}

#[tokio::test]
async fn all_engine_launch_branches_match_preview_and_keep_roles_separate() {
    for (key, iteration) in [
        (spec::ARCHITECT, 0),
        (spec::ARCHITECT, 1),
        (spec::ORCHESTRATOR, 1),
        (spec::IMPLEMENTER, 1),
        (spec::IMPLEMENTER, 2),
        (spec::CODE_REVIEW, 1),
        (spec::SECURITY_REVIEW, 1),
        (spec::FINAL_ARCHITECT, 1),
    ] {
        let h = engine().await;
        h.launchable();
        add_orchestrator(&h);
        sources(Path::new(&h.load().task.worktree_path));
        h.with_task(|s| {
            s.task.review_iteration = iteration.max(1);
            let i = s.node_index(key).unwrap();
            s.nodes[i].state = "READY".into();
            s.nodes[i].iteration = iteration;
        });
        let preview = h
            .engine
            .task_instructions("TEST-1", PreviewQuery { node_key: Some(key.into()), ..Default::default() })
            .await
            .unwrap();
        h.engine.run_node("TEST-1", key, RunNodeRequest::default(), false).await.unwrap();
        let state = h.load();
        let run = state.runs.last().unwrap();
        let role = state.nodes.iter().find(|n| n.id == run.node_id).unwrap().role.as_ref().unwrap();
        let prompt = agentflow_core::pyfile::read_text(Path::new(run.prompt_path.as_ref().unwrap())).unwrap();
        assert!(prompt.contains(&format!("Unique {role}\n")), "{key}");
        assert_eq!(prompt.matches("Unique ").count(), 1, "{key}");
        assert_eq!(
            preview["briefing"]
                .as_str()
                .unwrap()
                .replace("--run 0", &format!("--run {}", run.id))
                .replace("run id (`0`)", &format!("run id (`{}`)", run.id)),
            prompt
        );
        let metadata =
            agentflow_core::workspace::read_json(&Path::new(run.run_dir.as_ref().unwrap()).join("instructions.json"))
                .unwrap();
        assert_eq!(metadata["briefingHash"], guidance::content_hash(&prompt));
        let snapshot =
            guidance::active_snapshot(&agentflow_core::engine::snapshot::workspace(&state.task).root).unwrap().unwrap();
        assert_eq!(metadata["version"], snapshot.version);
    }
}

#[tokio::test]
async fn updates_are_explicit_restart_safe_and_preserve_historical_runs() {
    let h = engine().await;
    h.launchable();
    sources(Path::new(&h.load().task.worktree_path));
    h.engine.run_node("TEST-1", spec::ARCHITECT, RunNodeRequest::default(), false).await.unwrap();
    let state = h.load();
    let directory = agentflow_core::engine::snapshot::workspace(&state.task).root;
    let previous = guidance::active_snapshot(&directory).unwrap().unwrap();
    let root = Path::new(&state.task.worktree_path);
    guidance::save_source(root, "architect", "UPDATED", previous.files["architect"].hash.as_deref()).unwrap();
    let preview = h.engine.task_instructions("TEST-1", PreviewQuery::default()).await.unwrap();
    assert_eq!(preview["differs"], true);
    assert!(!preview["briefing"].as_str().unwrap().contains("UPDATED"));
    let request = ApplyUpdate {
        expected_version: previous.version.clone(),
        expected_source_hashes: guidance::read_set(root).unwrap().into_iter().map(|(r, f)| (r, f.hash)).collect(),
        fresh_conversations: true,
    };
    let updated = h.engine.apply_instructions("TEST-1", request).await.unwrap();
    assert_ne!(updated["version"], previous.version);
    let restarted = h.restarted();
    let launched_before = h.terminals.calls().iter().filter(|c| c.starts_with("new_tab")).count();
    restarted.recover().await.unwrap();
    restarted.recover_node("TEST-1", spec::ARCHITECT, false).await.unwrap();
    assert_eq!(h.terminals.calls().iter().filter(|c| c.starts_with("new_tab")).count(), launched_before);
    let preview = restarted.task_instructions("TEST-1", PreviewQuery::default()).await.unwrap();
    assert!(preview["briefing"].as_str().unwrap().ends_with("UPDATED"));
    let run = &state.runs[0];
    assert!(guidance::needs_fresh(&guidance::active_snapshot(&directory).unwrap().unwrap(), run.run_dir.as_deref()));
    let history = restarted
        .task_instructions("TEST-1", PreviewQuery { run_id: Some(run.id), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(history["metadata"]["version"], previous.version);
    assert!(!history["briefing"].as_str().unwrap().contains("UPDATED"));
    assert!(directory.join("instructions").join(format!("{}.json", previous.version)).exists());
}

#[tokio::test]
async fn source_edit_api_requires_cas_and_keeps_saved_changes_out_of_snapshot() {
    let h = engine().await;
    h.launchable();
    let path = "/api/tasks/TEST-1/instructions/architect";
    let (status, _) = h.route("PUT", path, Some(json!({"content": "first", "expectedHash": null}))).await;
    assert_eq!(status, 200);
    let (status, _) = h.route("PUT", path, Some(json!({"content": "lost", "expectedHash": null}))).await;
    assert_eq!(status, 409);
    let (status, _) = h.route("PUT", path, Some(json!({"content": "lost"}))).await;
    assert_eq!(status, 422);
    assert!(
        guidance::active_snapshot(&agentflow_core::engine::snapshot::workspace(&h.load().task).root).unwrap().is_none()
    );
}

#[tokio::test]
async fn update_fresh_choice_controls_future_processes_only() {
    let h = engine().await;
    h.launchable();
    let root = std::path::PathBuf::from(&h.load().task.worktree_path);
    sources(&root);
    h.with_task(|s| {
        let i = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[i].state = "READY".into();
    });
    h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
    for fresh in [false, true] {
        let directory = agentflow_core::engine::snapshot::workspace(&h.load().task).root;
        let previous = guidance::active_snapshot(&directory).unwrap().unwrap();
        guidance::save_source(
            &root,
            "implementer",
            &format!("Updated fresh={fresh}"),
            previous.files["implementer"].hash.as_deref(),
        )
        .unwrap();
        let before = h.terminals.calls();
        h.engine
            .apply_instructions(
                "TEST-1",
                ApplyUpdate {
                    expected_version: previous.version,
                    expected_source_hashes: guidance::read_set(&root)
                        .unwrap()
                        .into_iter()
                        .map(|(r, f)| (r, f.hash))
                        .collect(),
                    fresh_conversations: fresh,
                },
            )
            .await
            .unwrap();
        assert_eq!(h.terminals.calls(), before, "Updating never touches a process");
        h.with_task(|s| {
            let i = s.node_index(spec::IMPLEMENTER).unwrap();
            s.nodes[i].state = "READY".into();
            for run in &mut s.runs {
                run.state = "COMPLETED".into();
            }
            for agent in &mut s.agents {
                agent.status = "exited".into();
            }
        });
        h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
        let script = h.launch_script();
        assert_eq!(script.contains("'--resume'"), !fresh, "{script}");
    }
}

#[tokio::test]
async fn failed_run_evidence_prevents_launch_and_records_failure() {
    let h = engine().await;
    h.launchable();
    h.with_task(|s| {
        let i = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[i].state = "READY".into();
    });
    let directory = agentflow_core::engine::snapshot::workspace(&h.load().task).root;
    // An unreplaceable artifact destination simulates a persistence failure.
    fs::create_dir_all(directory.join("runs/implementer/01/instructions.json")).unwrap();
    assert!(h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.is_err());
    let state = h.load();
    assert_eq!(state.node(spec::IMPLEMENTER).unwrap().state, "FAILED");
    assert_eq!(state.runs.last().unwrap().state, "FAILED");
    assert!(!h.terminals.calls().iter().any(|c| c.starts_with("new_tab")));
}

#[cfg(windows)]
#[test]
fn two_local_adapter_stubs_read_the_same_file_on_launch_and_resume() {
    use agentflow_core::{config::HarnessConfig, harness::HarnessAdapter};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sources(root);
    let snapshot = guidance::capture(&root.join("task"), root, "revision", false).unwrap();
    let briefing = guidance::compose("flowctl complete", &snapshot, "implementer", 73).unwrap();
    let prompt = root.join("prompt.md");
    fs::write(&prompt, &briefing).unwrap();
    let stub = root.join("stub.ps1");
    fs::write(
        &stub,
        r#"$reference = $args | Where-Object { $_ -like 'Read the file *' } | Select-Object -First 1
$match = [regex]::Match($reference, 'Read the file (.+?prompt\.md) and follow')
if (-not $match.Success) { exit 2 }
$bytes = [IO.File]::ReadAllBytes($match.Groups[1].Value)
$stream = [Console]::OpenStandardOutput()
$stream.Write($bytes, 0, $bytes.Length)
"#,
    )
    .unwrap();
    for kind in ["claude", "copilot"] {
        let config: HarnessConfig = serde_json::from_value(json!({"type": kind, "command": "pwsh",
            "args": ["-NoLogo", "-NoProfile", "-File", stub]}))
        .unwrap();
        let adapter = HarnessAdapter::with_home("stub", config, root.join("home"));
        let reference = HarnessAdapter::prompt_reference(&prompt);
        let launch = adapter.build_launch(None, None, &reference);
        let resume = adapter.build_resume(launch.harness_session_id.as_deref(), None, None, &reference).unwrap();
        for spec in [launch, resume] {
            let output =
                std::process::Command::new(&spec.argv[0]).args(&spec.argv[1..]).current_dir(root).output().unwrap();
            assert!(output.status.success(), "{kind}: {}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.stdout, briefing.as_bytes());
        }
    }
}

#[test]
fn interrupted_initial_capture_recovers_exact_content_and_then_fails_closed() {
    for stage in ["snapshot", "pointer", "established"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sources(root);
        let task = root.join("task");
        // Construct the on-disk states at each interrupted initial transaction boundary.
        let snapshot = guidance::capture(&root.join("reference"), root, "original", false).unwrap();
        guidance::atomic_json(
            &task.join("instructions-required.json"),
            &json!({"schema": 2, "state": "pending", "snapshot": snapshot}),
        )
        .unwrap();
        let version = task.join("instructions").join(format!("{}.json", snapshot.version));
        if stage != "snapshot" {
            guidance::atomic_json(&version, &snapshot).unwrap();
        }
        if stage == "established" {
            guidance::atomic_json(&task.join("instructions/active.json"), &json!({"version": snapshot.version}))
                .unwrap();
        }
        // A blocked destination also proves retry is possible after a real write error.
        if stage == "snapshot" {
            fs::create_dir_all(&version).unwrap();
            assert!(guidance::active_snapshot(&task).is_err());
            fs::remove_dir(&version).unwrap();
        }
        let old = guidance::read_source(root, "architect").unwrap();
        guidance::save_source(root, "architect", "Changed after interruption", old.hash.as_deref()).unwrap();
        let recovered = guidance::active_snapshot(&task).unwrap().unwrap();
        assert_eq!(serde_json::to_value(recovered).unwrap(), serde_json::to_value(&snapshot).unwrap());
        assert_eq!(guidance::active_snapshot(&task).unwrap().unwrap().version, snapshot.version);
        fs::remove_file(task.join("instructions/active.json")).unwrap();
        assert!(guidance::active_snapshot(&task).is_err());
    }
}

#[test]
fn malformed_pending_capture_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = guidance::capture(&dir.path().join("reference"), dir.path(), "original", false).unwrap();
    let mut pending = serde_json::to_value(snapshot).unwrap();
    pending["files"]["architect"]["hash"] = json!("invalid");
    let task = dir.path().join("task");
    guidance::atomic_json(
        &task.join("instructions-required.json"),
        &json!({"schema": 2, "state": "pending", "snapshot": pending}),
    )
    .unwrap();
    assert!(guidance::active_snapshot(&task).is_err());
}
