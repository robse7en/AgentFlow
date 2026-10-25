mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::{notifications::Notifier, service::start_chat},
    config::{Config, SlackConfig},
    events::Broadcaster,
};
use chat_support::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[tokio::test]
async fn test_a_title_that_cannot_be_edited_does_not_hold_up_notices() {
    let h = engine_support::engine().await;
    question(&h).await;
    let a = Arc::new(Adapter::default());
    let n = Notifier::new(h.engine.db.clone(), a.clone(), SlackConfig::default(), "D-operator".into());
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    a.fail_update.store(1, Ordering::SeqCst);
    h.set_status("NEEDS_HUMAN_REVIEW");
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    assert!(a.sent.lock().unwrap().iter().any(|m| m.text.starts_with("Needs human review")));
}
#[tokio::test]
async fn test_open_questions_are_announced_when_chat_first_starts() {
    let h = engine_support::engine().await;
    question(&h).await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    until(|| a.sent.lock().unwrap().iter().any(|m| m.prompt.is_some())).await;
    assert_eq!(a.sent.lock().unwrap().iter().filter(|m| m.prompt.is_some()).count(), 1);
    s.stop();
}
#[tokio::test]
async fn test_a_task_that_fails_while_chat_is_connecting_is_still_announced() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_connect.store(1, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    h.add_run(agentflow_core::engine::spec::ARCHITECT, "FAILED", None);
    h.engine.broadcaster.broadcast(&json!({"type":"task","task":h.engine.snapshot("TEST-1").unwrap()}));
    until(|| a.sent.lock().unwrap().iter().any(|m| m.text == "Architect failed")).await;
    assert!(a.connections.load(Ordering::SeqCst) >= 2);
    s.stop();
}
#[tokio::test]
async fn test_chat_failures_never_break_the_workflow_and_are_retried() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_send.store(1, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    question(&h).await;
    until(|| a.sent.lock().unwrap().iter().any(|m| m.prompt.is_some())).await;
    assert_eq!(h.load().task.status, "ACTIVE");
    assert_eq!(a.sent.lock().unwrap().iter().filter(|m| m.prompt.is_some()).count(), 1);
    s.stop();
}
#[tokio::test]
async fn test_a_failing_listener_does_not_stop_the_broadcast() {
    let b = Broadcaster::default();
    let called = Arc::new(AtomicUsize::new(0));
    b.subscribe(|_| panic!("listener failure"));
    let c = called.clone();
    b.subscribe(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let mut client = b.connect();
    b.broadcast(&json!({"ok":true}));
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert!(client.recv().await.unwrap().contains("true"));
}
#[tokio::test]
async fn test_commands_answer_only_the_operator() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    assert_eq!(s.command("intruder", "list").await.text, "You are not authorized to use AgentFlow.");
    for text in ["list", "status", "status test-1"] {
        assert!(s.command("U-operator", text).await.text.contains("TEST-1"));
    }
    assert_eq!(s.command("U-operator", "status <nope>").await.text, "No AgentFlow task NOPE.");
    assert!(s.command("U-operator", "").await.text.contains("`/agentflow`"));
}
#[tokio::test]
async fn test_start_chat_explains_why_it_is_off() {
    let h = engine_support::engine().await;
    assert!(start_chat(h.engine.clone()).is_none());
    h.configure(|c| {
        c.slack.enabled = true;
        c.slack.user_id = None;
    });
    assert!(start_chat(h.engine.clone()).is_none());
    let reasons = h.config().slack.problems();
    assert!(reasons.iter().any(|s| s.contains("slack.userId")));
}
#[test]
fn test_tokens_prefer_environment_and_never_reach_agents() {
    if std::env::var("AGENTFLOW_CHAT_ENV_PROBE").as_deref() != Ok("1") {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "test_tokens_prefer_environment_and_never_reach_agents"])
            .env("AGENTFLOW_CHAT_ENV_PROBE", "1")
            .env("AGENTFLOW_SLACK_BOT_TOKEN", "xoxb-fixture")
            .env("AGENTFLOW_SLACK_APP_TOKEN", "xapp-fixture")
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
        return;
    }
    let s =
        SlackConfig { bot_token: Some("xoxb-file".into()), app_token: Some("xapp-file".into()), ..Default::default() };
    assert_eq!(s.resolved_bot_token().as_deref(), Some("xoxb-fixture"));
    assert_eq!(s.resolved_app_token().as_deref(), Some("xapp-fixture"));
    let env = agentflow_core::process::clean_env(&Default::default());
    assert!(!env.keys().any(|k| k.starts_with("AGENTFLOW_SLACK")));
}
#[test]
fn test_user_id_must_be_a_slack_member_id() {
    let mut c = Config::default();
    c.slack.user_id = Some("@rob".into());
    assert!(Config::from_value(serde_json::to_value(&c).unwrap()).is_err());
    c.slack.user_id = Some("U123AB".into());
    assert!(Config::from_value(serde_json::to_value(c).unwrap()).is_ok());
}
#[tokio::test]
async fn test_slack_without_a_command_listens_for_none() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { command: None, ..Default::default() });
    s.start();
    emit(&a,json!({"envelope_id":"slash","type":"slash_commands","payload":{"command":"/agentflow","user_id":"U-operator","text":"list"}})).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "__ack")).await;
    assert!(!a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postEphemeral"));
    s.stop();
}
#[tokio::test]
async fn test_backend_starts_and_pairs_while_slack_is_unreachable() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_connect.store(100, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    until(|| a.connections.load(Ordering::SeqCst) > 0).await;
    let (status, body) = h.route("GET", "/api/tasks", None).await;
    assert_eq!(status, 200);
    assert!(body.to_string().contains("TEST-1"));
    s.stop();
}
