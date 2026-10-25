mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::{answers, models::*, notifications::Notifier},
    config::SlackConfig,
    engine::spec,
};
use chat_support::*;
use std::sync::Arc;
#[tokio::test]
async fn test_a_choice_is_answered_by_index_and_the_notice_loses_its_buttons() {
    let h = engine_support::engine().await;
    let uid = question(&h).await;
    let a = Arc::new(Adapter::default());
    let settings = SlackConfig { user_id: Some("U-operator".into()), ..Default::default() };
    let n = Notifier::new(h.engine.db.clone(), a.clone(), settings.clone(), "D-operator".into());
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    assert_eq!(a.sent.lock().unwrap()[1].prompt.as_ref().unwrap().choices.len(), 2);
    let request = AnswerRequest {
        user_id: "U-operator".into(),
        channel: "D-operator".into(),
        jira_key: "TEST-1".into(),
        question_uid: uid,
        choice_index: Some(0),
        text: Some("forged".into()),
        ..Default::default()
    };
    assert!(answers::answer(&h.engine, &settings, Some("D-operator"), &request).await.is_none());
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    assert!(
        a.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, v)| m == "chat.update" && v["text"].as_str().unwrap().starts_with("*Answered:* Gradual"))
    );
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"], "Gradual");
}
#[tokio::test]
async fn test_no_buttons_when_question_text_is_withheld() {
    let h = engine_support::engine().await;
    question(&h).await;
    let a = Arc::new(Adapter::default());
    Notifier::new(
        h.engine.db.clone(),
        a.clone(),
        SlackConfig { include_question_text: false, ..Default::default() },
        "D-operator".into(),
    )
    .process(&h.engine.snapshot("TEST-1").unwrap(), false)
    .await
    .unwrap();
    let sent = a.sent.lock().unwrap();
    assert!(sent[1].prompt.is_none());
    assert!(sent[1].buttons.is_empty());
    assert!(!sent[1].text.contains("Which rollout"));
}
#[tokio::test]
async fn test_notices_answered_elsewhere_or_abandoned_are_updated() {
    let h = engine_support::engine().await;
    let uid = question(&h).await;
    let a = Arc::new(Adapter::default());
    let n = Notifier::new(h.engine.db.clone(), a.clone(), SlackConfig::default(), "D-operator".into());
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    h.engine.answer_question("TEST-1", &uid, "Gradual", None).await.unwrap();
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    assert!(
        a.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, v)| m == "chat.update" && v["text"].as_str().unwrap().starts_with("*Answered:* Gradual"))
    );
    let run = h.engine.snapshot("TEST-1").unwrap()["questions"][0]["runId"].as_i64().unwrap();
    h.engine.agent_question("TEST-1", spec::IMPLEMENTER, Some(run), "Second?", &[]).await.unwrap();
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    h.with_task(|t| {
        let i = t.node_index(spec::IMPLEMENTER).unwrap();
        t.nodes[i].state = "FAILED".into();
        for r in &mut t.runs {
            r.state = "FAILED".into();
        }
    });
    n.process(&h.engine.snapshot("TEST-1").unwrap(), false).await.unwrap();
    assert!(
        a.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, v)| m == "chat.update" && v["text"].as_str().unwrap().starts_with("*No longer waiting"))
    );
}
#[tokio::test]
async fn test_slack_button_click_reaches_the_service_and_reports_refusals() {
    let h = engine_support::engine().await;
    let uid = question(&h).await;
    h.engine.answer_question("TEST-1", &uid, "Gradual", None).await.unwrap();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    let mut click = clicked("answer", "answer_choice", &format!("TEST-1|{uid}|1"), "U-operator", "D-operator");
    click["payload"]["actions"][0]["action_id"] = "agentflow_answer_choice_1".into();
    emit(&a, click).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postMessage")).await;
    let text = a.calls.lock().unwrap().iter().find(|(m, _)| m == "chat.postMessage").unwrap().1["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains("answered") || text.contains("no longer open"));
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"], "Gradual");
    s.stop();
}
