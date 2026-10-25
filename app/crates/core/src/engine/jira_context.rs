//! Best-effort Jira context, title retrieval and human-triggered status transitions.
use super::{Engine, store::TaskState};
use crate::jira::{JiraError, JiraService, JiraTransport, TicketSync};
use serde_json::json;
use std::{path::Path, sync::Arc};

impl Engine {
    pub fn set_jira_transport(&self, transport: Arc<dyn JiraTransport>) {
        *self.jira_transport.write().unwrap_or_else(|p| p.into_inner()) = transport;
    }
    pub fn jira_service(&self) -> Result<Option<JiraService>, JiraError> {
        let config = self.config().jira.clone();
        if !config.missing().is_empty() {
            return Ok(None);
        }
        JiraService::with_transport(config, self.jira_transport.read().unwrap_or_else(|p| p.into_inner()).clone())
            .map(Some)
    }
    pub(crate) async fn sync_jira_context(&self, state: &mut TaskState) {
        if super::lifecycle::is_local_task(&state.task.jira_key) {
            return;
        }
        let result = match self.jira_service() {
            Ok(Some(service)) => {
                service.sync_ticket(&state.task.jira_key, &Path::new(&state.task.task_dir).join("context"), false).await
            }
            Ok(None) => return,
            Err(error) => Err(error),
        };
        match result {
            Ok(result) => self.log_jira_sync(state, &result),
            Err(error) => state.log(format!("Could not sync Jira ticket context: {error}"), None, "warning", None),
        }
    }
    pub(crate) fn log_jira_sync(&self, state: &mut TaskState, result: &TicketSync) {
        let problems = !result.skipped.is_empty() || !result.frame_errors.is_empty() || result.comments_error.is_some();
        state.log(format!("Jira context synced: {}", result.summary()), None, if problems { "warning" } else { "info" },
            Some(json!({"added": result.added, "removed": result.removed,
                "skipped": result.skipped.iter().map(|s| json!({"id":s.id,"file":s.filename,"reason":s.reason})).collect::<Vec<_>>(),
                "frames":result.frames,"frameErrors":result.frame_errors})));
    }
    pub(crate) async fn transition_jira(&self, state: &mut TaskState) {
        let config = self.config();
        let Some(target) = config.jira.auto_transition_status.as_deref().filter(|s| !s.is_empty()) else { return };
        if super::lifecycle::is_local_task(&state.task.jira_key) {
            return;
        }
        let result = match self.jira_service() {
            Ok(Some(service)) => {
                service.transition_to(&state.task.jira_key, target, &config.jira.auto_transition_from_statuses).await
            }
            Ok(None) => return,
            Err(error) => Err(error),
        };
        match result {
            Ok(true) => state.info(format!("Jira ticket transitioned to {target}")),
            Ok(false) => {}
            Err(error) => state.log(format!("Could not update Jira status: {error}"), None, "warning", None),
        }
    }
    pub(crate) fn queue_jira_title(&self, key: &str) {
        if super::lifecycle::is_local_task(key) {
            return;
        }
        let Ok(Some(service)) = self.jira_service() else { return };
        let mut pending = self.jira_titles.lock().unwrap_or_else(|p| p.into_inner());
        if pending.get(key).is_some_and(|h| !h.is_finished()) {
            return;
        }
        let key = key.to_string();
        let task_key = key.clone();
        let weak = self.me.clone();
        pending.insert(
            key,
            tokio::spawn(async move {
                let Ok(issue) = service.fetch_issue(&task_key).await else { return };
                let Some(title) = issue["fields"]["summary"].as_str().filter(|s| !s.is_empty()) else { return };
                let Some(engine) = weak.upgrade() else { return };
                let lock = engine.lock(&task_key);
                let _held = lock.lock().await;
                let changed = engine.db.session(|tx| -> Result<bool, super::WorkflowError> {
                    let mut state = TaskState::load(tx, &task_key)?;
                    if state.task.jira_title.is_some() {
                        return Ok(false);
                    }
                    state.task.jira_title = Some(title.into());
                    state.flush(tx)?;
                    Ok(true)
                });
                if matches!(changed, Ok(true)) {
                    let _ = engine.publish(&task_key);
                }
            }),
        );
    }
}
