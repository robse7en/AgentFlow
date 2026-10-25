mod engine_support;

#[tokio::test]
async fn close_warning_tracks_running_work() {
    let harness = engine_support::engine().await;
    assert!(harness.engine.active_work_labels().unwrap().is_empty());
    harness.add_run("architect", "RUNNING", None);
    assert_eq!(harness.engine.active_work_labels().unwrap(), vec!["TEST-1"]);
}
