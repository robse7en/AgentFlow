#![allow(dead_code)]
use crate::engine_support;
use agentflow_core::{
    chat::{models::*, service::ChatService},
    config::SlackConfig,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
#[derive(Default)]
pub struct Adapter {
    pub sent: Mutex<Vec<ChatMessage>>,
    pub calls: Mutex<Vec<(String, Value)>>,
    pub connections: AtomicUsize,
    pub disconnects: AtomicUsize,
    pub input: Mutex<Option<mpsc::Sender<Value>>>,
    pub fail_connect: AtomicUsize,
    pub fail_post: AtomicUsize,
    pub fail_send: AtomicUsize,
    pub fail_update: AtomicUsize,
    pub fail_reaction: AtomicUsize,
}
#[async_trait::async_trait]
impl ChatAdapter for Adapter {
    async fn connect(&self) -> Result<mpsc::Receiver<Value>, ChatError> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        if self.fail_connect.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            return Err(ChatError::temporary("offline"));
        }
        let (tx, rx) = mpsc::channel(128);
        *self.input.lock().unwrap() = Some(tx);
        Ok(rx)
    }
    async fn disconnect(&self) {
        self.disconnects.fetch_add(1, Ordering::SeqCst);
        self.input.lock().unwrap().take();
    }
    async fn open_direct(&self, _user: &str) -> Result<String, ChatError> {
        Ok("D-operator".into())
    }
    async fn send(&self, message: ChatMessage) -> Result<String, ChatError> {
        if self.fail_send.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            return Err(ChatError::temporary("PRIVATE transport error"));
        }
        let mut sent = self.sent.lock().unwrap();
        sent.push(message);
        Ok(format!("ts{}", sent.len()))
    }
    async fn update(&self, channel: &str, id: &str, text: &str) -> Result<(), ChatError> {
        self.calls.lock().unwrap().push(("chat.update".into(), json!({"channel":channel,"ts":id,"text":text})));
        if self.fail_update.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            return Err(ChatError::temporary("PRIVATE edit error"));
        }
        Ok(())
    }
    async fn api(&self, method: &str, body: Value) -> Result<Value, ChatError> {
        self.calls.lock().unwrap().push((method.into(), body));
        if method.starts_with("reactions.")
            && self.fail_reaction.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok()
        {
            return Err(ChatError::temporary("reaction refused"));
        }
        if method == "chat.postMessage"
            && self.fail_post.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok()
        {
            return Err(ChatError::temporary("PRIVATE secret"));
        }
        Ok(json!({"ok":true,"ts":"reply"}))
    }
}
pub async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
pub fn service(h: &engine_support::Harness, a: &Arc<Adapter>, settings: SlackConfig) -> Arc<ChatService> {
    let service = ChatService::new(
        h.engine.clone(),
        a.clone(),
        SlackConfig { user_id: Some("U-operator".into()), ..settings },
        Duration::from_millis(20),
    );
    *service.channel.write().unwrap() = Some("D-operator".into());
    service
}
pub fn message(text: &str, thread: Option<&str>) -> MessageRequest {
    MessageRequest {
        user_id: "U-operator".into(),
        channel: "D-operator".into(),
        thread_id: thread.map(str::to_owned),
        text: text.into(),
    }
}
pub async fn emit(a: &Arc<Adapter>, envelope: Value) {
    until(|| a.input.lock().unwrap().is_some()).await;
    let tx = a.input.lock().unwrap().as_ref().unwrap().clone();
    tx.send(envelope).await.unwrap();
}
pub fn clicked(id: &str, kind: &str, value: &str, user: &str, channel: &str) -> Value {
    json!({"envelope_id":id,"type":"interactive","payload":{"type":"block_actions","user":{"id":user},"channel":{"id":channel},"message":{"ts":"action-ts"},"actions":[{"action_id":format!("agentflow_{kind}_0"),"value":value}]}})
}

pub fn action(kind: &str, value: &str) -> ActionRequest {
    ActionRequest { user_id: "U-operator".into(), channel: "D-operator".into(), kind: kind.into(), value: value.into() }
}
pub async fn question(h: &engine_support::Harness) -> String {
    use agentflow_core::engine::spec;
    let run = h.add_run(spec::IMPLEMENTER, "RUNNING", None);
    engine_support::add_session(h, run);
    engine_support::tab_seven(h);
    h.engine
        .agent_question(
            "TEST-1",
            spec::IMPLEMENTER,
            Some(run),
            "Which rollout?",
            &["Gradual".into(), "Immediate".into()],
        )
        .await
        .unwrap();
    h.engine.snapshot("TEST-1").unwrap()["questions"][0]["uid"].as_str().unwrap().into()
}
pub fn gate(h: &engine_support::Harness, documents: &[(&str, &str)]) {
    use agentflow_core::engine::spec;
    h.with_task(|t| {
        let i = t.node_index(spec::REQUIREMENTS_GATE).unwrap();
        t.nodes[i].state = "WAITING_FOR_APPROVAL".into();
        let i = t.node_index(spec::ARCHITECT).unwrap();
        t.nodes[i].state = "COMPLETED".into();
    });
    h.engine.db.session(|tx| tx.execute("delete from requirements_approvals", [])).unwrap();
    let dir = h.task_dir().join("TEST-1/requirements");
    std::fs::create_dir_all(&dir).unwrap();
    for (_, name) in agentflow_core::workspace::REQUIREMENT_FILES {
        std::fs::write(dir.join(name), "").unwrap();
    }
    for (name, text) in documents {
        std::fs::write(dir.join(name), text).unwrap();
    }
}
pub fn approval(a: &Adapter) -> String {
    a.calls
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, v)| v["blocks"].as_array().into_iter().flatten())
        .flat_map(|b| b["elements"].as_array().into_iter().flatten())
        .find(|b| b["action_id"].as_str().is_some_and(|s| s.starts_with("agentflow_approve_requirements_")))
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .into()
}
