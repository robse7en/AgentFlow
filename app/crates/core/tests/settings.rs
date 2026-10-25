//! AF-AUDIT-001: settings saves are serialized, written atomically, and applied to the live
//! configuration only once saved; reloads take part in the same ordering.

mod engine_support;

use std::sync::Arc;

use agentflow_core::api::{self, App};
use axum::body::Body;
use axum::http::Request;
use engine_support::{Harness, engine};
use serde_json::{Value, json};
use tower::ServiceExt;

/// One app (one settings lock, as in production) with a paired browser.
struct Ui {
    app: Arc<App>,
    cookie: String,
}

impl Ui {
    async fn new(harness: &Harness) -> Ui {
        let config_file = harness.tmp().join("config.json");
        std::fs::write(&config_file, "{}").unwrap();
        let app = App::around(harness.engine.clone(), config_file);
        let code = app.human.lock().unwrap().pairing_code.clone();
        let pair = Request::builder()
            .method("POST")
            .uri("/api/auth/pair")
            .header("host", "127.0.0.1")
            .header("origin", "http://127.0.0.1")
            .header("content-type", "application/json")
            .body(Body::from(json!({"code": code}).to_string()))
            .unwrap();
        let paired = api::router(app.clone()).oneshot(pair).await.unwrap();
        let cookie = paired.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        Ui { app, cookie }
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> u16 {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1")
            .header("origin", "http://127.0.0.1")
            .header("cookie", &self.cookie)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = api::router(self.app.clone()).oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        if status >= 400 {
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            eprintln!("{method} {path} -> {status}: {}", String::from_utf8_lossy(&body));
        }
        status
    }

    fn file(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.app.config_file).unwrap()).unwrap()
    }
}

/// Saves of two different settings, started together and released together, both survive on
/// disk and in the live configuration. Holding the settings lock makes the overlap certain.
#[tokio::test(flavor = "multi_thread")]
async fn overlapping_saves_of_different_settings_keep_both() {
    let harness = engine().await;
    let ui = Arc::new(Ui::new(&harness).await);
    let held = ui.app.settings.lock().await;
    let repository = {
        let ui = ui.clone();
        tokio::spawn(async move {
            ui.call("PUT", "/api/config/default-repository", json!({"path": "D:/Dev/AgentFlow"})).await
        })
    };
    let rate = {
        let ui = ui.clone();
        tokio::spawn(async move { ui.call("PUT", "/api/config/exchange-rate", json!({"usdToNzdRate": 2.5})).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(ui.file(), json!({}), "both saves wait for the settings lock");
    drop(held);
    assert_eq!((repository.await.unwrap(), rate.await.unwrap()), (200, 200));
    let file = ui.file();
    assert_eq!(file["workspace"]["defaultRepositoryDirectory"], "D:/Dev/AgentFlow");
    assert_eq!(file["workflow"]["usdToNzdRate"], 2.5);
    let live = harness.engine.config();
    assert_eq!(live.workspace.default_repository_directory.as_deref(), Some("D:/Dev/AgentFlow"));
    assert_eq!(live.workflow.usd_to_nzd_rate, 2.5);
}

/// Many rounds of concurrent cost-control and ordinary saves: no round loses either change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cost_controls_and_ordinary_saves_never_lose_each_other() {
    let harness = engine().await;
    let ui = Arc::new(Ui::new(&harness).await);
    for round in 1..=25 {
        let rate = 1.0 + f64::from(round);
        let budget = f64::from(round);
        let saves = {
            let (a, b) = (ui.clone(), ui.clone());
            tokio::join!(
                tokio::spawn(
                    async move { a.call("PUT", "/api/config/exchange-rate", json!({"usdToNzdRate": rate})).await }
                ),
                tokio::spawn(async move {
                    b.call(
                        "PUT",
                        "/api/config/cost-controls",
                        json!({"controls": {"taskBudgetUsd": budget}, "billing": {}}),
                    )
                    .await
                }),
            )
        };
        assert_eq!((saves.0.unwrap(), saves.1.unwrap()), (200, 200));
        let file = ui.file();
        assert_eq!(
            (file["workflow"]["usdToNzdRate"].as_f64(), file["workflow"]["costControls"]["taskBudgetUsd"].as_f64()),
            (Some(rate), Some(budget)),
            "round {round}"
        );
        let live = harness.engine.config();
        assert_eq!((live.workflow.usd_to_nzd_rate, live.workflow.cost_controls.task_budget_usd), (rate, Some(budget)));
    }
}

/// A reload running alongside a save never leaves the live configuration older than the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_reload_cannot_restore_an_older_configuration() {
    let harness = engine().await;
    // A reload re-reads the whole file, so it must hold what the engine was built with; and it
    // detects every harness, so none may name a real CLI.
    let ui = Arc::new(Ui::new(&harness).await);
    let mut file: Value = serde_json::from_str(&harness.engine.config().to_json()).unwrap();
    for harness in file["harnesses"].as_object_mut().unwrap().values_mut() {
        harness["command"] = json!("agentflow-test-not-installed");
    }
    // Placeholders that satisfy the startup Jira check; nothing contacts Jira.
    file["jira"]["enabled"] = json!(true);
    file["jira"]["baseUrl"] = json!("https://example.invalid");
    file["jira"]["apiToken"] = json!("placeholder");
    std::fs::write(&ui.app.config_file, file.to_string()).unwrap();
    for round in 1..=15 {
        let rate = 1.0 + f64::from(round);
        let (a, b) = (ui.clone(), ui.clone());
        let (save, reload) = tokio::join!(
            tokio::spawn(
                async move { a.call("PUT", "/api/config/exchange-rate", json!({"usdToNzdRate": rate})).await }
            ),
            tokio::spawn(async move { b.call("POST", "/api/config/reload", json!({})).await }),
        );
        assert_eq!((save.unwrap(), reload.unwrap()), (200, 200), "round {round}");
        assert_eq!(harness.engine.config().workflow.usd_to_nzd_rate, rate, "round {round}");
        assert_eq!(ui.file()["workflow"]["usdToNzdRate"].as_f64(), Some(rate));
    }
}

/// A save that cannot replace the file reports failure, leaves the previous file whole and the
/// live configuration unchanged, and leaves no temporary file behind.
#[cfg(windows)]
#[tokio::test]
async fn a_failed_write_keeps_the_previous_configuration() {
    let harness = engine().await;
    let ui = Ui::new(&harness).await;
    let before = harness.engine.config().workflow.usd_to_nzd_rate;
    let mut permissions = std::fs::metadata(&ui.app.config_file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&ui.app.config_file, permissions.clone()).unwrap();
    let status = ui.call("PUT", "/api/config/exchange-rate", json!({"usdToNzdRate": 9.5})).await;
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&ui.app.config_file, permissions).unwrap();
    assert_eq!(status, 500);
    assert_eq!(ui.file(), json!({}));
    assert_eq!(harness.engine.config().workflow.usd_to_nzd_rate, before);
    let leftovers = std::fs::read_dir(harness.tmp())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
}

/// Cost controls wait for a launch in progress and then complete: settings lock first, launch
/// lock second, so the two never deadlock.
#[tokio::test(flavor = "multi_thread")]
async fn cost_controls_wait_for_launches_without_deadlock() {
    let harness = engine().await;
    let ui = Arc::new(Ui::new(&harness).await);
    let launching = harness.engine.launch_lock.lock().await;
    let save = {
        let ui = ui.clone();
        tokio::spawn(async move {
            ui.call("PUT", "/api/config/cost-controls", json!({"controls": {"taskBudgetUsd": 3}, "billing": {}})).await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!save.is_finished(), "a policy change waits for the launch");
    // Another setting queues behind the policy change, which holds the settings lock; both
    // finish once the launch does.
    let rate = {
        let ui = ui.clone();
        tokio::spawn(async move { ui.call("PUT", "/api/config/exchange-rate", json!({"usdToNzdRate": 3.5})).await })
    };
    drop(launching);
    let (save, rate) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async { (save.await, rate.await) }).await.unwrap();
    assert_eq!((save.unwrap(), rate.unwrap()), (200, 200));
    let file = ui.file();
    assert_eq!(
        (file["workflow"]["costControls"]["taskBudgetUsd"].as_f64(), file["workflow"]["usdToNzdRate"].as_f64()),
        (Some(3.0), Some(3.5))
    );
}
