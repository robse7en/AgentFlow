//! Fast form validation precedes acknowledgement; filesystem/Git work runs afterwards.
use super::{actions, models::*, render, service::ChatService};
use crate::{
    config::SlackAction,
    db::{TaskRow, table},
    engine::{lifecycle::CreateTaskRequest, pr_tracking::clean_pr_url},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

pub fn jira_key(value: &str) -> bool {
    regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_]*-\d+$").unwrap().is_match(value)
}
impl ChatService {
    fn repositories(&self) -> Vec<String> {
        let mut paths = self.engine.config().workspace.default_repository_directory.iter().cloned().collect::<Vec<_>>();
        if let Ok(used) = self.engine.db.session(|tx| {
            let mut query = tx.prepare(
                "select t.repository_path from tasks t group by t.repository_path order by max(t.created_at) desc",
            )?;
            query.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()
        }) {
            for path in used {
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        paths
    }
    pub fn new_task_reply(&self, key: Option<&str>) -> Reply {
        if !self.settings.allows(SlackAction::Create) {
            return Reply::text(self.off("Creating tasks", "create"));
        }
        let key = key.filter(|k| jira_key(k)).unwrap_or("").to_uppercase();
        Reply {
            text: "Fill in the form to create a task.".into(),
            buttons: vec![actions::button(
                "new_task",
                &if key.is_empty() { "Create a task".into() } else { format!("Create {key}") },
                &key,
                None,
            )],
            offer: None,
        }
    }
    pub fn new_task_form(&self, user: &str, key: Option<&str>) -> Result<TaskForm, String> {
        if !self.authorized(user) {
            return Err("You are not authorized to use AgentFlow.".into());
        }
        if !self.settings.allows(SlackAction::Create) {
            return Err(self.off("Creating tasks", "create"));
        }
        let config = self.engine.config();
        Ok(TaskForm {
            jira_key: key.map(str::trim).filter(|k| jira_key(k)).map(str::to_uppercase),
            repository: self.repositories().into_iter().next(),
            search_tickets: self.engine.jira_service().ok().flatten().is_some(),
            implementation_mode: serde_json::to_value(config.workflow.implementation_mode)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            base_branch: "main".into(),
        })
    }
    pub async fn options(&self, request: &OptionsRequest) -> Vec<(String, String)> {
        if !self.authorized(&request.user_id) || !self.settings.allows(SlackAction::Create) {
            return vec![];
        }
        let query = request.query.trim();
        match request.field.as_str() {
            "ticket" => {
                let mut options = vec![];
                if !query.is_empty()
                    && let Ok(Some(jira)) = self.engine.jira_service()
                    && let Ok(Ok(found)) =
                        tokio::time::timeout(Duration::from_millis(2400), jira.search_issues(query, 100)).await
                {
                    options = found
                        .iter()
                        .filter_map(|f| {
                            Some((
                                f["key"].as_str()?.into(),
                                format!("{} · {}", f["key"].as_str()?, f["summary"].as_str().unwrap_or("")),
                            ))
                        })
                        .collect();
                }
                if jira_key(query) && !options.iter().any(|(k, _)| *k == query.to_uppercase()) {
                    options.insert(0, (query.to_uppercase(), format!("{} (as typed)", query.to_uppercase())));
                }
                options.truncate(100);
                options
            }
            "repository" => {
                let mut found: Vec<_> = self
                    .repositories()
                    .into_iter()
                    .filter(|p| p.to_lowercase().contains(&query.to_lowercase()))
                    .map(|p| (p.clone(), p))
                    .collect();
                if query.contains(['/', '\\']) && !found.iter().any(|(p, _)| p == query) {
                    found.insert(0, (query.into(), format!("{query} (as typed)")));
                }
                found.truncate(100);
                found
            }
            "branch" | "project" => self.path_options(request, query).await,
            _ => vec![],
        }
    }
    async fn path_options(&self, request: &OptionsRequest, query: &str) -> Vec<(String, String)> {
        let Some(repository) = request.repository.as_deref().filter(|s| !s.is_empty()) else {
            return vec![];
        };
        let project = request.field == "project";
        let cache = if project { &self.folders } else { &self.branches };
        let cached = cache
            .lock()
            .unwrap()
            .get(repository)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(60))
            .map(|(_, v)| v.clone());
        let values = if let Some(values) = cached {
            values
        } else {
            let lookup = async {
                let info = self.engine.git.inspect_repository(repository).await?;
                if project { self.engine.git.directories(Path::new(&info.root)).await } else { Ok(info.branches) }
            };
            let (values, succeeded) = match tokio::time::timeout(Duration::from_millis(2400), lookup).await {
                Ok(Ok(values)) => (values, true),
                _ => (vec![], false),
            };
            let values: Vec<_> = values
                .into_iter()
                .filter(|f| {
                    !project || !f.split('/').any(|p| p.starts_with('.') || p.eq_ignore_ascii_case("node_modules"))
                })
                .collect();
            if succeeded {
                cache.lock().unwrap().insert(repository.into(), (Instant::now(), values.clone()));
            }
            values
        };
        let query = if project { query.replace('\\', "/").trim_matches('/').to_string() } else { query.into() };
        let mut found: Vec<_> =
            values.iter().filter(|b| b.to_lowercase().contains(&query.to_lowercase())).cloned().collect();
        found.sort_by_key(|b| {
            (
                !b.to_lowercase().starts_with(&query.to_lowercase()),
                if project { b.matches('/').count() } else { 0 },
                b.clone(),
            )
        });
        let typed = !query.is_empty() && !values.contains(&query);
        let mut options =
            if project && query.is_empty() { vec![(".".into(), "Whole repository".into())] } else { vec![] };
        options.extend(found.into_iter().take(100 - options.len() - usize::from(typed)).map(|b| (b.clone(), b)));
        if typed {
            options.push((query.clone(), format!("{query} (as typed)")));
        }
        options
    }
    pub fn submit_task(self: &Arc<Self>, draft: TaskDraft) -> Value {
        if !self.authorized(&draft.user_id) {
            return json!({"ticket":"You are not authorized to use AgentFlow."});
        }
        if !self.settings.allows(SlackAction::Create) {
            return json!({"ticket":self.off("Creating tasks","create")});
        }
        let mut errors = serde_json::Map::new();
        if draft.repository_path.trim().is_empty() {
            errors.insert("repository".into(), "Choose a repository.".into());
        }
        if draft.base_branch.trim().is_empty() {
            errors.insert("branch".into(), "Choose a base branch.".into());
        }
        if !errors.is_empty() {
            return errors.into();
        }
        let request = CreateTaskRequest {
            task_source: "jira".into(),
            prompt: None,
            jira_key: draft.jira_key,
            jira_title: None,
            repository_path: draft.repository_path.trim().into(),
            base_branch: draft.base_branch.trim().into(),
            project_relative_path: draft.project_relative_path.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
            implementation_mode: draft.implementation_mode,
            additional_context: draft.additional_context.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
            agent_overrides: Default::default(),
            project_id: None,
        };
        let request = match request.validated() {
            Ok(request) => request,
            Err(error) => {
                let (field, reason) = error.split_once(':').unwrap_or(("ticket", &error));
                let field = match field {
                    "repositoryPath" => "repository",
                    "baseBranch" => "branch",
                    "projectRelativePath" => "project",
                    _ => "ticket",
                };
                return json!({field:reason.trim()});
            }
        };
        if let Ok(Some(task)) =
            self.engine.db.session(|tx| table::select_one::<TaskRow>(tx, "WHERE jira_key = ?1", [&request.jira_key]))
        {
            return json!({"ticket":if task.archived_at.is_some() { format!("A task for {} exists but is archived. Unarchive or delete it in AgentFlow first.",request.jira_key) } else { format!("A task for {} already exists.",request.jira_key) }});
        }
        let service = self.clone();
        if !self.spawn(async move {
            service.create(request).await;
        }) {
            return json!({"ticket":"AgentFlow is busy; try again shortly."});
        }
        json!({})
    }
    async fn create(&self, request: CreateTaskRequest) {
        let key = request.jira_key.clone();
        match self.engine.create_task(request).await {
            Ok(_) => {
                self.created.lock().unwrap().insert(key.clone());
                self.queue(&key, None);
            }
            Err(_) => {
                let _ = self
                    .post(
                        None,
                        Reply::text(format!(
                            "*Could not create {}.*\nThe reason is in the AgentFlow backend log.",
                            render::escape(&key)
                        )),
                    )
                    .await;
            }
        }
    }
    pub fn text_form(&self, user: &str, kind: &str, key: &str) -> Result<TextForm, String> {
        if !self.authorized(user) {
            return Err("You are not authorized to use AgentFlow.".into());
        }
        let snapshot = self.snapshot(key).ok_or_else(|| "That task no longer exists.".to_owned())?;
        let key = actions::s(&snapshot, "jiraKey");
        let (action, setting, what) = if kind == "request_changes" {
            (SlackAction::RequestChanges, "requestChanges", "Requesting changes")
        } else if kind == "mark_pr" {
            (SlackAction::MarkPr, "markPr", "Marking pull requests")
        } else {
            return Err("That form is not available.".into());
        };
        if !self.settings.allows(action) {
            return Err(self.off(what, setting));
        }
        if kind == "request_changes" {
            if !actions::gate_waiting(&snapshot) {
                return Err(format!("{key}'s requirements are not waiting for approval."));
            }
            Ok(TextForm {
                kind: kind.into(),
                value: key.into(),
                title: "Request changes".into(),
                intro: format!("{key}: the requirements go back to the Architect with your feedback, to be revised."),
                label: "What should change?".into(),
                multiline: true,
                max_length: 4000,
            })
        } else {
            if snapshot["status"] != "READY_FOR_PR" {
                return Err(format!(
                    "{key} is {}; a PR can be recorded only when it is Ready for PR.",
                    actions::s(&snapshot, "status")
                ));
            }
            Ok(TextForm {
                kind: kind.into(),
                value: key.into(),
                title: "Mark PR created".into(),
                intro: format!("Record the pull request for {key}."),
                label: "Pull request URL".into(),
                multiline: false,
                max_length: 2000,
            })
        }
    }
    pub fn submit_text(self: &Arc<Self>, submission: TextSubmission) -> Value {
        let form = match self.text_form(&submission.user_id, &submission.kind, &submission.value) {
            Ok(form) => form,
            Err(reason) => return json!({"text":reason}),
        };
        let text = submission.text.trim();
        if text.is_empty() && form.kind == "request_changes" {
            return json!({"text":"Feedback cannot be empty."});
        }
        if text.chars().count() > form.max_length {
            return json!({"text":format!("Text must be at most {} characters",form.max_length)});
        }
        let text = if form.kind == "mark_pr" {
            match clean_pr_url(text) {
                Ok(url) => url,
                Err(error) => return json!({"text":error}),
            }
        } else {
            text.to_owned()
        };
        let service = self.clone();
        if !self.spawn(async move {
            let result = if form.kind == "request_changes" {
                service.engine.request_requirements_changes(&form.value, &text).await
            } else {
                service.engine.mark_pr_created(&form.value, &text).await
            };
            let reply = match result {
                Ok(snapshot) => Reply {
                    text: if form.kind == "request_changes" {
                        "Requirements sent back to the Architect.".into()
                    } else {
                        "Pull request recorded.".into()
                    },
                    offer: None,
                    buttons: if form.kind == "request_changes" {
                        actions::run_buttons(&snapshot, &service.settings, service.transition().as_deref())
                            .into_iter()
                            .filter(|b| b.value.ends_with("|architect"))
                            .collect()
                    } else {
                        vec![]
                    },
                },
                Err(e) => Reply::text(render::escape(&actions::refusal(e, "Not done"))),
            };
            let _ = service.post(Some(&form.value), reply).await;
        }) {
            return json!({"text":"AgentFlow is busy; try again shortly."});
        }
        json!({})
    }
}
