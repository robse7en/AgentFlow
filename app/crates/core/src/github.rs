//! GitHub PR lookups using the operator's gh login. The one write is retargeting a PR to another
//! base branch, and only when the human asks for it.
use crate::config::GitHubConfig;
use crate::db::DbTime;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

const FIELDS: &str = "url,state,createdAt,mergedAt,isCrossRepository,headRefOid,baseRefName";
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequest {
    pub url: String,
    pub state: String,
    pub created_at: DbTime,
    pub merged_at: Option<DbTime>,
    pub head_sha: Option<String>,
    /// The branch the PR merges into.
    pub base_branch: Option<String>,
}
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct GitHubError {
    pub message: String,
    pub unavailable: bool,
}
impl GitHubError {
    pub fn lookup(message: impl Into<String>) -> Self {
        Self { message: message.into(), unavailable: false }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self { message: message.into(), unavailable: true }
    }
}
pub fn repo_from_remote(url: &str, hosts: &[String]) -> Option<String> {
    let pattern = regex::Regex::new(
        r"^(?:https?|ssh|git)://(?:[^@/]+@)?(?P<host>[^/:]+)(?::\d+)?/(?P<path>[^?#]+?)(?:\.git)?/?$",
    )
    .unwrap();
    let scp = regex::Regex::new(r"^(?:[^@/]+@)?(?P<host>[^/:]+):(?P<path>[^/][^?#]*?)(?:\.git)?/?$").unwrap();
    let found = pattern.captures(url).or_else(|| scp.captures(url))?;
    let host = found.name("host")?.as_str();
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return None;
    }
    let path = found.name("path")?.as_str().trim_matches('/');
    let parts = path.split('/').collect::<Vec<_>>();
    (parts.len() == 2 && parts.iter().all(|p| !p.is_empty())).then(|| format!("{host}/{path}"))
}
/// GitHub's new-PR form for `repo` (`host/owner/repo`, as [`repo_from_remote`] gives it) with the
/// base branch already chosen, so the PR cannot default to the repository's default branch.
pub fn compare_url(repo: &str, base: &str, branch: &str) -> String {
    format!("https://{repo}/compare/{}...{}?expand=1", url_path(base), url_path(branch))
}
/// Percent-encodes everything but unreserved characters and `/`, which branch names use.
fn url_path(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
pub fn is_pr_url(url: &str, hosts: &[String]) -> bool {
    let re = regex::Regex::new(r"^https://(?P<host>[^/]+)/[^/]+/[^/]+/pull/\d+/?$").unwrap();
    re.captures(url).is_some_and(|c| hosts.iter().any(|h| h.eq_ignore_ascii_case(&c["host"])))
}
pub fn parse_pr(item: &Value) -> Result<PullRequest, GitHubError> {
    let string = |key: &str| item[key].as_str().ok_or_else(|| GitHubError::lookup("gh returned unreadable output"));
    let time = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| DbTime(d.naive_utc()))
            .map_err(|_| GitHubError::lookup("gh returned unreadable timestamps"))
    };
    Ok(PullRequest {
        url: string("url")?.into(),
        state: string("state")?.into(),
        created_at: time(string("createdAt")?)?,
        merged_at: item["mergedAt"].as_str().filter(|s| !s.is_empty()).map(time).transpose()?,
        head_sha: item["headRefOid"].as_str().filter(|s| !s.is_empty()).map(String::from),
        base_branch: item["baseRefName"].as_str().filter(|s| !s.is_empty()).map(String::from),
    })
}
pub fn pick_pull_request(prs: &[PullRequest], since: &DbTime) -> Option<PullRequest> {
    for state in ["OPEN", "MERGED"] {
        if let Some(pr) =
            prs.iter().filter(|p| p.state == state && p.created_at.0 >= since.0).max_by_key(|p| p.created_at.0)
        {
            return Some(pr.clone());
        }
    }
    None
}
#[async_trait::async_trait]
pub trait GitHubSource: Send + Sync {
    async fn pull_requests_for_branch(
        &self,
        settings: &GitHubConfig,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<PullRequest>, GitHubError>;
    async fn pull_request(&self, settings: &GitHubConfig, url: &str) -> Result<PullRequest, GitHubError>;
    /// Change the branch the PR at `url` merges into.
    async fn retarget(&self, settings: &GitHubConfig, url: &str, base: &str) -> Result<(), GitHubError>;
}
pub struct GitHubCli;
impl GitHubCli {
    pub async fn call(settings: &GitHubConfig, args: &[&str]) -> Result<Value, GitHubError> {
        serde_json::from_str(&Self::run(settings, args).await?)
            .map_err(|_| GitHubError::lookup("gh returned unreadable output"))
    }
    /// gh's stdout, for commands that print text rather than JSON.
    async fn run(settings: &GitHubConfig, args: &[&str]) -> Result<String, GitHubError> {
        let exe = crate::process::resolve_executable(&settings.command).ok_or_else(|| {
            GitHubError::unavailable(format!("GitHub CLI '{}' was not found on PATH", settings.command))
        })?;
        let mut env = HashMap::new();
        for name in
            ["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN", "GH_HOST", "GH_CONFIG_DIR"]
        {
            if let Ok(value) = std::env::var(name)
                && !value.is_empty()
            {
                env.insert(name.into(), value);
            }
        }
        for (name, value) in [("GH_PROMPT_DISABLED", "1"), ("GH_NO_UPDATE_NOTIFIER", "1"), ("NO_COLOR", "1")] {
            env.insert(name.into(), value.into());
        }
        let mut argv = vec![crate::paths::py_str(&exe)];
        argv.extend(args.iter().map(|s| s.to_string()));
        let result = crate::process::run(&argv, None, &env, Duration::from_secs(30), None)
            .await
            .map_err(|e| GitHubError::lookup(e.to_string()))?;
        if result.returncode == 4 {
            return Err(GitHubError::unavailable("GitHub CLI is not logged in; run `gh auth login`"));
        }
        if !result.ok() {
            // Provider diagnostics can contain credentials; do not echo arbitrary stderr to the UI.
            return Err(GitHubError::lookup(format!(
                "gh {} {} failed: exit {}",
                args.first().unwrap_or(&""),
                args.get(1).unwrap_or(&""),
                result.returncode
            )));
        }
        Ok(result.stdout)
    }
}
#[async_trait::async_trait]
impl GitHubSource for GitHubCli {
    async fn pull_requests_for_branch(
        &self,
        settings: &GitHubConfig,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<PullRequest>, GitHubError> {
        let items = Self::call(
            settings,
            &["pr", "list", "--repo", repo, "--head", branch, "--state", "all", "--limit", "20", "--json", FIELDS],
        )
        .await?;
        items
            .as_array()
            .ok_or_else(|| GitHubError::lookup("gh returned unreadable output"))?
            .iter()
            .filter(|i| i["isCrossRepository"] != true)
            .map(parse_pr)
            .collect()
    }
    async fn pull_request(&self, settings: &GitHubConfig, url: &str) -> Result<PullRequest, GitHubError> {
        parse_pr(&Self::call(settings, &["pr", "view", url, "--json", FIELDS]).await?)
    }
    async fn retarget(&self, settings: &GitHubConfig, url: &str, base: &str) -> Result<(), GitHubError> {
        Self::run(settings, &["pr", "edit", url, "--base", base]).await.map(drop)
    }
}
