//! A backend started in process on a temp config, called through its router.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use agentflow_core::api::{self, App, Startup};
use agentflow_core::config::{Locations, read_config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

pub const HOST: &str = "127.0.0.1:8765";
pub const ORIGIN: &str = "http://127.0.0.1:8765";

pub struct Running {
    pub app: Arc<App>,
    pub dir: tempfile::TempDir,
    pub cookie: String,
}

/// A backend on a config.json holding `raw` plus data paths inside a temp directory, paired.
pub async fn start(raw: Value) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut raw = raw;
    raw["app"] = json!({"dataDirectory": data, "databasePath": data.join("agentflow.db")});
    let workspace = raw.get("workspace").cloned().unwrap_or(json!({}));
    let mut workspace = workspace.as_object().unwrap().clone();
    workspace.insert("worktreeDirectory".into(), json!(data.join("worktrees")));
    workspace.insert("taskDirectory".into(), json!(data.join("tasks")));
    raw["workspace"] = Value::Object(workspace);
    let config_file = dir.path().join("config.json");
    std::fs::write(&config_file, raw.to_string()).unwrap();
    let config = read_config(&config_file).unwrap();
    let app = App::start(Startup {
        config,
        config_file,
        locations: Locations::from_env_with_checkout(None),
        flowctl: PathBuf::from("flowctl"),
        probe_tools: false,
        terminals: Arc::new(agentflow_core::terminal::testing::FakeTerminals::default()),
        leftover_zellij: None,
        git: Arc::new(agentflow_core::vcs::FakeGit::default()),
    })
    .await
    .unwrap();
    let code = app.human.lock().unwrap().pairing_code.clone();
    let mut running = Running { app, dir, cookie: String::new() };
    let response = running.call("POST", "/api/auth/pair", Some(json!({"code": code})), true).await;
    let set_cookie = response.headers()["set-cookie"].to_str().unwrap().to_string();
    running.cookie = set_cookie.split(';').next().unwrap().to_string();
    running
}

impl Running {
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>, from_ui: bool) -> axum::response::Response {
        let mut request = Request::builder().method(method).uri(path).header("host", HOST);
        if from_ui {
            request = request.header("origin", ORIGIN);
        }
        if !self.cookie.is_empty() {
            request = request.header("cookie", &self.cookie);
        }
        let body = match body {
            Some(value) => {
                request = request.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        api::router(self.app.clone()).oneshot(request.body(body).unwrap()).await.unwrap()
    }

    pub async fn json(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let response = self.call(method, path, body, true).await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    pub fn config_file(&self) -> PathBuf {
        self.dir.path().join("config.json")
    }

    pub fn on_disk(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.config_file()).unwrap()).unwrap()
    }
}
