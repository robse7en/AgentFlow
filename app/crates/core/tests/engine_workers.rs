mod engine_support;
use agentflow_core::{
    db::{AgentSessionRow, DbTime, Json, WorkflowNodeRow, table::Tracked},
    engine::{
        launch::RunNodeRequest,
        lifecycle::{AgentOverride, CreateTaskRequest},
        spec,
        work_plans::WorkPlan,
    },
    vcs::Outcome,
};
use engine_support::{Harness, engine};
use serde_json::{Value, json};
fn plan(count: usize) -> Value {
    json!({"summary":"Focused work","subtasks":(0..count).map(|i|json!({"key":format!("part-{i}"),"title":format!("Part {i}"),"brief":"Build the owned change","ownedPaths":[format!("src/part-{i}")],"acceptanceCriteria":["Tests pass"],"dependencies":[]})).collect::<Vec<_>>()})
}
async fn parallel() -> Harness {
    let h = engine().await;
    h.launchable();
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
        });
    });
    h.git.returns("is_ancestor", Outcome::Ok(json!(true)));
    h
}
fn workers(h: &Harness) -> Vec<Value> {
    h.engine.snapshot("TEST-1").unwrap()["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["worker"].is_object())
        .cloned()
        .collect()
}
async fn submit(h: &Harness, p: Value) -> Vec<Value> {
    let id = h.add_run(spec::ORCHESTRATOR, "RUNNING", None);
    h.engine.agent_complete("TEST-1", "orchestrator", Some(id), p.as_object().unwrap().clone()).await.unwrap();
    workers(h)
}
async fn finish(h: &Harness, n: &Value) -> Value {
    h.engine
        .agent_complete(
            "TEST-1",
            "implementer",
            n["lastRun"]["id"].as_i64(),
            json!({"summary":"Done","status":"completed","tests":{"passed":1,"failed":0}}).as_object().unwrap().clone(),
        )
        .await
        .unwrap()
}
#[test]
fn unsafe_ownership_dependencies_cycles_and_overlap_rejected() {
    for path in
        ["../outside", "/absolute", "C:/outside", "src\\file", ".git/config", "src/*.rs", "src/..", "src/trailing."]
    {
        let mut p = plan(1);
        p["subtasks"][0]["ownedPaths"] = json!([path]);
        assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err(), "{path}");
    }
    for deps in [json!(["missing"]), json!(["part-0"])] {
        let mut p = plan(1);
        p["subtasks"][0]["dependencies"] = deps;
        assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err());
    }
    let mut p = plan(2);
    p["subtasks"][1]["ownedPaths"] = json!(["SRC/part-0/sub"]);
    assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err());
    let mut p = plan(2);
    p["subtasks"][0]["dependencies"] = json!(["part-1"]);
    p["subtasks"][1]["dependencies"] = json!(["part-0"]);
    assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err());
}
#[tokio::test]
async fn test_three_fresh_workers_then_queued_work_then_integration() {
    let h = parallel().await;
    let root = std::path::PathBuf::from(&h.load().task.worktree_path);
    let original =
        agentflow_core::instructions::save_source(&root, "implementer", "Frozen worker guidance", None).unwrap();
    let first = submit(&h, plan(4)).await;
    agentflow_core::instructions::save_source(&root, "implementer", "Future worker guidance", original.hash.as_deref())
        .unwrap();
    assert_eq!(first.iter().filter(|n| n["state"] == "RUNNING").count(), 3);
    assert_eq!(first[3]["state"], "PENDING");
    finish(&h, &first[0]).await;
    assert_eq!(workers(&h)[3]["state"], "RUNNING");
    let directory = agentflow_core::engine::snapshot::workspace(&h.load().task).root;
    let snapshot = agentflow_core::instructions::active_snapshot(&directory).unwrap().unwrap();
    let update = agentflow_core::engine::instruction_ui::ApplyUpdate {
        expected_version: snapshot.version.clone(),
        fresh_conversations: true,
        expected_source_hashes: agentflow_core::instructions::read_set(&root)
            .unwrap()
            .into_iter()
            .map(|(r, f)| (r, f.hash))
            .collect(),
    };
    assert!(h.engine.apply_instructions("TEST-1", update).await.unwrap_err().message.contains("parallel batch"));
    for worker in workers(&h) {
        let run_dir = std::path::Path::new(worker["lastRun"]["runDir"].as_str().unwrap());
        let metadata = agentflow_core::workspace::read_json(&run_dir.join("instructions.json")).unwrap();
        assert_eq!(metadata["version"], snapshot.version);
        assert_eq!(metadata["source"]["content"], "Frozen worker guidance");
    }
    let launches = h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count();
    finish(&h, &first[0]).await;
    assert_eq!(h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count(), launches);
    for n in workers(&h).iter().filter(|n| n["state"] == "RUNNING") {
        finish(&h, n).await;
    }
    assert_eq!(h.load().nodes.iter().find(|n| n.key == spec::IMPLEMENTER).unwrap().state, "READY");
}
#[tokio::test]
async fn test_dependencies_wait_and_context_is_focused() {
    let h = parallel().await;
    let mut p = plan(2);
    p["subtasks"][1]["dependencies"] = json!(["part-0"]);
    let first = submit(&h, p).await;
    assert_eq!(first[1]["state"], "PENDING");
    finish(&h, &first[0]).await;
    assert_eq!(workers(&h)[1]["state"], "RUNNING");
    assert_eq!(h.git.calls_of("merge_worker_checkpoint").len(), 1);
    let prompt = std::fs::read_to_string(workers(&h)[1]["lastRun"]["promptPath"].as_str().unwrap()).unwrap();
    assert!(prompt.contains("Done"));
}
#[tokio::test]
async fn test_ownership_failure_does_not_start_reviews() {
    let h = parallel().await;
    let first = submit(&h, plan(1)).await;
    h.git.returns("changed_paths", Outcome::Ok(json!(["src/unassigned/file.rs"])));
    assert_eq!(finish(&h, &first[0]).await["state"], "FAILED");
    assert_eq!(h.load().nodes.iter().find(|n| n.key == spec::CODE_REVIEW).unwrap().state, "PENDING");
}
#[tokio::test]
async fn test_integration_requires_every_checkpoint() {
    let h = parallel().await;
    agentflow_core::instructions::save_source(
        std::path::Path::new(&h.load().task.worktree_path),
        "implementer",
        "Shared integration guidance",
        None,
    )
    .unwrap();
    let first = submit(&h, plan(1)).await;
    finish(&h, &first[0]).await;
    let snap = h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
    let n = snap["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::IMPLEMENTER).unwrap();
    assert!(
        std::fs::read_to_string(n["lastRun"]["promptPath"].as_str().unwrap())
            .unwrap()
            .contains("Shared integration guidance")
    );
    h.git.returns("is_ancestor", Outcome::Ok(json!(false)));
    assert_eq!(finish(&h, n).await["state"], "FAILED");
}
#[tokio::test]
async fn test_worker_settings_are_inherited_and_cannot_be_overridden() {
    let h = parallel().await;
    let first = submit(&h, plan(2)).await;
    let a = first[0]["key"].as_str().unwrap();
    let b = first[1]["key"].as_str().unwrap();
    assert_ne!(
        h.engine.terminal_target("TEST-1", a).await.unwrap(),
        h.engine.terminal_target("TEST-1", b).await.unwrap()
    );
    assert!(
        h.engine
            .set_node_override("TEST-1", a, &AgentOverride { model: Some("different".into()), ..Default::default() })
            .await
            .unwrap_err()
            .message
            .contains("parent Implementer")
    );
    h.with_task(|s| {
        let n = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[n].config_override = Json(json!({"model":"shared-model"}));
    });
    let snap = h.engine.snapshot("TEST-1").unwrap();
    let child = snap["nodes"].as_array().unwrap().iter().find(|n| n["key"] == a).unwrap();
    assert_eq!(child["effectiveConfig"]["model"], "shared-model");
}
#[tokio::test]
async fn test_restart_does_not_spawn_or_replan_and_cancel_stops_workers() {
    let h = parallel().await;
    submit(&h, plan(4)).await;
    let before = h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count();
    let restart = h.restarted();
    restart.recover().await.unwrap();
    assert_eq!(h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count(), before);
    let nodes = workers(&h);
    assert_eq!(nodes.iter().filter(|n| n["state"] == "INTERRUPTED").count(), 3);
    assert_eq!(nodes[3]["state"], "READY");
    let error = h
        .engine
        .run_node("TEST-1", nodes[3]["key"].as_str().unwrap(), RunNodeRequest::default(), false)
        .await
        .unwrap_err();
    assert!(error.message.contains("three worker slots"));
    h.engine.cancel_task("TEST-1").await.unwrap();
    assert!(!h.load().agents.iter().any(|a| a.status == "running"));
    assert_eq!(workers(&h).len(), 4);
}

#[tokio::test]
async fn test_worker_credential_cannot_report_for_another_worker() {
    let h = parallel().await;
    let first = submit(&h, plan(2)).await;
    let credential: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(first[0]["lastRun"]["runDir"].as_str().unwrap()).join("credential.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {}", credential["token"].as_str().unwrap()).parse().unwrap());
    let app = agentflow_core::api::App::around(h.engine.clone(), h.tmp().join("config.json"));
    use tower::ServiceExt;
    let body = json!({"role":"implementer","runId":first[1]["lastRun"]["id"],"message":"other worker"});
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/agent/TEST-1/question")
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .header("authorization", headers["authorization"].clone())
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let response = agentflow_core::api::router(app).oneshot(request).await.unwrap();
    assert_eq!(response.status().as_u16(), 403);
}

#[tokio::test]
async fn test_terminals_questions_and_retry_are_worker_specific() {
    let h = parallel().await;
    let first = submit(&h, plan(2)).await;
    let target = h.engine.terminal_target("TEST-1", first[1]["key"].as_str().unwrap()).await.unwrap();
    h.engine
        .agent_question("TEST-1", "implementer", first[1]["lastRun"]["id"].as_i64(), "Which behavior?", &[])
        .await
        .unwrap();
    let snap = h.engine.snapshot("TEST-1").unwrap();
    let uid = snap["questions"][0]["uid"].as_str().unwrap();
    h.engine.answer_question("TEST-1", uid, "Approved behavior", None).await.unwrap();
    h.engine.agent_fail("TEST-1", "implementer", first[0]["lastRun"]["id"].as_i64(), "Retry me").await.unwrap();
    h.engine.run_node("TEST-1", first[0]["key"].as_str().unwrap(), RunNodeRequest::default(), false).await.unwrap();
    assert_eq!(h.engine.terminal_target("TEST-1", first[1]["key"].as_str().unwrap()).await.unwrap(), target);
    assert!(
        h.engine
            .agent_complete(
                "TEST-1",
                "implementer",
                first[0]["lastRun"]["id"].as_i64(),
                json!({"summary":"Done","status":"completed"}).as_object().unwrap().clone()
            )
            .await
            .unwrap_err()
            .message
            .contains("Stale")
    );
}

#[tokio::test]
async fn test_worker_retry_briefing_carries_the_failed_attempt() {
    let h = parallel().await;
    let first = submit(&h, plan(4)).await[0].clone();
    let run_id = first["lastRun"]["id"].as_i64();
    h.engine.agent_question("TEST-1", "implementer", run_id, "Keep the old route?", &[]).await.unwrap();
    let uid = h.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_string();
    h.engine.answer_question("TEST-1", &uid, "Keep it", None).await.unwrap();
    h.engine.agent_fail("TEST-1", "implementer", run_id, "Out of context").await.unwrap();
    let head = "d".repeat(40);
    h.git.returns("head_sha", Outcome::Ok(json!(head)));
    h.git.returns("changed_paths", Outcome::Ok(json!(["src/part-0/api.py"])));
    h.git.returns("dirty_paths", Outcome::Ok(json!(["?? src/part-0/draft.py"])));
    let key = first["key"].as_str().unwrap();
    let snap = h.engine.run_node("TEST-1", key, RunNodeRequest::default(), false).await.unwrap();
    let retry = snap["nodes"].as_array().unwrap().iter().find(|n| n["key"] == key).unwrap();
    let prompt = std::fs::read_to_string(retry["lastRun"]["promptPath"].as_str().unwrap()).unwrap();
    let section = prompt.split("## Previous attempt at this stage").nth(1).unwrap();
    assert!(
        section.contains("Out of context") && section.contains("Keep the old route?") && section.contains("Keep it")
    );
    assert!(section.contains(&format!("HEAD moved from `{}`", "a".repeat(40))), "{section}");
    assert!(section.contains("src/part-0/api.py") && section.contains("?? src/part-0/draft.py"));
    let worktree = first["worker"]["worktreePath"].as_str().unwrap().to_string();
    let committed: Vec<_> = h.git.calls_of("changed_paths").into_iter().filter(|c| c[0] == worktree).collect();
    assert_eq!(committed, vec![vec![worktree, "a".repeat(40), head]]);
}

#[tokio::test]
async fn test_worker_harness_exit_is_retried_automatically_in_its_own_slot() {
    let h = parallel().await;
    h.configure(|c| c.workflow.max_automatic_retries = 1);
    h.engine.set_automatic_retry_delay(std::time::Duration::ZERO);
    let first = submit(&h, plan(4)).await[0].clone();
    let run_id = first["lastRun"]["id"].as_i64().unwrap();
    h.engine.agent_exited("TEST-1", "implementer", Some(run_id), 1).await.unwrap();
    h.engine.join_automatic_retry(run_id).await;
    let snap = h.engine.snapshot("TEST-1").unwrap();
    let nodes = snap["nodes"].as_array().unwrap();
    let retried = nodes.iter().find(|n| n["key"] == first["key"]).unwrap();
    assert_eq!(retried["state"], "RUNNING");
    assert_ne!(retried["lastRun"]["id"].as_i64(), Some(run_id));
    let prompt = std::fs::read_to_string(retried["lastRun"]["promptPath"].as_str().unwrap()).unwrap();
    assert!(prompt.contains("## Previous attempt at this stage"));
    // The other workers keep their slots; the fourth is still queued.
    let states: Vec<&str> = nodes
        .iter()
        .filter(|n| n["key"].as_str().unwrap().starts_with("worker_"))
        .map(|n| n["state"].as_str().unwrap())
        .collect();
    assert_eq!(states.iter().filter(|s| **s == "RUNNING").count(), 3);
    assert_eq!(states.iter().filter(|s| **s == "PENDING").count(), 1);
}

#[tokio::test]
async fn test_launch_failure_is_durable_and_recovery_finds_guid_tab() {
    let h = parallel().await;
    h.terminals.fail("new_tab", "fixture failure");
    let first = submit(&h, plan(1)).await;
    assert_eq!(first[0]["state"], "FAILED");
    assert!(first[0]["lastRun"]["runDir"].is_string());
    h.terminals.succeed("new_tab");
    h.engine.run_node("TEST-1", first[0]["key"].as_str().unwrap(), RunNodeRequest::default(), false).await.unwrap();
    let count = h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count();
    h.restarted().recover().await.unwrap();
    assert_eq!(h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count(), count);
    assert_eq!(workers(&h)[0]["state"], "INTERRUPTED");
}

#[tokio::test]
async fn test_approval_starts_orchestrator_for_new_tasks() {
    let h = parallel().await;
    let task_id = h.load().task.id;
    h.engine.db.session(|tx| tx.execute("DELETE FROM requirements_approvals WHERE task_id=?1", [task_id])).unwrap();
    h.with_task(|s| {
        let n = s.node_index(spec::REQUIREMENTS_GATE).unwrap();
        s.nodes[n].state = "WAITING_FOR_APPROVAL".into();
    });
    let docs = agentflow_core::engine::snapshot::workspace(&h.load().task).requirement_files().unwrap();
    h.engine
        .approve(
            "TEST-1",
            spec::REQUIREMENTS_GATE,
            agentflow_core::engine::approvals::ApprovalRequest {
                expected_requirements_digest: Some(agentflow_core::engine::rules::documents_digest(&docs)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(h.load().node(spec::ORCHESTRATOR).unwrap().state, "READY");
    assert_eq!(h.load().node(spec::IMPLEMENTER).unwrap().state, "PENDING");
}

#[tokio::test]
async fn test_repeated_completion_does_not_dispatch_twice() {
    let h = parallel().await;
    let first = submit(&h, plan(4)).await;
    finish(&h, &first[0]).await;
    finish(&h, &first[0]).await;
    assert_eq!(h.terminals.calls().iter().filter(|c| c.starts_with("new_tab ")).count(), 4);
    assert_eq!(h.git.calls_of("checkpoint").len(), 1);
}

#[tokio::test]
async fn test_restart_exposes_pending_work_but_preserves_capacity() {
    let h = parallel().await;
    let first = submit(&h, plan(4)).await;
    h.restarted().recover().await.unwrap();
    assert_eq!(workers(&h)[3]["state"], "READY");
    assert!(
        h.engine
            .run_node("TEST-1", first[3]["key"].as_str().unwrap(), RunNodeRequest::default(), false)
            .await
            .unwrap_err()
            .message
            .contains("three worker slots")
    );
}

#[test]
fn test_unsafe_ownership_rejected() {
    for path in ["../outside", "/absolute", "C:/outside", "src\\file", ".git/config", "src/*.rs"] {
        let mut p = plan(1);
        p["subtasks"][0]["ownedPaths"] = json!([path]);
        assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err());
    }
}

#[test]
fn test_invalid_dependencies_and_overlapping_ownership_rejected() {
    let mut p = plan(2);
    p["subtasks"][1]["dependencies"] = json!(["missing"]);
    assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err());
    let mut p = plan(2);
    p["subtasks"][1]["ownedPaths"] = json!(["src/part-0/sub"]);
    assert!(serde_json::from_value::<WorkPlan>(p).unwrap().validated().is_err());
}

#[tokio::test]
async fn test_real_git_workers_dependencies_and_integration() {
    use agentflow_core::{
        engine::{Engine, EngineSetup},
        git::GitService,
    };
    use std::{path::Path, sync::Arc, time::Duration};
    let mut h = parallel().await;
    let repo = h.tmp().join("real-repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: Vec<String>, cwd: std::path::PathBuf| async move {
        let mut argv = vec!["git".into()];
        argv.extend(args);
        let output =
            agentflow_core::process::run(&argv, Some(&cwd), &Default::default(), Duration::from_secs(20), None)
                .await
                .unwrap();
        assert!(output.ok(), "{}", output.stderr);
        output.stdout.trim().to_string()
    };
    git(vec!["init".into(), "-b".into(), "main".into()], repo.clone()).await;
    std::fs::write(repo.join("README.md"), "Local fixture\n").unwrap();
    git(vec!["add".into(), ".".into()], repo.clone()).await;
    git(
        vec![
            "-c".into(),
            "user.name=Fixture".into(),
            "-c".into(),
            "user.email=fixture@example.test".into(),
            "commit".into(),
            "-m".into(),
            "initial".into(),
        ],
        repo.clone(),
    )
    .await;
    let vcs = Arc::new(GitService::default());
    let tree = h.load().task.worktree_path.clone();
    std::fs::remove_dir(&tree).unwrap();
    vcs.create_worktree(&repo, Path::new(&tree), "agent/TEST-1", "main").await.unwrap();
    let sha = vcs.head_sha(Path::new(&tree)).await.unwrap();
    h.with_task(|s| {
        s.task.repository_root = repo.to_string_lossy().into();
        s.task.repository_path = repo.to_string_lossy().into();
        s.task.current_sha = Some(sha);
    });
    h.engine = Engine::new(EngineSetup {
        locations: h.engine.locations.clone(),
        config: h.config(),
        harnesses: h.engine.harnesses(),
        db: h.engine.db.clone(),
        git: vcs.clone(),
        terminals: h.terminals.clone(),
        leftover_zellij: None,
        worktree_killer: h.killer.clone(),
        broadcaster: h.engine.broadcaster.clone(),
        api_base_url: h.engine.api_base_url.clone(),
        flowctl: h.engine.flowctl.clone(),
    })
    .unwrap();
    let mut p = plan(2);
    p["subtasks"][1]["dependencies"] = json!(["part-0"]);
    let first = submit(&h, p).await;
    let root = Path::new(first[0]["worker"]["worktreePath"].as_str().unwrap());
    std::fs::create_dir_all(root.join("src/part-0")).unwrap();
    std::fs::write(root.join("src/part-0/result.rs"), "first\n").unwrap();
    finish(&h, &first[0]).await;
    let second = workers(&h)[1].clone();
    let root = Path::new(second["worker"]["worktreePath"].as_str().unwrap());
    assert!(root.join("src/part-0/result.rs").is_file());
    std::fs::create_dir_all(root.join("src/part-1")).unwrap();
    std::fs::write(root.join("src/part-1/result.rs"), "second\n").unwrap();
    finish(&h, &second).await;
    for n in workers(&h) {
        vcs.merge_worker_checkpoint(Path::new(&tree), n["checkpointSha"].as_str().unwrap()).await.unwrap();
    }
    let snap = h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
    let integrator = snap["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::IMPLEMENTER).unwrap();
    assert_eq!(finish(&h, integrator).await["state"], "COMPLETED");
}

/// New Parallel tasks plan with the Architect: the planning stage joins the Architect's
/// conversation, and the flowctl it reports with adds a status the plan itself never has.
#[tokio::test]
async fn test_architect_planning_resumes_the_discovery_conversation() {
    let h = parallel().await;
    let discovery = h.add_run(spec::ARCHITECT, "COMPLETED", None);
    h.with_task(|s| {
        let n = s.node_index(spec::ORCHESTRATOR).unwrap();
        s.nodes[n].role = Some("architect".into());
        s.nodes[n].label = spec::ARCHITECT_PLANNING_LABEL.into();
        s.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: s.task.id,
            role: "architect".into(),
            harness: "claude".into(),
            model: None,
            reasoning: None,
            zellij_tab_name: "Architect".into(),
            zellij_tab_id: None,
            zellij_pane_id: None,
            harness_session_id: Some("discovery-1".into()),
            status: "stopped".into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: Some(discovery),
            started_at: DbTime::now(),
            ended_at: None,
        }));
    });
    let snap = h.engine.run_node("TEST-1", spec::ORCHESTRATOR, RunNodeRequest::default(), false).await.unwrap();
    let script = h.launch_script();
    assert!(script.contains("'--resume' 'discovery-1'"), "{script}");
    let run = &snap["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::ORCHESTRATOR).unwrap()["lastRun"];
    let prompt_path = std::path::PathBuf::from(run["promptPath"].as_str().unwrap());
    let prompt = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(prompt.contains("# Subtask planning for TEST-1") && prompt.contains("instead of exploring it again"));
    assert_eq!(prompt_path.parent().unwrap().parent().unwrap().file_name().unwrap(), "architect");

    let mut result = plan(2);
    result["status"] = json!("completed");
    h.engine
        .agent_complete("TEST-1", "architect", run["id"].as_i64(), result.as_object().unwrap().clone())
        .await
        .unwrap();
    assert_eq!(h.load().node(spec::ORCHESTRATOR).unwrap().state, "COMPLETED");
    assert_eq!(workers(&h).len(), 2);
}

#[tokio::test]
async fn test_parallel_planning_goes_to_the_architect_unless_an_orchestrator_is_configured() {
    for (configured, overrides, role, label) in [
        (false, json!({}), "architect", "Architect Planning"),
        (false, json!({"orchestrator": {"harness": "", "model": ""}}), "architect", "Architect Planning"),
        (true, json!({}), "orchestrator", "Orchestrator"),
        (false, json!({"orchestrator": {"model": "planner-model"}}), "orchestrator", "Orchestrator"),
    ] {
        let h = engine().await;
        if configured {
            h.configure(|config| config.agents.orchestrator = Some(config.agents.architect.clone()));
        }
        h.git.returns(
            "inspect_repository",
            Outcome::Ok(json!({"path": h.tmp(), "root": h.tmp(), "branches": ["main"]})),
        );
        h.git.returns("create_worktree", Outcome::Ok(json!({"branch": "agent/NEW-1"})));
        let request: CreateTaskRequest = serde_json::from_value(json!({
            "jiraKey": "NEW-1", "repositoryPath": h.tmp(), "baseBranch": "main",
            "implementationMode": "parallel", "agentOverrides": overrides,
        }))
        .unwrap();
        let snapshot = h.engine.create_task(request.validated().unwrap()).await.unwrap();
        let planner = snapshot["nodes"].as_array().unwrap().iter().find(|n| n["key"] == spec::ORCHESTRATOR).unwrap();
        assert_eq!((planner["role"].as_str(), planner["label"].as_str()), (Some(role), Some(label)), "{overrides}");
    }
}
