mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::actions,
    config::{SlackAction, SlackConfig},
};
use chat_support::*;
use serde_json::json;
use std::sync::Arc;
fn settings() -> SlackConfig {
    SlackConfig {
        allow_actions: vec![SlackAction::ApproveRequirements, SlackAction::RequestChanges],
        ..Default::default()
    }
}
#[tokio::test]
async fn test_approving_from_slack_is_off_by_default() {
    let h = engine_support::engine().await;
    gate(&h, &[("requirements.md", "Scope")]);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    assert!(s.message(&message("approve TEST-1", None)).await.unwrap().text.contains("turned off"));
    for kind in ["review_requirements", "approve_requirements"] {
        assert!(s.action(&action(kind, "TEST-1")).await.unwrap().contains("turned off"));
    }
    assert!(a.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_review_posts_every_document_in_full_then_a_bound_approve_button() {
    let h = engine_support::engine().await;
    gate(
        &h,
        &[
            ("requirements.md", "Must <!channel> log in & out."),
            ("acceptance-criteria.md", "Tests pass."),
            ("implementation-plan.md", "Edit auth.py."),
        ],
    );
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert_eq!(s.message(&message("approve test-1", None)).await.unwrap().buttons[0].kind, "review_requirements");
    assert!(s.action(&action("review_requirements", "TEST-1")).await.is_none());
    let calls = a.calls.lock().unwrap();
    let posted = calls.iter().map(|(_, v)| v["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join("\n");
    for text in ["Must &lt;!channel&gt; log in &amp; out.", "Tests pass.", "Edit auth.py."] {
        assert!(posted.contains(text));
    }
    let last = calls.last().unwrap().1.to_string();
    assert!(last.contains("agentflow_approve_requirements"));
    assert!(last.contains(h.engine.snapshot("TEST-1").unwrap()["requirementsDraftDigest"].as_str().unwrap()));
}
#[tokio::test]
async fn test_requirements_changed_after_they_were_posted_are_not_approved() {
    let h = engine_support::engine().await;
    gate(&h, &[("requirements.md", "Scope"), ("implementation-plan.md", "Edit auth")]);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    s.action(&action("review_requirements", "TEST-1")).await;
    let bound = approval(&a);
    std::fs::write(h.task_dir().join("TEST-1/requirements/implementation-plan.md"), "Drop tests").unwrap();
    assert!(s.action(&action("approve_requirements", &bound)).await.unwrap().contains("Requirements changed"));
    assert!(s.action(&action("approve_requirements", "TEST-1|bad-digest")).await.unwrap().contains("not valid"));
    assert!(h.engine.snapshot("TEST-1").unwrap()["approvedRequirements"].is_null());
}
#[tokio::test]
async fn test_long_or_unready_requirements_are_left_to_agentflow() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert!(s.action(&action("review_requirements", "TEST-1")).await.unwrap().contains("not waiting"));
    gate(&h, &[("requirements.md", &"x\n".repeat(31000))]);
    assert!(s.action(&action("review_requirements", "TEST-1")).await.unwrap().contains("too long"));
    assert!(a.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_every_document_the_digest_covers_is_shown() {
    let h = engine_support::engine().await;
    gate(
        &h,
        &[("requirements.md", "Scope"), ("risks.md", "Rollback the migration"), ("assumptions.md", "All signed in")],
    );
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert!(s.action(&action("review_requirements", "TEST-1")).await.is_none());
    let calls = a.calls.lock().unwrap();
    let posted = calls.iter().map(|(_, v)| v["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join("\n");
    assert!(posted.contains("Rollback the migration"));
    assert!(posted.contains("All signed in"));
    assert!(calls.last().unwrap().1.to_string().contains("approve_requirements"));
    // Future document keys are rendered in full without a title allowlist.
    assert!(
        agentflow_core::chat::rich::requirements("TEST-1", &[("rollbackPlan".into(), "Revert it".into())])
            .join("\n")
            .contains("Revert it")
    );
}
#[tokio::test]
async fn test_no_approve_button_when_there_is_nothing_to_show() {
    let h = engine_support::engine().await;
    gate(&h, &[]);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    assert!(s.action(&action("review_requirements", "TEST-1")).await.unwrap().contains("no requirements"));
    assert!(a.calls.lock().unwrap().is_empty());
}
#[test]
fn test_the_gate_notice_offers_review_and_changes() {
    let snapshot = json!({"jiraKey":"TEST-1"});
    let buttons = actions::notice_buttons(&snapshot, "node:requirements_gate:WAITING_FOR_APPROVAL", &settings(), None);
    assert_eq!(buttons.iter().map(|b| b.kind.as_str()).collect::<Vec<_>>(), ["review_requirements", "request_changes"]);
}
#[tokio::test]
async fn test_slack_review_and_approve_buttons_reach_the_service() {
    let h = engine_support::engine().await;
    gate(
        &h,
        &[("requirements.md", "Scope"), ("acceptance-criteria.md", "Tests"), ("implementation-plan.md", "Edit auth")],
    );
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings());
    s.start();
    emit(&a, clicked("review", "review_requirements", "TEST-1", "U-operator", "D-operator")).await;
    until(|| a.calls.lock().unwrap().iter().any(|(_, v)| v.to_string().contains("agentflow_approve_requirements")))
        .await;
    assert!(!a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.update"));
    let bound = approval(&a);
    let mut click = clicked("approve", "approve_requirements", &bound, "U-operator", "D-operator");
    click["payload"]["message"]["blocks"] = json!([{"type":"section","text":{"type":"mrkdwn","text":"Approve these requirements for TEST-1?"}},{"type":"actions","elements":[]}]);
    emit(&a, click).await;
    until(|| {
        a.calls.lock().unwrap().iter().any(|(m, v)| {
            m == "chat.update" && v["text"] == "Approve these requirements for TEST-1?\n*Requirements approved.*"
        })
    })
    .await;
    assert!(h.engine.snapshot("TEST-1").unwrap()["approvedRequirements"].is_object());
    s.stop();
}
