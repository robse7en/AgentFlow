//! Jira REST calls and bounded, origin-checked attachment downloads.
use crate::config::JiraConfig;
use base64::Engine as _;
use serde_json::{Value, json};
use std::{collections::HashMap, io::Read, path::Path, sync::Arc, time::Duration};
use url::Url;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct JiraError(pub String);
impl From<std::io::Error> for JiraError {
    fn from(_: std::io::Error) -> Self {
        Self("Jira context file operation failed".into())
    }
}
pub struct HttpReply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}
#[async_trait::async_trait]
pub trait JiraTransport: Send + Sync {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: HashMap<String, String>,
        body: Option<Value>,
        limit: u64,
        timeout: Duration,
    ) -> Result<HttpReply, JiraError>;
}
pub struct UreqTransport;
#[async_trait::async_trait]
impl JiraTransport for UreqTransport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: HashMap<String, String>,
        body: Option<Value>,
        limit: u64,
        timeout: Duration,
    ) -> Result<HttpReply, JiraError> {
        let (method, url) = (method.to_string(), url.to_string());
        tokio::task::spawn_blocking(move || {
            let mut builder = ureq::AgentBuilder::new().redirects(0).timeout(timeout);
            // Match the standard HTTPS CA override used by Python/httpx (enterprise Jira and
            // localhost TLS fixtures). Certificate verification is never disabled.
            if let Ok(path) = std::env::var("SSL_CERT_FILE") {
                let pem = std::fs::read_to_string(path).map_err(|_| JiraError("Cannot read SSL_CERT_FILE".into()))?;
                let mut roots = rustls::RootCertStore::empty();
                for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
                    let der = block
                        .split("-----END CERTIFICATE-----")
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .collect::<String>();
                    let der = base64::engine::general_purpose::STANDARD
                        .decode(der)
                        .map_err(|_| JiraError("Invalid SSL_CERT_FILE".into()))?;
                    roots
                        .add(rustls::pki_types::CertificateDer::from(der))
                        .map_err(|_| JiraError("Invalid SSL_CERT_FILE".into()))?;
                }
                if roots.is_empty() {
                    return Err(JiraError("SSL_CERT_FILE contains no certificates".into()));
                }
                builder = builder.tls_config(Arc::new(
                    rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth(),
                ));
            }
            let agent = builder.build();
            let mut request = agent.request(&method, &url);
            for (name, value) in headers {
                request = request.set(&name, &value);
            }
            let response = match body {
                Some(body) => request.send_json(body),
                None => request.call(),
            };
            let response = match response {
                Ok(r) | Err(ureq::Error::Status(_, r)) => r,
                Err(_) => return Err(JiraError("Cannot reach Jira".into())),
            };
            let status = response.status();
            let headers = response
                .headers_names()
                .into_iter()
                .filter_map(|k| response.header(&k).map(|v| (k.to_lowercase(), v.to_string())))
                .collect();
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|_| JiraError("Cannot read Jira response".into()))?;
            if bytes.len() as u64 > limit {
                return Err(JiraError("Attachment download exceeds configured byte limit".into()));
            }
            Ok(HttpReply { status, headers, body: bytes })
        })
        .await
        .map_err(|_| JiraError("Jira request failed".into()))?
    }
}
#[derive(Clone)]
pub struct JiraService {
    pub(crate) config: JiraConfig,
    pub(crate) base: Url,
    pub(crate) transport: Arc<dyn JiraTransport>,
}
pub fn build_service(config: &JiraConfig) -> Result<Option<JiraService>, JiraError> {
    if !config.missing().is_empty() {
        return Ok(None);
    }
    Ok(Some(JiraService::with_transport(config.clone(), Arc::new(UreqTransport))?))
}
impl JiraService {
    pub fn with_transport(config: JiraConfig, transport: Arc<dyn JiraTransport>) -> Result<Self, JiraError> {
        let base = Url::parse(config.base_url.as_deref().unwrap_or("").trim_end_matches('/'))
            .map_err(|_| JiraError("Invalid attachment destination".into()))?;
        let service = Self { config, base, transport };
        service.validate_url(service.base.as_str(), true)?;
        Ok(service)
    }
    pub fn validate_url(&self, value: &str, initial: bool) -> Result<Url, JiraError> {
        let url = Url::parse(value).map_err(|_| JiraError("Invalid attachment destination".into()))?;
        let allowed = url.origin() == self.base.origin()
            || (!initial
                && url.port_or_known_default() == Some(443)
                && url.host_str().is_some_and(|h| self.config.attachment_hosts.iter().any(|s| s == h)));
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || !allowed
        {
            return Err(JiraError("Untrusted Jira attachment destination".into()));
        }
        Ok(url)
    }
    fn headers(&self, same_origin: bool) -> HashMap<String, String> {
        let mut headers = HashMap::from([
            ("Accept".into(), "application/json".into()),
            ("Accept-Encoding".into(), "identity".into()),
        ]);
        if same_origin {
            let token = self.config.api_token.as_deref().unwrap_or("");
            let auth = if let Some(email) = self.config.email.as_deref().filter(|s| !s.is_empty()) {
                format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{email}:{token}")))
            } else {
                format!("Bearer {token}")
            };
            headers.insert("Authorization".into(), auth);
        }
        headers
    }
    pub(crate) fn endpoint(&self, path: &str, query: &[(&str, String)]) -> Result<Url, JiraError> {
        let mut url = Url::parse(&format!("{}{path}", self.base.as_str().trim_end_matches('/')))
            .map_err(|_| JiraError("Invalid Jira endpoint".into()))?;
        for (key, value) in query {
            url.query_pairs_mut().append_pair(key, value);
        }
        Ok(url)
    }
    pub(crate) async fn api(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value, JiraError> {
        let url = self.endpoint(path, query)?;
        let reply = self
            .transport
            .request(method, url.as_str(), self.headers(true), body, 10_000_000, Duration::from_secs(15))
            .await?;
        match reply.status {
            401 | 403 => return Err(JiraError("Jira rejected the configured credentials".into())),
            404 => {
                return Err(JiraError(format!("Jira issue {} not found", path.split('/').nth(5).unwrap_or("unknown"))));
            }
            status if status >= 300 => return Err(JiraError(format!("Jira request failed ({status})"))),
            _ => {}
        }
        if reply.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&reply.body).map_err(|_| JiraError("Jira returned unreadable output".into()))
    }
    pub async fn search_issues(&self, query: &str, max: usize) -> Result<Vec<Value>, JiraError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // Without currentJQL the picker only searches the account's view history (its
        // "History Search" section), so tickets never opened in Jira would not be found.
        let data = self
            .api(
                "GET",
                "/rest/api/3/issue/picker",
                &[("query", query.trim().into()), ("currentJQL", "order by updated DESC".into())],
                None,
            )
            .await?;
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for section in data["sections"].as_array().into_iter().flatten() {
            for issue in section["issues"].as_array().into_iter().flatten() {
                // A viewed ticket is listed in both sections; keep its first (History) entry.
                if !issue["key"].as_str().is_some_and(|s| !s.is_empty() && seen.insert(s.to_string())) {
                    continue;
                }
                let icon = issue["img"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .and_then(|s| self.base.join(s).ok())
                    .map(|u| u.to_string());
                results.push(json!({"key":issue["key"],"summary":issue["summaryText"].as_str().filter(|s|!s.is_empty()).or(issue["summary"].as_str()).unwrap_or(""),"iconUrl":icon,"section":section["label"].as_str().filter(|s|!s.is_empty())}));
                if results.len() >= max {
                    return Ok(results);
                }
            }
        }
        Ok(results)
    }
    pub async fn fetch_myself(&self) -> Result<String, JiraError> {
        let version = if self.config.email.as_deref().is_some_and(|s| !s.is_empty()) { 3 } else { 2 };
        let data = self.api("GET", &format!("/rest/api/{version}/myself"), &[], None).await?;
        if !data.is_object() {
            return Err(JiraError("Jira returned an unreadable account".into()));
        }
        Ok(["displayName", "emailAddress", "name"]
            .iter()
            .find_map(|k| data[k].as_str().filter(|s| !s.is_empty()))
            .unwrap_or("unknown account")
            .into())
    }
    pub async fn fetch_issue(&self, key: &str) -> Result<Value, JiraError> {
        self.api(
            "GET",
            &format!("/rest/api/3/issue/{key}"),
            &[("fields", "summary,description,attachment,updated".into()), ("expand", "renderedFields".into())],
            None,
        )
        .await
    }
    pub async fn fetch_comments(&self, key: &str) -> Result<Vec<Value>, JiraError> {
        let mut comments = Vec::new();
        for _ in 0..50 {
            let data = self
                .api(
                    "GET",
                    &format!("/rest/api/3/issue/{key}/comment"),
                    &[
                        ("expand", "renderedBody".into()),
                        ("startAt", comments.len().to_string()),
                        ("maxResults", "100".into()),
                    ],
                    None,
                )
                .await?;
            let batch = data["comments"].as_array().cloned().unwrap_or_default();
            let empty = batch.is_empty();
            comments.extend(batch);
            if empty || comments.len() as u64 >= data["total"].as_u64().unwrap_or(0) {
                break;
            }
        }
        Ok(comments)
    }
    pub async fn transition_to(&self, key: &str, target: &str, from: &[String]) -> Result<bool, JiraError> {
        let data = self.api("GET", &format!("/rest/api/3/issue/{key}"), &[("fields", "status".into())], None).await?;
        if !from
            .iter()
            .any(|s| data["fields"]["status"]["name"].as_str().is_some_and(|name| s.eq_ignore_ascii_case(name)))
        {
            return Ok(false);
        }
        let path = format!("/rest/api/3/issue/{key}/transitions");
        let data = self.api("GET", &path, &[], None).await?;
        let Some(found) = data["transitions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["to"]["name"].as_str().is_some_and(|s| s.eq_ignore_ascii_case(target)))
        else {
            return Ok(false);
        };
        self.api("POST", &path, &[], Some(json!({"transition":{"id":found["id"]}}))).await?;
        Ok(true)
    }
    pub async fn download(&self, url: &str, dest: &Path, limit: u64) -> Result<u64, JiraError> {
        let mut current = self.validate_url(url, true)?;
        for _ in 0..6 {
            self.validate_url(current.as_str(), false)?;
            let reply = self
                .transport
                .request(
                    "GET",
                    current.as_str(),
                    self.headers(current.origin() == self.base.origin()),
                    None,
                    limit,
                    Duration::from_secs(60),
                )
                .await?;
            if (300..400).contains(&reply.status) {
                let location = reply
                    .headers
                    .get("location")
                    .ok_or_else(|| JiraError("Attachment redirect has no destination".into()))?;
                current = current.join(location).map_err(|_| JiraError("Invalid attachment destination".into()))?;
                continue;
            }
            if reply.status >= 400 {
                return Err(JiraError(format!("Attachment download failed ({})", reply.status)));
            }
            if reply.headers.get("content-encoding").is_some_and(|s| !s.eq_ignore_ascii_case("identity")) {
                return Err(JiraError("Encoded attachment responses are not accepted".into()));
            }
            if reply.body.len() as u64 > limit {
                return Err(JiraError("Attachment download exceeds configured byte limit".into()));
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)
                .map_err(|_| JiraError("Attachment destination already exists".into()))?;
            if std::io::Write::write_all(&mut output, &reply.body).is_err() {
                drop(output);
                let _ = std::fs::remove_file(dest);
                return Err(JiraError("Attachment write failed".into()));
            }
            return Ok(reply.body.len() as u64);
        }
        Err(JiraError("Too many attachment redirects".into()))
    }
}
