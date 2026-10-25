//! Config reload, in its own test process: it sets AGENTFLOW_SKIP_JIRA_CHECK, which the Jira
//! check in tests/api.rs must not see.

mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::start;

#[tokio::test]
async fn reload_takes_effect_and_refuses_restart_only_changes() {
    // Reload re-detects harnesses by running them: a harness that is never installed keeps the
    // user's real CLIs out of the test.
    let stub = json!({"type": "generic-cli", "command": "agentflow-test-not-installed"});
    let agents: serde_json::Map<String, serde_json::Value> =
        ["architect", "implementer", "codeReviewer", "securityReviewer"]
            .iter()
            .map(|r| (r.to_string(), json!({"harness": "stub"})))
            .collect();
    let backend = start(json!({"harnesses": {"stub": stub}, "agents": agents})).await;
    let mut changed = backend.on_disk();
    changed["workflow"] = json!({"maxReviewIterations": 7});
    std::fs::write(backend.config_file(), changed.to_string()).unwrap();
    unsafe_skip_jira();
    let (status, body) = backend.json("POST", "/api/config/reload", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["workflow"]["maxReviewIterations"], 7);
    assert_eq!(backend.app.engine.config().workflow.max_review_iterations, 7);
    changed["zellij"] = json!({"command": "other-zellij"});
    std::fs::write(backend.config_file(), changed.to_string()).unwrap();
    let (status, body) = backend.json("POST", "/api/config/reload", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["detail"], "Restart AgentFlow to change Zellij configuration");

    // A desktop's runtime origin differs from its saved server settings. Ordinary reload keeps
    // that origin; changing the saved server settings still requires a restart.
    changed.as_object_mut().unwrap().remove("zellij");
    let mut runtime = (*backend.app.engine.config()).clone();
    let configured = runtime.server.clone();
    runtime.server.port += 1;
    let running_port = runtime.server.port;
    *backend.app.configured_server.lock().unwrap() = Some(configured.clone());
    backend.app.engine.set_config(std::sync::Arc::new(runtime));
    std::fs::write(backend.config_file(), changed.to_string()).unwrap();
    let (status, body) = backend.json("POST", "/api/config/reload", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(backend.app.engine.config().server.port, running_port);
    changed["server"] = serde_json::to_value(configured).unwrap();
    changed["server"]["port"] = (running_port + 1).into();
    std::fs::write(backend.config_file(), changed.to_string()).unwrap();
    let (status, _) = backend.json("POST", "/api/config/reload", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// Reloading needs Jira unless the check is skipped, as --skip-jira-check does for the server.
fn unsafe_skip_jira() {
    // SAFETY: only this test sets the variable, and only to the value every reload test expects.
    unsafe { std::env::set_var("AGENTFLOW_SKIP_JIRA_CHECK", "1") };
}
