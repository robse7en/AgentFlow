//! Moving the Jira ticket to In Code Review once a task's pull request is created.
mod engine_support;
use agentflow_core::{
    config::JiraConfig,
    db::DbTime,
    engine::Engine,
    github::PullRequest,
    jira::{HttpReply, JiraError, JiraService, JiraTransport, TransitionOutcome},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

const URL: &str = "https://github.com/acme/repo/pull/7";

/// Replies in order and records (method, body). With an engine, fails the test if TEST-1's
/// lock is held while Jira is called.
#[derive(Default)]
struct Jira {
    calls: Mutex<Vec<(String, Option<Value>)>>,
    replies: Mutex<VecDeque<(u16, Value)>>,
    engine: Mutex<Option<Weak<Engine>>>,
}
impl Jira {
    fn reply(&self, status: u16, body: Value) {
        self.replies.lock().unwrap().push_back((status, body));
    }
    fn status(&self, name: &str) {
        self.reply(200, json!({"fields":{"status":{"name":name}}}));
    }
    fn review(&self) {
        self.reply(200, json!({"transitions":[{"id":"41","to":{"name":"In Code Review"}}]}));
    }
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}
#[async_trait::async_trait]
impl JiraTransport for Jira {
    async fn request(
        &self,
        method: &str,
        _: &str,
        _: HashMap<String, String>,
        body: Option<Value>,
        _: u64,
        _: Duration,
    ) -> Result<HttpReply, JiraError> {
        if let Some(engine) = self.engine.lock().unwrap().as_ref().and_then(Weak::upgrade) {
            assert!(engine.lock("TEST-1").try_lock().is_ok(), "Jira called with the task lock held");
        }
        self.calls.lock().unwrap().push((method.into(), body));
        let (status, body) = self.replies.lock().unwrap().pop_front().expect("unexpected request");
        Ok(HttpReply { status, headers: HashMap::new(), body: serde_json::to_vec(&body).unwrap() })
    }
}
fn config() -> JiraConfig {
    JiraConfig {
        enabled: true,
        base_url: Some("https://jira.example.test".into()),
        email: Some("test@example.test".into()),
        api_token: Some("fixture-token".into()),
        ..Default::default()
    }
}
fn service() -> (JiraService, Arc<Jira>) {
    let t = Arc::new(Jira::default());
    (JiraService::with_transport(config(), t.clone()).unwrap(), t)
}

#[test]
fn test_defaults_move_to_in_code_review_from_any_status() {
    let config = JiraConfig::default();
    assert_eq!(config.pr_created_transition_status.as_deref(), Some("In Code Review"));
    assert_eq!(config.pr_created_transition_from_statuses, vec!["*".to_string()]);
    let off: JiraConfig = serde_json::from_value(json!({"prCreatedTransitionStatus":null})).unwrap();
    assert!(off.pr_created_transition_status.is_none());
}
#[tokio::test]
async fn test_star_allows_any_status() {
    let (s, t) = service();
    t.status("Blocked");
    t.review();
    t.reply(204, json!({}));
    let outcome = s.transition("DE-1", "In Code Review", &["*".into()]).await.unwrap();
    assert_eq!(outcome, TransitionOutcome::Moved("Blocked".into()));
    assert_eq!(t.calls.lock().unwrap()[2], ("POST".into(), Some(json!({"transition":{"id":"41"}}))));
}
#[tokio::test]
async fn test_transition_says_why_it_did_not_move() {
    // Already there: no self-transition.
    let (s, t) = service();
    t.status("in code review");
    let outcome = s.transition("DE-1", "In Code Review", &["*".into()]).await.unwrap();
    assert_eq!(outcome, TransitionOutcome::Already("in code review".into()));
    assert_eq!(t.calls(), 1);

    let (s, t) = service();
    t.status("Blocked");
    let outcome = s.transition("DE-1", "In Code Review", &["In Progress".into()]).await.unwrap();
    assert_eq!(outcome, TransitionOutcome::NotAllowed("Blocked".into()));
    assert_eq!(t.calls(), 1);

    // The workflow has no such status.
    let (s, t) = service();
    t.status("In Progress");
    t.reply(200, json!({"transitions":[]}));
    assert!(!s.transition_to("DE-1", "In Code Review", &["*".into()]).await.unwrap());
    assert_eq!(t.calls(), 2);
}

// engine -------------------------------------------------------------------------------------
fn ready(h: &engine_support::Harness) -> Arc<Jira> {
    h.configure(|c| c.jira = config());
    h.set_status("READY_FOR_PR");
    let t = Arc::new(Jira::default());
    *t.engine.lock().unwrap() = Some(Arc::downgrade(&h.engine));
    h.engine.set_jira_transport(t.clone());
    t
}
fn events(snap: &Value) -> Vec<(String, String)> {
    snap["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["level"].as_str().unwrap().into(), e["message"].as_str().unwrap().into()))
        .collect()
}
fn pr(state: &str) -> PullRequest {
    PullRequest {
        url: URL.into(),
        state: state.into(),
        created_at: DbTime::now(),
        merged_at: (state == "MERGED").then(DbTime::now),
        head_sha: Some(engine_support::SHA.into()),
        base_branch: Some("main".into()),
    }
}
#[tokio::test]
async fn test_marking_the_pr_created_moves_the_ticket_outside_the_task_lock() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    t.status("In Progress");
    t.review();
    t.reply(204, json!({}));
    let snap = h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert_eq!(snap["status"], "PR_CREATED");
    assert!(events(&snap).contains(&("info".into(), "Jira ticket moved from In Progress to In Code Review".into())));
    assert_eq!(t.calls(), 3);
}
#[tokio::test]
async fn test_a_jira_failure_never_undoes_the_pr() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    t.reply(401, json!({}));
    let snap = h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert_eq!(snap["status"], "PR_CREATED");
    assert!(events(&snap).iter().any(|(level, message)| level == "warning"
        && message.starts_with("Could not move the Jira ticket to In Code Review")));
}
#[tokio::test]
async fn test_a_missing_review_status_is_logged_as_a_warning() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    t.status("In Progress");
    t.reply(200, json!({"transitions":[]}));
    let snap = h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(
        events(&snap).iter().any(|(level, message)| level == "warning"
            && message.contains("its workflow has no transition to In Code Review"))
    );
}
#[tokio::test]
async fn test_already_in_review_logs_nothing() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    t.status("In Code Review");
    let snap = h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(!events(&snap).iter().any(|(_, message)| message.contains("Jira")));
}
#[tokio::test]
async fn test_custom_status_and_from_list_are_used_and_null_turns_it_off() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    h.configure(|c| {
        c.jira.pr_created_transition_status = Some("Review".into());
        c.jira.pr_created_transition_from_statuses = vec!["To Do".into()];
    });
    t.status("In Progress");
    let snap = h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(
        events(&snap)
            .iter()
            .any(|(_, m)| m
                == "Jira ticket left in In Progress: jira.prCreatedTransitionFromStatuses does not include it")
    );
    h.engine.revert_pr_created("TEST-1").await.unwrap();
    h.configure(|c| c.jira.pr_created_transition_status = None);
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert_eq!(t.calls(), 1);
}
#[tokio::test]
async fn test_github_sync_moves_the_ticket_once_when_it_finds_the_pr() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    h.configure(|c| c.github.pr_sync = true);
    t.status("In Progress");
    t.review();
    t.reply(204, json!({}));
    h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap();
    h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap(); // unchanged
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    h.engine.revert_pr_merged("TEST-1").await.unwrap(); // back to PR Created: not a new PR
    assert_eq!(h.load().task.status, "PR_CREATED");
    assert_eq!(t.calls(), 3);
}
#[tokio::test]
async fn test_a_pr_first_seen_merged_does_not_move_the_ticket_to_review() {
    let h = engine_support::engine().await;
    let t = ready(&h);
    h.configure(|c| c.github.pr_sync = true);
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    assert_eq!(h.load().task.status, "MERGED");
    assert_eq!(t.calls(), 0);
}
