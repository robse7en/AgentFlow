use agentflow_core::{
    config::GitHubConfig,
    github::{GitHubCli, GitHubSource},
};
use serde_json::json;
#[tokio::test]
async fn test_cli_reports_missing_or_logged_out_gh_as_unavailable() {
    let settings = GitHubConfig { command: "nonexistent-agentflow-fixture-gh".into(), ..Default::default() };
    assert!(GitHubCli::call(&settings, &["pr", "list"]).await.unwrap_err().unavailable);
}
#[cfg(windows)]
#[tokio::test]
async fn test_cli_lists_same_repo_prs_only_and_disables_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh.cmd");
    let log = dir.path().join("argv.txt");
    let row = json!({"url":"https://github.com/acme/repo/pull/7","state":"OPEN","createdAt":"2026-10-04T00:00:00Z","mergedAt":null,"isCrossRepository":false,"headRefOid":"abcdef"});
    let mut fork = row.clone();
    fork["isCrossRepository"] = json!(true);
    std::fs::write(&path,format!("@echo off\r\necho %* > \"{}\"\r\nif not \"%GH_PROMPT_DISABLED%\"==\"1\" exit /b 9\r\nif not \"%GH_NO_UPDATE_NOTIFIER%\"==\"1\" exit /b 9\r\necho {}\r\n",log.display(),json!([row,fork]))).unwrap();
    let settings = GitHubConfig { command: path.to_string_lossy().into(), ..Default::default() };
    let prs = GitHubCli.pull_requests_for_branch(&settings, "github.com/acme/repo", "agent/TEST-1").await.unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].head_sha.as_deref(), Some("abcdef"));
    let args = std::fs::read_to_string(log).unwrap();
    assert!(args.contains("--state all"));
    assert!(args.contains("--limit 20"));
    assert!(args.contains("--head agent/TEST-1"));
}
#[cfg(windows)]
#[tokio::test]
async fn test_cli_reports_other_failures_per_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh.cmd");
    let settings = GitHubConfig { command: path.to_string_lossy().into(), ..Default::default() };
    for code in [4, 1] {
        std::fs::write(&path, format!("@echo off\r\necho fixture-secret 1>&2\r\nexit /b {code}\r\n")).unwrap();
        let error = GitHubCli::call(&settings, &["pr", "list"]).await.unwrap_err();
        assert_eq!(error.unavailable, code == 4);
        assert!(!error.message.contains("fixture-secret"));
    }
}

#[cfg(windows)]
#[tokio::test]
async fn test_cli_reads_the_pr_head_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh.cmd");
    std::fs::write(&path,format!("@echo off\r\necho {}\r\n",json!({"url":"https://github.com/acme/repo/pull/7","state":"MERGED","createdAt":"2026-10-04T00:00:00Z","mergedAt":"2026-10-04T01:00:00Z","headRefOid":"abcdef"}))).unwrap();
    let settings = GitHubConfig { command: path.to_string_lossy().into(), ..Default::default() };
    assert_eq!(
        GitHubCli.pull_request(&settings, "https://github.com/acme/repo/pull/7").await.unwrap().head_sha.as_deref(),
        Some("abcdef")
    );
}
