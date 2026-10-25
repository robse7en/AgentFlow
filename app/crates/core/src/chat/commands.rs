//! Read-only commands and explicit offers for answers typed in task threads.
use super::{actions, details, models::*, render, rich, service::ChatService};
use crate::{config::SlackDetail, engine::rules};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn no_task(key: &str) -> String {
    let safe: String = key.chars().filter(|c| c.is_ascii_alphanumeric() || "_-".contains(*c)).take(64).collect();
    format!("No AgentFlow task {}.", safe.to_uppercase())
}
pub fn help(command: Option<&str>) -> String {
    format!(
        "`list`: active tasks and what needs you\n`status KEY`: one task's phase and pending actions\n`details KEY [requirements|findings|errors|cost|activity|pr|ticket]`: task details, as allowed by slack.details\n`activity KEY [N]`: the task's last N events\n`digest`: every active task, what is waiting on you and today's spend\n`new [KEY]` (or \"create a new task\"): create a task\n`run [KEY] [STAGE]`, `changes [KEY]`, `approve [KEY]`, `cancel [KEY]`, `pr [KEY]`: a button to start a stage, request requirement changes, review and approve the requirements, cancel the task, or mark its PR created or merged\n(`new` and those five need slack.allowActions.)\n`ask QUESTION`: a plain-words answer from AgentFlow's data, if slack.ask is enabled\nType these in your DM with AgentFlow{}. In a task's thread the key is optional, and other text can be sent as the answer to the agent's open question.",
        command.map(|c| format!(", or after `{c}`")).unwrap_or_default()
    )
}
pub fn next_digest_at(now: chrono::NaiveDateTime, at: &str) -> Option<chrono::NaiveDateTime> {
    let time = chrono::NaiveTime::parse_from_str(at, "%H:%M").ok()?;
    let today = now.date().and_time(time);
    Some(if today > now { today } else { today + chrono::Duration::days(1) })
}
const ACTIONS: [&str; 5] = ["run", "changes", "approve", "cancel", "pr"];
impl ChatService {
    pub async fn command(self: &Arc<Self>, user: &str, text: &str) -> Reply {
        if !self.authorized(user) {
            return Reply::text("You are not authorized to use AgentFlow.");
        }
        let reply = self.command_reply(text, None, None).await;
        if reply.buttons.is_empty() {
            return reply;
        }
        if self.post(None, reply).await.is_err() {
            return Reply::text("AgentFlow could not post the button to your direct messages; try again.");
        }
        Reply::text("The button is in your direct messages with AgentFlow.")
    }
    pub async fn message(self: &Arc<Self>, request: &MessageRequest) -> Option<Reply> {
        if !self.authorized(&request.user_id) || self.dm().as_deref() != Some(&request.channel) {
            return None;
        }
        let key = request.thread_id.as_deref().and_then(|t| self.thread_task(t));
        let words: Vec<_> = request.text.split_whitespace().collect();
        let command = words.first().map(|w| w.to_lowercase()).unwrap_or_default();
        if command == "new" && (words.len() == 1 || words.len() == 2 && super::creation::jira_key(words[1])) {
            return Some(self.new_task_reply(words.get(1).copied()));
        }
        let offer = key.as_deref().and_then(|key| self.offer_answer(key, &request.text));
        let pattern = regex::Regex::new(r"(?i)^(?:(?:please|can you|could you)\s+)?(?:new|(?:create|make|start|open|add)(?:\s+(?:me\s+)?an?)?(?:\s+new)?)\s+task(?:\s+(?:for\s+)?([A-Za-z][A-Za-z0-9_]*-\d+))?(?:,?\s+please)?\s*[.!?]*$").unwrap();
        let mut reply = if let Some(found) = pattern.captures(request.text.trim()) {
            self.new_task_reply(found.get(1).map(|m| m.as_str()))
        } else if ACTIONS.contains(&command.as_str())
            || ["list", "status", "details", "activity", "digest", "new", "ask", "help"].contains(&command.as_str())
        {
            self.command_reply(&request.text, key.as_deref(), request.thread_id.as_deref()).await
        } else if let Some(offer) = offer {
            return Some(offer);
        } else if self.settings.ask.enabled && (words.len() > 1 || request.text.trim_end().ends_with('?')) {
            return Some(self.ask_reply(&request.text, key.as_deref(), request.thread_id.as_deref()).await);
        } else {
            Reply::text(help(self.settings.command.as_deref()))
        };
        if command != "ask" && (words.len() > 1 || ACTIONS.contains(&command.as_str())) {
            reply.offer = offer.and_then(|r| r.offer);
        }
        Some(reply)
    }
    fn offer_answer(&self, key: &str, text: &str) -> Option<Reply> {
        if !self.settings.answering() {
            return None;
        }
        let snapshot = self.snapshot(key)?;
        let pending: Vec<_> = snapshot["questions"].as_array()?.iter().filter(|q| q["actionable"] == true).collect();
        if pending.is_empty() {
            return None;
        }
        if pending.len() > 1 {
            return Some(Reply::text(format!(
                "{key} has {} open questions; use Reply… on the one you mean.",
                pending.len()
            )));
        }
        match crate::engine::agents::clean_answer(text) {
            Ok(text) => Some(Reply {
                text: format!("{key} is waiting for your answer."),
                offer: Some(AnswerOffer {
                    jira_key: key.into(),
                    question_uid: actions::s(pending[0], "uid").into(),
                    text,
                }),
                buttons: vec![],
            }),
            Err(reason) => Some(Reply::text(format!("Not offered as an answer: {reason}."))),
        }
    }
    async fn command_reply(&self, text: &str, key: Option<&str>, thread: Option<&str>) -> Reply {
        let words: Vec<_> = text.split_whitespace().collect();
        let command = words.first().map(|w| w.to_lowercase()).unwrap_or_else(|| "help".into());
        let args = words.get(1..).unwrap_or_default();
        if ACTIONS.contains(&command.as_str()) {
            return self.action_reply(&command, args, key);
        }
        if command == "ask" {
            return self.ask_reply(text.trim().get(3..).unwrap_or(""), key, thread).await;
        }
        let result = match command.as_str() {
            "list" => render::render_list(&self.engine.list_tasks("exclude").await.unwrap_or_default()),
            "status" if args.is_empty() && key.is_none() => {
                render::render_list(&self.engine.list_tasks("exclude").await.unwrap_or_default())
            }
            "status" => {
                let target = args.first().copied().or(key).unwrap_or("");
                self.snapshot(target)
                    .map(|s| render::render_status(&s, &self.task_url(actions::s(&s, "jiraKey"))))
                    .unwrap_or_else(|| no_task(target))
            }
            "details" | "activity" => self.details_reply(&command, args, key),
            "digest" => self.digest().await,
            "new" => "Type `new` in your direct messages with AgentFlow to create a task.".into(),
            _ => help(self.settings.command.as_deref()),
        };
        Reply::text(result)
    }
    pub(crate) fn allowed_details(&self) -> Vec<String> {
        self.settings.details.iter().filter_map(|d| serde_json::to_value(d).ok()?.as_str().map(str::to_owned)).collect()
    }
    pub(crate) fn with_ticket(&self, mut snapshot: Value, wanted: &[String]) -> Value {
        if !self.settings.details.contains(&SlackDetail::Ticket) || !wanted.iter().any(|s| s == "ticket") {
            return snapshot;
        }
        let issue = self
            .engine
            .db
            .session(|tx| {
                crate::db::table::select_one::<crate::db::TaskRow>(
                    tx,
                    "WHERE jira_key = ?1",
                    [actions::s(&snapshot, "jiraKey")],
                )
            })
            .ok()
            .flatten()
            .and_then(|t| crate::engine::snapshot::workspace(&t).jira_issue().ok().flatten());
        snapshot["jiraIssue"] = issue.unwrap_or(Value::Null);
        snapshot
    }
    fn details_reply(&self, command: &str, args: &[&str], key: Option<&str>) -> String {
        let allowed = self.allowed_details();
        if allowed.is_empty() {
            return "Task details are off. Choose what may be posted with slack.details in config.json.".into();
        }
        let mut sections: Vec<String> = if command == "activity" { vec!["activity".into()] } else { vec![] };
        let mut target = key;
        let mut limit = 10;
        for arg in args {
            let lower = arg.to_lowercase();
            if details::SECTIONS.contains(&lower.as_str()) {
                sections.push(lower);
            } else if command == "activity" && arg.bytes().all(|b| b.is_ascii_digit()) {
                limit = arg.parse().unwrap_or(30);
            } else {
                target = Some(arg);
            }
        }
        let Some(target) = target else {
            return format!("Which task? Use `{command} KEY`, or ask in the task's thread.");
        };
        let refused: Vec<_> = sections.iter().filter(|s| !allowed.contains(s)).cloned().collect();
        if !refused.is_empty() {
            return format!("Not enabled in slack.details: {}.", refused.join(", "));
        }
        let Some(snapshot) = self.snapshot(target) else {
            return no_task(target);
        };
        let snapshot = self.with_ticket(snapshot, if sections.is_empty() { &allowed } else { &sections });
        rich::render_details(
            &snapshot,
            &details::task_details(&snapshot, &allowed, (!sections.is_empty()).then_some(sections.as_slice()), limit),
            &self.task_url(actions::s(&snapshot, "jiraKey")),
        )
    }
    async fn digest(&self) -> String {
        let tasks = self.engine.list_tasks("exclude").await.unwrap_or_default();
        let spend = if self.settings.details.contains(&SlackDetail::Cost) {
            self.engine.db.session(|tx| rules::load_spending(tx, -1, rules::budget_day())).ok().map(|rows| {
                rules::budget_status(&rows.spending(), None, &self.engine.config().workflow.cost_controls)["daily"]
                    .clone()
            })
        } else {
            None
        };
        rich::digest(&tasks, spend.as_ref())
    }
    /// One scheduled delivery; failures are logged and do not interrupt the service.
    pub async fn post_daily_digest(&self) {
        if self.dm().is_none()
            || !self
                .engine
                .list_tasks("exclude")
                .await
                .is_ok_and(|tasks| tasks.iter().any(|t| !matches!(actions::s(t, "status"), "MERGED" | "CANCELLED")))
        {
            return;
        }
        if self.post(None, Reply::text(self.digest().await)).await.is_err() {
            tracing::warn!("Daily slack digest failed");
        }
    }
    pub(crate) async fn digest_loop(&self) {
        self.run_digest_schedule(|| chrono::Local::now().naive_local(), std::time::Duration::from_secs(60)).await;
    }
    /// Clock seam for deterministic scheduling tests; production uses local wall time.
    pub async fn run_digest_schedule(&self, now: impl Fn() -> chrono::NaiveDateTime, poll: std::time::Duration) {
        let Some(at) = self.settings.digest_at.as_deref() else {
            return;
        };
        loop {
            let Some(due) = next_digest_at(now(), at) else {
                return;
            };
            loop {
                let left = (due - now()).num_milliseconds();
                if left <= 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(left as u64).min(poll)).await;
            }
            self.post_daily_digest().await;
        }
    }
    /// Build model data using the same allowlist as chat views, without local paths or hidden text.
    pub async fn ask_context(&self, question: &str, key: Option<&str>) -> Value {
        let allowed = self.allowed_details();
        let tasks = self.engine.list_tasks("exclude").await.unwrap_or_default();
        let mut context =
            json!({"allowedDetails":allowed,"tasks":tasks.iter().take(50).map(brief).collect::<Vec<_>>()});
        let mentioned = regex::Regex::new(r"[A-Za-z][A-Za-z0-9_]*-\d+").unwrap();
        let target = key.map(str::to_owned).or_else(|| {
            mentioned.find_iter(question).map(|m| m.as_str().to_uppercase()).find(|k| self.snapshot(k).is_some())
        });
        if let Some(snapshot) = target.and_then(|key| self.snapshot(&key)) {
            let snapshot = self.with_ticket(snapshot, &allowed);
            let mut task = brief(&snapshot);
            task["stages"] = snapshot["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|n| json!({"stage":n["label"],"state":n["state"]}))
                .collect::<Vec<_>>()
                .into();
            task["openAgentQuestions"] = if self.settings.include_question_text {
                snapshot["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|q| q["actionable"] == true)
                    .map(|q| json!({"stage":q["nodeKey"],"question":q["message"],"choices":q["choices"]}))
                    .collect::<Vec<_>>()
                    .into()
            } else {
                "not shared".into()
            };
            task["details"] = details::task_details(&snapshot, &allowed, None, 10);
            context["task"] = task;
        }
        context
    }
}
fn brief(task: &Value) -> Value {
    let mut value: serde_json::Map<String, Value> =
        ["jiraKey", "jiraTitle", "status", "phase", "createdAt", "updatedAt"]
            .iter()
            .map(|&k| (k.into(), task[k].clone()))
            .collect();
    value.insert(
        "needsYou".into(),
        task["attention"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| json!({"stage":a["label"],"action":a["action"],"since":a["since"],"questions":a["questions"]}))
            .collect::<Vec<_>>()
            .into(),
    );
    value.into()
}
