mod engine_support;
use agentflow_core::{
    config::GitHubConfig,
    db::DbTime,
    github::{GitHubError, GitHubSource, PullRequest},
};
use engine_support::engine;
use std::sync::{Arc, Mutex};
const URL: &str = "https://github.com/acme/repo/pull/7";
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
/// TEST-1's event messages, filtered by an SQL `WHERE` clause (or "").
fn messages(h: &engine_support::Harness, filter: &str) -> Vec<String> {
    h.engine
        .db
        .session(|tx| {
            let mut statement = tx.prepare(&format!("SELECT message FROM workflow_events {filter} ORDER BY id"))?;
            let messages = statement.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, rusqlite::Error>(messages)
        })
        .unwrap()
}
fn warnings(h: &engine_support::Harness) -> Vec<String> {
    messages(h, "WHERE level = 'warning'")
}
#[tokio::test]
async fn test_pr_lifecycle_advances_and_records_url() {
    let h = engine().await;
    h.configure(|c| c.github.pr_sync = true);
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert_eq!(h.load().task.status, "PR_CREATED");
    h.engine.mark_pr_merged("TEST-1").await.unwrap();
    assert!(h.load().task.merged_at.is_some());
    h.engine.revert_pr_merged("TEST-1").await.unwrap();
    assert!(h.load().task.pr_sync_paused);
    assert!(h.load().task.merged_at.is_none());
    h.engine.revert_pr_created("TEST-1").await.unwrap();
    assert_eq!(h.load().task.status, "READY_FOR_PR");
    assert!(h.load().task.pr_url.is_none());
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(!h.load().task.pr_sync_paused);
}
#[tokio::test]
async fn test_pr_lifecycle_rejects_out_of_order_transitions() {
    let h = engine().await;
    assert_eq!(h.engine.mark_pr_created("TEST-1", URL).await.unwrap_err().status, 409);
    assert_eq!(h.engine.mark_pr_merged("TEST-1").await.unwrap_err().status, 409);
    assert!(h.engine.revert_pr_created("TEST-1").await.is_err());
    assert!(h.engine.revert_pr_merged("TEST-1").await.is_err());
}
#[tokio::test]
async fn test_open_pr_marks_task_pr_created() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    assert!(h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap().is_some());
    assert!(h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap().is_none());
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    assert_eq!(h.load().task.status, "MERGED");
    assert_eq!(h.load().task.merged_head_sha.as_deref(), Some(engine_support::SHA));
    h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap();
    assert_eq!(h.load().task.status, "MERGED");
}
#[tokio::test]
async fn test_already_merged_pr_moves_straight_to_merged() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    assert!(h.engine.apply_github_pr("TEST-1", &pr("CLOSED")).await.unwrap().is_none());
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    assert_eq!(h.load().task.status, "MERGED");
}
#[tokio::test]
async fn test_closed_pr_is_noted_once_and_cleared_on_reopen() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(h.engine.apply_github_pr("TEST-1", &pr("CLOSED")).await.unwrap().is_some());
    assert!(h.engine.apply_github_pr("TEST-1", &pr("CLOSED")).await.unwrap().is_none());
    assert!(h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap().is_some());
    let mut other = pr("MERGED");
    other.url = "https://github.com/acme/repo/pull/8".into();
    assert!(h.engine.apply_github_pr("TEST-1", &other).await.unwrap().is_none());
    assert_eq!(h.load().task.status, "PR_CREATED");
}
#[tokio::test]
async fn test_undo_pauses_sync_until_the_human_marks_by_hand() {
    let h = engine().await;
    h.configure(|c| c.github.pr_sync = true);
    h.set_status("READY_FOR_PR");
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    h.engine.revert_pr_merged("TEST-1").await.unwrap();
    assert!(h.load().task.merged_head_sha.is_none());
    assert!(h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap().is_none());
    assert_eq!(h.load().task.status, "PR_CREATED");
    h.engine.mark_pr_merged("TEST-1").await.unwrap();
    assert!(!h.load().task.pr_sync_paused);
}
#[tokio::test]
async fn test_undo_does_not_pause_when_sync_is_off() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.revert_pr_created("TEST-1").await.unwrap();
    assert!(!h.load().task.pr_sync_paused);
}
#[test]
fn pr_url_validation_and_selection_dates() {
    use agentflow_core::{engine::pr_tracking::clean_pr_url, github::pick_pull_request};
    assert_eq!(clean_pr_url(&format!(" {URL} ")).unwrap(), URL);
    for url in ["javascript:alert(1)", "https://x/\nfoo", "https://x/\tfoo"] {
        assert!(clean_pr_url(url).is_err());
    }
    let since = DbTime::now();
    let mut old = pr("MERGED");
    old.created_at.0 = since.0 - chrono::Duration::days(1);
    assert!(pick_pull_request(&[old], &since).is_none());
    let mut m = pr("MERGED");
    m.created_at.0 = since.0 + chrono::Duration::hours(1);
    let mut o = pr("OPEN");
    o.created_at.0 = since.0 + chrono::Duration::seconds(1);
    assert_eq!(pick_pull_request(&[m, o], &since).unwrap().state, "OPEN");
}
struct Source {
    answer: Mutex<Result<PullRequest, GitHubError>>,
    calls: Mutex<Vec<String>>,
}
impl Source {
    fn answering(answer: Result<PullRequest, GitHubError>) -> Arc<Self> {
        Arc::new(Source { answer: Mutex::new(answer), calls: Mutex::new(vec![]) })
    }
}
#[async_trait::async_trait]
impl GitHubSource for Source {
    async fn pull_requests_for_branch(
        &self,
        _: &GitHubConfig,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<PullRequest>, GitHubError> {
        self.calls.lock().unwrap().push(format!("{repo} {branch}"));
        self.answer.lock().unwrap().clone().map(|p| vec![p])
    }
    async fn pull_request(&self, _: &GitHubConfig, url: &str) -> Result<PullRequest, GitHubError> {
        self.calls.lock().unwrap().push(url.into());
        self.answer.lock().unwrap().clone()
    }
    async fn retarget(&self, _: &GitHubConfig, url: &str, base: &str) -> Result<(), GitHubError> {
        self.calls.lock().unwrap().push(format!("retarget {url} {base}"));
        self.answer.lock().unwrap().clone().map(drop)
    }
}
#[tokio::test]
async fn polling_recorded_url_errors_dedup_recovery_and_pause() {
    let h = engine().await;
    h.configure(|c| c.github.pr_sync = true);
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    let s =
        Arc::new(Source { answer: Mutex::new(Err(GitHubError::lookup("fixture error"))), calls: Mutex::new(vec![]) });
    h.engine.set_github_source(s.clone());
    h.engine.poll_prs().await.unwrap();
    h.engine.poll_prs().await.unwrap();
    assert_eq!(h.engine.pr_sync_problems().len(), 1);
    *s.answer.lock().unwrap() = Ok(pr("MERGED"));
    h.engine.poll_prs().await.unwrap();
    assert!(h.engine.pr_sync_problems().is_empty());
    assert_eq!(h.load().task.status, "MERGED");
    h.engine.revert_pr_merged("TEST-1").await.unwrap();
    let count = s.calls.lock().unwrap().len();
    h.engine.poll_prs().await.unwrap();
    assert_eq!(s.calls.lock().unwrap().len(), count);
}

#[tokio::test]
async fn polling_origin_lookup_ignores_non_github_and_backfills_existing_worktree() {
    let h = engine().await;
    h.configure(|c| c.github.pr_sync = true);
    h.set_status("READY_FOR_PR");
    let s = Arc::new(Source { answer: Mutex::new(Ok(pr("MERGED"))), calls: Mutex::new(vec![]) });
    h.engine.set_github_source(s.clone());
    h.git
        .returns("remote_url", agentflow_core::vcs::Outcome::Ok(serde_json::json!("https://gitlab.com/acme/repo.git")));
    h.engine.poll_prs().await.unwrap();
    assert!(s.calls.lock().unwrap().is_empty());
    h.git
        .returns("remote_url", agentflow_core::vcs::Outcome::Ok(serde_json::json!("https://github.com/acme/repo.git")));
    h.engine.poll_prs().await.unwrap();
    assert_eq!(h.load().task.status, "MERGED");
    assert!(s.calls.lock().unwrap()[0].starts_with("github.com/acme/repo "));
    h.with_task(|task| task.task.merged_head_sha = None);
    let count = s.calls.lock().unwrap().len();
    h.engine.poll_prs().await.unwrap();
    assert_eq!(s.calls.lock().unwrap().len(), count);
    std::fs::create_dir_all(&h.load().task.worktree_path).unwrap();
    h.engine.poll_prs().await.unwrap();
    assert_eq!(h.load().task.merged_head_sha.as_deref(), Some(engine_support::SHA));
}

#[tokio::test]
async fn test_pr_lifecycle_is_reversible() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.mark_pr_merged("TEST-1").await.unwrap();
    h.engine.revert_pr_merged("TEST-1").await.unwrap();
    assert_eq!(h.load().task.pr_url.as_deref(), Some(URL));
    assert!(h.load().task.merged_at.is_none());
    h.engine.revert_pr_created("TEST-1").await.unwrap();
    assert!(h.load().task.pr_created_at.is_none());
}
#[tokio::test]
async fn test_no_further_nodes_approvals_or_agent_signals_once_pr_created() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert_eq!(h.engine.approve("TEST-1", "requirements_gate", Default::default()).await.unwrap_err().status, 409);
    assert_eq!(h.engine.run_node("TEST-1", "implementer", Default::default(), false).await.unwrap_err().status, 409);
    assert_eq!(h.engine.agent_running("TEST-1", "implementer", None).await.unwrap_err().status, 409);
}
#[tokio::test]
async fn test_mark_pr_created_endpoint_validates_url_and_auth() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    let app = agentflow_core::api::App::around(h.engine.clone(), h.tmp().join("config.json"));
    let request = Request::builder()
        .method("POST")
        .uri("/api/tasks/TEST-1/pr/created")
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({"prUrl":URL}).to_string()))
        .unwrap();
    assert_eq!(agentflow_core::api::router(app).oneshot(request).await.unwrap().status().as_u16(), 401);
    assert_eq!(
        h.route("POST", "/api/tasks/TEST-1/pr/created", Some(serde_json::json!({"prUrl":"invalid"}))).await.0,
        422
    );
    assert_eq!(h.route("POST", "/api/tasks/TEST-1/pr/created", Some(serde_json::json!({"prUrl":URL}))).await.0, 200);
}
#[test]
fn test_repo_from_remote() {
    use agentflow_core::github::repo_from_remote;
    for (remote, expected) in [
        ("https://github.com/acme/repo.git", Some("github.com/acme/repo")),
        ("https://user@github.com/acme/repo.git/", Some("github.com/acme/repo")),
        ("ssh://git@github.com:22/acme/repo.git", Some("github.com/acme/repo")),
        ("/local/path/repo", None),
        ("https://github.com/acme/group/repo.git", None),
    ] {
        assert_eq!(repo_from_remote(remote, &["github.com".into()]).as_deref(), expected);
    }
}
#[test]
fn test_enterprise_host_is_only_used_when_configured() {
    use agentflow_core::github::{is_pr_url, repo_from_remote};
    let remote = "git@github.acme.test:team/repo.git";
    assert!(repo_from_remote(remote, &["github.com".into()]).is_none());
    assert!(repo_from_remote(remote, &["github.acme.test".into()]).is_some());
    assert!(is_pr_url("https://github.acme.test/team/repo/pull/3", &["GitHub.acme.test".into()]));
}
#[test]
fn test_is_pr_url() {
    use agentflow_core::github::is_pr_url;
    let hosts = ["github.com".into()];
    assert!(is_pr_url(URL, &hosts));
    assert!(!is_pr_url(&format!("{URL}/files"), &hosts));
    assert!(!is_pr_url("https://bitbucket.org/acme/repo/pull-requests/7", &hosts));
}
#[test]
fn test_pick_prefers_newest_open_then_newest_merged() {
    use agentflow_core::github::pick_pull_request;
    let since = DbTime::now();
    let mut one = pr("MERGED");
    one.created_at.0 = since.0 + chrono::Duration::minutes(1);
    let mut two = one.clone();
    two.url = "newer".into();
    two.created_at.0 += chrono::Duration::minutes(1);
    let mut open = one.clone();
    open.state = "OPEN".into();
    assert_eq!(pick_pull_request(&[one.clone(), two.clone(), open], &since).unwrap().state, "OPEN");
    assert_eq!(pick_pull_request(&[one, two], &since).unwrap().url, "newer");
}
#[test]
fn test_pick_ignores_prs_older_than_the_task() {
    let since = DbTime::now();
    let mut old = pr("MERGED");
    old.created_at.0 = since.0 - chrono::Duration::days(30);
    assert!(agentflow_core::github::pick_pull_request(&[old], &since).is_none());
}
#[tokio::test]
async fn test_unchanged_pr_writes_and_publishes_nothing() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    let p = pr("OPEN");
    h.engine.apply_github_pr("TEST-1", &p).await.unwrap();
    let at = h.load().task.updated_at;
    let count = h.count("workflow_events");
    assert!(h.engine.apply_github_pr("TEST-1", &p).await.unwrap().is_none());
    assert_eq!(h.load().task.updated_at, at);
    assert_eq!(h.count("workflow_events"), count);
}
#[tokio::test]
async fn test_closed_pr_never_marks_a_ready_task() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    assert!(h.engine.apply_github_pr("TEST-1", &pr("CLOSED")).await.unwrap().is_none());
    assert_eq!(h.load().task.status, "READY_FOR_PR");
}
#[tokio::test]
async fn test_github_state_only_applies_to_the_recorded_pr() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    let mut other = pr("MERGED");
    other.url += "8";
    assert!(h.engine.apply_github_pr("TEST-1", &other).await.unwrap().is_none());
    assert!(h.load().task.merged_head_sha.is_none());
}
#[tokio::test]
async fn test_manual_pr_created_resumes_sync() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.configure(|c| c.github.pr_sync = true);
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.revert_pr_created("TEST-1").await.unwrap();
    assert!(h.load().task.pr_sync_paused);
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(!h.load().task.pr_sync_paused);
}
async fn poll_fixture() -> (engine_support::Harness, Arc<Source>) {
    let h = engine().await;
    h.configure(|c| c.github.pr_sync = true);
    h.set_status("READY_FOR_PR");
    h.git
        .returns("remote_url", agentflow_core::vcs::Outcome::Ok(serde_json::json!("https://github.com/acme/repo.git")));
    let source = Arc::new(Source { answer: Mutex::new(Ok(pr("OPEN"))), calls: Mutex::new(vec![]) });
    h.engine.set_github_source(source.clone());
    (h, source)
}
#[tokio::test]
async fn test_poll_finds_ready_task_pr_by_branch_on_origin() {
    let (h, s) = poll_fixture().await;
    h.engine.poll_prs().await.unwrap();
    assert_eq!(h.load().task.status, "PR_CREATED");
    assert_eq!(s.calls.lock().unwrap()[0], "github.com/acme/repo agent/TEST-1");
}
#[tokio::test]
async fn test_poll_follows_recorded_pr_url_for_pr_created_task() {
    let (h, s) = poll_fixture().await;
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.poll_prs().await.unwrap();
    assert_eq!(s.calls.lock().unwrap()[0], URL);
}
#[tokio::test]
async fn test_poll_skips_non_github_remotes_and_pr_urls() {
    let (h, s) = poll_fixture().await;
    h.git
        .returns("remote_url", agentflow_core::vcs::Outcome::Ok(serde_json::json!("https://gitlab.com/acme/repo.git")));
    h.engine.poll_prs().await.unwrap();
    h.engine.mark_pr_created("TEST-1", "https://example.test/pr/3").await.unwrap();
    h.engine.poll_prs().await.unwrap();
    assert!(s.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_poll_skips_paused_tasks() {
    let (h, s) = poll_fixture().await;
    h.with_task(|t| t.task.pr_sync_paused = true);
    h.engine.poll_prs().await.unwrap();
    assert!(s.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_poll_logs_a_gh_problem_once_and_recovery() {
    let (h, s) = poll_fixture().await;
    *s.answer.lock().unwrap() = Err(GitHubError::unavailable("fixture login failure"));
    h.engine.poll_prs().await.unwrap();
    h.engine.poll_prs().await.unwrap();
    assert_eq!(h.engine.pr_sync_problems().len(), 1);
    *s.answer.lock().unwrap() = Ok(pr("OPEN"));
    h.engine.poll_prs().await.unwrap();
    assert!(h.engine.pr_sync_problems().is_empty());
}
#[tokio::test]
async fn test_poll_does_not_claim_recovery_without_a_gh_answer() {
    let (h, s) = poll_fixture().await;
    *s.answer.lock().unwrap() = Err(GitHubError::unavailable("fixture login failure"));
    h.engine.poll_prs().await.unwrap();
    h.git
        .returns("remote_url", agentflow_core::vcs::Outcome::Ok(serde_json::json!("https://gitlab.com/acme/repo.git")));
    h.engine.poll_prs().await.unwrap();
    assert!(h.engine.pr_sync_problems().contains_key(&None));
}
#[tokio::test]
async fn test_poll_survives_a_failing_lookup() {
    let (h, s) = poll_fixture().await;
    *s.answer.lock().unwrap() = Err(GitHubError::lookup("fixture HTTP 404"));
    h.engine.poll_prs().await.unwrap();
    assert_eq!(h.load().task.status, "READY_FOR_PR");
    assert!(h.engine.pr_sync_problems().contains_key(&Some("TEST-1".into())));
}
#[tokio::test]
async fn test_detected_merge_records_the_merged_head_and_undo_clears_it() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    assert!(h.load().task.merged_head_sha.is_some());
    h.engine.revert_pr_merged("TEST-1").await.unwrap();
    assert!(h.load().task.merged_head_sha.is_none());
}
#[tokio::test]
async fn test_merged_head_is_backfilled_for_a_task_marked_merged_by_hand() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.mark_pr_merged("TEST-1").await.unwrap();
    assert!(h.load().task.merged_head_sha.is_none());
    h.engine.apply_github_pr("TEST-1", &pr("MERGED")).await.unwrap();
    assert_eq!(h.load().task.merged_head_sha.as_deref(), Some(engine_support::SHA));
}
#[tokio::test]
async fn test_merged_head_is_not_recorded_unless_github_shows_that_pr_merged() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.mark_pr_merged("TEST-1").await.unwrap();
    h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap();
    assert!(h.load().task.merged_head_sha.is_none());
}
#[tokio::test]
async fn test_poll_backfills_merged_tasks_only_while_their_worktree_exists() {
    let (h, s) = poll_fixture().await;
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    h.engine.mark_pr_merged("TEST-1").await.unwrap();
    *s.answer.lock().unwrap() = Ok(pr("MERGED"));
    h.engine.poll_prs().await.unwrap();
    assert!(s.calls.lock().unwrap().is_empty());
    std::fs::create_dir_all(&h.load().task.worktree_path).unwrap();
    h.engine.poll_prs().await.unwrap();
    assert!(h.load().task.merged_head_sha.is_some());
}

fn targeting(state: &str, base: &str) -> PullRequest {
    PullRequest { base_branch: Some(base.into()), ..pr(state) }
}
#[tokio::test]
async fn test_detected_pr_records_its_base_and_warns_only_when_it_differs() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.apply_github_pr("TEST-1", &pr("OPEN")).await.unwrap();
    assert_eq!(h.load().task.pr_base_branch.as_deref(), Some("main"));
    assert!(warnings(&h).is_empty());
    // Only the base changes (retargeted on GitHub): still saved, published and warned about once.
    let wrong = targeting("OPEN", "release/candidate");
    assert!(h.engine.apply_github_pr("TEST-1", &wrong).await.unwrap().is_some());
    assert_eq!(h.load().task.pr_base_branch.as_deref(), Some("release/candidate"));
    assert_eq!(warnings(&h), ["The pull request targets release/candidate, but the task started from main"]);
    assert!(h.engine.apply_github_pr("TEST-1", &wrong).await.unwrap().is_none());
    assert_eq!(warnings(&h).len(), 1);
    let snapshot = h.engine.snapshot("TEST-1").unwrap();
    assert_eq!(snapshot["prBaseBranch"], "release/candidate");
}
#[tokio::test]
async fn test_base_of_another_or_closed_pr_is_not_recorded() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.apply_github_pr("TEST-1", &targeting("CLOSED", "develop")).await.unwrap();
    assert!(h.load().task.pr_base_branch.is_none());
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    let mut other = targeting("OPEN", "develop");
    other.url += "8";
    assert!(h.engine.apply_github_pr("TEST-1", &other).await.unwrap().is_none());
    assert!(h.load().task.pr_base_branch.is_none());
}
#[tokio::test]
async fn test_marking_or_undoing_pr_created_forgets_the_recorded_base() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    h.engine.apply_github_pr("TEST-1", &targeting("OPEN", "develop")).await.unwrap();
    h.engine.revert_pr_created("TEST-1").await.unwrap();
    assert!(h.load().task.pr_base_branch.is_none());
    h.engine.apply_github_pr("TEST-1", &targeting("OPEN", "develop")).await.unwrap();
    h.with_task(|t| t.task.status = "READY_FOR_PR".into());
    h.engine.mark_pr_created("TEST-1", URL).await.unwrap();
    assert!(h.load().task.pr_base_branch.is_none());
}
#[tokio::test]
async fn test_retarget_points_the_recorded_pr_at_the_task_base() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    let s = Source::answering(Ok(pr("OPEN")));
    h.engine.set_github_source(s.clone());
    assert_eq!(h.engine.retarget_pr("TEST-1").await.unwrap_err().status, 409);
    h.engine.apply_github_pr("TEST-1", &targeting("OPEN", "develop")).await.unwrap();
    let snapshot = h.engine.retarget_pr("TEST-1").await.unwrap();
    assert_eq!(snapshot["prBaseBranch"], "main");
    assert_eq!(*s.calls.lock().unwrap(), [format!("retarget {URL} main")]);
    assert!(messages(&h, "").iter().any(|m| m == "Human retargeted the pull request to main"));
}
#[tokio::test]
async fn test_retarget_refuses_links_it_cannot_edit_and_keeps_the_base_when_gh_fails() {
    let h = engine().await;
    h.set_status("READY_FOR_PR");
    let s = Source::answering(Err(GitHubError::lookup("gh pr edit failed: exit 1")));
    h.engine.set_github_source(s.clone());
    h.engine.mark_pr_created("TEST-1", "https://example.test/pr/3").await.unwrap();
    assert_eq!(h.engine.retarget_pr("TEST-1").await.unwrap_err().status, 409);
    assert!(s.calls.lock().unwrap().is_empty());
    h.with_task(|t| {
        t.task.pr_url = Some(URL.into());
        t.task.pr_base_branch = Some("develop".into());
    });
    let error = h.engine.retarget_pr("TEST-1").await.unwrap_err();
    assert_eq!(error.status, 502);
    assert!(error.message.contains("gh pr edit failed"));
    assert_eq!(h.load().task.pr_base_branch.as_deref(), Some("develop"));
}

/// Ready for PR on a GitHub origin, the branch at the reviewed checkpoint.
fn openable(h: &engine_support::Harness) {
    use agentflow_core::vcs::Outcome;
    h.set_status("READY_FOR_PR");
    h.with_task(|t| t.task.base_branch = "release/candidate".into());
    h.git.returns("remote_url", Outcome::Ok(serde_json::json!("git@github.com:acme/repo.git")));
    h.git.returns("branch_sha", Outcome::Ok(serde_json::json!(engine_support::SHA)));
}
#[tokio::test]
async fn test_open_pr_pushes_the_reviewed_branch_and_targets_the_task_base() {
    let h = engine().await;
    openable(&h);
    let answer = h.engine.open_pr("TEST-1").await.unwrap();
    assert_eq!(answer["url"], "https://github.com/acme/repo/compare/release/candidate...agent/TEST-1?expand=1");
    assert_eq!(answer["task"]["status"], "READY_FOR_PR");
    let root = h.load().task.repository_root.clone();
    // No worktree directory in this fixture, so the push runs from the repository.
    assert_eq!(h.git.calls_of("push_branch"), [vec![root, "agent/TEST-1".to_string()]]);
    std::fs::create_dir_all(&h.load().task.worktree_path).unwrap();
    h.engine.open_pr("TEST-1").await.unwrap();
    assert_eq!(h.git.calls_of("push_branch")[1][0], h.load().task.worktree_path.clone());
}
#[tokio::test]
async fn test_open_pr_pushes_nothing_unless_it_can_target_the_base() {
    use agentflow_core::vcs::Outcome;
    let h = engine().await;
    openable(&h);
    h.set_status("PR_CREATED");
    assert_eq!(h.engine.open_pr("TEST-1").await.unwrap_err().status, 409);
    h.set_status("READY_FOR_PR");
    h.git.returns("remote_url", Outcome::Ok(serde_json::json!("https://gitlab.com/acme/repo.git")));
    let error = h.engine.open_pr("TEST-1").await.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.message.contains("into release/candidate yourself"));
    h.git.returns("remote_url", Outcome::Ok(serde_json::json!(null)));
    assert_eq!(h.engine.open_pr("TEST-1").await.unwrap_err().status, 409);
    h.git.returns("remote_url", Outcome::Ok(serde_json::json!("https://github.com/acme/repo.git")));
    h.git.returns("branch_sha", Outcome::Ok(serde_json::json!("b".repeat(40))));
    assert!(h.engine.open_pr("TEST-1").await.unwrap_err().message.contains("not at the reviewed checkpoint"));
    h.git.returns("branch_sha", Outcome::Ok(serde_json::json!(null)));
    assert_eq!(h.engine.open_pr("TEST-1").await.unwrap_err().status, 409);
    assert!(h.git.calls_of("push_branch").is_empty());
}
#[tokio::test]
async fn test_open_pr_reports_a_failed_push() {
    use agentflow_core::vcs::Outcome;
    let h = engine().await;
    openable(&h);
    h.git.returns("push_branch", Outcome::Failed("Pushing agent/TEST-1 to origin failed (git exit 1).".into()));
    let error = h.engine.open_pr("TEST-1").await.unwrap_err();
    assert_eq!(error.status, 502);
    assert!(error.message.contains("git exit 1"));
}
#[tokio::test]
async fn test_open_and_retarget_endpoints() {
    let h = engine().await;
    openable(&h);
    let (status, body) = h.route("POST", "/api/tasks/TEST-1/pr/open", None).await;
    assert_eq!(status, 200);
    assert!(body["url"].as_str().unwrap().contains("/compare/release/candidate...agent/TEST-1"));
    assert_eq!(h.route("POST", "/api/tasks/TEST-1/pr/retarget", None).await.0, 409);
    // An agent in the worktree can neither push nor edit the PR.
    for action in ["open", "retarget"] {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let app = agentflow_core::api::App::around(h.engine.clone(), h.tmp().join("config.json"));
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/tasks/TEST-1/pr/{action}"))
            .header("host", "127.0.0.1")
            .header("authorization", "Bearer test-token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(agentflow_core::api::router(app).oneshot(request).await.unwrap().status().as_u16(), 403);
    }
    assert!(h.git.calls_of("push_branch").len() == 1);
}
#[test]
fn test_compare_url_encodes_branch_names_but_keeps_slashes() {
    use agentflow_core::github::compare_url;
    assert_eq!(
        compare_url("github.com/acme/repo", "release/2.0", "agent/DE-1#x+y"),
        "https://github.com/acme/repo/compare/release/2.0...agent/DE-1%23x%2By?expand=1"
    );
}
#[test]
fn test_wrong_pr_base_is_announced_with_pr_notices_until_it_matches() {
    use agentflow_core::chat::notices::notices;
    let notify = ["READY_FOR_PR".to_string()];
    let snapshot = |status: &str, target: Option<&str>| serde_json::json!({"status": status, "baseBranch": "release/candidate", "prBaseBranch": target});
    let found = notices(&snapshot("PR_CREATED", Some("main")), &notify, false);
    assert_eq!(found.keys().collect::<Vec<_>>(), ["pr:base-mismatch"]);
    assert!(!found["pr:base-mismatch"].summary.contains("main"));
    for quiet in [
        snapshot("PR_CREATED", Some("release/candidate")),
        snapshot("PR_CREATED", None),
        snapshot("MERGED", Some("main")),
    ] {
        assert!(notices(&quiet, &notify, false).is_empty());
    }
    assert!(notices(&snapshot("PR_CREATED", Some("main")), &[], false).is_empty());
}
