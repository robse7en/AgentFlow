mod engine_support;
use agentflow_core::api::{self, App};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

#[tokio::test]
async fn vue_history_routes_assets_head_and_api_boundaries() {
    let harness = engine_support::engine().await;
    let dist = harness.tmp().join("dist");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    std::fs::write(dist.join("index.html"), "<html>Vue</html>").unwrap();
    std::fs::write(dist.join("assets/app.js"), "export const app = true;").unwrap();
    std::fs::write(dist.join("secrets.json"), "private").unwrap();
    std::fs::File::create(dist.join("assets/oversized.js")).unwrap().set_len(33 * 1024 * 1024).unwrap();
    let app = App::around(harness.engine.clone(), harness.tmp().join("config.json"));
    let router = api::router_with_ui(app, Some(dist));
    for path in ["/", "/tasks/TEST-1", "/settings", "/decisions"] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).header("host", "localhost").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "<html>Vue</html>");
    }
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/app.js")
                .method("HEAD")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "24");
    assert!(to_bytes(response.into_body(), 1024).await.unwrap().is_empty());
    for path in [
        "/assets/missing.js",
        "/assets/oversized.js",
        "/assets/missing",
        "/secrets.json",
        "/.git/config",
        "/%2e%2e/config.json",
        "/api",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).header("host", "localhost").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
    let response = router
        .clone()
        .oneshot(Request::builder().uri("/api/tasks").header("host", "localhost").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let response = router
        .clone()
        .oneshot(Request::builder().uri("/").header("host", "evil.example").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/tasks/TEST-1")
                .method("POST")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 405);
}
