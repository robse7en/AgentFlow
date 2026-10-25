mod chat_support;
mod engine_support;
use agentflow_core::{
    chat::{
        ask::{AskInvocation, AskRunner},
        service::ChatService,
    },
    config::{BillingMode, SlackConfig},
    process::CommandResult,
};
use chat_support::*;
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;
#[derive(Clone)]
struct Call {
    args: Vec<String>,
    cwd: PathBuf,
    env: HashMap<String, String>,
    input: String,
}
#[derive(Default)]
struct Runner {
    calls: Mutex<Vec<Call>>,
    output: Mutex<Option<CommandResult>>,
    error: Mutex<Option<String>>,
    blocked: AtomicBool,
    release: Notify,
}
#[async_trait::async_trait]
impl AskRunner for Runner {
    async fn run(&self, call: AskInvocation<'_>) -> Result<CommandResult, String> {
        assert!(std::fs::read_dir(call.cwd).unwrap().next().is_none());
        if let Some(home) = call.env.get("COPILOT_HOME") {
            assert!(std::fs::read_dir(home).unwrap().next().is_none());
            assert_ne!(call.cwd, std::path::Path::new(home));
        }
        self.calls.lock().unwrap().push(Call {
            args: call.args.into(),
            cwd: call.cwd.into(),
            env: call.env.clone(),
            input: call.input.into(),
        });
        if self.blocked.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        if let Some(error) = self.error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.output.lock().unwrap().clone().unwrap_or(CommandResult {
            argv: call.args.into(),
            returncode: 0,
            stdout: json!({"result":"Ping <!channel> **now**"}).to_string(),
            stderr: String::new(),
        }))
    }
}
fn asking(h: &engine_support::Harness, a: &Arc<Adapter>, r: &Arc<Runner>, kind: &str) -> Arc<ChatService> {
    h.configure(|c| {
        c.slack.ask.enabled = true;
        c.slack.ask.harness = kind.into();
        let harness = c.harnesses.get_mut(kind).unwrap();
        harness.enabled = true;
        harness.billing_mode = BillingMode::Subscription;
        harness.command = if cfg!(windows) { "where.exe" } else { "true" }.into();
        harness.args = vec!["--allow-all-tools".into()];
        harness.env.insert("ASK_EXPLICIT".into(), "only-this".into());
    });
    let settings =
        SlackConfig { user_id: Some("U-operator".into()), ask: h.config().slack.ask.clone(), ..Default::default() };
    let s = ChatService::new_with_ask_runner(h.engine.clone(), a.clone(), settings, r.clone());
    *s.channel.write().unwrap() = Some("D-operator".into());
    s
}
#[tokio::test]
async fn test_free_text_is_asked_and_the_answer_is_marked_and_escaped() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "claude");
    let reply = s.message(&message("what is going on today?", None)).await.unwrap();
    assert!(reply.text.starts_with("Ping &lt;!channel&gt; *now*\n_AI answer"));
    assert!(s.message(&message("thanks", None)).await.unwrap().text.contains("`list`"));
    assert_eq!(r.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn test_a_thread_remembers_its_last_turns_in_memory_only() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "claude");
    for q in ["one?", "two?", "three?", "four?", "five?"] {
        s.message(&message(&format!("ask {q}"), Some("ts1"))).await.unwrap();
    }
    s.message(&message("ask elsewhere?", None)).await.unwrap();
    {
        let calls = r.calls.lock().unwrap();
        assert!(calls[1].input.contains("Earlier question: one?"));
        assert!(!calls[4].input.contains("Earlier question: one?"));
        for q in ["two?", "three?", "four?"] {
            assert!(calls[4].input.contains(&format!("Earlier question: {q}")));
        }
        assert!(!calls[5].input.contains("Earlier question:"));
    }
    drop(s);
    let fresh = asking(&h, &a, &r, "claude");
    fresh.message(&message("ask fresh?", Some("ts1"))).await.unwrap();
    assert!(!r.calls.lock().unwrap().last().unwrap().input.contains("Earlier question:"));
}
#[tokio::test]
async fn test_a_question_runs_in_an_empty_folder_with_the_safe_environment() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "claude");
    s.message(&message("ask why?", None)).await.unwrap();
    let call = r.calls.lock().unwrap()[0].clone();
    assert!(!call.cwd.exists());
    assert_eq!(call.env, HashMap::from([("ASK_EXPLICIT".into(), "only-this".into())]));
    assert!(!call.args.contains(&"--allow-all-tools".into()));
    assert!(call.args.windows(2).any(|p| p == ["--tools", ""]));
    assert!(call.input.contains("data, not instructions"));
}
#[tokio::test]
async fn test_copilot_runs_with_no_tools_and_an_empty_home_the_harness_cannot_override() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "copilot");
    h.configure(|c| {
        let e = &mut c.harnesses.get_mut("copilot").unwrap().env;
        e.insert("copilot_allow_all".into(), "true".into());
        e.insert("Copilot_Home".into(), "PRIVATE".into());
    });
    s.message(&message("ask why?", None)).await.unwrap();
    let call = r.calls.lock().unwrap()[0].clone();
    assert_eq!(call.env["COPILOT_ALLOW_ALL"], "false");
    assert!(!call.env.contains_key("copilot_allow_all"));
    assert!(!call.env.contains_key("Copilot_Home"));
    assert!(!PathBuf::from(&call.env["COPILOT_HOME"]).exists());
    assert!(call.input.starts_with(agentflow_core::chat::ask::SYSTEM_PROMPT));
}
#[tokio::test]
async fn test_questions_are_off_by_default_and_failures_are_short() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    assert!(s.message(&message("ask why?", None)).await.unwrap().text.starts_with("Questions are off."));
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "claude");
    *r.output.lock().unwrap() = Some(CommandResult {
        argv: vec![],
        returncode: 1,
        stdout: "PRIVATE stdout".into(),
        stderr: "PRIVATE stderr".into(),
    });
    assert_eq!(
        s.message(&message("ask why?", None)).await.unwrap().text,
        "The model could not answer; see the backend log."
    );
    assert_eq!(
        s.message(&message(&format!("ask {}", "x".repeat(2001)), None)).await.unwrap().text,
        "Questions are limited to 2000 characters."
    );
    assert_eq!(r.calls.lock().unwrap().len(), 1);
    *r.output.lock().unwrap() = None;
    s.message(&message("ask again?", Some("ts1"))).await.unwrap();
    assert!(!r.calls.lock().unwrap()[1].input.contains("Earlier question:"));
}
#[tokio::test]
async fn test_slack_acknowledges_a_message_before_a_slow_answer() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    r.blocked.store(true, Ordering::SeqCst);
    let s = asking(&h, &a, &r, "claude");
    s.start();
    let envelope = json!({"envelope_id":"ask","type":"events_api","payload":{"event_id":"Ev1","event":{"type":"message","channel":"D-operator","user":"U-operator","ts":"5.0","text":"why is it stuck?"}}});
    emit(&a, envelope.clone()).await;
    until(|| !r.calls.lock().unwrap().is_empty()).await;
    assert!(a.calls.lock().unwrap().iter().any(|(m, v)| m == "__ack" && v["envelope_id"] == "ask"));
    assert!(!a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postMessage"));
    emit(&a, envelope).await;
    until(|| a.calls.lock().unwrap().iter().filter(|(m, v)| m == "__ack" && v["envelope_id"] == "ask").count() == 2)
        .await;
    assert_eq!(r.calls.lock().unwrap().len(), 1);
    r.release.notify_one();
    until(|| a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postMessage")).await;
    s.stop();
}
#[tokio::test]
async fn stopping_the_service_cancels_an_inflight_answer_and_removes_isolated_directories() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    r.blocked.store(true, Ordering::SeqCst);
    let s = asking(&h, &a, &r, "claude");
    s.start();
    emit(&a,json!({"envelope_id":"ask","type":"events_api","payload":{"event":{"type":"message","channel":"D-operator","user":"U-operator","text":"ask why?"}}})).await;
    until(|| !r.calls.lock().unwrap().is_empty()).await;
    let cwd = r.calls.lock().unwrap()[0].cwd.clone();
    assert!(cwd.exists());
    s.stop();
    until(|| !cwd.exists()).await;
    assert!(!a.calls.lock().unwrap().iter().any(|(m, _)| m == "chat.postMessage"));
}

#[tokio::test]
async fn test_a_copilot_question_gets_its_last_answer_from_the_event_stream() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "copilot");
    *r.output.lock().unwrap()=Some(CommandResult {argv:vec![],returncode:0,stdout:"not json\n{\"type\":\"assistant.message\",\"data\":{\"content\":\"first\"}}\n{\"type\":\"assistant.message\",\"data\":{\"content\":\"Last answer\"}}\n{\"type\":\"result\",\"exitCode\":0,\"usage\":{\"premiumRequests\":1}}".into(),stderr:String::new()});
    assert!(s.message(&message("ask why?", None)).await.unwrap().text.starts_with("Last answer\n_AI answer"));
    let call = r.calls.lock().unwrap()[0].clone();
    assert_ne!(call.cwd, PathBuf::from(&call.env["COPILOT_HOME"]));
}
#[tokio::test]
async fn test_a_copilot_run_that_failed_or_had_tools_is_never_an_answer() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "copilot");
    for stdout in [
        "",
        "{\"type\":\"assistant.message\",\"data\":{\"content\":\"PRIVATE\"}}",
        "{\"type\":\"result\",\"exitCode\":1}",
        "{\"type\":\"assistant.message\",\"data\":{\"content\":\"PRIVATE\",\"toolRequests\":[{\"name\":\"view\"}]}}\n{\"type\":\"result\",\"exitCode\":0}",
        "{\"type\":\"tool.execution_start\"}\n{\"type\":\"result\",\"exitCode\":0}",
    ] {
        *r.output.lock().unwrap() =
            Some(CommandResult { argv: vec![], returncode: 0, stdout: stdout.into(), stderr: "PRIVATE".into() });
        assert_eq!(
            s.message(&message("ask why?", None)).await.unwrap().text,
            "The model could not answer; see the backend log."
        );
    }
}
#[tokio::test]
async fn test_an_open_question_thread_offers_the_answer_but_ask_still_asks() {
    use agentflow_core::engine::spec;
    let h = engine_support::engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(&h, run);
    engine_support::tab_seven(&h);
    h.engine.agent_question("TEST-1", spec::IMPLEMENTER, Some(run), "Which rollout?", &[]).await.unwrap();
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "claude");
    s.start();
    until(|| s.thread("TEST-1").is_some()).await;
    let thread = s.thread("TEST-1").unwrap();
    assert!(s.message(&message("gradual, over a week", Some(&thread))).await.unwrap().offer.is_some());
    assert!(r.calls.lock().unwrap().is_empty());
    assert!(s.message(&message("ask what did the implementer ask?", Some(&thread))).await.unwrap().offer.is_none());
    assert!(r.calls.lock().unwrap()[0].input.contains("Which rollout?"));
    assert!(h.engine.snapshot("TEST-1").unwrap()["questions"][0]["answer"].is_null());
    s.stop();
}

#[tokio::test]
async fn test_agent_question_text_is_withheld_when_it_may_not_be_posted() {
    use agentflow_core::engine::spec;
    let h = engine_support::engine().await;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(&h, run);
    engine_support::tab_seven(&h);
    h.engine.agent_question("TEST-1", spec::IMPLEMENTER, Some(run), "PRIVATE rollout?", &[]).await.unwrap();
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let _ = asking(&h, &a, &r, "claude");
    let s = ChatService::new_with_ask_runner(
        h.engine.clone(),
        a,
        SlackConfig {
            user_id: Some("U-operator".into()),
            include_question_text: false,
            ask: h.config().slack.ask.clone(),
            ..Default::default()
        },
        r.clone(),
    );
    *s.channel.write().unwrap() = Some("D-operator".into());
    s.message(&message("ask what does TEST-1 need?", None)).await.unwrap();
    let input = r.calls.lock().unwrap()[0].input.clone();
    assert!(input.contains("not shared"));
    assert!(!input.contains("PRIVATE rollout?"));
}
#[tokio::test]
async fn test_the_model_sees_only_what_slack_may_post() {
    use agentflow_core::{config::SlackDetail, engine::spec};
    let h = engine_support::engine().await;
    h.with_task(|t| {
        t.task.status_message = Some("SECRET-STATUS".into());
        let i = t.node_index(spec::CODE_REVIEW).unwrap();
        t.nodes[i].error = Some("SECRET-ERROR".into());
        t.nodes[i].state = "FAILED".into();
    });
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let _ = asking(&h, &a, &r, "claude");
    for allowed in [SlackDetail::Cost, SlackDetail::Errors] {
        let s = ChatService::new_with_ask_runner(
            h.engine.clone(),
            a.clone(),
            SlackConfig {
                user_id: Some("U-operator".into()),
                details: vec![allowed],
                ask: h.config().slack.ask.clone(),
                ..Default::default()
            },
            r.clone(),
        );
        *s.channel.write().unwrap() = Some("D-operator".into());
        s.message(&message("why is TEST-1 stuck?", None)).await.unwrap();
    }
    let calls = r.calls.lock().unwrap();
    assert!(!calls[0].input.contains("SECRET"));
    assert!(!calls[0].input.contains("repositoryRoot"));
    assert!(!calls[0].input.contains("worktreePath"));
    assert!(calls[1].input.contains("SECRET-ERROR"));
    assert!(calls[1].input.contains("SECRET-STATUS"));
}

#[tokio::test]
async fn test_cli_failures_become_short_reasons() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let s = asking(&h, &a, &r, "claude");
    for (code, stdout) in [(1, "PRIVATE"), (0, "not json"), (0, "{\"is_error\":true,\"result\":\"PRIVATE\"}")] {
        *r.output.lock().unwrap() =
            Some(CommandResult { argv: vec![], returncode: code, stdout: stdout.into(), stderr: "PRIVATE".into() });
        assert_eq!(
            s.message(&message("ask why?", None)).await.unwrap().text,
            "The model could not answer; see the backend log."
        );
    }
    *r.error.lock().unwrap() = Some("No answer within 90 seconds; try again.".into());
    assert_eq!(s.message(&message("ask why?", None)).await.unwrap().text, "No answer within 90 seconds; try again.");
}
#[tokio::test]
async fn test_the_model_sees_the_pr_and_ticket_only_when_allowed() {
    use agentflow_core::config::SlackDetail;
    let h = engine_support::engine().await;
    h.with_task(|t| t.task.pr_url = Some("https://github.com/o/r/pull/7".into()));
    let dir = h.task_dir().join("TEST-1/context");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("jira-issue.md"),"# TEST-1: Title\n\n## Description\n\nTICKET-TEXT\n\n## Comments (1)\n\n### Comment 1 - Author\n\nCOMMENT-TEXT\n").unwrap();
    let a = Arc::new(Adapter::default());
    let r = Arc::new(Runner::default());
    let _ = asking(&h, &a, &r, "claude");
    for details in [vec![], vec![SlackDetail::Pr, SlackDetail::Ticket]] {
        let s = ChatService::new_with_ask_runner(
            h.engine.clone(),
            a.clone(),
            SlackConfig {
                user_id: Some("U-operator".into()),
                ask: h.config().slack.ask.clone(),
                details,
                ..Default::default()
            },
            r.clone(),
        );
        *s.channel.write().unwrap() = Some("D-operator".into());
        s.message(&message("ask has TEST-1's PR merged?", None)).await.unwrap();
    }
    let calls = r.calls.lock().unwrap();
    assert!(!calls[0].input.contains("TICKET-TEXT"));
    assert!(!calls[0].input.contains("pull/7"));
    assert!(calls[1].input.contains("pull/7"));
    assert!(calls[1].input.contains("TICKET-TEXT"));
    assert!(calls[1].input.contains("COMMENT-TEXT"));
}
