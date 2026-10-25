mod engine_support;
use agentflow_core::{
    chat::{answers, models::AnswerRequest},
    config::SlackConfig,
    engine::spec,
};
use engine_support::{Harness, add_session, engine, tab_seven};

async fn asked() -> (Harness, SlackConfig, AnswerRequest) {
    let h = engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&h, run);
    tab_seven(&h);
    h.engine
        .agent_question("TEST-1", "implementer", Some(run), "Choose?", &["Gradual".into(), "All at once".into()])
        .await
        .unwrap();
    let uid = h.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_owned();
    let settings = SlackConfig { user_id: Some("U-operator".into()), ..Default::default() };
    let request = AnswerRequest {
        user_id: "U-operator".into(),
        channel: "D-operator".into(),
        jira_key: "TEST-1".into(),
        question_uid: uid,
        choice_index: Some(1),
        text: Some("forged choice text".into()),
        ..Default::default()
    };
    (h, settings, request)
}
#[tokio::test]
async fn test_a_choice_is_answered_by_index_and_audited_via_slack() {
    let (h, s, r) = asked().await;
    assert_eq!(answers::answer(&h.engine, &s, Some("D-operator"), &r).await, None);
    assert!(h.terminals.calls().iter().any(|s| s == "write_line agentflow-TEST-1 terminal_7 All at once"));
    let snapshot = h.engine.snapshot("TEST-1").unwrap();
    assert_eq!(snapshot["questions"][0]["answer"], "All at once");
    assert!(
        snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"].as_str().unwrap_or("").ends_with("via Slack"))
    );
}
#[tokio::test]
async fn test_free_text_answers_use_the_engines_validation() {
    let (h, s, mut r) = asked().await;
    r.choice_index = None;
    r.text = Some("one\ntwo".into());
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &r).await.as_deref(),
        Some("Not sent: Answer must be a single line.")
    );
    assert!(!h.terminals.calls().iter().any(|s| s.starts_with("write_line")));
    r.text = Some("  Gradual  ".into());
    assert!(answers::answer(&h.engine, &s, Some("D-operator"), &r).await.is_none());
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"], "Gradual");
}
#[tokio::test]
async fn test_answers_are_accepted_only_from_the_operator_in_their_dm() {
    let (h, s, r) = asked().await;
    let mut wrong = r.clone();
    wrong.user_id = "U-intruder".into();
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &wrong).await.as_deref(),
        Some("You are not authorized to answer AgentFlow questions.")
    );
    wrong = r.clone();
    wrong.channel = "C-public".into();
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &wrong).await.as_deref(),
        Some("Answer from your AgentFlow direct messages.")
    );
    wrong = r.clone();
    wrong.choice_index = Some(9);
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &wrong).await.as_deref(),
        Some("That choice is no longer offered.")
    );
    wrong = r.clone();
    wrong.question_uid = "unknown".into();
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &wrong).await.as_deref(),
        Some("That question is no longer open.")
    );
    wrong = r.clone();
    wrong.jira_key = "MISSING-1".into();
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &wrong).await.as_deref(),
        Some("That task no longer exists.")
    );
    assert!(!h.terminals.calls().iter().any(|s| s.starts_with("write_line")));
}
#[tokio::test]
async fn test_answering_can_be_switched_off() {
    let (h, mut s, r) = asked().await;
    s.allow_actions.clear();
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &r).await.as_deref(),
        Some("Answering from Slack is turned off; answer in AgentFlow.")
    );
    s = SlackConfig { user_id: s.user_id, include_question_text: false, ..Default::default() };
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &r).await.as_deref(),
        Some("Answering from Slack is turned off; answer in AgentFlow.")
    );
}
#[tokio::test]
async fn test_delivery_failures_are_reported_without_command_output() {
    let (h, s, r) = asked().await;
    h.terminals.fail("write_line", "PRIVATE token=secret C:/Users/private");
    assert_eq!(
        answers::answer(&h.engine, &s, Some("D-operator"), &r).await.as_deref(),
        Some("Not sent: AgentFlow could not deliver the answer. Check AgentFlow.")
    );
}
