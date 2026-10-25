//! Offline provider and attachment tests; transport records requests without networking.
use agentflow_core::{
    config::JiraConfig,
    github,
    jira::{self, HttpReply, JiraError, JiraService, JiraTransport},
    media,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

type Call = (String, String, HashMap<String, String>, Option<Value>);
#[derive(Default)]
struct Transport {
    calls: Mutex<Vec<Call>>,
    replies: Mutex<VecDeque<HttpReply>>,
}
impl Transport {
    fn reply(&self, status: u16, body: Value) {
        self.replies.lock().unwrap().push_back(HttpReply {
            status,
            headers: HashMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
        });
    }
    fn binary(&self, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        self.replies.lock().unwrap().push_back(HttpReply {
            status,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.to_vec(),
        });
    }
}
#[async_trait::async_trait]
impl JiraTransport for Transport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: HashMap<String, String>,
        body: Option<Value>,
        limit: u64,
        _: Duration,
    ) -> Result<HttpReply, JiraError> {
        self.calls.lock().unwrap().push((method.into(), url.into(), headers, body));
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected request");
        if reply.body.len() as u64 > limit {
            return Err(JiraError("Attachment download exceeds configured byte limit".into()));
        }
        Ok(reply)
    }
}
fn config() -> JiraConfig {
    JiraConfig {
        enabled: true,
        base_url: Some("https://jira.example.test".into()),
        email: Some("test@example.test".into()),
        api_token: Some("fixture-token".into()),
        max_frames_per_video: 0,
        ..Default::default()
    }
}
fn service(config: JiraConfig) -> (JiraService, Arc<Transport>) {
    let t = Arc::new(Transport::default());
    (JiraService::with_transport(config, t.clone()).unwrap(), t)
}
fn issue(ids: &[&str]) -> Value {
    json!({"fields":{"summary":"Fixture issue","attachment":ids.iter().map(|id|json!({"id":id,"filename":"../../evil.png","size":3,"mimeType":"image/png","content":format!("https://jira.example.test/rest/api/3/attachment/content/{id}")})).collect::<Vec<_>>()},"renderedFields":{"description":"&lt;p&gt;It broke &amp;amp; needs a fix&lt;/p&gt;"}})
}
fn sync_replies(t: &Transport, ids: &[&str]) {
    t.reply(200, issue(ids));
    t.reply(200, json!({"comments":[],"total":0}));
}
#[test]
fn jira_configuration_and_unsafe_origins() {
    assert!(jira::build_service(&JiraConfig::default()).unwrap().is_none());
    let (s, _) = service(config());
    for url in [
        "http://jira.example.test/file",
        "https://evil.example.test/file",
        "https://user@jira.example.test/file",
        "file:///test",
    ] {
        assert!(s.validate_url(url, true).is_err(), "{url}");
    }
    assert!(s.validate_url("https://api.media.atlassian.com/file", false).is_ok());
    assert!(s.validate_url("https://api.media.atlassian.com:8443/file", false).is_err());
}
#[tokio::test]
async fn test_search_issues_parses_sections_and_stops_at_max_results() {
    let (s, t) = service(config());
    t.reply(200,json!({"sections":[{"label":"Current Search","issues":[{"key":"DE-1","summaryText":"First","img":"/icon"},{"key":"DE-2","summaryText":"Second"},{"key":"DE-3"}]}]}));
    let results = s.search_issues("de & x", 2).await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["iconUrl"], "https://jira.example.test/icon");
    assert_eq!(results[0]["section"], "Current Search");
    assert!(t.calls.lock().unwrap()[0].1.contains("query=de+%26+x"));
    assert!(t.calls.lock().unwrap()[0].1.contains("currentJQL=order+by+updated+DESC"));
    assert!(t.calls.lock().unwrap()[0].2.values().any(|v| v.starts_with("Basic ")));
    assert!(s.search_issues(" ", 20).await.unwrap().is_empty());
}
#[tokio::test]
async fn test_search_issues_lists_a_ticket_in_both_sections_once() {
    let (s, t) = service(config());
    t.reply(200,json!({"sections":[{"label":"History Search","issues":[{"key":"DE-1","summaryText":"First"}]},{"label":"Current Search","issues":[{"key":"DE-1","summaryText":"First"},{"key":"DE-2","summaryText":"Second"}]}]}));
    let rows = s.search_issues("de", 20).await.unwrap();
    let keys: Vec<_> = rows.iter().map(|r| (r["key"].as_str().unwrap(), r["section"].as_str().unwrap())).collect();
    assert_eq!(keys, [("DE-1", "History Search"), ("DE-2", "Current Search")]);
}
#[tokio::test]
async fn test_search_issues_raises_on_bad_credentials() {
    let (s, t) = service(config());
    t.reply(401, json!({"secret":"fixture-token"}));
    let error = s.search_issues("x", 20).await.unwrap_err().to_string();
    assert!(!error.contains("fixture-token"));
    assert!(error.contains("credentials"));
}
#[tokio::test]
async fn attachment_redirect_strips_credentials() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    t.binary(303, &[("location", "https://api.media.atlassian.com/file?a=fixture")], b"");
    t.binary(200, &[], b"abc");
    assert_eq!(s.download("https://jira.example.test/file", &dir.path().join("download"), 3).await.unwrap(), 3);
    let calls = t.calls.lock().unwrap();
    assert!(calls[0].2.keys().any(|k| k.eq_ignore_ascii_case("authorization")));
    assert!(!calls[1].2.keys().any(|k| k.eq_ignore_ascii_case("authorization")));
}
#[tokio::test]
async fn attachment_rejects_redirect_and_encoded_response() {
    for headers in [vec![("location", "http://127.0.0.1/private")], vec![("content-encoding", "gzip")]] {
        let (s, t) = service(config());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("download");
        t.binary(if headers[0].0 == "location" { 302 } else { 200 }, &headers, b"abc");
        assert!(s.download("https://jira.example.test/file", &dest, 10).await.is_err());
        assert!(!dest.exists());
        assert_eq!(t.calls.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn jira_sync_incremental_removed_and_truncated() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    sync_replies(&t, &["9"]);
    t.binary(200, &[], b"abc");
    let first = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(first.added, ["9-evil.png"]);
    assert!(std::fs::read_to_string(dir.path().join("jira-issue.md")).unwrap().contains("It broke & needs a fix"));
    sync_replies(&t, &["9"]);
    let kept = s.sync_ticket("DE-1", dir.path(), false).await.unwrap();
    assert_eq!(kept.kept, ["9-evil.png"]);
    std::fs::write(dir.path().join("attachments/9-evil.png"), b"x").unwrap();
    std::fs::write(dir.path().join("attachments/stale.part"), b"x").unwrap();
    sync_replies(&t, &["9"]);
    t.binary(200, &[], b"abc");
    assert_eq!(s.sync_ticket("DE-1", dir.path(), false).await.unwrap().added, ["9-evil.png"]);
    assert!(!dir.path().join("attachments/stale.part").exists());
    sync_replies(&t, &[]);
    assert_eq!(s.sync_ticket("DE-1", dir.path(), false).await.unwrap().removed, ["9-evil.png"]);
    assert!(!dir.path().join("attachments/9-evil.png").exists());
}
#[tokio::test]
async fn jira_attachment_limits_keep_processing_and_comments_failure() {
    let mut cfg = config();
    cfg.max_attachments = 1;
    let (s, t) = service(cfg);
    let dir = tempfile::tempdir().unwrap();
    t.reply(200, issue(&["1", "2"]));
    t.reply(403, json!({}));
    t.binary(200, &[], b"abc");
    let result = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(result.added.len(), 1);
    assert_eq!(result.skipped.len(), 1);
    assert!(result.comments_error.is_some());
    let mut cfg = config();
    cfg.max_attachment_bytes = 2;
    let (s, t) = service(cfg);
    let dir = tempfile::tempdir().unwrap();
    sync_replies(&t, &["1"]);
    assert_eq!(s.sync_ticket("DE-1", dir.path(), true).await.unwrap().skipped.len(), 1);
}
#[tokio::test]
async fn test_transition_to_matches_destination_status_case_insensitively_and_posts() {
    let (s, t) = service(config());
    t.reply(200, json!({"fields":{"status":{"name":"In Review"}}}));
    assert!(!s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.unwrap());
    assert_eq!(t.calls.lock().unwrap().len(), 1);
    t.reply(200, json!({"fields":{"status":{"name":"to do"}}}));
    t.reply(200, json!({"transitions":[{"id":"31","to":{"name":"IN PROGRESS"}}]}));
    t.binary(204, &[], b"");
    assert!(s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.unwrap());
    assert_eq!(t.calls.lock().unwrap().last().unwrap().3, Some(json!({"transition":{"id":"31"}})));
}
#[tokio::test]
async fn jira_myself_pat_uses_v2_and_bearer() {
    let mut cfg = config();
    cfg.email = None;
    let (s, t) = service(cfg);
    t.reply(200, json!({"displayName":"Fixture User"}));
    assert_eq!(s.fetch_myself().await.unwrap(), "Fixture User");
    let calls = t.calls.lock().unwrap();
    assert!(calls[0].1.ends_with("/rest/api/2/myself"));
    assert!(calls[0].2.values().any(|v| v == "Bearer fixture-token"));
}
#[test]
fn test_sync_marks_embedded_images_inline_in_description_and_comments() {
    assert_eq!(jira::sanitize_filename("../../evil.png"), "evil.png");
    let entries = BTreeMap::from([(
        "9".into(),
        json!({"status":"present","file":"9-shot.png","filename":"shot.png","mimeType":"image/png"}),
    )]);
    let text =
        jira::html_to_text("<p>Look <img src=\"/rest/api/3/attachment/content/9\"></p>", &entries, &BTreeMap::new());
    assert!(text.contains("[image: attachments/9-shot.png]"));
    assert_eq!(jira::referenced_ids("<img data-attachment-id=\"9\">", &BTreeMap::new()), ["9"]);
}
#[test]
fn test_is_video_by_mime_or_extension() {
    assert!(media::is_video("x.MP4", None));
    assert!(media::is_video("x.bin", Some("video/mp4")));
    assert!(media::is_video("x.gif", None));
    assert!(!media::is_video("x.png", None));
    let dir = tempfile::tempdir().unwrap();
    for name in ["frame-02.png", "frame-01.png", "raw-01.png"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    assert_eq!(media::frames_done(dir.path()), ["frame-01.png", "frame-02.png"]);
}
#[tokio::test]
async fn test_missing_ffmpeg_is_a_retryable_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("frames");
    let error = media::extract_frames(
        &dir.path().join("video.mp4"),
        &dest,
        4,
        Duration::from_secs(1),
        Some("nonexistent-agentflow-fixture-ffmpeg"),
    )
    .await
    .unwrap_err();
    assert!(error.tool_missing);
    assert!(!dest.exists());
}
#[test]
fn github_remote_hosts_and_pr_urls() {
    let hosts = vec!["github.com".into()];
    for remote in
        ["https://github.com/org/repo.git", "git@github.com:org/repo.git", "ssh://git@github.com/org/repo.git"]
    {
        assert_eq!(github::repo_from_remote(remote, &hosts), Some("github.com/org/repo".into()));
    }
    assert!(github::repo_from_remote("https://enterprise.test/org/repo.git", &hosts).is_none());
    assert!(github::repo_from_remote("https://github.com/org/repo/extra", &hosts).is_none());
    assert!(github::is_pr_url("https://github.com/org/repo/pull/3", &hosts));
    assert!(!github::is_pr_url("http://github.com/org/repo/pull/3", &hosts));
}

#[tokio::test]
async fn test_first_sync_of_a_pre_manifest_task_reports_what_changed() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("attachments")).unwrap();
    std::fs::write(dir.path().join("attachments/9-evil.png"), b"abc").unwrap();
    sync_replies(&t, &["9"]);
    t.reply(200, issue(&["9"]));
    t.reply(200, json!({"comments":[],"total":0}));
    s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    jira::mark_prompted(dir.path(), 1);
    s.sync_ticket("DE-1", dir.path(), false).await.unwrap();
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("jira-manifest.json")).unwrap()).unwrap();
    assert_eq!(m["syncGeneration"], 2);
    assert_eq!(m["lastPromptedGeneration"], 1);
    assert_eq!(m["attachments"][0]["firstSeenGeneration"], 0);
}
#[tokio::test]
async fn jira_comments_paginate_and_transition_no_match_or_auth_error() {
    let (s, t) = service(config());
    t.reply(200, json!({"comments":[{"id":"1"}],"total":2}));
    t.reply(200, json!({"comments":[{"id":"2"}],"total":2}));
    assert_eq!(s.fetch_comments("DE-1").await.unwrap().len(), 2);
    assert!(t.calls.lock().unwrap()[1].1.contains("startAt=1"));
    t.reply(200, json!({"fields":{"status":{"name":"To Do"}}}));
    t.reply(200, json!({"transitions":[]}));
    assert!(!s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.unwrap());
    t.reply(401, json!({}));
    assert!(s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.is_err());
}

#[test]
fn test_build_service_is_none_when_not_fully_configured() {
    for c in [
        JiraConfig::default(),
        JiraConfig { enabled: true, ..Default::default() },
        JiraConfig { enabled: true, base_url: Some("https://jira.example.test".into()), ..Default::default() },
    ] {
        assert!(jira::build_service(&c).unwrap().is_none());
    }
}
#[test]
fn test_build_service_returns_a_client_when_fully_configured() {
    assert!(jira::build_service(&config()).unwrap().is_some());
}
#[tokio::test]
async fn test_search_issues_carries_section_label_and_resolves_relative_icon() {
    let (s, t) = service(config());
    t.reply(200,json!({"sections":[{"label":"Current Search","issues":[{"key":"DE-1","summaryText":"First","img":"/secure/icon"}]}]}));
    let rows = s.search_issues("de", 20).await.unwrap();
    assert_eq!(rows[0]["section"], "Current Search");
    assert_eq!(rows[0]["iconUrl"], "https://jira.example.test/secure/icon");
}
#[tokio::test]
async fn test_sync_ticket_sanitizes_filenames_and_writes_issue_markdown() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    sync_replies(&t, &["9"]);
    t.binary(200, &[], b"abc");
    let r = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(r.added, ["9-evil.png"]);
    let doc = std::fs::read_to_string(dir.path().join("jira-issue.md")).unwrap();
    assert!(doc.contains("# DE-1: Fixture issue"));
    assert!(doc.contains("It broke & needs a fix"));
}
#[tokio::test]
async fn test_sync_skips_an_oversize_attachment_and_keeps_going() {
    let mut c = config();
    c.max_attachment_bytes = 3;
    let (s, t) = service(c);
    let dir = tempfile::tempdir().unwrap();
    let mut data = issue(&["1", "2"]);
    data["fields"]["attachment"][0]["size"] = json!(4);
    t.reply(200, data);
    t.reply(200, json!({"comments":[],"total":0}));
    t.binary(200, &[], b"abc");
    let r = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.added, ["2-evil.png"]);
}
#[tokio::test]
async fn test_sync_skips_attachments_over_the_count_limit_instead_of_failing() {
    let mut c = config();
    c.max_attachments = 1;
    let (s, t) = service(c);
    let dir = tempfile::tempdir().unwrap();
    sync_replies(&t, &["1", "2"]);
    t.binary(200, &[], b"abc");
    let r = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(r.added.len(), 1);
    assert_eq!(r.skipped[0].id, "2");
}
#[tokio::test]
async fn test_sync_is_incremental_and_moves_removed_attachments_aside() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    sync_replies(&t, &["1"]);
    t.binary(200, &[], b"abc");
    s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    sync_replies(&t, &[]);
    let r = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(r.removed, ["1-evil.png"]);
    assert!(dir.path().join("attachments-removed/1-evil.png").exists());
}
#[tokio::test]
async fn test_sync_redownloads_a_truncated_file_and_clears_leftover_parts() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("attachments")).unwrap();
    std::fs::write(dir.path().join("attachments/1-evil.png"), b"x").unwrap();
    std::fs::write(dir.path().join("attachments/old.part"), b"x").unwrap();
    sync_replies(&t, &["1"]);
    t.binary(200, &[], b"abc");
    s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("attachments/1-evil.png")).unwrap(), b"abc");
    assert!(!dir.path().join("attachments/old.part").exists());
}
#[tokio::test]
async fn test_sync_still_writes_the_issue_when_comments_cannot_be_fetched() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    t.reply(200, issue(&[]));
    t.reply(403, json!({}));
    let r = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert!(r.comments_error.is_some());
    let doc = std::fs::read_to_string(dir.path().join("jira-issue.md")).unwrap();
    assert!(doc.contains("## Description"));
    assert!(doc.contains("read them through your Jira MCP"));
}
#[tokio::test]
async fn test_mark_prompted_records_what_the_architect_was_shown() {
    let (s, t) = service(config());
    let dir = tempfile::tempdir().unwrap();
    sync_replies(&t, &[]);
    s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    jira::mark_prompted(dir.path(), 1);
    sync_replies(&t, &[]);
    s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("jira-manifest.json")).unwrap()).unwrap();
    assert_eq!(m["syncGeneration"], 2);
    assert_eq!(m["lastPromptedGeneration"], 1);
}
#[tokio::test]
async fn test_transition_to_skips_when_current_status_not_allowed() {
    let (s, t) = service(config());
    t.reply(200, json!({"fields":{"status":{"name":"In Review"}}}));
    assert!(!s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.unwrap());
    assert_eq!(t.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn test_transition_to_returns_false_when_no_matching_transition_available() {
    let (s, t) = service(config());
    t.reply(200, json!({"fields":{"status":{"name":"To Do"}}}));
    t.reply(200, json!({"transitions":[{"id":"1","to":{"name":"Done"}}]}));
    assert!(!s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.unwrap());
    assert_eq!(t.calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn test_transition_to_raises_on_bad_status_fetch() {
    let (s, t) = service(config());
    t.reply(401, json!({}));
    assert!(s.transition_to("DE-1", "In Progress", &["To Do".into()]).await.is_err());
}

mod engine_support;
struct CheckingTransport {
    inner: Arc<Transport>,
    engine: std::sync::Weak<agentflow_core::engine::Engine>,
}
#[async_trait::async_trait]
impl JiraTransport for CheckingTransport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: HashMap<String, String>,
        body: Option<Value>,
        limit: u64,
        timeout: Duration,
    ) -> Result<HttpReply, JiraError> {
        assert!(self.engine.upgrade().unwrap().launch_lock.try_lock().is_ok());
        self.inner.request(method, url, headers, body, limit, timeout).await
    }
}
#[tokio::test]
async fn test_run_node_auto_transitions_jira_and_still_starts_when_it_fails() {
    let h = engine_support::engine().await;
    h.launchable();
    h.configure(|c| {
        c.jira = config();
        c.jira.auto_transition_status = Some("In Progress".into());
    });
    let t = Arc::new(Transport::default());
    t.binary(200, &[], b"unreadable JSON");
    t.reply(401, json!({}));
    h.engine.set_jira_transport(Arc::new(CheckingTransport { inner: t, engine: Arc::downgrade(&h.engine) }));
    let snap = h.engine.run_node("TEST-1", "architect", Default::default(), false).await.unwrap();
    assert_eq!(h.load().node("architect").unwrap().state, "RUNNING");
    let events = snap["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["message"].as_str().unwrap().contains("Could not update Jira status")));
    assert!(events.iter().any(|e| e["message"].as_str().unwrap().contains("Could not sync Jira ticket context")));
}
#[tokio::test]
async fn test_architect_launch_records_the_jira_generation_it_was_shown() {
    let h = engine_support::engine().await;
    h.launchable();
    h.configure(|c| c.jira = config());
    let context = std::path::Path::new(&h.load().task.task_dir).join("context");
    std::fs::create_dir_all(&context).unwrap();
    std::fs::write(context.join("jira-manifest.json"), json!({"syncGeneration":5,"attachments":[]}).to_string())
        .unwrap();
    let t = Arc::new(Transport::default());
    sync_replies(&t, &[]);
    h.engine.set_jira_transport(t);
    h.engine.run_node("TEST-1", "architect", Default::default(), false).await.unwrap();
    let m: Value = serde_json::from_str(&std::fs::read_to_string(context.join("jira-manifest.json")).unwrap()).unwrap();
    assert_eq!(m["syncGeneration"], 6);
    assert_eq!(m["lastPromptedGeneration"], 6);
}

#[cfg(windows)]
fn frame_tools(dir: &std::path::Path, dest: &std::path::Path, fail: bool, slow: bool) -> String {
    std::fs::write(dir.join("ffprobe.cmd"), "@echo off\r\necho N/A\r\n").unwrap();
    let exe = dir.join("ffmpeg.cmd");
    std::fs::write(
        &exe,
        format!(
            "@echo off\r\n{}echo png > \"{}\"\r\necho [showinfo] pts_time:0.0 1>&2\r\nexit /b {}\r\n",
            if slow { "ping 127.0.0.1 -n 2 > nul\r\n" } else { "" },
            dest.display(),
            i32::from(fail)
        ),
    )
    .unwrap();
    exe.to_string_lossy().into()
}
fn videos(ids: &[&str]) -> Value {
    let mut data = issue(ids);
    for a in data["fields"]["attachment"].as_array_mut().unwrap() {
        a["filename"] = json!("repro.mp4");
        a["mimeType"] = json!("video/mp4");
    }
    data
}
#[cfg(windows)]
#[tokio::test]
async fn test_sync_records_a_frame_failure_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let exe = frame_tools(dir.path(), &dir.path().join("frames/7.tmp/raw-001.png"), true, false);
    let mut c = config();
    c.ffmpeg_path = Some(exe);
    c.max_frames_per_video = 1;
    let (s, t) = service(c);
    t.reply(200, videos(&["7"]));
    t.reply(200, json!({"comments":[],"total":0}));
    t.binary(200, &[], b"abc");
    let r = s.sync_ticket("DE-1", dir.path(), false).await.unwrap();
    assert_eq!(r.added, ["7-repro.mp4"]);
    assert!(r.frame_errors.contains_key("7"));
    t.reply(200, videos(&["7"]));
    t.reply(200, json!({"comments":[],"total":0}));
    assert!(s.sync_ticket("DE-1", dir.path(), false).await.unwrap().frame_errors.is_empty());
}
#[cfg(windows)]
#[tokio::test]
async fn test_keyframes_share_one_time_budget_per_sync() {
    let dir = tempfile::tempdir().unwrap();
    let exe = frame_tools(dir.path(), &dir.path().join("frames/7.tmp/raw-001.png"), false, true);
    let mut c = config();
    c.ffmpeg_path = Some(exe);
    c.max_frames_per_video = 1;
    c.frame_extraction_timeout_seconds = agentflow_core::config::Float::new(1.8);
    let (s, t) = service(c);
    t.reply(200, videos(&["7", "8"]));
    t.reply(200, json!({"comments":[],"total":0}));
    t.binary(200, &[], b"abc");
    t.binary(200, &[], b"abc");
    let r = s.sync_ticket("DE-1", dir.path(), false).await.unwrap();
    assert_eq!(r.frames.get("7"), Some(&1));
    assert!(r.frame_errors["8"].contains("deferred to the next sync"));
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("jira-manifest.json")).unwrap()).unwrap();
    assert_ne!(m["attachments"][1]["framesChecked"], true);
}
#[tokio::test]
async fn test_sync_extracts_video_keyframes_only_on_architect_runs() {
    let mut cfg = config();
    cfg.max_frames_per_video = 4;
    cfg.ffmpeg_path = Some("nonexistent-agentflow-fixture-ffmpeg".into());
    let (s, t) = service(cfg);
    let dir = tempfile::tempdir().unwrap();
    let mut video = issue(&["7"]);
    video["fields"]["attachment"][0]["filename"] = json!("repro.mp4");
    video["fields"]["attachment"][0]["mimeType"] = json!("video/mp4");
    video["renderedFields"]["description"] =
        json!("<a href=\"https://jira.example.test/rest/api/3/attachment/content/7\">repro.mp4</a>");
    t.reply(200, video.clone());
    t.reply(200, json!({"comments":[],"total":0}));
    t.binary(200, &[], b"abc");
    let r = s.sync_ticket("DE-1", dir.path(), true).await.unwrap();
    assert!(r.frame_errors.is_empty());
    for _ in 0..2 {
        t.reply(200, video.clone());
        t.reply(200, json!({"comments":[],"total":0}));
        let r = s.sync_ticket("DE-1", dir.path(), false).await.unwrap();
        assert!(r.frame_errors.contains_key("7"));
    }
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("jira-manifest.json")).unwrap()).unwrap();
    assert_eq!(m["attachments"][0]["framesChecked"], false);
    #[cfg(windows)]
    {
        let exe = frame_tools(dir.path(), &dir.path().join("frames/7.tmp/raw-001.png"), false, false);
        let mut cfg = config();
        cfg.ffmpeg_path = Some(exe);
        cfg.max_frames_per_video = 1;
        let (s, t) = service(cfg);
        t.reply(200, video.clone());
        t.reply(200, json!({"comments":[],"total":0}));
        let r = s.sync_ticket("DE-1", dir.path(), false).await.unwrap();
        assert_eq!(r.frames.get("7"), Some(&1));
        assert!(std::fs::read_to_string(dir.path().join("jira-issue.md")).unwrap().contains("keyframes in frames/7/"));
        t.reply(200, video);
        t.reply(200, json!({"comments":[],"total":0}));
        assert!(s.sync_ticket("DE-1", dir.path(), false).await.unwrap().frames.is_empty());
    }
}
