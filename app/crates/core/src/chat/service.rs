//! Background lifecycle, coalesced snapshots and reconnect. Never part of a workflow await.
use super::{models::*, notifications::Notifier, slack::SlackAdapter};
use crate::{
    config::SlackConfig,
    db::{ChatThreadRow, TaskRow, table},
    engine::Engine,
};
use indexmap::IndexMap;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::sync::Notify;

pub struct ChatService {
    pub engine: Arc<Engine>,
    pub adapter: Arc<dyn ChatAdapter>,
    pub settings: SlackConfig,
    pub channel: RwLock<Option<String>>,
    pending: Mutex<IndexMap<String, Option<Value>>>,
    silent: Mutex<HashSet<String>>,
    pub(crate) created: Mutex<HashSet<String>>,
    wake: Notify,
    jobs: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    listener: Mutex<Option<u64>>,
    pub(crate) branches: Mutex<HashMap<String, (std::time::Instant, Vec<String>)>>,
    pub(crate) folders: Mutex<HashMap<String, (std::time::Instant, Vec<String>)>>,
    pub(crate) ask_runner: Arc<dyn super::ask::AskRunner>,
    pub(crate) ask_lock: tokio::sync::Mutex<()>,
    pub(crate) ask_history: Mutex<HashMap<String, Vec<(String, String)>>>,
    pub(crate) interactions: Arc<tokio::sync::Semaphore>,
    pub retry_delay: Duration,
    pub(crate) envelopes: Mutex<IndexMap<String, Option<Value>>>,
    pub(crate) event_ids: Mutex<IndexMap<String, ()>>,
    stopped: std::sync::atomic::AtomicBool,
}
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub struct ChatHandle {
    pub service: Arc<ChatService>,
}
impl Drop for ChatHandle {
    fn drop(&mut self) {
        self.service.stop();
    }
}
pub fn start_chat(engine: Arc<Engine>) -> Option<ChatHandle> {
    let settings = engine.config().slack.clone();
    if !settings.enabled {
        tracing::info!("Slack is disabled");
        return None;
    }
    let problems = settings.problems();
    if !problems.is_empty() {
        tracing::warn!(problems=?problems,"Slack is not configured; backend remains available");
        return None;
    }
    let adapter = Arc::new(SlackAdapter::new(
        settings.resolved_bot_token().unwrap_or_default(),
        settings.resolved_app_token().unwrap_or_default(),
    ));
    let service = ChatService::new(engine, adapter, settings, Duration::from_secs(5));
    service.start();
    Some(ChatHandle { service })
}
impl ChatService {
    pub fn new(
        engine: Arc<Engine>,
        adapter: Arc<dyn ChatAdapter>,
        settings: SlackConfig,
        retry_delay: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            adapter,
            settings,
            channel: RwLock::new(None),
            pending: Mutex::new(IndexMap::new()),
            silent: Mutex::new(HashSet::new()),
            created: Mutex::new(HashSet::new()),
            wake: Notify::new(),
            jobs: Mutex::new(vec![]),
            listener: Mutex::new(None),
            branches: Mutex::new(HashMap::new()),
            folders: Mutex::new(HashMap::new()),
            ask_runner: Arc::new(super::ask::CliAskRunner),
            ask_lock: tokio::sync::Mutex::new(()),
            ask_history: Mutex::new(HashMap::new()),
            interactions: Arc::new(tokio::sync::Semaphore::new(16)),
            retry_delay,
            envelopes: Mutex::new(IndexMap::new()),
            event_ids: Mutex::new(IndexMap::new()),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
    }
    /// Construct a service with an injected CLI boundary; transport and authorization are unchanged.
    pub fn new_with_ask_runner(
        engine: Arc<Engine>,
        adapter: Arc<dyn ChatAdapter>,
        settings: SlackConfig,
        runner: Arc<dyn super::ask::AskRunner>,
    ) -> Arc<Self> {
        let mut service = Self::new(engine, adapter, settings, Duration::from_secs(5));
        Arc::get_mut(&mut service).expect("new service has one owner").ask_runner = runner;
        service
    }
    pub fn dm(&self) -> Option<String> {
        self.channel.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
    pub fn authorized(&self, user: &str) -> bool {
        self.settings.user_id.as_deref() == Some(user)
    }
    pub fn task_url(&self, key: &str) -> String {
        format!("{}/tasks/{key}", self.settings.ui_base_url.trim_end_matches('/'))
    }
    pub fn snapshot(&self, key: &str) -> Option<Value> {
        self.engine.snapshot(&key.to_uppercase()).ok()
    }
    pub fn thread(&self, key: &str) -> Option<String> {
        let channel = self.dm()?;
        self.engine
            .db
            .session(|tx| -> rusqlite::Result<_> {
                let task = table::select_one::<TaskRow>(tx, "WHERE jira_key = ?1", [key])?;
                match task {
                    Some(task) => Ok(table::select_one::<ChatThreadRow>(
                        tx,
                        "WHERE task_id = ?1 AND platform = ?2 AND channel = ?3",
                        (task.id, "slack", channel),
                    )?
                    .and_then(|r| r.thread_id)),
                    None => Ok(None),
                }
            })
            .ok()
            .flatten()
    }
    pub fn thread_task(&self, thread: &str) -> Option<String> {
        let channel = self.dm()?;
        self.engine.db.session(|tx| tx.query_row("select t.jira_key from tasks t join chat_threads c on c.task_id = t.id where c.platform = ?1 and c.channel = ?2 and c.thread_id = ?3",("slack",channel,thread),|row| row.get::<_,String>(0))).ok()
    }
    pub fn queue(&self, key: &str, snapshot: Option<Value>) {
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(key.into(), snapshot);
        self.wake.notify_one();
    }
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) -> bool {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.retain(|j| !j.is_finished());
        if jobs.len() >= 64 || self.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        jobs.push(tokio::spawn(future));
        true
    }
    pub fn start(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let id = self.engine.broadcaster.subscribe(move |event| {
            if event["type"] == "task"
                && let Some(key) = event["task"]["jiraKey"].as_str()
                && let Some(service) = weak.upgrade()
            {
                service.queue(key, Some(event["task"].clone()));
            }
        });
        *self.listener.lock().unwrap_or_else(|p| p.into_inner()) = Some(id);
        let _ = self.catch_up();
        let service = self.clone();
        self.spawn(async move {
            service.run().await;
        });
        if self.settings.digest_at.is_some() {
            let service = self.clone();
            self.spawn(async move {
                service.digest_loop().await;
            });
        }
    }
    fn catch_up(&self) -> rusqlite::Result<()> {
        let rows = self.engine.db.session(|tx| {
            let mut statement = tx.prepare("select t.jira_key, t.status, c.id from tasks t left join chat_threads c on c.task_id = t.id and c.platform = ?1")?;
            statement.query_map(["slack"],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<i64>>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        for (key, status, thread) in rows {
            if thread.is_none() && !matches!(status.as_str(), "ACTIVE" | "NEEDS_HUMAN_REVIEW") {
                self.silent.lock().unwrap().insert(key.clone());
            }
            self.queue(&key, None);
        }
        Ok(())
    }
    async fn run(self: Arc<Self>) {
        let mut delay = self.retry_delay;
        loop {
            let connection = async {
                let receiver = self.adapter.connect().await?;
                let channel = self.adapter.open_direct(self.settings.user_id.as_deref().unwrap_or("")).await?;
                Ok::<_, ChatError>((receiver, channel))
            }
            .await;
            match connection {
                Ok((mut events, channel)) => {
                    delay = self.retry_delay;
                    *self.channel.write().unwrap() = Some(channel.clone());
                    // Requeue after a cancelled delivery without resetting startup suppression.
                    if let Ok(keys) = self.engine.db.session(|tx| {
                        let mut q = tx.prepare("select t.jira_key from tasks t")?;
                        q.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()
                    }) {
                        let mut pending = self.pending.lock().unwrap();
                        for key in keys {
                            pending.entry(key).or_insert(None);
                        }
                    }
                    let service = self.clone();
                    let notification_job = AbortOnDrop(tokio::spawn(async move {
                        service.deliver(channel).await;
                    }));
                    while let Some(envelope) = events.recv().await {
                        if envelope["type"] == "disconnect" {
                            break;
                        }
                        let Ok(permit) = self.interactions.clone().try_acquire_owned() else {
                            self.busy(&envelope).await;
                            continue;
                        };
                        let service = self.clone();
                        self.spawn(async move {
                            let _permit = permit;
                            service.envelope(envelope).await;
                        });
                    }
                    drop(notification_job);
                    *self.channel.write().unwrap() = None;
                }
                Err(_) => {
                    tracing::warn!("Slack connection failed; retrying in background");
                }
            }
            self.adapter.disconnect().await;
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(300));
        }
    }
    async fn deliver(self: Arc<Self>, channel: String) {
        let notifier = Notifier::new(self.engine.db.clone(), self.adapter.clone(), self.settings.clone(), channel)
            .with_transition(self.transition());
        let mut failures: HashMap<String, (u32, tokio::time::Instant)> = HashMap::new();
        loop {
            let next = self.pending.lock().unwrap().shift_remove_index(0);
            if let Some((key, value)) = next {
                let Some(snapshot) = value.or_else(|| self.snapshot(&key)) else {
                    continue;
                };
                let silent = self.silent.lock().unwrap().contains(&key);
                let created = self.created.lock().unwrap().contains(&key);
                let result = notifier.process_created(&snapshot, silent, created).await;
                if result.is_ok() {
                    self.silent.lock().unwrap().remove(&key);
                    self.created.lock().unwrap().remove(&key);
                    failures.remove(&key);
                } else {
                    let count = failures.get(&key).map(|(n, _)| n + 1).unwrap_or(1);
                    let delay =
                        (self.retry_delay * 2_u32.saturating_pow(count.min(10) - 1)).min(Duration::from_secs(300));
                    failures.insert(key, (count, tokio::time::Instant::now() + delay));
                }
                continue;
            }
            tokio::select! { _ = self.wake.notified() => {},_ = tokio::time::sleep(Duration::from_millis(250)) => {
                let due: Vec<_> = failures.iter().filter(|(_,(_,due))| *due <= tokio::time::Instant::now()).map(|(key,_)| key.clone()).collect();
                for key in due { if let Some((_,due)) = failures.get_mut(&key) { *due = tokio::time::Instant::now()+Duration::from_secs(300); }self.queue(&key,None); }
            } }
        }
    }
    pub async fn post(&self, key: Option<&str>, reply: Reply) -> Result<(), ChatError> {
        let channel = self.dm().ok_or_else(|| ChatError::temporary("Slack is still connecting"))?;
        let thread = key.and_then(|k| self.thread(k));
        let mut body = super::protocol::reply_body(&channel, &reply);
        if let Some(thread) = thread {
            body["thread_ts"] = thread.into();
        }
        self.adapter.api("chat.postMessage", body).await?;
        Ok(())
    }
    pub fn stop(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(id) = self.listener.lock().unwrap_or_else(|p| p.into_inner()).take() {
            self.engine.broadcaster.unsubscribe(id);
        }
        for job in self.jobs.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            job.abort();
        }
        *self.channel.write().unwrap_or_else(|p| p.into_inner()) = None;
        let adapter = self.adapter.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                adapter.disconnect().await;
            });
        }
    }
}
