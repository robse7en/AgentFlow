mod chat_support;
mod engine_support;
use agentflow_core::{
    config::{SlackAction, SlackConfig},
    engine::spec,
};
use chat_support::*;
use serde_json::{Value, json};
use std::sync::Arc;

fn submission(id: &str, user: &str, view: Value, field: &str, text: &str) -> Value {
    let mut view = view;
    view["state"] = json!({"values":{field:{"value":{"type":"plain_text_input","value":text}}}});
    json!({"envelope_id":id,"type":"interactive","payload":{"type":"view_submission","user":{"id":user},"view":view}})
}
async fn view(a: &Adapter) -> Value {
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "views.open")).await;
    a.calls.lock().unwrap().iter().find(|(m, _)| m == "views.open").unwrap().1["view"].clone()
}
async fn ack(a: &Adapter, id: &str) -> Value {
    until(|| a.calls.lock().unwrap().iter().any(|(m, v)| m == "__ack" && v["envelope_id"] == id)).await;
    a.calls.lock().unwrap().iter().find(|(m, v)| m == "__ack" && v["envelope_id"] == id).unwrap().1.clone()
}
#[tokio::test]
async fn test_slack_form_button_opens_the_form_and_the_submission_reaches_the_service() {
    let h = engine_support::engine().await;
    h.with_task(|t| {
        let i = t.node_index(spec::REQUIREMENTS_GATE).unwrap();
        t.nodes[i].state = "WAITING_FOR_APPROVAL".into();
    });
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::RequestChanges], ..Default::default() });
    s.start();
    let mut click = clicked("open", "request_changes", "TEST-1", "U-operator", "D-operator");
    click["payload"]["trigger_id"] = "t3".into();
    emit(&a, click).await;
    let modal = view(&a).await;
    assert_eq!(modal["callback_id"], "agentflow_text_form");
    assert_eq!(modal["blocks"][0]["text"]["type"], "plain_text");
    emit(&a, submission("empty", "U-operator", modal.clone(), "text", "  ")).await;
    assert_eq!(
        ack(&a, "empty").await["payload"],
        json!({"response_action":"errors","errors":{"text":"Feedback cannot be empty."}})
    );
    emit(&a, submission("intruder", "U-intruder", modal.clone(), "text", "secret")).await;
    assert!(ack(&a, "intruder").await["payload"]["errors"]["text"].as_str().unwrap().contains("not authorized"));
    assert_eq!(h.engine.snapshot("TEST-1").unwrap()["requirementsFeedback"], json!([]));
    emit(&a, submission("valid", "U-operator", modal, "text", "Cover the <admin> role too")).await;
    assert!(ack(&a, "valid").await.get("payload").is_none());
    until(|| h.engine.snapshot("TEST-1").unwrap()["requirementsFeedback"].as_array().is_some_and(|v| !v.is_empty()))
        .await;
    assert_eq!(
        h.engine.snapshot("TEST-1").unwrap()["requirementsFeedback"][0]["feedback"],
        "Cover the <admin> role too"
    );
    s.stop();
}
#[tokio::test]
async fn test_a_pr_link_is_checked_on_the_form_and_recorded_afterwards() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::MarkPr], ..Default::default() });
    assert!(s.text_form("U-operator", "mark_pr", "TEST-1").unwrap_err().contains("Ready for PR"));
    h.set_status("READY_FOR_PR");
    s.start();
    emit(&a, clicked("open", "mark_pr", "TEST-1", "U-operator", "D-operator")).await;
    let modal = view(&a).await;
    emit(&a, submission("invalid", "U-operator", modal.clone(), "text", "ftp://x")).await;
    assert_eq!(ack(&a, "invalid").await["payload"]["errors"]["text"], "PR URL must start with http:// or https://");
    emit(&a, submission("valid", "U-operator", modal, "text", " https://git.example/pr/7 ")).await;
    assert!(ack(&a, "valid").await.get("payload").is_none());
    until(|| h.engine.snapshot("TEST-1").unwrap()["prUrl"] == "https://git.example/pr/7").await;
    until(|| {
        a.calls.lock().unwrap().iter().any(|(m, v)| m == "chat.postMessage" && v["text"] == "Pull request recorded.")
    })
    .await;
    s.stop();
}
#[tokio::test]
async fn test_slack_reply_opens_a_modal_and_submission_reaches_the_service() {
    let h = engine_support::engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(&h, run);
    engine_support::tab_seven(&h);
    h.engine.agent_question("TEST-1", spec::IMPLEMENTER, Some(run), "Which rollout?", &[]).await.unwrap();
    let uid = h.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_owned();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    let question = json!({"type":"section","text":{"type":"mrkdwn","text":"Which rollout?"}});
    let mut click = clicked("open", "answer_reply", &format!("TEST-1|{uid}|reply"), "U-operator", "D-operator");
    click["payload"]["actions"][0]["action_id"] = "agentflow_answer_reply".into();
    click["payload"]["message"]["blocks"] = json!([question,{"type":"actions","elements":[]}]);
    click["payload"]["message"]["thread_ts"] = "1.0".into();
    emit(&a, click).await;
    let modal = view(&a).await;
    assert_eq!(modal["blocks"][0], question);
    let meta: Value = serde_json::from_str(modal["private_metadata"].as_str().unwrap()).unwrap();
    assert_eq!(meta["channel"], "D-operator");
    assert_eq!(meta["question_uid"], uid);
    assert!(h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"].is_null());
    emit(&a, submission("intruder", "U-intruder", modal.clone(), "answer", "forged")).await;
    ack(&a, "intruder").await;
    emit(&a, submission("valid", "U-operator", modal, "answer", "Ship it gradually")).await;
    ack(&a, "valid").await;
    until(|| h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"] == "Ship it gradually").await;
    assert!(h.terminals.calls().iter().any(|c| c.ends_with("Ship it gradually")));
    assert!(!h.terminals.calls().iter().any(|c| c.ends_with("forged")));
    s.stop();
}
/// A decision tree's branch with a follow-up swaps the notice's controls in place ("‹ Change
/// branch" swaps them back); a follow-up option sends the path. Nothing else is posted.
#[tokio::test]
async fn test_slack_tree_branch_edits_the_notice_in_place_and_an_option_sends_the_path() {
    let h = engine_support::engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(&h, run);
    engine_support::tab_seven(&h);
    let tree = json!({"context": null, "risk": "high", "choices": [
        {"label": "Soft-delete", "recommended": true, "followUp": {"message": "Purge when?", "choices": [
            {"label": "Later"}, {"label": "Never", "note": "Legal asked.", "source": "docs/privacy.md"}]}},
        {"label": "Hard-delete"}]});
    let labels = ["Soft-delete".to_string(), "Hard-delete".to_string()];
    h.engine
        .agent_decision("TEST-1", spec::IMPLEMENTER, Some(run), "Which cleanup?", &labels, Some(tree))
        .await
        .unwrap();
    let uid = h.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_owned();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    let question = json!({"type":"section","text":{"type":"mrkdwn","text":"Implementer asked a question"}});
    let click = |id: &str, value: &str, blocks: Value| {
        let mut click = clicked(id, "answer_choice_0", &format!("TEST-1|{uid}|{value}"), "U-operator", "D-operator");
        click["payload"]["actions"][0]["action_id"] = id.into();
        click["payload"]["message"] = json!({"ts": "2.0", "thread_ts": "1.0", "text": "Question", "blocks": blocks});
        click
    };
    let updates = |a: &Adapter| -> Vec<Value> {
        // The clicked notice only (the notifier may edit the thread title meanwhile).
        let calls = a.calls.lock().unwrap();
        calls.iter().filter(|(m, v)| m == "chat.update" && v["ts"] == "2.0").map(|(_, v)| v.clone()).collect()
    };
    let values = |blocks: &Value| -> Vec<String> {
        let actions = blocks.as_array().unwrap().iter().find(|b| b["type"] == "actions").unwrap();
        actions["elements"].as_array().unwrap().iter().map(|b| b["value"].as_str().unwrap().to_owned()).collect()
    };

    emit(&a, click("agentflow_answer_choice_0", "0", json!([question.clone()]))).await;
    until(|| updates(&a).len() == 1).await;
    let shown = updates(&a)[0].clone();
    assert_eq!((shown["ts"].clone(), shown["channel"].clone()), (json!("2.0"), json!("D-operator")));
    let blocks = shown["blocks"].clone();
    assert_eq!(blocks[0], question, "the question text stays");
    assert_eq!(blocks[1]["block_id"], "followup");
    assert!(blocks[1]["text"]["text"].as_str().unwrap().contains("Purge when?"));
    let expected: Vec<String> = ["0.0", "0.1", "back", "reply"].iter().map(|v| format!("TEST-1|{uid}|{v}")).collect();
    assert_eq!(values(&blocks), expected);

    emit(&a, click("agentflow_answer_back", "back", blocks.clone())).await;
    until(|| updates(&a).len() == 2).await;
    let restored = updates(&a)[1]["blocks"].clone();
    assert!(restored.as_array().unwrap().iter().all(|b| b["block_id"] != "followup"));
    let expected: Vec<String> = ["0", "1", "reply"].iter().map(|v| format!("TEST-1|{uid}|{v}")).collect();
    assert_eq!(values(&restored), expected);

    emit(&a, click("agentflow_answer_follow_1", "0.1", blocks)).await;
    until(|| h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answered"] == true).await;
    let line = "1.2: Soft-delete \u{2192} Never";
    assert!(h.terminals.calls().iter().any(|c| c == &format!("write_line agentflow-TEST-1 terminal_7 {line}")));
    assert_eq!(
        updates(&a)
            .iter()
            .filter(|u| u["ts"] == "2.0" && u.get("blocks").is_some_and(|b| b.to_string().contains("followup")))
            .count(),
        1
    );
    s.stop();
}
#[tokio::test]
async fn test_slack_new_opens_the_form_and_searches_and_submissions_reach_the_service() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::Create], ..Default::default() });
    s.start();
    emit(&a, clicked("open", "new_task", "test-2", "U-operator", "D-operator")).await;
    let modal = view(&a).await;
    assert_eq!(modal["callback_id"], "agentflow_task_form");
    emit(&a,json!({"envelope_id":"options","type":"interactive","payload":{"type":"block_suggestion","user":{"id":"U-operator"},"action_id":"agentflow_task_repository","value":"D:/typed","view":modal}})).await;
    assert_eq!(ack(&a, "options").await["payload"]["options"][0]["value"], "D:/typed");
    emit(&a, submission("missing", "U-operator", modal.clone(), "ticket", "TEST-2")).await;
    assert_eq!(ack(&a, "missing").await["payload"]["errors"]["repository"], "Choose a repository.");
    assert!(h.engine.snapshot("TEST-2").is_err());
    h.usable_harnesses();
    h.git.returns(
        "inspect_repository",
        agentflow_core::vcs::Outcome::Ok(json!({"path":h.tmp(),"root":h.tmp(),"branches":["main"]})),
    );
    h.git.returns("create_worktree", agentflow_core::vcs::Outcome::Ok(json!({"branch": "agent/TEST-2"})));
    let mut envelope = submission("valid", "U-operator", modal, "ticket", "TEST-2");
    envelope["payload"]["view"]["state"]["values"]["repository"] =
        json!({"select":{"selected_option":{"value":h.tmp()}}});
    envelope["payload"]["view"]["state"]["values"]["branch"] = json!({"select":{"selected_option":{"value":"main"}}});
    emit(&a, envelope).await;
    assert!(ack(&a, "valid").await.get("payload").is_none());
    until(|| h.engine.snapshot("TEST-2").is_ok()).await;
    s.stop();
}
