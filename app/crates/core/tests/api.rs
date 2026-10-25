//! The HTTP API in process (router, no network), with the Python API tests ported by name:
//! backend/tests/test_config_api.py, test_diagnostics.py and test_startup_jira.py. The contract
//! suite (contract/test_auth.py, test_config.py) checks the same routes over HTTP.

mod support;

use std::path::Path;

use agentflow_core::api;
use agentflow_core::config::{Config, read_config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use support::{HOST, ORIGIN, start};
use tower::ServiceExt;

// Ported from backend/tests/test_config_api.py ------------------------------------------------------

#[tokio::test]
async fn test_set_default_repository_persists_to_disk_and_state() {
    let backend = start(json!({})).await;
    let repo = backend.dir.path().join("MyRepo").to_string_lossy().into_owned();
    let (status, result) = backend.json("PUT", "/api/config/default-repository", Some(json!({"path": repo}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["defaultRepositoryDirectory"], json!(repo));
    assert_eq!(backend.app.engine.config().workspace.default_repository_directory.as_deref(), Some(repo.as_str()));
    assert_eq!(backend.on_disk()["workspace"]["defaultRepositoryDirectory"], json!(repo));
}

#[tokio::test]
async fn test_set_default_repository_trims_and_clears() {
    let backend = start(json!({})).await;
    let repo = backend.dir.path().join("MyRepo").to_string_lossy().into_owned();
    backend.json("PUT", "/api/config/default-repository", Some(json!({"path": format!("  {repo}  ")}))).await;
    assert_eq!(backend.app.engine.config().workspace.default_repository_directory.as_deref(), Some(repo.as_str()));
    let (_, result) = backend.json("PUT", "/api/config/default-repository", Some(json!({"path": null}))).await;
    assert_eq!(result["defaultRepositoryDirectory"], Value::Null);
    assert_eq!(backend.app.engine.config().workspace.default_repository_directory, None);
    assert_eq!(backend.on_disk()["workspace"]["defaultRepositoryDirectory"], Value::Null);
}

#[tokio::test]
async fn test_set_default_repository_rejects_invalid_config() {
    let backend = start(json!({})).await;
    std::fs::write(backend.config_file(), json!({"server": {"host": "not-a-loopback-host"}}).to_string()).unwrap();
    let (status, _) = backend.json("PUT", "/api/config/default-repository", Some(json!({"path": "/tmp/x"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_cost_controls_persist_without_changing_role_assignments() {
    let backend = start(json!({})).await;
    let before = backend.app.engine.config().agents.clone();
    let body = json!({"controls": {"dailyBudgetUsd": 20}, "billing": {"copilot": "subscription"}});
    let (status, _) = backend.json("PUT", "/api/config/cost-controls", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    let config = backend.app.engine.config();
    assert_eq!(config.workflow.cost_controls.daily_budget_usd, Some(20.0));
    assert_eq!(serde_json::to_value(config.harnesses["copilot"].billing_mode).unwrap(), "subscription");
    assert_eq!(config.agents, before);
    let loaded = read_config(&backend.config_file()).unwrap();
    assert_eq!(serde_json::to_value(loaded.harnesses["copilot"].billing_mode).unwrap(), "subscription");
}

/// A config.json without a harnesses section relies on the defaults; saving one harness's billing
/// mode must not leave a file whose agents name harnesses it no longer has (fixed in both backends
/// on 2026-10-03; Python wrote only the edited harness).
#[tokio::test]
async fn test_billing_edit_keeps_the_default_harnesses_a_file_relies_on() {
    let backend = start(json!({})).await;
    let body = json!({"controls": {}, "billing": {"copilot": "subscription"}});
    assert_eq!(backend.json("PUT", "/api/config/cost-controls", Some(body)).await.0, StatusCode::OK);
    let saved = backend.on_disk();
    let mut names: Vec<&String> = saved["harnesses"].as_object().unwrap().keys().collect();
    names.sort();
    let defaults = Config::default();
    let mut expected: Vec<&String> = defaults.harnesses.keys().collect();
    expected.sort();
    assert_eq!(names, expected);
    let loaded = read_config(&backend.config_file()).unwrap();
    assert_eq!(serde_json::to_value(loaded.harnesses["copilot"].billing_mode).unwrap(), "subscription");
    assert_eq!(loaded.harnesses["claude"].billing_mode, defaults.harnesses["claude"].billing_mode);
}

// Ported from backend/tests/test_diagnostics.py ------------------------------------------------------
// (test_log_is_structured_rotating_and_omits_exception_secrets is in src/diagnostics.rs, and
// test_internal_error_has_correlated_safe_diagnostic in src/api/mod.rs.)

#[tokio::test]
async fn test_diagnostics_require_pairing_and_reject_agent_credentials() {
    let mut backend = start(json!({})).await;
    agentflow_core::diagnostics::open_log(&backend.app.diagnostic_path).unwrap();
    agentflow_core::diagnostics::event("application.starting", json!({}));
    let response = backend.call("GET", "/api/diagnostics", None, false).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
    let length: usize = response.headers()["content-length"].to_str().unwrap().parse().unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(length, body.len());
    assert!(String::from_utf8_lossy(&body).contains("application.starting"));
    let agent = Request::builder()
        .uri("/api/diagnostics")
        .header("host", HOST)
        .header("authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(api::router(backend.app.clone()).oneshot(agent).await.unwrap().status(), StatusCode::FORBIDDEN);
    backend.cookie.clear();
    assert_eq!(backend.call("GET", "/api/diagnostics", None, false).await.status(), StatusCode::UNAUTHORIZED);
    agentflow_core::diagnostics::close_log();
}

// Ported from backend/tests/test_workflow_safety.py (the human-access ones) -------------------------

async fn raw(
    backend: &support::Running,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> StatusCode {
    let mut request = Request::builder().method(method).uri(path).header("host", HOST);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    api::router(backend.app.clone()).oneshot(request.body(body).unwrap()).await.unwrap().status()
}

#[tokio::test]
async fn test_pairing_is_required_and_one_time() {
    let mut backend = start(json!({})).await;
    backend.cookie.clear();
    let restarted = agentflow_core::api::auth::HumanAccess::new(backend.dir.path().join("none"));
    *backend.app.human.lock().unwrap() = restarted;
    assert_eq!(raw(&backend, "GET", "/api/tasks", &[], None).await, StatusCode::UNAUTHORIZED);
    let code = backend.app.human.lock().unwrap().pairing_code.clone();
    let pair = json!({"code": code});
    let evil = [("origin", "http://evil.invalid")];
    let ui = [("origin", ORIGIN)];
    assert_eq!(raw(&backend, "POST", "/api/auth/pair", &evil, Some(pair.clone())).await, StatusCode::FORBIDDEN);
    let response = backend.call("POST", "/api/auth/pair", Some(pair.clone()), true).await;
    assert_eq!(response.status(), StatusCode::OK);
    backend.cookie = response.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    assert_eq!(backend.call("GET", "/api/tasks", None, false).await.status(), StatusCode::OK);
    assert_eq!(raw(&backend, "POST", "/api/auth/pair", &ui, Some(pair)).await, StatusCode::UNAUTHORIZED);
    let cookie = backend.cookie.clone();
    let evil_with_session = [("origin", "http://evil.invalid"), ("cookie", cookie.as_str())];
    assert_eq!(
        raw(&backend, "POST", "/api/tasks/TEST-1/cancel", &evil_with_session, None).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn test_paired_session_survives_restart_and_stores_only_a_hash() {
    let backend = start(json!({})).await;
    let token = backend.cookie.split_once('=').unwrap().1.to_string();
    let file = backend.dir.path().join("data").join("browser-session");
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(stored == agentflow_core::api::auth::digest(&token) && !stored.contains(&token));
    // A restart reads the session back; pairing again replaces it, signing out the old cookie.
    let mut restarted = agentflow_core::api::auth::HumanAccess::new(file);
    let cookie = format!("agentflow_human={token}");
    assert!(restarted.authenticated(Some(&cookie)));
    let code = restarted.pairing_code.clone();
    let new_token = restarted.pair(&code).unwrap();
    assert!(restarted.authenticated(Some(&format!("agentflow_human={new_token}"))));
    assert!(!restarted.authenticated(Some(&cookie)));
}

/// The pairing response's cookie lasts (Max-Age), as Python's `set_cookie(max_age=...)`.
#[tokio::test]
async fn pairing_sets_a_lasting_http_only_cookie() {
    let mut backend = start(json!({})).await;
    backend.cookie.clear();
    *backend.app.human.lock().unwrap() = agentflow_core::api::auth::HumanAccess::new(backend.dir.path().join("x"));
    let code = backend.app.human.lock().unwrap().pairing_code.clone();
    let response = backend.call("POST", "/api/auth/pair", Some(json!({"code": code})), true).await;
    let cookie = response.headers()["set-cookie"].to_str().unwrap().to_lowercase();
    assert!(
        cookie.contains("max-age") && cookie.contains("httponly") && cookie.contains("samesite=strict"),
        "{cookie}"
    );
}

/// Serving the built UI is not ported yet; such paths are 404s either way.
#[tokio::test]
async fn test_file_traversal_is_rejected() {
    let backend = start(json!({})).await;
    assert_eq!(raw(&backend, "GET", "/%2e%2e%5c%2e%2e%5cREADME.md", &[], None).await, StatusCode::NOT_FOUND);
    assert_eq!(raw(&backend, "GET", "/%2e%2e%2f%2e%2e%2fREADME.md", &[], None).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_pairing_payload_and_websocket_origin_validation() {
    let mut backend = start(json!({})).await;
    let cookie = std::mem::take(&mut backend.cookie);
    let ui = [("origin", ORIGIN)];
    assert_eq!(raw(&backend, "POST", "/api/auth/pair", &ui, Some(json!([]))).await, StatusCode::UNPROCESSABLE_ENTITY);
    let upgrade = |origin: &'static str, cookie: Option<String>| {
        let mut request = Request::builder()
            .uri("/api/ws")
            .header("host", HOST)
            .header("origin", origin)
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.body(Body::empty()).unwrap()
    };
    // Without a session, and with a session from a foreign origin, the upgrade is refused.
    let unpaired = api::router(backend.app.clone()).oneshot(upgrade("http://127.0.0.1", None)).await.unwrap();
    assert_ne!(unpaired.status(), StatusCode::SWITCHING_PROTOCOLS);
    let foreign = api::router(backend.app.clone()).oneshot(upgrade("http://evil.invalid", Some(cookie))).await.unwrap();
    assert_ne!(foreign.status(), StatusCode::SWITCHING_PROTOCOLS);
}

// Ported from backend/tests/test_security_improvements.py ---------------------------------------------

#[tokio::test]
async fn test_agent_cannot_import_usage() {
    let backend = start(json!({})).await;
    let request = Request::builder()
        .method("PUT")
        .uri("/api/tasks/TEST-1/runs/1/usage")
        .header("host", HOST)
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = api::router(backend.app.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// Ported from backend/tests/test_startup_jira.py -----------------------------------------------------
// (Starting the binary with and without Jira is in crates/server/tests/startup.rs.)

const CLOUD: &str = r#"{"enabled": true, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t"}"#;

#[tokio::test]
async fn test_config_reload_allows_disabling_jira() {
    let backend = start(json!({"jira": serde_json::from_str::<Value>(CLOUD).unwrap()})).await;
    let mut dropped = backend.on_disk();
    dropped.as_object_mut().unwrap().remove("jira");
    std::fs::write(backend.config_file(), dropped.to_string()).unwrap();
    let (status, body) = backend.json("POST", "/api/config/reload", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn system_info_reports_paths_the_way_python_prints_them() {
    let backend = start(json!({})).await;
    let (_, config) = backend.json("GET", "/api/config", None).await;
    let data = backend.dir.path().join("data");
    assert_eq!(config["dataDirectory"], json!(agentflow_core::paths::py_str(&data)));
    // An unset orchestrator runs as the architect (AgentsConfig.for_role).
    let architect = Config::default().agents.for_role(agentflow_core::config::Role::Architect).clone();
    assert_eq!(config["agents"]["orchestrator"], json!(architect));
    assert!(Path::new(config["configPath"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn test_terminal_input_rejects_empty_oversize_and_unbound_input() {
    let backend = start(json!({})).await;
    let path = "/api/tasks/TEST-1/terminal/architect/input";
    for body in [
        json!({"input": "", "pane_id": "pty_1"}),
        json!({"input": "x".repeat(4097), "pane_id": "pty_1"}),
        json!({"input": "yes\r"}),
    ] {
        assert_eq!(backend.json("POST", path, Some(body)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn test_terminal_input_requires_human_cookie_and_same_origin() {
    let backend = start(json!({})).await;
    let path = "/api/tasks/TEST-1/terminal/architect/input";
    let body = json!({"input": "yes", "pane_id": "stale"});
    assert_eq!(raw(&backend, "POST", path, &[("origin", ORIGIN)], Some(body.clone())).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        raw(
            &backend,
            "POST",
            path,
            &[("origin", "http://evil.invalid"), ("cookie", backend.cookie.as_str())],
            Some(body)
        )
        .await,
        StatusCode::FORBIDDEN
    );
}
