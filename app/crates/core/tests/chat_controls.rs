mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::{actions, models::*},
    config::{SlackAction, SlackConfig},
    engine::{launch::RunNodeRequest, spec},
};
use chat_support::*;
use serde_json::json;
use std::sync::Arc;
fn settings() -> SlackConfig {
    SlackConfig {
        allow_actions: vec![
            SlackAction::Run,
            SlackAction::Cancel,
            SlackAction::MarkPr,
            SlackAction::RequestChanges,
            SlackAction::ApproveRequirements,
        ],
        ..Default::default()
    }
}
#[tokio::test]
async fn test_ready_only_runs_refuse_retries_and_resumes_under_the_lock() {
    let h = engine_support::engine().await;
    h.add_run(spec::IMPLEMENTER, "FAILED", None);
    assert!(
        h.engine
            .run_node("TEST-1", spec::IMPLEMENTER, RunNodeRequest::default(), true)
            .await
            .unwrap_err()
            .message
            .contains("no longer ready")
    );
    h.with_task(|t| {
        let i = t.node_index(spec::ARCHITECT).unwrap();
        t.nodes[i].state = "READY".into();
    });
    h.set_status("NEEDS_HUMAN_REVIEW");
    let before = h.load().task.review_iteration;
    assert!(h.engine.run_node("TEST-1", spec::ARCHITECT, RunNodeRequest::default(), true).await.is_err());
    assert_eq!(h.load().task.review_iteration, before);
    assert_eq!(h.load().task.status, "NEEDS_HUMAN_REVIEW");
}
#[tokio::test]
async fn test_the_engine_checks_pr_links_and_feedback_itself() {
    let h = engine_support::engine().await;
    h.set_status("READY_FOR_PR");
    for bad in
        ["javascript:alert(1)".into(), "https://x.io/pr/1\nInjected".into(), format!("https://{}", "x".repeat(2000))]
    {
        assert!(h.engine.mark_pr_created("TEST-1", &bad).await.unwrap_err().message.contains("PR URL must"));
    }
    h.engine.mark_pr_created("TEST-1", "  https://git.example/pr/1  ").await.unwrap();
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["prUrl"], "https://git.example/pr/1");
    h.set_status("ACTIVE");
    gate(&h, &[]);
    assert!(
        h.engine.request_requirements_changes("TEST-1", &"x".repeat(4001)).await.unwrap_err().message.contains("4000")
    );
}
#[tokio::test]
async fn test_acting_commands_are_off_by_default() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    for verb in ["cancel", "run", "changes", "approve", "pr"] {
        let reply = s.message(&message(&format!("{verb} TEST-1"), None)).await.unwrap();
        assert!(reply.text.contains("turned off"));
        assert!(reply.buttons.is_empty());
    }
    assert_eq!(h.load().task.status, "ACTIVE");
}
#[tokio::test]
async fn test_slash_command_buttons_are_posted_in_the_dm() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    let reply = s.command("U-operator", "cancel TEST-1").await;
    assert!(reply.text.contains("direct messages"));
    let calls = a.calls.lock().unwrap();
    assert_eq!(calls[0].0, "chat.postMessage");
    assert_eq!(calls[0].1["channel"], "D-operator");
    assert!(calls[0].1["blocks"].to_string().contains("confirm"));
    assert_eq!(h.load().task.status, "ACTIVE");
}
#[tokio::test]
async fn test_run_typed_offers_what_can_start_and_explains_the_rest() {
    let h = engine_support::engine().await;
    h.launchable();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    let reply = s.message(&message("run TEST-1", None)).await.unwrap();
    assert_eq!(reply.buttons.len(), 1);
    assert_eq!(reply.buttons[0].value, "TEST-1|architect");
    let reply = s.message(&message("run TEST-1 implementer", None)).await.unwrap();
    assert!(reply.buttons.is_empty());
    assert!(reply.text.contains("PENDING"));
    assert!(s.message(&message("run TEST-1 missing", None)).await.unwrap().text.contains("no stage"));
}
#[tokio::test]
async fn test_run_in_a_thread_can_name_just_the_stage() {
    let h = engine_support::engine().await;
    h.launchable();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    let reply = s.action_reply("run", &["Architect"], Some("TEST-1"));
    assert_eq!(reply.buttons[0].value, "TEST-1|architect");
    assert!(s.action_reply("run", &["Code", "Review"], Some("TEST-1")).text.contains("PENDING"));
}
#[tokio::test]
async fn test_pr_typed_follows_the_task_status() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    for (status, kind) in
        [("ACTIVE", None), ("READY_FOR_PR", Some("mark_pr")), ("PR_CREATED", Some("mark_merged")), ("MERGED", None)]
    {
        h.set_status(status);
        let reply = s.message(&message("pr TEST-1", None)).await.unwrap();
        assert_eq!(reply.buttons.first().map(|b| b.kind.as_str()), kind);
    }
}
#[tokio::test]
async fn test_cancel_and_merge_buttons_act_through_the_engine() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert!(s.action(&action("mark_merged", "TEST-1")).await.is_some());
    h.set_status("PR_CREATED");
    assert!(s.action(&action("mark_merged", "TEST-1")).await.is_none());
    assert_eq!(h.load().task.status, "MERGED");
    h.set_status("ACTIVE");
    h.launchable();
    assert!(s.action(&action("cancel", "TEST-1")).await.is_none());
    assert_eq!(h.load().task.status, "CANCELLED");
    assert!(std::path::Path::new(&h.load().task.worktree_path).exists());
    assert_eq!(h.load().node(spec::ARCHITECT).unwrap().state, "CANCELLED");
}
#[tokio::test]
async fn test_a_stage_that_changed_since_the_check_is_refused_by_the_engine() {
    let h = engine_support::engine().await;
    h.launchable();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    let reply = s.message(&message("run TEST-1", None)).await.unwrap();
    h.add_run(spec::ARCHITECT, "RUNNING", None);
    assert!(s.action(&action("run", &reply.buttons[0].value)).await.unwrap().contains("RUNNING"));
    assert!(!h.terminals.calls().iter().any(|c| c.starts_with("launch")));
}
#[tokio::test]
async fn test_requested_changes_go_to_the_architect_with_a_run_button() {
    let h = engine_support::engine().await;
    h.launchable();
    gate(&h, &[("requirements.md", "Scope")]);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert_eq!(
        s.submit_text(TextSubmission {
            user_id: "U-operator".into(),
            kind: "request_changes".into(),
            value: "TEST-1".into(),
            text: "Cover admin".into()
        }),
        json!({})
    );
    until(|| a.calls.lock().unwrap().iter().any(|(_, v)| v["text"] == "Requirements sent back to the Architect."))
        .await;
    let calls = a.calls.lock().unwrap();
    let reply = &calls.iter().find(|(_, v)| v["text"] == "Requirements sent back to the Architect.").unwrap().1;
    assert!(reply["blocks"].to_string().contains("TEST-1|architect"));
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["requirementsFeedback"][0]["feedback"], "Cover admin");
}
#[tokio::test]
async fn test_notices_carry_the_action_they_invite() {
    let h = engine_support::engine().await;
    h.launchable();
    let snapshot = h.engine.snapshot("TEST-1").unwrap();
    let cfg = settings();
    assert_eq!(actions::notice_buttons(&snapshot, "node:architect:READY", &cfg, None)[0].kind, "run");
    assert_eq!(
        actions::notice_buttons(&snapshot, "node:requirements_gate:WAITING_FOR_APPROVAL", &cfg, None)[0].kind,
        "review_requirements"
    );
    assert_eq!(actions::notice_buttons(&snapshot, "status:READY_FOR_PR", &cfg, None)[0].kind, "mark_pr");
}
#[tokio::test]
async fn test_a_ready_stage_starts_from_a_button() {
    let h = engine_support::engine().await;
    h.launchable();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert!(s.action(&action("run", "TEST-1|architect")).await.is_none());
    assert_eq!(h.load().node(spec::ARCHITECT).unwrap().state, "RUNNING");
    assert!(s.action(&action("run", "TEST-1|architect")).await.unwrap().contains("RUNNING"));
    h.engine.shutdown();
}
#[tokio::test]
async fn test_starting_a_stage_is_refused_when_the_ui_must_decide() {
    let h = engine_support::engine().await;
    h.launchable();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert!(
        s.action(&ActionRequest { user_id: "intruder".into(), ..action("run", "TEST-1|architect") })
            .await
            .unwrap()
            .contains("not authorized")
    );
    assert!(
        s.action(&ActionRequest { channel: "public".into(), ..action("run", "TEST-1|architect") })
            .await
            .unwrap()
            .contains("direct messages")
    );
    for value in ["MISSING-1|architect", "TEST-1|unknown"] {
        assert!(s.action(&action("run", value)).await.unwrap().contains("no longer exists"));
    }
    h.set_status("NEEDS_HUMAN_REVIEW");
    assert!(s.action(&action("run", "TEST-1|architect")).await.unwrap().contains("start stages in AgentFlow"));
}
#[tokio::test]
async fn test_a_launch_the_engine_declines_is_reported() {
    let h = engine_support::engine().await;
    h.launchable();
    h.configure(|c| c.workflow.max_concurrent_agents = 0);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    let reason = s.action(&action("run", "TEST-1|architect")).await.unwrap();
    assert!(reason.contains("capacity"));
    assert_eq!(h.load().node(spec::ARCHITECT).unwrap().state, "READY");
}
