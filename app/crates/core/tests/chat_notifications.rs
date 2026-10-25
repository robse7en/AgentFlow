mod engine_support;
use agentflow_core::{
    chat::{models::*, notifications::Notifier},
    config::SlackConfig,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Default)]
struct Adapter {
    sent: Mutex<Vec<ChatMessage>>,
    updates: Mutex<Vec<String>>,
    reject: Mutex<Option<ChatError>>,
}
#[async_trait::async_trait]
impl ChatAdapter for Adapter {
    async fn connect(&self) -> Result<mpsc::Receiver<Value>, ChatError> {
        let (_, rx) = mpsc::channel(1);
        Ok(rx)
    }
    async fn disconnect(&self) {}
    async fn open_direct(&self, _user: &str) -> Result<String, ChatError> {
        Ok("D-operator".into())
    }
    async fn api(&self, _method: &str, _body: Value) -> Result<Value, ChatError> {
        Ok(json!({}))
    }
    async fn send(&self, message: ChatMessage) -> Result<String, ChatError> {
        if message.thread_id.is_some()
            && let Some(error) = self.reject.lock().unwrap().take()
        {
            return Err(error);
        }
        let mut sent = self.sent.lock().unwrap();
        sent.push(message);
        Ok(format!("ts{}", sent.len()))
    }
    async fn update(&self, _channel: &str, _id: &str, text: &str) -> Result<(), ChatError> {
        self.updates.lock().unwrap().push(text.into());
        Ok(())
    }
}
fn snapshot() -> Value {
    json!({"jiraKey":"TEST-1","status":"ACTIVE","phase":"Requirements","attention":[{"nodeKey":"Architect","label":"Architect","state":"WAITING_FOR_USER"}],
        "questions":[{"uid":"question-guid","nodeKey":"Architect","actionable":true,"message":"<!here> Choose?","choices":["first","second"]}]})
}
fn notifier(h: &engine_support::Harness, a: &Arc<Adapter>) -> Notifier {
    Notifier::new(h.engine.db.clone(), a.clone(), SlackConfig::default(), "D-operator".into())
}

#[tokio::test]
async fn test_question_is_announced_once_in_a_dm_thread() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let n = notifier(&h, &a);
    n.process(&snapshot(), false).await.unwrap();
    n.process(&snapshot(), false).await.unwrap();
    let sent = a.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].thread_id, None);
    assert_eq!(sent[1].thread_id.as_deref(), Some("ts1"));
    assert_eq!(sent[1].channel, "D-operator");
    assert!(sent[1].text.contains("&lt;!here&gt;"));
    assert_eq!(sent[1].prompt.as_ref().unwrap().choices, vec!["first", "second"]);
}
#[tokio::test]
async fn test_restart_repeats_nothing_and_catches_up_in_the_same_thread() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    notifier(&h, &a).process(&snapshot(), false).await.unwrap();
    let mut next = snapshot();
    next["status"] = "NEEDS_HUMAN_REVIEW".into();
    notifier(&h, &a).process(&next, false).await.unwrap();
    let sent = a.sent.lock().unwrap();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2].thread_id.as_deref(), Some("ts1"));
    assert!(sent[2].text.starts_with("Needs human review"));
}
#[tokio::test]
async fn test_a_condition_that_clears_and_returns_is_announced_again() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let n = notifier(&h, &a);
    n.process(&snapshot(), false).await.unwrap();
    let mut cleared = snapshot();
    cleared["attention"] = json!([]);
    cleared["questions"][0]["answered"] = true.into();
    cleared["questions"][0]["answer"] = "yes".into();
    n.process(&cleared, false).await.unwrap();
    n.process(&snapshot(), false).await.unwrap();
    assert_eq!(a.sent.lock().unwrap().len(), 3);
    assert!(a.updates.lock().unwrap().iter().any(|t| t.contains("*Answered:* yes")));
}
#[tokio::test]
async fn test_first_start_records_finished_history_silently() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let n = notifier(&h, &a);
    let mut history = snapshot();
    history["status"] = "READY_FOR_PR".into();
    history["attention"] = json!([]);
    n.process(&history, true).await.unwrap();
    notifier(&h, &a).process(&history, false).await.unwrap();
    assert!(a.sent.lock().unwrap().is_empty());
    n.process(&snapshot(), false).await.unwrap();
    assert_eq!(a.sent.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn failed_notice_keeps_thread_and_retries_only_unsent_content() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let n = notifier(&h, &a);
    *a.reject.lock().unwrap() = Some(ChatError::temporary("unavailable"));
    assert!(n.process(&snapshot(), false).await.is_err());
    assert_eq!(a.sent.lock().unwrap().len(), 1);
    notifier(&h, &a).process(&snapshot(), false).await.unwrap();
    assert_eq!(a.sent.lock().unwrap().len(), 2);
    assert_eq!(a.sent.lock().unwrap()[1].thread_id.as_deref(), Some("ts1"));
}
#[tokio::test]
async fn test_rejected_content_is_recorded_not_retried() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let n = notifier(&h, &a);
    *a.reject.lock().unwrap() = Some(ChatError::permanent("invalid_blocks"));
    n.process(&snapshot(), false).await.unwrap();
    n.process(&snapshot(), false).await.unwrap();
    assert_eq!(a.sent.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn test_question_text_can_be_withheld() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let settings = SlackConfig { include_question_text: false, ..Default::default() };
    let n = Notifier::new(h.engine.db.clone(), a.clone(), settings, "D-operator".into());
    n.process(&snapshot(), false).await.unwrap();
    let sent = a.sent.lock().unwrap();
    assert!(sent[1].prompt.is_none());
    assert!(!sent[1].text.contains("Choose?"));
    assert!(!sent[1].text.contains("first"));
}
#[tokio::test]
async fn test_findings_status_messages_and_errors_are_never_posted() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let mut s = snapshot();
    s["statusMessage"] = "PRIVATE TOKEN".into();
    s["attention"][0]["error"] = "PRIVATE TOKEN".into();
    s["findings"] = json!([{"title":"PRIVATE TOKEN"}]);
    notifier(&h, &a).process(&s, false).await.unwrap();
    assert!(a.sent.lock().unwrap().iter().all(|m| !m.text.contains("PRIVATE TOKEN")));
}
