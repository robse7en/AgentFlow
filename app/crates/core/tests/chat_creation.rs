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
fn create_settings() -> SlackConfig {
    SlackConfig { allow_actions: vec![SlackAction::Create], ..Default::default() }
}
#[tokio::test]
async fn failed_repository_lookups_are_retried_without_waiting_for_the_cache() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, create_settings());
    h.git.then("inspect_repository", Outcome::Failed("PRIVATE".into()));
    h.git.returns(
        "inspect_repository",
        Outcome::Ok(json!({"path":"repo","root":"repo","branches":["main","feature/retry"]})),
    );
    let request = OptionsRequest {
        user_id: "U-operator".into(),
        field: "branch".into(),
        query: String::new(),
        repository: Some("repo".into()),
    };
    assert!(s.options(&request).await.is_empty());
    assert_eq!(s.options(&request).await.len(), 2);
    assert_eq!(s.options(&request).await.len(), 2);
    assert_eq!(h.git.calls_of("inspect_repository").len(), 2);
}
#[tokio::test]
async fn test_the_project_field_offers_the_repository_folders() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, create_settings());
    h.git.returns("inspect_repository", Outcome::Ok(json!({"path":"repo","root":"repo","branches":[]})));
    h.git.returns(
        "directories",
        Outcome::Ok(json!(["backend", "backend/src", "frontend", ".git", "nested/.hidden", "node_modules/lib"])),
    );
    let request = OptionsRequest {
        user_id: "U-operator".into(),
        field: "project".into(),
        query: String::new(),
        repository: Some("repo".into()),
    };
    let all = s.options(&request).await;
    assert_eq!(all[0], (".".into(), "Whole repository".into()));
    assert_eq!(all.len(), 4);
    let typed = s.options(&OptionsRequest { query: "new\\folder".into(), ..request }).await;
    assert_eq!(typed, vec![("new/folder".into(), "new/folder (as typed)".into())]);
    assert_eq!(h.git.calls_of("directories").len(), 1);
}
#[tokio::test]
async fn test_creating_tasks_is_off_by_default() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    assert!(s.new_task_reply(Some("TEST-2")).buttons.is_empty());
    assert!(s.new_task_form("U-operator", None).unwrap_err().contains("slack.allowActions"));
    assert!(
        s.options(&OptionsRequest {
            user_id: "U-operator".into(),
            field: "branch".into(),
            query: String::new(),
            repository: Some("repo".into())
        })
        .await
        .is_empty()
    );
    assert!(h.git.methods().is_empty());
}
#[tokio::test]
async fn test_plain_words_ask_for_the_create_button() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, create_settings());
    for phrase in ["create a new task for test-2", "please make me a task TEST-2", "open a task for TEST-2, please!"] {
        let reply = s.message(&message(phrase, None)).await.unwrap();
        assert_eq!(reply.buttons[0].kind, "new_task");
        assert_eq!(reply.buttons[0].value, "TEST-2");
    }
    assert!(s.message(&message("I do not want to create a task", None)).await.unwrap().buttons.is_empty());
}
#[tokio::test]
async fn test_new_offers_a_button_that_opens_a_prefilled_form() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, create_settings());
    h.configure(|c| c.workspace.default_repository_directory = Some("repo".into()));
    let reply = s.new_task_reply(Some("test-2"));
    assert_eq!(reply.buttons[0].value, "TEST-2");
    let form = s.new_task_form("U-operator", Some(" test-2 ")).unwrap();
    assert_eq!(form.jira_key.as_deref(), Some("TEST-2"));
    assert_eq!(form.repository.as_deref(), Some("repo"));
    assert_eq!(form.base_branch, "main");
    assert!(h.engine.snapshot("TEST-2").is_err());
}
#[tokio::test]
async fn text_form_uses_the_engine_canonical_key() {
    let h = engine_support::engine().await;
    h.set_status("READY_FOR_PR");
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::MarkPr], ..Default::default() });
    assert_eq!(s.text_form("U-operator", "mark_pr", "test-1").unwrap().value, "TEST-1");
    assert_eq!(
        s.submit_text(TextSubmission {
            user_id: "U-operator".into(),
            kind: "mark_pr".into(),
            value: "test-1".into(),
            text: "https://git.example/pr/8".into()
        }),
        json!({})
    );
    until(|| h.engine.snapshot("TEST-1").unwrap()["prUrl"] == "https://git.example/pr/8").await;
    s.stop();
}
