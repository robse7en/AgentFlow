mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::models::*,
    config::{SlackAction, SlackConfig},
    vcs::Outcome,
};
use chat_support::*;
use serde_json::json;
use std::sync::Arc;
fn setup(h: &engine_support::Harness) {
    h.usable_harnesses();
    h.git.returns(
        "inspect_repository",
        Outcome::Ok(json!({"path":h.tmp(),"root":h.tmp(),"current_branch":"main","branches":["main","feature/a"]})),
    );
    h.git.returns("create_worktree", Outcome::Ok(json!({"branch": "agent/NEW-1"})));
}
fn draft(h: &engine_support::Harness) -> TaskDraft {
    TaskDraft {
        user_id: "U-operator".into(),
        jira_key: "NEW-1".into(),
        repository_path: h.tmp().to_string_lossy().into(),
        base_branch: "main".into(),
        project_relative_path: None,
        implementation_mode: Some("direct".into()),
        additional_context: None,
    }
}
fn settings(run: bool) -> SlackConfig {
    SlackConfig {
        allow_actions: if run { vec![SlackAction::Create, SlackAction::Run] } else { vec![SlackAction::Create] },
        ..Default::default()
    }
}
#[tokio::test]
async fn test_form_fields_offer_repositories_branches_and_what_was_typed() {
    let h = engine_support::engine().await;
    setup(&h);
    h.configure(|c| c.workspace.default_repository_directory = Some(h.tmp().to_string_lossy().into()));
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings(false));
    let repository = h.tmp().to_string_lossy().into_owned();
    let request = OptionsRequest {
        user_id: "U-operator".into(),
        field: "repository".into(),
        query: String::new(),
        repository: Some(repository.clone()),
    };
    assert!(s.options(&request).await.iter().any(|(v, _)| v == &repository));
    let branches = s.options(&OptionsRequest { field: "branch".into(), ..request.clone() }).await;
    assert_eq!(branches.len(), 2);
    let typed = s.options(&OptionsRequest { field: "branch".into(), query: "fresh".into(), ..request }).await;
    assert_eq!(typed.last().unwrap().0, "fresh");
    assert_eq!(h.git.calls_of("inspect_repository").len(), 1);
}
#[tokio::test]
async fn test_the_form_is_checked_quickly_and_creation_runs_afterwards() {
    let h = engine_support::engine().await;
    setup(&h);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings(false));
    assert!(s.submit_task(TaskDraft { jira_key: "ab c".into(), ..draft(&h) }).get("ticket").is_some());
    assert_eq!(
        s.submit_task(TaskDraft { repository_path: " ".into(), base_branch: String::new(), ..draft(&h) }),
        json!({"repository":"Choose a repository.","branch":"Choose a base branch."})
    );
    assert_eq!(
        s.submit_task(TaskDraft { jira_key: "test-1".into(), ..draft(&h) }),
        json!({"ticket":"A task for TEST-1 already exists."})
    );
    assert!(h.git.methods().is_empty());
    let lock = h.engine.lock("NEW-1");
    let held = lock.lock().await;
    assert_eq!(
        s.submit_task(TaskDraft { jira_key: "new-1".into(), additional_context: Some("  ".into()), ..draft(&h) }),
        json!({})
    );
    assert!(h.engine.snapshot("NEW-1").is_err());
    drop(held);
    until(|| h.engine.snapshot("NEW-1").is_ok()).await;
    assert!(h.engine.snapshot("NEW-1").unwrap()["additionalContext"].is_null());
    s.stop();
}
#[tokio::test]
async fn test_a_created_task_opens_its_thread_with_a_run_button() {
    let h = engine_support::engine().await;
    setup(&h);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings(true));
    s.start();
    assert_eq!(s.submit_task(draft(&h)), json!({}));
    until(|| a.sent.lock().unwrap().iter().any(|m| m.text.starts_with("Created from Slack"))).await;
    let (thread, intro) = {
        let sent = a.sent.lock().unwrap();
        let intro = sent.iter().find(|m| m.text.starts_with("Created from Slack")).unwrap();
        (intro.thread_id.clone().unwrap(), intro.clone())
    };
    assert_eq!(intro.text, "Created from Slack: branch `agent/NEW-1` from `main`.");
    assert_eq!(intro.buttons[0].value, "NEW-1|architect");
    assert_eq!(s.thread("NEW-1").as_deref(), Some(thread.as_str()));
    assert!(s.message(&message("status", Some(&thread))).await.unwrap().text.starts_with("*NEW-1*"));
    s.queue("NEW-1", None);
    s.stop();
    assert_eq!(a.sent.lock().unwrap().iter().filter(|m| m.text.starts_with("Created from Slack")).count(), 1);
}
#[tokio::test]
async fn test_a_stage_that_cannot_start_from_slack_is_explained_not_offered() {
    let h = engine_support::engine().await;
    setup(&h);
    h.configure(|c| c.workflow.cost_controls.require_metered_approval = true);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings(true));
    s.start();
    s.submit_task(draft(&h));
    until(|| a.sent.lock().unwrap().iter().any(|m| m.text.starts_with("Created from Slack"))).await;
    let sent = a.sent.lock().unwrap();
    let intro = sent.iter().find(|m| m.text.starts_with("Created from Slack")).unwrap();
    assert!(intro.text.contains("API launch approval"));
    assert!(intro.buttons.is_empty());
    drop(sent);
    s.stop();
}
#[tokio::test]
async fn test_no_run_button_unless_running_is_allowed() {
    let h = engine_support::engine().await;
    setup(&h);
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings(false));
    s.start();
    s.submit_task(draft(&h));
    until(|| a.sent.lock().unwrap().iter().any(|m| m.text.starts_with("Created from Slack"))).await;
    assert!(
        a.sent.lock().unwrap().iter().find(|m| m.text.starts_with("Created from Slack")).unwrap().buttons.is_empty()
    );
    s.stop();
}
#[tokio::test]
async fn test_a_failed_creation_is_a_direct_message_without_git_output() {
    let h = engine_support::engine().await;
    h.git.returns("inspect_repository", Outcome::Failed("PRIVATE token=secret <b>".into()));
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, settings(false));
    s.submit_task(draft(&h));
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postMessage")).await;
    let calls = a.calls.lock().unwrap();
    let v = &calls[0].1;
    assert!(v["text"].as_str().unwrap().contains("Could not create NEW-1"));
    assert!(!v.to_string().contains("PRIVATE"));
    assert!(v.get("thread_ts").is_none());
    assert!(h.engine.snapshot("NEW-1").is_err());
}
