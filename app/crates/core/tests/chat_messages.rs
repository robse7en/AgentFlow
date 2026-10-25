mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::{models::*, notifications::Notifier},
    config::{SlackAction, SlackConfig, SlackDetail},
    engine::spec,
};
use chat_support::*;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
async fn threaded(
    h: &engine_support::Harness,
    a: &Arc<Adapter>,
    settings: SlackConfig,
) -> (Arc<agentflow_core::chat::service::ChatService>, String) {
    question(h).await;
    let s = service(h, a, settings);
    Notifier::new(h.engine.db.clone(), a.clone(), s.settings.clone(), "D-operator".into())
        .process(&h.engine.snapshot("TEST-1").unwrap(), false)
        .await
        .unwrap();
    let thread = s.thread("TEST-1").unwrap();
    (s, thread)
}
#[tokio::test]
async fn test_typed_messages_use_the_same_commands_as_the_slash_command() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    for text in ["list", "Status test-1", "status", "digest", "help"] {
        assert_eq!(s.message(&message(text, None)).await.unwrap().text, s.command("U-operator", text).await.text);
    }
}
#[tokio::test]
async fn test_messages_from_anyone_else_or_anywhere_else_get_no_reply() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    assert!(s.message(&MessageRequest { user_id: "intruder".into(), ..message("list", None) }).await.is_none());
    assert!(s.message(&MessageRequest { channel: "public".into(), ..message("list", None) }).await.is_none());
    assert!(a.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_a_reply_in_a_task_thread_is_about_that_task() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let (s, thread) = threaded(&h, &a, SlackConfig::default()).await;
    assert!(s.message(&message("status", Some(&thread))).await.unwrap().text.starts_with("*TEST-1*"));
    assert!(s.message(&message("status", Some("unknown"))).await.unwrap().text.contains("TEST-1"));
    assert!(
        s.message(&MessageRequest { channel: "public".into(), ..message("status", Some(&thread)) }).await.is_none()
    );
}
#[tokio::test]
async fn test_no_offer_when_answering_is_off() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let (s, thread) = threaded(&h, &a, SlackConfig { allow_actions: vec![], ..Default::default() }).await;
    let reply = s.message(&message("Gradual", Some(&thread))).await.unwrap();
    assert!(reply.offer.is_none());
    assert!(reply.text.contains("`list`"));
}
#[tokio::test]
async fn test_details_are_off_by_default() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    for text in ["details TEST-1", "activity TEST-1"] {
        assert!(s.command("U-operator", text).await.text.starts_with("Task details are off."));
    }
}
#[tokio::test]
async fn test_details_post_only_the_allowed_sections() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let (s, thread) = threaded(
        &h,
        &a,
        SlackConfig { details: vec![SlackDetail::Requirements, SlackDetail::Activity], ..Default::default() },
    )
    .await;
    assert_eq!(
        s.command("U-operator", "details TEST-1 findings").await.text,
        "Not enabled in slack.details: findings."
    );
    let text = s.message(&message("details", Some(&thread))).await.unwrap().text;
    assert!(text.contains("*Requirements*"));
    assert!(text.contains("*Recent activity*"));
    assert!(!text.contains("*Cost*"));
    assert!(!text.contains("*Open findings*"));
    let text = s.command("U-operator", "activity TEST-1 1").await.text;
    assert!(!text.contains("*Requirements*"));
    assert_eq!(text.matches("\n• ").count(), 1);
}
#[tokio::test]
async fn test_requirements_changed_after_approval_are_not_called_approved() {
    let h = engine_support::engine().await;
    std::fs::write(h.task_dir().join("TEST-1/requirements/requirements.md"), "Rewritten after approval").unwrap();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { details: vec![SlackDetail::Requirements], ..Default::default() });
    let text = s.command("U-operator", "details TEST-1").await.text;
    assert!(text.contains("changed since approval"));
    assert!(text.contains("Rewritten after approval"));
}
#[tokio::test]
async fn test_the_synced_ticket_is_read_only_when_ticket_details_are_asked_for() {
    let h = engine_support::engine().await;
    let dir = h.task_dir().join("TEST-1/context");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("jira-issue.md"), "# TEST-1: Title\n\n## Description\n\nTICKET-TEXT\n\n## Comments (0)\n")
        .unwrap();
    let a = Arc::new(Adapter::default());
    for (allowed, command, shared) in [
        (vec![SlackDetail::Pr, SlackDetail::Cost], "details TEST-1", false),
        (vec![SlackDetail::Pr, SlackDetail::Ticket], "details TEST-1 pr", false),
        (vec![SlackDetail::Pr, SlackDetail::Ticket], "details TEST-1", true),
    ] {
        let s = service(&h, &a, SlackConfig { details: allowed, ..Default::default() });
        assert_eq!(s.command("U-operator", command).await.text.contains("TICKET-TEXT"), shared);
    }
    assert!(h.git.methods().is_empty());
}
#[tokio::test]
async fn test_digest_lists_what_waits_and_shows_spend_only_when_cost_is_allowed() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let (s, _) = threaded(&h, &a, SlackConfig::default()).await;
    let text = s.command("U-operator", "digest").await.text;
    assert!(text.starts_with("*AgentFlow digest*: 1 active task, 1 waiting on you"));
    assert!(!text.contains("API spend"));
    let s = service(&h, &a, SlackConfig { details: vec![SlackDetail::Cost], ..Default::default() });
    assert!(s.command("U-operator", "digest").await.text.contains("Today's API spend: $0.00"));
}
fn event(id: &str, text: &str) -> Value {
    json!({"envelope_id":id,"type":"events_api","payload":{"event_id":id,"event":{"type":"message","user":"U-operator","channel":"D-operator","text":text,"ts":"message-ts"}}})
}
#[tokio::test]
async fn test_slack_dm_messages_reach_the_service_and_bots_are_ignored() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    let mut bot = event("bot", "status TEST-1");
    bot["payload"]["event"]["bot_id"] = "BOT".into();
    emit(&a, bot).await;
    let mut subtype = event("subtype", "status TEST-1");
    subtype["payload"]["event"]["subtype"] = "message_changed".into();
    emit(&a, subtype).await;
    emit(&a, event("valid", "status TEST-1")).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "reactions.remove")).await;
    assert_eq!(a.calls.lock().unwrap().iter().filter(|(m, _)| m == "chat.postMessage").count(), 1);
    s.stop();
}
#[tokio::test]
async fn test_slack_replies_without_the_reaction_when_slack_refuses_it() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_reaction.store(3, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    emit(&a, event("message", "status TEST-1")).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postMessage")).await;
    assert!(!a.calls.lock().unwrap().iter().any(|(m, _)| m == "reactions.remove"));
    assert!(
        a.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, v)| m == "chat.postMessage" && v["text"].as_str().unwrap().contains("TEST-1"))
    );
    s.stop();
}
#[tokio::test]
async fn test_slack_unanswered_message_loses_its_reaction_without_a_check_mark() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_post.store(1, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    emit(&a, event("message", "status TEST-1")).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "reactions.remove")).await;
    assert!(!a.calls.lock().unwrap().iter().any(|(m, v)| m == "reactions.add" && v["name"] == "white_check_mark"));
    let mut unauthorized = event("unauthorized", "status TEST-1");
    unauthorized["payload"]["event"]["user"] = "U-intruder".into();
    emit(&a, unauthorized).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, v)| m == "__ack" && v["envelope_id"] == "unauthorized")).await;
    assert_eq!(a.calls.lock().unwrap().iter().filter(|(m, _)| m.starts_with("reactions.")).count(), 2);
    s.stop();
}
#[tokio::test]
async fn test_slack_send_as_answer_delivers_the_text_and_drops_the_button() {
    let h = engine_support::engine().await;
    let uid = question(&h).await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    let mut click = clicked("answer", "answer_typed", &format!("TEST-1|{uid}|x | y"), "U-operator", "D-operator");
    click["payload"]["actions"][0]["action_id"] = "agentflow_answer_typed".into();
    emit(&a, click).await;
    until(|| {
        a.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, v)| m == "chat.update" && v["text"] == "*Sent as your answer.*\n> x | y")
    })
    .await;
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"], "x | y");
    s.stop();
}
#[tokio::test]
async fn test_new_in_a_question_thread_is_only_a_command_when_bare() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let (s, thread) = threaded(
        &h,
        &a,
        SlackConfig { allow_actions: vec![SlackAction::Answer, SlackAction::Create], ..Default::default() },
    )
    .await;
    assert!(s.message(&message("new approach, please", Some(&thread))).await.unwrap().offer.is_some());
    assert!(s.message(&message("new", Some(&thread))).await.unwrap().offer.is_none());
    assert_eq!(s.message(&message("new abc-2", Some(&thread))).await.unwrap().buttons[0].value, "ABC-2");
}
#[tokio::test]
async fn test_a_create_phrase_in_a_question_thread_is_also_offered_as_the_answer() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let (s, thread) = threaded(
        &h,
        &a,
        SlackConfig { allow_actions: vec![SlackAction::Answer, SlackAction::Create], ..Default::default() },
    )
    .await;
    let reply = s.message(&message("create a new task", Some(&thread))).await.unwrap();
    assert_eq!(reply.buttons[0].kind, "new_task");
    assert_eq!(reply.offer.unwrap().text, "create a new task");
}
#[tokio::test]
async fn test_a_create_phrase_says_when_creating_is_off() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    let reply = s.message(&message("create a new task", None)).await.unwrap();
    assert!(reply.text.starts_with("Creating tasks from Slack is turned off."));
    assert!(reply.buttons.is_empty());
}
#[tokio::test]
async fn test_slack_cancel_button_acts_and_says_so() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::Cancel], ..Default::default() });
    s.start();
    let mut click = clicked("cancel", "cancel", "TEST-1", "U-operator", "D-operator");
    click["payload"]["message"]["blocks"] =
        json!([{"type":"section","text":{"type":"mrkdwn","text":"Cancel TEST-1?"}},{"type":"actions","elements":[]}]);
    emit(&a, click).await;
    until(|| {
        a.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, v)| m == "chat.update" && v["text"] == "Cancel TEST-1?\n*Task cancelled.*")
    })
    .await;
    assert_eq!(h.load().task.status, "CANCELLED");
    s.stop();
}
#[tokio::test]
async fn test_slack_run_button_starts_the_stage_and_replaces_the_buttons() {
    let h = engine_support::engine().await;
    h.launchable();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::Run], ..Default::default() });
    s.start();
    let mut click = clicked("run", "run", "TEST-1|architect", "U-operator", "D-operator");
    click["payload"]["actions"][0]["text"] = json!({"text":"Run Architect"});
    emit(&a, click).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, v)| m == "chat.update" && v["text"] == "*Architect started.*"))
        .await;
    assert_eq!(h.load().node(spec::ARCHITECT).unwrap().state, "RUNNING");
    s.stop();
    h.engine.shutdown();
}
