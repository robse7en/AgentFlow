//! Slice 4 tests ported by name from test_review_loop.py and test_workflow_safety.py.
mod engine_support;
use agentflow_core::db::table;
use agentflow_core::db::{Json, RunEvidenceRow, new_uid};
use agentflow_core::engine::approvals::ApprovalRequest;
use agentflow_core::engine::completion::{history_finding, normalize};
use agentflow_core::engine::launch::RunNodeRequest;
use agentflow_core::engine::{review_loop, spec};
use engine_support::{Harness, SHA, engine};
use serde_json::{Value, json};

fn blocker() -> Value {
    json!({"title": "Null deref", "severity": "high", "blocking": true})
}
fn minor() -> Value {
    json!({"title": "Naming", "severity": "low", "blocking": false})
}
fn verdict(id: &str, status: &str) -> Value {
    json!({"id": id, "status": status, "evidence": "src/app.py:12 checks for None"})
}
fn state(h: &Harness, key: &str) -> String {
    h.load().node(key).unwrap().state.clone()
}
fn findings(h: &Harness) -> Vec<Value> {
    h.engine.snapshot("TEST-1").unwrap()["findings"].as_array().unwrap().clone()
}
fn start_reviews(h: &Harness) {
    h.with_task(|s| {
        s.task.max_review_iterations = 5;
        for key in [spec::CODE_REVIEW, spec::SECURITY_REVIEW] {
            let n = s.node_index(key).unwrap();
            s.nodes[n].iteration = s.task.review_iteration;
        }
    });
}
async fn complete(h: &Harness, role: &str, run: i64, result: Value) -> Value {
    h.engine.agent_complete("TEST-1", role, Some(run), result.as_object().unwrap().clone()).await.unwrap()
}
async fn rejected(h: &Harness, role: &str, run: i64, result: Value, message: &str) {
    let error =
        h.engine.agent_complete("TEST-1", role, Some(run), result.as_object().unwrap().clone()).await.unwrap_err();
    assert_eq!(error.status, 400);
    assert!(error.message.contains(message), "{error}");
}
async fn round(h: &Harness, mut code: Value) {
    start_reviews(h);
    let c = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    let s = h.add_run(spec::SECURITY_REVIEW, "RUNNING", None);
    code["reviewedSha"] = json!(SHA);
    assert_eq!(complete(h, "code_reviewer", c, code).await["ok"], true);
    assert_eq!(complete(h, "security_reviewer", s, json!({"reviewedSha": SHA})).await["ok"], true);
}

#[tokio::test]
async fn test_followup_review_is_refused_until_every_prior_blocker_has_a_verdict() {
    let h = engine().await;
    round(&h, json!({"findings": [blocker(), {"title":"Race", "severity":"high", "blocking":true}, minor()]})).await;
    start_reviews(&h);
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    rejected(
        &h,
        "code_reviewer",
        run,
        json!({"reviewedSha":SHA, "priorFindings":[verdict("CODE-001", "fixed")]}),
        "no verdict for prior blocking finding(s): CODE-002",
    )
    .await;
    assert_eq!(state(&h, spec::CODE_REVIEW), "RUNNING");
    complete(
        &h,
        "code_reviewer",
        run,
        json!({"reviewedSha":SHA, "priorFindings":[verdict("CODE-001", "fixed"), verdict("CODE-002", "withdrawn")]}),
    )
    .await;
    let run = h.add_run(spec::SECURITY_REVIEW, "RUNNING", None);
    complete(&h, "security_reviewer", run, json!({"reviewedSha":SHA})).await;
    assert_eq!(
        findings(&h).iter().map(|f| f["status"].as_str().unwrap()).collect::<Vec<_>>(),
        ["resolved", "withdrawn", "superseded"]
    );
    assert_eq!(state(&h, spec::FINAL_ARCHITECT), "READY");
}

#[tokio::test]
async fn test_not_fixed_blocker_must_be_reraised_and_keep_blocking() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker(), minor()]})).await;
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    rejected(
        &h,
        "code_reviewer",
        run,
        json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","not_fixed")]}),
        "not re-reported",
    )
    .await;
    rejected(&h, "code_reviewer", run, json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","not_fixed")],"findings":[{"title":"Bug", "blocking":false, "priorId":"CODE-001"}]}), "must keep \"blocking\": true").await;
    rejected(&h, "code_reviewer", run, json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","fixed")],"findings":[{"priorId":"CODE-002", "blocking":true}]}), "not one of your previous blocking findings").await;
    let dir = h.tmp().join("review");
    h.with_task(|s| s.runs.last_mut().unwrap().run_dir = Some(dir.to_string_lossy().into()));
    let mut reraised = blocker();
    reraised["priorId"] = json!("CODE-001");
    h.engine
        .agent_finding(
            "TEST-1",
            "code_reviewer",
            Some(run),
            agentflow_core::engine::agents::finding_dump(&reraised).unwrap(),
        )
        .await
        .unwrap();
    complete(&h, "code_reviewer", run, json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","not_fixed")],"findings":[{"title":"New leak","severity":"high"}]})).await;
    let rows = findings(&h);
    assert!(rows.iter().any(|f| f["id"] == "CODE-001" && f["iteration"] == 2 && f["occurrence"] == 2));
    assert!(rows.iter().any(|f| f["id"] == "CODE-003" && f["iteration"] == 2 && f["occurrence"] == 1));
}

#[tokio::test]
async fn test_contract_check_sees_exactly_the_findings_that_get_stored() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker()]})).await;
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    rejected(&h, "code_reviewer", run, json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","fixed")],"findings":[{"prior_id":"CODE-001","blocking":false}]}), "must keep \"blocking\": true").await;
    h.with_task(|s| s.runs.last_mut().unwrap().run_dir = Some(h.tmp().join("review").to_string_lossy().into()));
    let mut f = blocker();
    f["priorId"] = json!("CODE-001");
    h.engine
        .agent_finding("TEST-1", "code_reviewer", Some(run), agentflow_core::engine::agents::finding_dump(&f).unwrap())
        .await
        .unwrap();
    complete(
        &h,
        "code_reviewer",
        run,
        json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","not_fixed")],"findings":[f]}),
    )
    .await;
    assert_eq!(findings(&h).iter().filter(|f| f["iteration"] == 2).count(), 1);
}

#[tokio::test]
async fn test_prior_id_is_refused_on_a_first_review() {
    let h = engine().await;
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    rejected(
        &h,
        "code_reviewer",
        run,
        json!({"reviewedSha":SHA,"findings":[{"priorId":"CODE-001"}]}),
        "no previous review",
    )
    .await;
}

#[tokio::test]
async fn test_blocker_raised_a_third_time_goes_to_a_human() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker()]})).await;
    let mut f = blocker();
    f["priorId"] = json!("CODE-001");
    let again = json!({"priorFindings":[verdict("CODE-001","not_fixed")],"findings":[f]});
    round(&h, again.clone()).await;
    assert_eq!(h.load().task.review_iteration, 3);
    round(&h, again).await;
    assert_eq!(h.load().task.status, "NEEDS_HUMAN_REVIEW");
    assert!(
        h.load().task.status_message.as_ref().unwrap().contains("Raised 3 or more times without being fixed: CODE-001")
    );
}

#[tokio::test]
async fn test_dispute_upheld_by_the_reviewer_goes_to_a_human() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker(),minor()]})).await;
    h.git.returns("checkpoint", agentflow_core::vcs::Outcome::Ok(json!([SHA, true])));
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    rejected(&h, "implementer", run, json!({}), "no response for blocking finding").await;
    rejected(
        &h,
        "implementer",
        run,
        json!({"findingResponses":[{"id":"CODE-001","action":"deferred","rationale":"later"}]}),
        "cannot be \"deferred\"",
    )
    .await;
    assert_eq!(complete(&h,"implementer",run,json!({"findingResponses":[{"id":"CODE-001","action":"disputed","rationale":"Input guaranteed"},{"id":"CODE-002","action":"deferred","rationale":"out of scope"}]})).await["ok"],true);
    let mut f = blocker();
    f["priorId"] = json!("CODE-001");
    round(&h, json!({"priorFindings":[verdict("CODE-001","not_fixed")],"findings":[f]})).await;
    assert!(
        h.load()
            .task
            .status_message
            .as_ref()
            .unwrap()
            .contains("Disputed by the implementer and upheld by the reviewer")
    );
}

#[test]
fn test_review_contract_checks_report_every_problem_at_once() {
    let raw = json!({"priorFindings":[{"id":"CODE-001","status":"resolved","evidence":""},{"id":"CODE-009","status":"fixed"}]});
    let p = review_loop::review_problems(raw.as_object().unwrap(), Some(&["CODE-001".into(), "CODE-002".into()]), &[]);
    assert_eq!(
        p,
        [
            "CODE-001: status must be one of fixed, not_fixed, withdrawn (got 'resolved')",
            "CODE-001: evidence is required (the file:line or test that shows it)",
            "priorFindings entry 'CODE-009' is not one of your previous blocking findings",
            "no verdict for prior blocking finding(s): CODE-002"
        ]
    );
    assert!(
        review_loop::response_problems(
            json!({"findingResponses":[{"id":["CODE-001"],"action":"fixed","rationale":"x"}]}).as_object().unwrap(),
            &[json!({"id":"CODE-001","blocking":true})]
        )[0]
        .starts_with("findingResponses entry")
    );
}

#[tokio::test]
async fn test_final_architect_history_keeps_the_reviewer_evidence_for_closed_findings() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker()]})).await;
    let task_id = h.load().task.id;
    let row = h
        .engine
        .db
        .session(|tx| table::select_one::<agentflow_core::db::ReviewFindingRow>(tx, "WHERE task_id = ?1", [task_id]))
        .unwrap()
        .unwrap();
    let mut row = row;
    row.status = "withdrawn".into();
    row.resolution = Some("Caller guarantees a value".into());
    row.response = Some("disputed".into());
    row.response_note = Some("Validated upstream".into());
    let closed = history_finding(&row);
    assert_eq!(closed["implementerResponse"]["action"], "disputed");
    assert!(closed.get("description").is_none());
}

#[tokio::test]
async fn test_changes_required_without_blocking_findings_passes() {
    let h = engine().await;
    round(&h, json!({"verdict":"changes_required","findings":[minor()]})).await;
    assert_eq!(state(&h, spec::CONSOLIDATE), "COMPLETED");
    assert_eq!(state(&h, spec::FINAL_ARCHITECT), "READY");
    assert_eq!(findings(&h)[0]["blocking"], false);
}

#[tokio::test]
async fn test_prefix_sha_rejected() {
    let h = engine().await;
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    complete(&h, "code_reviewer", run, json!({"reviewedSha":"a"})).await;
    assert_eq!(state(&h, spec::CODE_REVIEW), "FAILED");
}

#[tokio::test]
async fn test_incremental_findings_not_lost() {
    let h = engine().await;
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    h.with_task(|s| s.runs.last_mut().unwrap().run_dir = Some(h.tmp().join("review").to_string_lossy().into()));
    h.engine
        .agent_finding(
            "TEST-1",
            "code_reviewer",
            Some(run),
            agentflow_core::engine::agents::finding_dump(&blocker()).unwrap(),
        )
        .await
        .unwrap();
    complete(&h, "code_reviewer", run, json!({"reviewedSha":SHA,"findings":[minor()]})).await;
    assert_eq!(findings(&h).len(), 2);
}

#[tokio::test]
async fn test_invalid_result_failure_is_persisted() {
    let h = engine().await;
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    let response = complete(&h, "code_reviewer", run, json!({"verdict":"nonsense"})).await;
    assert_eq!(response["ok"], false);
    assert_eq!(state(&h, spec::CODE_REVIEW), "FAILED");
    assert!(h.load().runs.last().unwrap().error.as_ref().unwrap().starts_with("Result could not be accepted"));
}

#[test]
fn test_review_result_camel_case() {
    assert_eq!(normalize(json!({"reviewedSha":"abc"}).as_object().unwrap(), "review").unwrap()["reviewedSha"], "abc");
}
#[test]
fn test_final_assessment_risk_normalised() {
    assert_eq!(normalize(json!({"overallRisk":"low"}).as_object().unwrap(), "final").unwrap()["overallRisk"], "LOW");
}
#[test]
fn test_test_counts_cannot_be_negative_or_inconsistent() {
    for tests in [json!({"executed":-1}), json!({"executed":2,"passed":3}), json!({"executed":2,"passed":2,"failed":1})]
    {
        assert!(normalize(json!({"tests":tests}).as_object().unwrap(), "implementation").is_err());
    }
}

#[tokio::test]
async fn test_human_resume_increments_task_iteration() {
    let h = engine().await;
    h.launchable();
    h.with_task(|s| {
        s.task.status = "NEEDS_HUMAN_REVIEW".into();
        s.task.review_iteration = 2;
        let n = s.node_index(spec::IMPLEMENTER).unwrap();
        s.nodes[n].state = "READY".into();
    });
    h.engine.run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), false).await.unwrap();
    assert_eq!(h.load().task.review_iteration, 3);
    assert_eq!(h.load().task.status, "ACTIVE");
}

#[tokio::test]
async fn test_unverified_assessment_requires_reason_and_records_evidence() {
    let h = engine().await;
    let run = h.add_run(spec::FINAL_ARCHITECT, "RUNNING", None);
    complete(&h, "architect", run, json!({"requirementsVerified":false,"overallRisk":"HIGH"})).await;
    assert!(
        h.engine
            .approve(
                "TEST-1",
                spec::READY_FOR_PR,
                ApprovalRequest { expected_sha: Some(SHA.into()), ..Default::default() }
            )
            .await
            .unwrap_err()
            .message
            .contains("Override reason")
    );
    let snap = h
        .engine
        .approve(
            "TEST-1",
            spec::READY_FOR_PR,
            ApprovalRequest {
                expected_sha: Some(SHA.into()),
                reason: Some("Reviewed manually".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(snap["status"], "READY_FOR_PR");
    let event =
        snap["events"].as_array().unwrap().iter().find(|e| e["message"] == "Human overrode final blockers").unwrap();
    assert!(event["data"]["blockers"].as_array().unwrap().contains(&json!("Overall risk is high")));
}

#[tokio::test]
async fn test_request_requirements_changes_sends_architect_back_for_revision() {
    let h = engine().await;
    h.with_task(|s| {
        s.task.max_requirements_revisions = 3;
        let gate = s.node_index(spec::REQUIREMENTS_GATE).unwrap();
        s.nodes[gate].state = "WAITING_FOR_APPROVAL".into();
    });
    let snap = h.engine.request_requirements_changes("TEST-1", "  Please cover mobile too  ").await.unwrap();
    assert_eq!(state(&h, spec::REQUIREMENTS_GATE), "PENDING");
    assert_eq!(state(&h, spec::ARCHITECT), "READY");
    assert_eq!(snap["requirementsFeedback"][0]["feedback"], "Please cover mobile too");
}

#[tokio::test]
async fn test_request_requirements_changes_requires_nonblank_feedback() {
    let h = engine().await;
    h.with_task(|s| {
        let n = s.node_index(spec::REQUIREMENTS_GATE).unwrap();
        s.nodes[n].state = "WAITING_FOR_APPROVAL".into();
    });
    assert!(
        h.engine
            .request_requirements_changes("TEST-1", "   ")
            .await
            .unwrap_err()
            .message
            .contains("Feedback is required")
    );
}

#[tokio::test]
async fn test_request_requirements_changes_enforces_revision_cap() {
    let h = engine().await;
    h.with_task(|s| {
        s.task.max_requirements_revisions = 1;
        let n = s.node_index(spec::REQUIREMENTS_GATE).unwrap();
        s.nodes[n].state = "WAITING_FOR_APPROVAL".into();
        let n = s.node_index(spec::ARCHITECT).unwrap();
        s.nodes[n].iteration = 1;
    });
    assert!(
        h.engine
            .request_requirements_changes("TEST-1", "again")
            .await
            .unwrap_err()
            .message
            .contains("Maximum requirement revisions reached")
    );
}

#[tokio::test]
async fn test_failed_tests_block_even_if_final_assessment_says_verified() {
    let h = engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "COMPLETED", Some(json!({"tests":{"executed":2,"passed":1,"failed":1}})));
    let mut evidence = RunEvidenceRow {
        uid: new_uid(),
        run_id: run,
        requirements_digest: None,
        verification: Json(json!({"sha":SHA,"passed":true})),
        usage: Json(Value::Null),
        confirmed_model: None,
        billing_mode: "unknown".into(),
    };
    h.engine.db.session(|tx| table::insert(tx, &mut evidence)).unwrap();
    let run = h.add_run(spec::FINAL_ARCHITECT, "RUNNING", None);
    complete(
        &h,
        "architect",
        run,
        json!({"requirementsVerified":true,"overallRisk":"LOW","taskSummary":"done","testsPerformed":"tested"}),
    )
    .await;
    let snap = h.engine.snapshot("TEST-1").unwrap();
    assert!(snap["blockers"].as_array().unwrap().contains(&json!("Tests failed")));
}

#[tokio::test]
async fn test_high_security_risk_blocks_despite_low_overall_risk() {
    let h = engine().await;
    let run = h.add_run(spec::FINAL_ARCHITECT, "RUNNING", None);
    complete(&h, "architect", run, json!({"overallRisk":"LOW","securityRisk":"HIGH: unsafe"})).await;
    assert!(
        h.engine.snapshot("TEST-1").unwrap()["blockers"].as_array().unwrap().contains(&json!("Security risk is high"))
    );
}

fn earlier_code_review(h: &Harness, iteration: i64, failed: bool, verification: Option<Value>) {
    h.launchable();
    h.with_task(|s| s.task.review_iteration = 2);
    let old = h.add_run(spec::CODE_REVIEW, if failed { "FAILED" } else { "COMPLETED" }, None);
    engine_support::add_session(h, old);
    h.with_task(|s| {
        let run = s.runs.iter_mut().find(|r| r.id == old).unwrap();
        run.iteration = iteration;
        if failed {
            run.error = Some("Result could not be accepted: bad JSON".into());
        }
        let agent = s.agents.last_mut().unwrap();
        agent.role = "code_reviewer".into();
        agent.harness = "claude".into();
        agent.harness_session_id = Some("review-round-1".into());
        agent.status = "stopped".into();
        agent.zellij_pane_id = None;
        let n = s.node_index(spec::CODE_REVIEW).unwrap();
        s.nodes[n].state = "READY".into();
    });
    let implementation =
        h.add_run(spec::IMPLEMENTER, "COMPLETED", Some(json!({"summary":"Fixed every blocking finding"})));
    if let Some(verification) = verification {
        let mut row = RunEvidenceRow {
            uid: new_uid(),
            run_id: implementation,
            requirements_digest: None,
            verification: Json(verification),
            usage: Json(Value::Null),
            confirmed_model: None,
            billing_mode: "unknown".into(),
        };
        h.engine.db.session(|tx| table::insert(tx, &mut row)).unwrap();
    }
}
async fn latest_review_prompt(h: &Harness) -> String {
    h.engine.run_node("TEST-1", spec::CODE_REVIEW, Default::default(), false).await.unwrap();
    let s = h.load();
    let run = s.runs.last().unwrap();
    agentflow_core::pyfile::read_text(std::path::Path::new(run.prompt_path.as_ref().unwrap())).unwrap()
}
#[tokio::test]
async fn test_reviewer_starts_each_round_in_a_fresh_conversation() {
    let h = engine().await;
    earlier_code_review(&h, 1, false, None);
    latest_review_prompt(&h).await;
    assert!(!h.launch_script().contains("--resume"));
    assert!(
        h.engine.snapshot("TEST-1").unwrap()["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"].as_str().unwrap().contains("fresh conversation for review round 2"))
    );
}
#[tokio::test]
async fn test_reviewer_retry_within_the_same_round_still_resumes() {
    let h = engine().await;
    earlier_code_review(&h, 2, true, None);
    latest_review_prompt(&h).await;
    assert!(h.launch_script().contains("--resume"));
    assert!(h.launch_script().contains("review-round-1"));
}
/// A resumed agent remembers its old run id; if it skips the new briefing it would keep
/// signalling with that id and every signal would be rejected as stale.
#[tokio::test]
async fn test_resumed_conversation_is_told_its_new_run_id() {
    let h = engine().await;
    earlier_code_review(&h, 2, true, None);
    latest_review_prompt(&h).await;
    let run_id = h.load().runs.last().unwrap().id;
    assert!(h.launch_script().contains(&format!("--run {run_id} on every flowctl command")));
}
#[tokio::test]
async fn test_reviewer_sees_agentflow_verification_before_the_implementer_account() {
    let h = engine().await;
    earlier_code_review(
        &h,
        1,
        false,
        Some(
            json!({"sha":SHA,"passed":true,"commands":[{"command":["dotnet","test"],"exitCode":0,"output":"Passed: 41"}]}),
        ),
    );
    let text = latest_review_prompt(&h).await;
    assert!(text.contains("`dotnet test` exited with code 0"));
    assert!(text.contains("Passed: 41"));
    assert!(
        text.find("## Independent verification").unwrap() < text.find("## Implementer's account (unverified)").unwrap()
    );
    assert!(
        text.find("## Implementer's account (unverified)").unwrap()
            < text.find("Fixed every blocking finding").unwrap()
    );
}
#[tokio::test]
async fn test_verification_of_another_checkpoint_is_not_shown_as_evidence() {
    let h = engine().await;
    earlier_code_review(
        &h,
        1,
        false,
        Some(
            json!({"sha":"c".repeat(40),"passed":true,"commands":[{"command":["dotnet","test"],"exitCode":0,"output":"ok"}]}),
        ),
    );
    assert!(latest_review_prompt(&h).await.contains("AgentFlow ran no verification commands for this checkpoint"));
}
#[tokio::test]
async fn test_history_finding_with_open_status() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker()]})).await;
    let task_id = h.load().task.id;
    let row = h
        .engine
        .db
        .session(|tx| table::select_one::<agentflow_core::db::ReviewFindingRow>(tx, "WHERE task_id=?1", [task_id]))
        .unwrap()
        .unwrap();
    let history = history_finding(&row);
    assert_eq!(history["status"], "open");
    assert_eq!(history["description"], Value::Null);
    assert!(history.get("resolution").is_none());
}
#[tokio::test]
async fn test_history_finding_with_superseded_status() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker()]})).await;
    let task_id = h.load().task.id;
    let mut row = h
        .engine
        .db
        .session(|tx| table::select_one::<agentflow_core::db::ReviewFindingRow>(tx, "WHERE task_id=?1", [task_id]))
        .unwrap()
        .unwrap();
    row.status = "superseded".into();
    let history = history_finding(&row);
    assert_eq!(history["status"], "superseded");
    assert!(history.get("description").is_none());
    assert!(history.get("blocking").is_none());
}
#[tokio::test]
async fn a_failed_review_artifact_write_does_not_insert_or_resolve_findings() {
    let h = engine().await;
    round(&h, json!({"findings":[blocker()]})).await;
    let path = agentflow_core::engine::snapshot::workspace(&h.load().task).findings_dir.join("review-02.json");
    std::fs::create_dir_all(&path).unwrap();
    let run = h.add_run(spec::CODE_REVIEW, "RUNNING", None);
    h.with_task(|s| s.runs.last_mut().unwrap().run_dir = Some(h.tmp().join("review").to_string_lossy().into()));
    let response = complete(
        &h,
        "code_reviewer",
        run,
        json!({"reviewedSha":SHA,"priorFindings":[verdict("CODE-001","fixed")],"findings":[minor()]}),
    )
    .await;
    assert_eq!(response["ok"], false);
    assert_eq!(findings(&h).len(), 1);
    assert_eq!(findings(&h)[0]["status"], "open");
}
