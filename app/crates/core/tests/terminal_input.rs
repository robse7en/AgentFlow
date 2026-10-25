//! Human terminal controls: real router authorization and stream lifetime, recording terminals.
mod engine_support;

use std::sync::Arc;
use std::time::Duration;

use agentflow_core::api::{self, App};
use agentflow_core::engine::spec;
use agentflow_core::terminal::{Backend, PaneInfo};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use engine_support::{Harness, add_session, engine};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PATH: &str = "/api/tasks/TEST-1/terminal/implementer";

async fn fixture() -> (Harness, Arc<App>, String) {
    let h = engine().await;
    h.terminals.set_backend(Backend::Embedded);
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    add_session(&h, run);
    replace_pane(&h, "pty_7");
    let app = App::around(h.engine.clone(), h.tmp().join("config.json"));
    let token = {
        let mut human = app.human.lock().unwrap();
        let code = human.pairing_code.clone();
        human.pair(&code).unwrap()
    };
    (h, app, format!("agentflow_human={token}"))
}

fn replace_pane(h: &Harness, pane: &str) {
    h.with_task(|state| state.agents.last_mut().unwrap().zellij_pane_id = Some(pane.into()));
    h.terminals.add_pane(
        "agentflow-TEST-1",
        PaneInfo {
            pane_id: pane.into(),
            tab_id: 7,
            tab_name: "Implementer".into(),
            exited: false,
            exit_status: None,
            command: None,
        },
    );
}

async fn request(app: &Arc<App>, cookie: &str, body: Option<Value>) -> axum::response::Response {
    let input = body.is_some();
    let request = Request::builder()
        .method(if input { "POST" } else { "GET" })
        .uri(if input { format!("{PATH}/input") } else { PATH.into() })
        .header("host", "127.0.0.1")
        .header("origin", "http://127.0.0.1")
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    api::router(app.clone()).oneshot(request).await.unwrap()
}

#[tokio::test]
async fn current_binding_sends_keys_and_delayed_line_submission() {
    let (h, app, cookie) = fixture().await;
    let response = request(&app, &cookie, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-terminal-input"], "enabled");
    let binding = response.headers()["x-terminal-pane"].to_str().unwrap().to_string();
    drop(response);
    for (input, submit) in [("\u{1b}[A", false), ("yes", true)] {
        let response = request(
            &app,
            &cookie,
            Some(json!({
                "input": input, "pane_id": binding, "submit": submit,
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    let calls = h.terminals.calls();
    assert!(calls.contains(&"write_input agentflow-TEST-1 pty_7 \u{1b}[A".into()));
    assert!(calls.contains(&"write_line agentflow-TEST-1 pty_7 yes".into()));
}

#[tokio::test]
async fn stale_pane_and_previous_instance_bindings_never_write() {
    let (h, app, cookie) = fixture().await;
    let response = request(&app, &cookie, None).await;
    let old = response.headers()["x-terminal-pane"].to_str().unwrap().to_string();
    drop(response);
    replace_pane(&h, "pty_8");
    for binding in [old, format!("{}:pty_8", uuid::Uuid::new_v4())] {
        let response = request(&app, &cookie, Some(json!({"input":"y", "pane_id":binding}))).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    assert!(!h.terminals.calls().iter().any(|call| call.starts_with("write_")));
}

#[tokio::test]
async fn zellij_keeps_external_input_and_agent_credentials_cannot_type() {
    let (h, app, cookie) = fixture().await;
    h.terminals.set_backend(Backend::Zellij);
    let response = request(&app, &cookie, None).await;
    assert_eq!(response.headers()["x-terminal-input"], "disabled");
    let binding = response.headers()["x-terminal-pane"].to_str().unwrap().to_string();
    drop(response);
    let body = json!({"input":"yes", "pane_id":binding});
    assert_eq!(request(&app, &cookie, Some(body.clone())).await.status(), StatusCode::CONFLICT);
    let agent = Request::builder()
        .method("POST")
        .uri(format!("{PATH}/input"))
        .header("host", "127.0.0.1")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(api::router(app).oneshot(agent).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert!(!h.terminals.calls().iter().any(|call| call.starts_with("write_")));
}

#[tokio::test]
async fn pairing_revocation_ends_existing_terminal_stream_before_more_output() {
    let (h, app, cookie) = fixture().await;
    let response = request(&app, &cookie, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    assert!(stream.next().await.unwrap().is_ok());
    {
        let mut human = app.human.lock().unwrap();
        // Make a replacement pairing available while keeping the existing stream's watcher.
        human.pairing_code = "replacement-pairing".into();
        human.pair("replacement-pairing").unwrap();
    }
    h.terminals.push_screen("pty_7", &["must not reach the revoked browser"]);
    assert!(tokio::time::timeout(Duration::from_secs(1), stream.next()).await.unwrap().is_none());
    assert_eq!(
        request(&app, &cookie, Some(json!({"input":"y", "pane_id":"stale"}))).await.status(),
        StatusCode::UNAUTHORIZED
    );
}
