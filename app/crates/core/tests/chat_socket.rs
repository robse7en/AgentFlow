use agentflow_core::{
    chat::{
        models::{ChatAdapter, ChatMessage},
        slack::SlackAdapter,
    },
    jira::{HttpReply, JiraError, JiraTransport},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct Http {
    calls: Mutex<Vec<HttpCall>>,
    url: String,
}
type HttpCall = (String, HashMap<String, String>, Value);
#[async_trait::async_trait]
impl JiraTransport for Http {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: HashMap<String, String>,
        body: Option<Value>,
        _max_bytes: u64,
        _timeout: Duration,
    ) -> Result<HttpReply, JiraError> {
        assert_eq!(method, "POST");
        assert!(url.starts_with("https://slack.com/api/"));
        let api = url.rsplit('/').next().unwrap();
        self.calls.lock().unwrap().push((api.into(), headers, body.unwrap()));
        let value = match api {
            "apps.connections.open" => json!({"ok":true,"url":self.url}),
            "conversations.open" => json!({"ok":true,"channel":{"id":"D-local"}}),
            "chat.postMessage" => json!({"ok":true,"ts":"ts1"}),
            _ => json!({"ok":true}),
        };
        Ok(HttpReply { status: 200, headers: HashMap::new(), body: serde_json::to_vec(&value).unwrap() })
    }
}
#[tokio::test]
async fn real_loopback_socket_delivers_envelopes_acks_and_cached_replays() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let http = Arc::new(Http::default());
    let adapter =
        SlackAdapter::with_loopback_socket("fake-bot".into(), "fake-app".into(), http.clone(), address).unwrap();
    let peer = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let envelope = json!({"envelope_id":"same","type":"events_api","payload":{}});
        ws.send(tokio_tungstenite::tungstenite::Message::Text(envelope.to_string().into())).await.unwrap();
        let ack = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let ack: Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(ack["envelope_id"], "same");
        ws.send(tokio_tungstenite::tungstenite::Message::Text(envelope.to_string().into())).await.unwrap();
        let second = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&second).unwrap(), ack);
    });
    let mut events = adapter.connect().await.unwrap();
    let event = events.recv().await.unwrap();
    assert_eq!(event["envelope_id"], "same");
    adapter.api("__ack", json!({"envelope_id":"same"})).await.unwrap();
    peer.await.unwrap();
    adapter.disconnect().await;
    let calls = http.calls.lock().unwrap();
    assert_eq!(calls[0].0, "auth.test");
    assert_eq!(calls[0].1["Authorization"], "Bearer fake-bot");
    assert_eq!(calls[1].1["Authorization"], "Bearer fake-app");
}
#[tokio::test]
async fn production_socket_refuses_non_slack_destinations_and_arbitrary_api_methods() {
    for url in [
        "ws://slack.com/socket",
        "wss://slack.com.evil.test/socket",
        "wss://slack.com:8443/socket",
        "wss://user:password@slack.com/socket",
        "wss://127.0.0.1/socket",
    ] {
        let http = Arc::new(Http { url: url.into(), ..Default::default() });
        let adapter = SlackAdapter::with_transport("fake".into(), "fake".into(), http.clone());
        assert!(adapter.connect().await.is_err());
        assert!(adapter.api("admin.users.remove", json!({})).await.is_err());
        assert_eq!(http.calls.lock().unwrap().len(), 2);
    }
    assert!(
        SlackAdapter::with_loopback_socket(
            "fake".into(),
            "fake".into(),
            Arc::new(Http::default()),
            "192.0.2.1:80".parse().unwrap()
        )
        .is_err()
    );
}
#[tokio::test]
async fn sent_messages_escape_controls_and_clear_stale_buttons_on_update() {
    let http = Arc::new(Http::default());
    let adapter = SlackAdapter::with_transport("fake".into(), "fake".into(), http.clone());
    assert_eq!(adapter.open_direct("U-local").await.unwrap(), "D-local");
    adapter
        .send(ChatMessage {
            channel: "D-local".into(),
            text: "Safe &lt;!channel&gt;".into(),
            thread_id: Some("thread".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    adapter.update("D-local", "ts1", "Answered").await.unwrap();
    let calls = http.calls.lock().unwrap();
    assert_eq!(calls[1].2["thread_ts"], "thread");
    assert_eq!(calls[1].2["unfurl_links"], false);
    assert_eq!(calls[2].2["blocks"][0]["type"], "section");
    assert!(!calls[2].2["blocks"].to_string().contains("actions"));
}
