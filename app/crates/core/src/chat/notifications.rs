//! Durable notification delivery. No database transaction survives a transport await.
use super::{
    models::{AnswerPrompt, ChatAdapter, ChatError, ChatMessage},
    notices, render,
};
use crate::{
    config::SlackConfig,
    db::{
        ChatThreadRow, Database, DbTime, Json, TaskRow,
        table::{self, Tracked},
    },
};
use serde_json::{Value, json};
use std::sync::Arc;

impl From<rusqlite::Error> for ChatError {
    fn from(_: rusqlite::Error) -> Self {
        Self::temporary("Chat state unavailable")
    }
}

/// One operator's DM channel. A service supplies snapshots and retries transient failures.
pub struct Notifier {
    db: Arc<Database>,
    adapter: Arc<dyn ChatAdapter>,
    settings: SlackConfig,
    channel: String,
    delivery: tokio::sync::Mutex<()>,
    transition: Option<String>,
}
impl Notifier {
    pub fn new(db: Arc<Database>, adapter: Arc<dyn ChatAdapter>, settings: SlackConfig, channel: String) -> Self {
        Self { db, adapter, settings, channel, delivery: tokio::sync::Mutex::new(()), transition: None }
    }
    pub fn with_transition(mut self, transition: Option<String>) -> Self {
        self.transition = transition;
        self
    }
    fn save(&self, row: &mut Tracked<ChatThreadRow>) -> Result<(), ChatError> {
        self.db.session(|tx| row.flush(tx)).map_err(ChatError::from)
    }
    fn load(
        &self,
        key: &str,
        current: &Value,
        silently: bool,
        created: bool,
    ) -> Result<Option<Tracked<ChatThreadRow>>, ChatError> {
        self.db.session(|tx| {
            let Some(task) = table::select_one::<TaskRow>(tx, "WHERE jira_key = ?1", [key])? else {
                return Ok(None);
            };
            if let Some(row) =
                table::select_one::<ChatThreadRow>(tx, "WHERE task_id = ?1 AND platform = ?2", (task.id, "slack"))?
            {
                return Ok(Some(Tracked::loaded(row)));
            }
            if !silently && !created && current.as_object().is_none_or(|m| m.is_empty()) {
                return Ok(None);
            }
            let mut row = Tracked::new(ChatThreadRow {
                id: 0,
                task_id: task.id,
                platform: "slack".into(),
                channel: None,
                thread_id: None,
                title: None,
                notified: Json(if silently { current.clone() } else { json!({}) }),
                updated_at: DbTime::now(),
            });
            row.flush(tx)?;
            Ok(if silently { None } else { Some(row) })
        })
    }
    pub async fn process(&self, snapshot: &Value, record_silently: bool) -> Result<(), ChatError> {
        self.process_created(snapshot, record_silently, false).await
    }
    pub async fn process_created(
        &self,
        snapshot: &Value,
        record_silently: bool,
        created: bool,
    ) -> Result<(), ChatError> {
        let _delivery = self.delivery.lock().await;
        let key = snapshot["jiraKey"].as_str().ok_or_else(|| ChatError::permanent("Invalid task snapshot"))?;
        let notify = serde_json::to_value(&self.settings.notify).unwrap_or_default();
        let notify: Vec<String> = serde_json::from_value(notify).unwrap_or_default();
        let current = notices::notices(snapshot, &notify, self.settings.include_question_text);
        let initial: Value = current.keys().map(|k| (k.clone(), Value::Null)).collect::<serde_json::Map<_, _>>().into();
        let Some(mut row) = self.load(key, &initial, record_silently, created)? else {
            return Ok(());
        };
        let same_channel = row.channel.as_deref() == Some(&self.channel);
        let thread = if same_channel { row.thread_id.clone() } else { None };
        let url = format!("{}/tasks/{key}", self.settings.ui_base_url.trim_end_matches('/'));
        let title = render::render_title(snapshot, &url);
        let fresh: Vec<_> = current.keys().filter(|k| row.notified.0.get(*k).is_none()).cloned().collect();
        let thread = self.prepare_thread(&mut row, thread, title, !fresh.is_empty() || created).await?;
        if created && row.notified.0.get("created").is_none() {
            let mut text = format!(
                "Created from Slack: branch `{}` from `{}`.",
                super::render::escape(snapshot["branchName"].as_str().unwrap_or("")).replace('`', "'"),
                super::render::escape(snapshot["baseBranch"].as_str().unwrap_or("")).replace('`', "'")
            );
            if self.settings.allows(crate::config::SlackAction::Run) {
                for reason in snapshot["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|n| n["state"] == "READY")
                    .filter_map(|n| super::actions::launch_refusal(snapshot, n))
                {
                    text.push_str(&format!("\n{}", render::escape(&reason)));
                }
            }
            let result = self
                .adapter
                .send(ChatMessage {
                    channel: self.channel.clone(),
                    text,
                    thread_id: thread.clone(),
                    buttons: super::actions::run_buttons(snapshot, &self.settings, self.transition.as_deref()),
                    ..Default::default()
                })
                .await;
            if let Err(error) = result
                && !error.permanent
            {
                return Err(error);
            }
            row.notified.0["created"] = Value::Null;
            self.save(&mut row)?;
        }
        for condition in fresh {
            let notice = &current[&condition];
            let prompt = if self.settings.answering() {
                // Answering implies question text may be posted, a tree's notes included.
                notice.question_uid.as_ref().map(|uid| AnswerPrompt {
                    jira_key: key.into(),
                    question_uid: uid.clone(),
                    choices: notice.choices.clone(),
                    decision: snapshot["questions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|q| q["uid"].as_str() == Some(uid.as_str()))
                        .map(|q| q["decision"].clone())
                        .filter(|d| !d.is_null()),
                    branch: None,
                })
            } else {
                None
            };
            let message = ChatMessage {
                channel: self.channel.clone(),
                text: render::render_notice(notice, prompt.is_some()),
                thread_id: thread.clone(),
                buttons: if prompt.is_some() {
                    vec![]
                } else {
                    super::actions::notice_buttons(snapshot, &condition, &self.settings, self.transition.as_deref())
                },
                prompt,
            };
            let id = match self.adapter.send(message).await {
                Ok(id) => Value::String(id),
                Err(error) if error.permanent => Value::Null,
                Err(error) => return Err(error),
            };
            row.notified.0[&condition] = id;
            self.save(&mut row)?;
        }
        if same_channel {
            self.resolve(snapshot, &row, &current).await;
        }
        let remaining: Value =
            current.keys().map(|k| (k.clone(), row.notified.0[k].clone())).collect::<serde_json::Map<_, _>>().into();
        row.notified = Json(remaining);
        self.save(&mut row)
    }
    async fn prepare_thread(
        &self,
        row: &mut Tracked<ChatThreadRow>,
        thread: Option<String>,
        title: String,
        fresh: bool,
    ) -> Result<Option<String>, ChatError> {
        if thread.is_none() && fresh {
            let id = self
                .adapter
                .send(ChatMessage { channel: self.channel.clone(), text: title.clone(), ..Default::default() })
                .await?;
            row.channel = Some(self.channel.clone());
            row.thread_id = Some(id.clone());
            row.title = Some(title);
            self.save(row)?;
            return Ok(Some(id));
        }
        if let Some(id) = &thread
            && row.title.as_deref() != Some(&title)
        {
            let _ = self.adapter.update(&self.channel, id, &title).await;
            row.title = Some(title);
            self.save(row)?;
        }
        Ok(thread)
    }
    async fn resolve(
        &self,
        snapshot: &Value,
        row: &ChatThreadRow,
        current: &indexmap::IndexMap<String, notices::Notice>,
    ) {
        for (condition, id) in row.notified.0.as_object().into_iter().flatten() {
            let Some(uid) = condition.strip_prefix("question:").filter(|_| !current.contains_key(condition)) else {
                continue;
            };
            let Some(id) = id.as_str() else {
                continue;
            };
            let question =
                snapshot["questions"].as_array().into_iter().flatten().find(|q| q["uid"].as_str() == Some(uid));
            let text = render::render_resolved(question, self.settings.include_question_text);
            let _ = self.adapter.update(&self.channel, id, &text).await;
        }
    }
}
