mod chat_support;
mod engine_support;
use agentflow_core::config::{Config, SlackConfig};
use chat_support::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[test]
fn test_digest_time_must_be_a_time_of_day() {
    for bad in ["9:00", "24:00", "09:60", "morning"] {
        let mut c = Config::default();
        c.slack.digest_at = Some(bad.into());
        assert!(Config::from_value(serde_json::to_value(c).unwrap()).is_err());
    }
    let mut c = Config::default();
    c.slack.digest_at = Some("07:45".into());
    assert!(Config::from_value(serde_json::to_value(c).unwrap()).is_ok());
    assert!(SlackConfig::default().digest_at.is_none());
}
#[tokio::test]
async fn test_the_digest_is_posted_when_its_time_comes() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig { digest_at: Some("09:00".into()), ..Default::default() });
    let count = AtomicUsize::new(0);
    let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
    let clock = || {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            date.and_hms_opt(8, 59, 59).unwrap()
        } else {
            date.and_hms_opt(9, 0, 1).unwrap()
        }
    };
    let wait = async {
        until(|| {
            a.calls
                .lock()
                .unwrap()
                .iter()
                .any(|(_, v)| v["text"].as_str().is_some_and(|s| s.starts_with("*AgentFlow digest*")))
        })
        .await;
    };
    tokio::select! { _=s.run_digest_schedule(clock,Duration::from_millis(10))=>panic!("schedule exited"),_=wait=>{} }
    let calls = a.calls.lock().unwrap();
    let digests = calls
        .iter()
        .filter(|(_, v)| v["text"].as_str().is_some_and(|s| s.starts_with("*AgentFlow digest*")))
        .collect::<Vec<_>>();
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0].1["channel"], "D-operator");
    assert!(digests[0].1.get("thread_ts").is_none());
}
#[tokio::test]
async fn test_no_digest_when_nothing_is_active() {
    let h = engine_support::engine().await;
    h.set_status("MERGED");
    let a = Arc::new(Adapter::default());
    let s = service(&h, &a, SlackConfig::default());
    s.post_daily_digest().await;
    assert!(a.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn test_a_failed_digest_is_logged_not_raised() {
    let h = engine_support::engine().await;
    let a = Arc::new(Adapter::default());
    a.fail_post.store(1, Ordering::SeqCst);
    let s = service(&h, &a, SlackConfig::default());
    s.post_daily_digest().await;
    s.post_daily_digest().await;
    assert_eq!(a.calls.lock().unwrap().len(), 2);
}
