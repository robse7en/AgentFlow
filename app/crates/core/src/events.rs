//! WebSocket fan-out of workflow state changes, plus in-process listeners, ported from
//! `backend/agentflow/services/events.py`.
//!
//! Each message is serialized once (Python's `json.dumps`, compact) and handed to every connected
//! client through a bounded channel. A client that falls too far behind is disconnected instead of
//! buffering without limit; the UI reconnects and reloads its state.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Messages a slow client may lag behind before it is disconnected.
const CLIENT_BACKLOG: usize = 1024;

type Listener = Box<dyn Fn(&Value) + Send + Sync>;

pub struct Broadcaster {
    sender: broadcast::Sender<Arc<str>>,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_listener: Mutex<u64>,
}

impl Default for Broadcaster {
    fn default() -> Self {
        Broadcaster {
            sender: broadcast::Sender::new(CLIENT_BACKLOG),
            listeners: Mutex::new(Vec::new()),
            next_listener: Mutex::new(0),
        }
    }
}

impl Broadcaster {
    /// Register an in-process listener (e.g. chat notifications); returns its id for
    /// [`Broadcaster::unsubscribe`]. Listeners run synchronously inside every broadcast, so they
    /// must only hand the message off (queue it), never block.
    pub fn subscribe(&self, listener: impl Fn(&Value) + Send + Sync + 'static) -> u64 {
        let mut next = self.next_listener.lock().unwrap_or_else(|p| p.into_inner());
        *next += 1;
        self.listeners.lock().unwrap_or_else(|p| p.into_inner()).push((*next, Box::new(listener)));
        *next
    }

    pub fn unsubscribe(&self, id: u64) {
        self.listeners.lock().unwrap_or_else(|p| p.into_inner()).retain(|(i, _)| *i != id);
    }

    /// A receiver for one WebSocket client.
    pub fn connect(&self) -> broadcast::Receiver<Arc<str>> {
        self.sender.subscribe()
    }

    pub fn broadcast(&self, message: &Value) {
        for (_, listener) in self.listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            // A listener must never break a workflow action.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(message))).is_err() {
                tracing::error!("broadcast listener failed");
            }
        }
        let _ = self.sender.send(Arc::from(pyjson::dumps_compact(message)));
    }

    /// Tell the UI which step a long request has reached, for the toast that started it. Only a
    /// request the UI tagged with an operation id reports progress; chat and other callers stay
    /// silent. Sent to WebSocket clients only: in-process listeners follow workflow state, not steps.
    pub fn progress(&self, jira_key: Option<&str>, message: &str) {
        let Ok(operation) = OPERATION_ID.try_with(Clone::clone) else { return };
        let event = json!({"type": "progress", "operation": operation, "jiraKey": jira_key, "message": message});
        let _ = self.sender.send(Arc::from(pyjson::dumps_compact(&event)));
    }

    pub fn client_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

tokio::task_local! {
    /// The UI's id for the request in progress (the X-Operation-Id header).
    pub static OPERATION_ID: String;
}

/// An operation id the UI may send: short and plain, since it is echoed to every client.
pub fn valid_operation_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn progress_reaches_clients_only_inside_an_operation() {
        let broadcaster = Broadcaster::default();
        let mut client = broadcaster.connect();
        broadcaster.progress(Some("ABC-1"), "untagged");
        OPERATION_ID.scope("op-1".into(), async { broadcaster.progress(Some("ABC-1"), "Creating the worktree") }).await;
        let message: Value = serde_json::from_str(&client.recv().await.unwrap()).unwrap();
        assert_eq!(
            message,
            json!({"type": "progress", "operation": "op-1", "jiraKey": "ABC-1", "message": "Creating the worktree"})
        );
        assert!(client.try_recv().is_err());
    }

    #[test]
    fn operation_ids_are_short_and_plain() {
        assert!(valid_operation_id("3f2a-9c"));
        assert!(!valid_operation_id(""));
        assert!(!valid_operation_id(&"a".repeat(65)));
        assert!(!valid_operation_id("<script>"));
    }
}
