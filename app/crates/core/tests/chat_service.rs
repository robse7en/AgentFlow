mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::{actions, commands, models::*},
    config::{SlackAction, SlackConfig, SlackDetail},
    engine::spec,
};
use chat_support::*;
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
#[tokio::test]
async fn read_only_commands_and_allowlisted_details() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { details: vec![SlackDetail::Pr], ..Default::default() });
    let reply = s.message(&message("status test-1", None)).await.unwrap();
    assert!(reply.text.contains("TEST-1"));
    assert_eq!(
        s.message(&message("details TEST-1 errors", None)).await.unwrap().text,
        "Not enabled in slack.details: errors."
    );
    assert!(!s.message(&message("details TEST-1", None)).await.unwrap().text.contains("Requirements"));
    assert_eq!(s.command("intruder", "status TEST-1").await.text, "You are not authorized to use AgentFlow.");
    let mut request = message("status", None);
    request.channel = "public".into();
    assert!(s.message(&request).await.is_none());
    assert!(a.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_cancel_typed_or_from_a_thread_is_a_confirmed_button() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::Cancel], ..Default::default() });
    let reply = s.message(&message("cancel TEST-1", None)).await.unwrap();
    assert_eq!(reply.buttons[0].kind, "cancel");
    assert!(reply.buttons[0].confirm.is_some());
    assert_eq!(h.load().task.status, "ACTIVE");
    let reply = s.command("U-operator", "cancel TEST-1").await;
    assert!(reply.text.contains("direct messages"));
    assert_eq!(a.calls.lock().unwrap()[0].0, "chat.postMessage");
    assert_eq!(h.load().task.status, "ACTIVE");
}
#[tokio::test]
async fn socket_callbacks_are_authorized_acknowledged_and_replay_safe() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::Cancel], ..Default::default() });
    s.start();
    for (id, user, channel) in [("wrong-user", "U-intruder", "D-operator"), ("wrong-channel", "U-operator", "C-public")]
    {
        emit(&a, clicked(id, "cancel", "TEST-1", user, channel)).await;
    }
    until(|| a.calls.lock().unwrap().iter().filter(|(m, _)| m == "__ack").count() == 2).await;
    assert_eq!(h.load().task.status, "ACTIVE");
    let valid = clicked("valid", "cancel", "TEST-1", "U-operator", "D-operator");
    emit(&a, valid.clone()).await;
    emit(&a, valid).await;
    until(|| h.load().task.status == "CANCELLED" && a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.update"))
        .await;
    let calls = a.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|(m, _)| m == "chat.update").count(), 1);
    assert_eq!(calls.iter().find(|(m, v)| m == "__ack" && v["envelope_id"] == "valid").unwrap().0, "__ack");
    drop(calls);
    s.stop();
}
#[tokio::test]
async fn reconnects_retry_and_stopping_drops_workers_and_subscription() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_connect.store(1, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    until(|| a.connections.load(Ordering::SeqCst) >= 2 && s.dm().is_some()).await;
    emit(&a, json!({"type":"disconnect"})).await;
    until(|| a.connections.load(Ordering::SeqCst) >= 3 && s.dm().is_some()).await;
    let weak = Arc::downgrade(&s);
    s.stop();
    drop(s);
    until(|| weak.upgrade().is_none()).await;
    assert!(a.disconnects.load(Ordering::SeqCst) >= 2);
}
#[tokio::test]
async fn duplicate_message_event_is_processed_once_after_ack() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    for id in ["first", "second"] {
        emit(&a,json!({"envelope_id":id,"type":"events_api","payload":{"event_id":"same-event","event":{"type":"message","user":"U-operator","channel":"D-operator","text":"status TEST-1","ts":"message-ts"}}})).await;
    }
    until(|| {
        a.calls.lock().unwrap().iter().filter(|(m, _)| m == "__ack").count() == 2
            && a.calls.lock().unwrap().iter().any(|(m, _)| m == "reactions.remove")
    })
    .await;
    let calls = a.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|(m, _)| m == "chat.postMessage").count(), 1);
    assert_eq!(calls[0].0, "__ack");
    drop(calls);
    s.stop();
}
#[tokio::test]
async fn task_and_text_forms_reject_unauthorized_or_invalid_data_before_acceptance() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(
        &h,
        &a,
        SlackConfig { allow_actions: vec![SlackAction::Create, SlackAction::MarkPr], ..Default::default() },
    );
    let draft = TaskDraft {
        user_id: "U-intruder".into(),
        jira_key: "NEW-1".into(),
        repository_path: "".into(),
        base_branch: "".into(),
        project_relative_path: None,
        implementation_mode: None,
        additional_context: None,
    };
    assert!(s.submit_task(draft.clone())["ticket"].as_str().unwrap().contains("authorized"));
    let mut draft = draft;
    draft.user_id = "U-operator".into();
    assert_eq!(s.submit_task(draft)["repository"], "Choose a repository.");
    assert!(
        !s.submit_text(TextSubmission {
            user_id: "U-operator".into(),
            kind: "mark_pr".into(),
            value: "TEST-1".into(),
            text: "https://private".into()
        })["text"]
            .is_null()
    );
    assert!(a.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn requirements_review_posts_all_documents_and_refuses_stale_approval() {
    let h = engine_support::engine().await;
    h.with_task(|t| {
        let index = t.node_index(spec::REQUIREMENTS_GATE).unwrap();
        t.nodes[index].state = "WAITING_FOR_APPROVAL".into();
    });
    h.engine.db.session(|tx| tx.execute("delete from requirements_approvals", [])).unwrap();
    let dir = h.task_dir().join("TEST-1/requirements");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["requirements.md", "acceptance-criteria.md", "implementation-plan.md"] {
        std::fs::write(dir.join(name), format!("Full <!here> {name}")).unwrap();
    }
    let a = Arc::new(Adapter::default());
    let s =
        service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::ApproveRequirements], ..Default::default() });
    let request = ActionRequest {
        user_id: "U-operator".into(),
        channel: "D-operator".into(),
        kind: "review_requirements".into(),
        value: "TEST-1".into(),
    };
    assert_eq!(s.action(&request).await, None);
    let value = {
        let calls = a.calls.lock().unwrap();
        let posted = calls.iter().map(|(_, v)| v["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join("\n");
        assert!(posted.contains("Full &lt;!here&gt; requirements.md"));
        assert!(posted.contains("implementation-plan.md"));
        let last = &calls.last().unwrap().1;
        last["blocks"].as_array().unwrap().iter().find(|b| b["type"] == "actions").unwrap()["elements"][0]["value"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    std::fs::write(dir.join("requirements.md"), "Changed after review").unwrap();
    let reason = s.action(&ActionRequest { kind: "approve_requirements".into(), value, ..request }).await.unwrap();
    assert!(reason.contains("Requirements changed"));
    assert_eq!(h.load().node(spec::REQUIREMENTS_GATE).unwrap().state, "WAITING_FOR_APPROVAL");
}
#[tokio::test]
async fn test_nothing_to_approve_unless_every_document_was_posted() {
    let h = engine_support::engine().await;
    h.with_task(|t| {
        let index = t.node_index(spec::REQUIREMENTS_GATE).unwrap();
        t.nodes[index].state = "WAITING_FOR_APPROVAL".into();
    });
    let dir = h.task_dir().join("TEST-1/requirements");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("requirements.md"), "Review").unwrap();
    let a = Arc::new(Adapter::default());
    a.fail_post.store(1, Ordering::SeqCst);
    let s =
        service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::ApproveRequirements], ..Default::default() });
    assert!(
        s.action(&ActionRequest {
            user_id: "U-operator".into(),
            channel: "D-operator".into(),
            kind: "review_requirements".into(),
            value: "TEST-1".into()
        })
        .await
        .unwrap()
        .contains("nothing to approve")
    );
    assert!(!serde_json::to_string(&*a.calls.lock().unwrap()).unwrap().contains("agentflow_approve_requirements"));
}
#[test]
fn launch_controls_do_not_bypass_metered_approval_or_human_state() {
    let task = json!({"jiraKey":"TEST-1","status":"ACTIVE","budget":{},"nodes":[]});
    let node = json!({"key":"architect","label":"Architect","state":"READY","effectiveConfig":{"harnessAvailable":true,"billingMode":"metered","requiresMeteredApproval":true}});
    assert!(actions::launch_refusal(&task, &node).unwrap().contains("API launch approval"));
    let mut node = node;
    node["effectiveConfig"]["billingMode"] = "subscription".into();
    assert_eq!(actions::launch_refusal(&task, &node), None);
    node["state"] = "WAITING_FOR_APPROVAL".into();
    assert!(actions::launch_refusal(&task, &node).is_some());
}
#[test]
fn test_the_next_digest_is_today_until_its_time_then_tomorrow() {
    let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap().and_hms_opt(9, 0, 0).unwrap();
    assert_eq!(commands::next_digest_at(now, "09:00").unwrap(), now + chrono::Duration::days(1));
    assert_eq!(commands::next_digest_at(now, "10:00").unwrap(), now + chrono::Duration::hours(1));
    assert_eq!(commands::next_digest_at(now, "bad"), None);
}

#[tokio::test]
async fn missing_explicit_key_in_a_thread_never_targets_that_threads_task() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(
        &h,
        &a,
        SlackConfig { allow_actions: vec![SlackAction::Cancel, SlackAction::Run], ..Default::default() },
    );
    for command in ["cancel", "run"] {
        let reply = s.action_reply(command, &["MISSING-1"], Some("TEST-1"));
        assert_eq!(reply.text, "No AgentFlow task MISSING-1.");
        assert!(reply.buttons.is_empty());
    }
    assert_eq!(h.load().task.status, "ACTIVE");
}
#[tokio::test]
async fn model_context_contains_only_allowed_details_and_never_hidden_questions_or_paths() {
    let h = engine_support::engine().await;
    h.with_task(|t| {
        t.task.status_message = Some("SECRET status message".into());
    });
    let a = Arc::new(Adapter::default());
    let s = service(
        &h,
        &a,
        SlackConfig { include_question_text: false, details: vec![SlackDetail::Pr], ..Default::default() },
    );
    let context = s.ask_context("Why TEST-1?", None).await;
    assert_eq!(context["task"]["openAgentQuestions"], "not shared");
    assert_eq!(context["allowedDetails"], json!(["pr"]));
    let body = context.to_string();
    assert!(!body.contains("SECRET"));
    assert!(!body.contains("repositoryPath"));
    assert!(!body.contains("worktreePath"));
    assert_eq!(context["task"]["details"].as_object().unwrap().len(), 1);
}
#[tokio::test]
async fn test_typed_text_in_a_thread_is_offered_as_an_answer_never_sent() {
    let h = engine_support::engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(&h, run);
    engine_support::tab_seven(&h);
    h.engine.agent_question("TEST-1", spec::IMPLEMENTER, Some(run), "Choose?", &["yes".into()]).await.unwrap();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    agentflow_core::chat::notifications::Notifier::new(
        h.engine.db.clone(),
        a.clone(),
        s.settings.clone(),
        "D-operator".into(),
    )
    .process(&h.engine.snapshot("TEST-1").unwrap(), false)
    .await
    .unwrap();
    let thread = s.thread("TEST-1").unwrap();
    let reply = s.message(&message("yes please", Some(&thread))).await.unwrap();
    assert_eq!(reply.offer.unwrap().text, "yes please");
    assert!(!h.terminals.calls().iter().any(|c| c.starts_with("write_line")));
    assert!(s.message(&message("status", Some(&thread))).await.unwrap().offer.is_none());
    assert!(s.message(&message("ask Why?", Some(&thread))).await.unwrap().offer.is_none());
}
#[tokio::test]
async fn test_the_approve_button_approves_exactly_what_was_posted() {
    let h = engine_support::engine().await;
    h.launchable();
    h.with_task(|t| {
        let architect = t.node_index(spec::ARCHITECT).unwrap();
        let gate = t.node_index(spec::REQUIREMENTS_GATE).unwrap();
        t.nodes[architect].state = "COMPLETED".into();
        t.nodes[gate].state = "WAITING_FOR_APPROVAL".into();
    });
    h.engine.db.session(|tx| tx.execute("delete from requirements_approvals", [])).unwrap();
    let dir = h.task_dir().join("TEST-1/requirements");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["requirements.md", "acceptance-criteria.md", "implementation-plan.md"] {
        std::fs::write(dir.join(name), "Full approved content").unwrap();
    }
    let digest = h.engine.snapshot("TEST-1").unwrap()["requirementsDraftDigest"].as_str().unwrap().to_owned();
    let a = Arc::new(Adapter::default());
    let s = service(
        &h,
        &a,
        SlackConfig { allow_actions: vec![SlackAction::ApproveRequirements, SlackAction::Run], ..Default::default() },
    );
    assert_eq!(
        s.action(&ActionRequest {
            user_id: "U-operator".into(),
            channel: "D-operator".into(),
            kind: "approve_requirements".into(),
            value: format!("TEST-1|{digest}")
        })
        .await,
        None
    );
    let snapshot = h.engine.snapshot("TEST-1").unwrap();
    assert_eq!(snapshot["requirementsDigest"], digest);
    assert!(snapshot["events"].as_array().unwrap().iter().any(|e| e["message"] == "Requirements approved via Slack"));
    assert!(a.calls.lock().unwrap().iter().any(|(_, v)| v.to_string().contains("agentflow_run")));
}
#[tokio::test]
async fn socket_answer_choice_is_engine_owned() {
    let h = engine_support::engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(&h, run);
    engine_support::tab_seven(&h);
    h.engine
        .agent_question("TEST-1", spec::IMPLEMENTER, Some(run), "Choose?", &["Engine choice".into()])
        .await
        .unwrap();
    let uid = h.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().to_owned();
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    let mut envelope = clicked("answer", "answer_choice", &format!("TEST-1|{uid}|0"), "U-operator", "D-operator");
    envelope["payload"]["actions"][0]["action_id"] = "agentflow_answer_choice_0".into();
    emit(&a, envelope).await;
    until(|| h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"] == "Engine choice").await;
    assert!(h.terminals.calls().iter().any(|c| c.ends_with("Engine choice")));
    s.stop();
}
#[tokio::test]
async fn searchable_forms_authorize_before_repository_reads_and_allow_typed_values() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { allow_actions: vec![SlackAction::Create], ..Default::default() });
    let request = OptionsRequest {
        user_id: "U-intruder".into(),
        field: "repository".into(),
        query: "D:/private".into(),
        repository: None,
    };
    assert!(s.options(&request).await.is_empty());
    let request = OptionsRequest { user_id: "U-operator".into(), ..request };
    assert!(s.options(&request).await.iter().any(|(value, _)| value == "D:/private"));
    let request = OptionsRequest { field: "ticket".into(), query: "af-123".into(), ..request };
    assert_eq!(s.options(&request).await[0].0, "AF-123");
}
#[tokio::test]
async fn test_slack_slash_command_is_acked_and_answered_privately() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    emit(&a,json!({"envelope_id":"slash","type":"slash_commands","payload":{"command":"/agentflow","user_id":"U-operator","channel_id":"C-public","text":"status TEST-1","response_url":"https://untrusted.invalid/secret"}})).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postEphemeral")).await;
    let calls = a.calls.lock().unwrap();
    assert_eq!(calls[0].0, "__ack");
    let response = calls.iter().find(|(m, _)| m == "chat.postEphemeral").unwrap();
    assert_eq!(response.1["user"], "U-operator");
    assert!(!response.1.to_string().contains("untrusted"));
    drop(calls);
    s.stop();
}
#[tokio::test]
async fn test_slack_listens_only_on_its_configured_command() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.start();
    emit(&a,json!({"envelope_id":"other","type":"slash_commands","payload":{"command":"/other","user_id":"U-operator","text":"status"}})).await;
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "__ack")).await;
    assert_eq!(a.calls.lock().unwrap().len(), 1);
    s.stop();
}
