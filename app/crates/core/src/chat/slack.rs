//! Slack Web API and Socket Mode. Provider errors never quote response bodies or tokens.
use super::{models::*, render};
use crate::jira::{JiraTransport, UreqTransport};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

pub struct SlackAdapter {
    bot_token: String,
    app_token: String,
    http: Arc<dyn JiraTransport>,
    socket: Mutex<Option<mpsc::Sender<Message>>>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    acknowledgements: Arc<Mutex<indexmap::IndexMap<String, Value>>>,
    loopback_socket: Option<std::net::SocketAddr>,
}
impl SlackAdapter {
    pub fn new(bot_token: String, app_token: String) -> Self {
        Self::with_transport(bot_token, app_token, Arc::new(UreqTransport))
    }
    pub fn with_transport(bot_token: String, app_token: String, http: Arc<dyn JiraTransport>) -> Self {
        Self {
            bot_token,
            app_token,
            http,
            socket: Mutex::new(None),
            worker: Mutex::new(None),
            acknowledgements: Arc::new(Mutex::new(indexmap::IndexMap::new())),
            loopback_socket: None,
        }
    }
    /// Local acceptance seam: only a literal loopback address is permitted. HTTP remains injected.
    pub fn with_loopback_socket(
        bot: String,
        app: String,
        http: Arc<dyn JiraTransport>,
        address: std::net::SocketAddr,
    ) -> Result<Self, ChatError> {
        if !address.ip().is_loopback() {
            return Err(ChatError::permanent("Test socket must be loopback"));
        }
        let mut adapter = Self::with_transport(bot, app, http);
        adapter.loopback_socket = Some(address);
        Ok(adapter)
    }
    async fn request(&self, method: &str, body: Value) -> Result<Value, ChatError> {
        if !matches!(
            method,
            "auth.test"
                | "apps.connections.open"
                | "conversations.open"
                | "chat.postMessage"
                | "chat.postEphemeral"
                | "chat.update"
                | "views.open"
                | "reactions.add"
                | "reactions.remove"
        ) {
            return Err(ChatError::permanent("Unsupported Slack operation"));
        }
        let token = if method == "apps.connections.open" { &self.app_token } else { &self.bot_token };
        for attempt in 0..3 {
            let reply = self
                .http
                .request(
                    "POST",
                    &format!("https://slack.com/api/{method}"),
                    HashMap::from([
                        ("Authorization".into(), format!("Bearer {token}")),
                        ("Content-Type".into(), "application/json; charset=utf-8".into()),
                    ]),
                    Some(body.clone()),
                    2_000_000,
                    Duration::from_secs(15),
                )
                .await
                .map_err(|_| ChatError::temporary("Cannot reach Slack"))?;
            if reply.status == 429 {
                if attempt == 2 {
                    return Err(ChatError::temporary("Slack rate limit reached"));
                }
                let delay =
                    reply.headers.get("retry-after").and_then(|s| s.parse::<u64>().ok()).unwrap_or(1).clamp(1, 30);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
            if reply.status != 200 {
                return Err(ChatError::temporary("Slack request failed"));
            }
            let value: Value =
                serde_json::from_slice(&reply.body).map_err(|_| ChatError::temporary("Unreadable Slack response"))?;
            if value["ok"] != true {
                return Err(
                    if matches!(
                        value["error"].as_str(),
                        Some(
                            "invalid_blocks"
                                | "invalid_blocks_format"
                                | "msg_too_long"
                                | "invalid_arguments"
                                | "no_text"
                        )
                    ) {
                        ChatError::permanent("Slack rejected the message content")
                    } else {
                        ChatError::temporary("Slack operation failed")
                    },
                );
            }
            return Ok(value);
        }
        Err(ChatError::temporary("Slack request failed"))
    }
}
#[async_trait::async_trait]
impl ChatAdapter for SlackAdapter {
    async fn connect(&self) -> Result<mpsc::Receiver<Value>, ChatError> {
        self.disconnect().await;
        self.request("auth.test", json!({})).await?;
        let value = self.request("apps.connections.open", json!({})).await?;
        let destination = self
            .loopback_socket
            .map(|address| format!("ws://{address}"))
            .unwrap_or_else(|| value["url"].as_str().unwrap_or("").into());
        let url =
            url::Url::parse(&destination).map_err(|_| ChatError::temporary("Invalid Slack socket destination"))?;
        if self.loopback_socket.is_none()
            && (url.scheme() != "wss"
                || url.port_or_known_default() != Some(443)
                || !url.username().is_empty()
                || url.password().is_some()
                || !url.host_str().is_some_and(|h| h == "slack.com" || h.ends_with(".slack.com")))
        {
            return Err(ChatError::permanent("Untrusted Slack socket destination"));
        }
        let config = WebSocketConfig::default().max_message_size(Some(2_000_000)).max_frame_size(Some(2_000_000));
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false),
        )
        .await
        .map_err(|_| ChatError::temporary("Slack socket connection timed out"))?
        .map_err(|_| ChatError::temporary("Cannot connect to Slack socket"))?;
        let (mut write, mut read) = socket.split();
        let (out, mut sends) = mpsc::channel::<Message>(256);
        let (events, receiver) = mpsc::channel(256);
        let cached = self.acknowledgements.clone();
        *self.socket.lock().unwrap_or_else(|p| p.into_inner()) = Some(out);
        let worker = tokio::spawn(async move {
            loop {
                tokio::select! {
                    outgoing = sends.recv() => match outgoing { Some(message) => if write.send(message).await.is_err() { break; },None => break },
                    incoming = read.next() => {
                        let Some(Ok(message)) = incoming else { break; };
                        match message {
                            Message::Text(text) => {
                                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue; };
                                let prior = value["envelope_id"].as_str().and_then(|id| cached.lock().unwrap_or_else(|p| p.into_inner()).get(id).cloned());
                                if let Some(prior) = prior { if write.send(Message::Text(prior.to_string().into())).await.is_err() { break; } }
                                else if events.send(value).await.is_err() { break; }
                            },
                            Message::Ping(bytes) => if write.send(Message::Pong(bytes)).await.is_err() { break; },
                            Message::Close(_) => break,
                            _ => {},
                        }
                    }
                }
            }
        });
        *self.worker.lock().unwrap_or_else(|p| p.into_inner()) = Some(worker);
        Ok(receiver)
    }
    async fn disconnect(&self) {
        self.socket.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            worker.abort();
        }
    }
    async fn open_direct(&self, user: &str) -> Result<String, ChatError> {
        self.request("conversations.open", json!({"users":user})).await?["channel"]["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| ChatError::temporary("Slack did not open a DM"))
    }
    async fn send(&self, message: ChatMessage) -> Result<String, ChatError> {
        let blocks = if let Some(prompt) = &message.prompt {
            Some(render::notice_blocks(&message.text, prompt))
        } else {
            render::reply_blocks(&Reply { text: message.text.clone(), offer: None, buttons: message.buttons })
        };
        let mut body = json!({"channel":message.channel,"text":message.text,"mrkdwn":true,"unfurl_links":false,"unfurl_media":false});
        if let Some(thread) = message.thread_id {
            body["thread_ts"] = thread.into();
        }
        if let Some(blocks) = blocks {
            body["blocks"] = blocks.into();
        }
        self.request("chat.postMessage", body).await?["ts"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| ChatError::temporary("Slack message identifier missing"))
    }
    async fn update(&self, channel: &str, id: &str, text: &str) -> Result<(), ChatError> {
        self.request("chat.update", json!({"channel":channel,"ts":id,"text":text,"blocks":render::sections(text)}))
            .await?;
        Ok(())
    }
    async fn api(&self, method: &str, body: Value) -> Result<Value, ChatError> {
        if method != "__ack" {
            return self.request(method, body).await;
        }
        let id = body["envelope_id"]
            .as_str()
            .ok_or_else(|| ChatError::permanent("Missing Slack envelope identifier"))?
            .to_owned();
        {
            let mut cache = self.acknowledgements.lock().unwrap_or_else(|p| p.into_inner());
            cache.insert(id, body.clone());
            if cache.len() > 512 {
                cache.shift_remove_index(0);
            }
        }
        let socket = self
            .socket
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| ChatError::temporary("Slack socket is disconnected"))?;
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .map_err(|_| ChatError::temporary("Slack socket is disconnected"))?;
        Ok(json!({"ok":true}))
    }
}
impl Drop for SlackAdapter {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut().unwrap_or_else(|p| p.into_inner()).take() {
            worker.abort();
        }
    }
}
