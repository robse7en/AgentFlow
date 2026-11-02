//! Task routes, ported from `backend/agentflow/api/tasks.py` (in slices with the engine).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{ApiError, ApiResult, App, Body422};
use crate::engine::agents;
use crate::engine::launch::RunNodeRequest;
use crate::engine::lifecycle::CreateTaskRequest;
use crate::terminal::PaneEvent;

#[derive(Deserialize)]
pub struct ListQuery {
    archived: Option<String>,
}

#[derive(Deserialize)]
pub struct JiraSearchQuery {
    query: String,
}
pub async fn jira_search(State(app): State<Arc<App>>, Query(query): Query<JiraSearchQuery>) -> ApiResult {
    if query.query.is_empty() || query.query.chars().count() > 200 {
        return Err(ApiError::new(422, "query: must be 1 to 200 characters"));
    }
    let service = app
        .engine
        .jira_service()
        .map_err(|e| ApiError::new(502, e.to_string()))?
        .ok_or_else(|| ApiError::new(400, "Jira integration is not configured (see jira.* in config.json)"))?;
    let results = service.search_issues(&query.query, 20).await.map_err(|e| ApiError::new(502, e.to_string()))?;
    Ok(Json(json!(results)))
}

#[derive(Deserialize)]
pub struct TaskBudget {
    amount: Option<f64>,
}

#[derive(Deserialize)]
pub struct MarkPrCreated {
    #[serde(rename = "prUrl")]
    pr_url: String,
}
pub async fn mark_pr_created(
    State(app): State<Arc<App>>,
    Path(key): Path<String>,
    Body422(body): Body422<MarkPrCreated>,
) -> ApiResult {
    let url = crate::engine::pr_tracking::clean_pr_url(&body.pr_url).map_err(|e| ApiError::new(422, e))?;
    Ok(Json(app.engine.mark_pr_created(&key, &url).await?))
}
pub async fn revert_pr_created(State(app): State<Arc<App>>, Path(key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.revert_pr_created(&key).await?))
}
pub async fn mark_pr_merged(State(app): State<Arc<App>>, Path(key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.mark_pr_merged(&key).await?))
}
pub async fn revert_pr_merged(State(app): State<Arc<App>>, Path(key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.revert_pr_merged(&key).await?))
}
/// `POST /api/tasks/{key}/pr/open`: push the branch; answers `{url, task}`, `url` being GitHub's
/// new-PR form already targeting the task's base branch.
pub async fn open_pr(State(app): State<Arc<App>>, Path(key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.open_pr(&key).await?))
}
pub async fn retarget_pr(State(app): State<Arc<App>>, Path(key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.retarget_pr(&key).await?))
}

pub async fn set_task_budget(
    State(app): State<Arc<App>>,
    Path(key): Path<String>,
    Body422(body): Body422<TaskBudget>,
) -> ApiResult {
    if body.amount.is_some_and(|n| !n.is_finite() || n <= 0.0) {
        return Err(ApiError::new(422, "amount: must be positive and finite"));
    }
    Ok(Json(app.engine.set_task_budget(&key, body.amount).await?))
}

pub async fn import_usage(
    State(app): State<Arc<App>>,
    Path((key, run)): Path<(String, i64)>,
    Body422(mut body): Body422<serde_json::Map<String, Value>>,
) -> ApiResult {
    let billing = body.remove("billingMode");
    let billing = match billing {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if ["metered", "subscription", "unknown"].contains(&s.as_str()) => Some(s),
        _ => return Err(ApiError::new(422, "billingMode: must be metered, subscription or unknown")),
    };
    let report = crate::engine::usage::UsageReport::validate(&Value::Object(body))
        .map_err(|e| ApiError::new(422, format!("{e:?}")))?;
    if report.scope != "run" {
        return Err(ApiError::new(422, "Import usage for this run only, not cumulative session totals"));
    }
    Ok(Json(app.engine.import_usage(&key, run, &report, billing.as_deref()).await?))
}

pub async fn list_tasks(State(app): State<Arc<App>>, Query(query): Query<ListQuery>) -> ApiResult {
    let archived = query.archived.as_deref().unwrap_or("exclude");
    if !["exclude", "include", "only"].contains(&archived) {
        return Err(ApiError::new(422, "archived: must be exclude, include or only"));
    }
    Ok(Json(Value::Array(app.engine.list_tasks(archived).await?)))
}

pub async fn create_task(
    State(app): State<Arc<App>>,
    Body422(request): Body422<CreateTaskRequest>,
) -> Result<Response, ApiError> {
    let request = request.validated().map_err(|problem| ApiError::new(422, problem))?;
    let snapshot = app.engine.create_task(request).await?;
    Ok((StatusCode::CREATED, Json(snapshot)).into_response())
}

#[derive(Deserialize)]
pub struct ValidateRepository {
    path: String,
    #[serde(rename = "baseBranch")]
    base_branch: Option<String>,
    #[serde(rename = "projectRelativePath")]
    project_relative_path: Option<String>,
}

#[derive(Deserialize)]
pub struct CostEstimate {
    #[serde(rename = "implementationMode")]
    implementation_mode: Option<crate::config::ImplementationMode>,
    #[serde(rename = "agentOverrides", default)]
    agent_overrides: indexmap::IndexMap<String, crate::engine::lifecycle::AgentOverride>,
}

/// What a new task would cost on a route, from past runs (display only).
pub async fn cost_estimate(State(app): State<Arc<App>>, Body422(body): Body422<CostEstimate>) -> ApiResult {
    let mode = body.implementation_mode.map(|m| json!(m).as_str().unwrap_or_default().to_string());
    Ok(Json(app.engine.cost_estimate(mode.as_deref(), &body.agent_overrides)?))
}

/// Any failure (not a repository, git missing) becomes an actionable check, not an error status.
pub async fn validate_repository(
    State(app): State<Arc<App>>,
    Body422(body): Body422<ValidateRepository>,
) -> Json<Value> {
    let result = app
        .engine
        .validate_repository(&body.path, body.base_branch.as_deref(), body.project_relative_path.as_deref())
        .await;
    Json(result.unwrap_or_else(
        |error| json!({"ok": false, "checks": [{"ok": false, "message": error.to_string()}], "branches": []}),
    ))
}

pub async fn get_task(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.snapshot(&jira_key)?))
}

pub async fn approve_node(
    State(app): State<Arc<App>>,
    Path((key, node)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let request = if body.iter().all(u8::is_ascii_whitespace) {
        crate::engine::approvals::ApprovalRequest::default()
    } else {
        let value: Value = serde_json::from_slice(&body).map_err(|e| ApiError::new(422, e.to_string()))?;
        if value.is_null() {
            crate::engine::approvals::ApprovalRequest::default()
        } else {
            serde_json::from_value(value).map_err(|e| ApiError::new(422, e.to_string()))?
        }
    };
    validate_approval(&request)?;
    Ok(Json(app.engine.approve(&key, &node, request).await?))
}

fn validate_approval(request: &crate::engine::approvals::ApprovalRequest) -> Result<(), ApiError> {
    if request.reason.as_ref().is_some_and(|s| s.chars().count() > 4000) {
        return Err(ApiError::new(422, "reason: must be at most 4000 characters"));
    }
    if request.expected_requirements_digest.as_ref().is_some_and(|s| s.len() != 64) {
        return Err(ApiError::new(422, "expectedRequirementsDigest: must be 64 characters"));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct RequirementsFeedback {
    feedback: String,
}

pub async fn request_changes(
    State(app): State<Arc<App>>,
    Path((key, node)): Path<(String, String)>,
    Body422(body): Body422<RequirementsFeedback>,
) -> ApiResult {
    if node != crate::engine::spec::REQUIREMENTS_GATE {
        return Err(ApiError::new(404, "Requesting changes is only supported for the requirements gate"));
    }
    if !(1..=4000).contains(&body.feedback.chars().count()) {
        return Err(ApiError::new(422, "feedback: must be 1 to 4000 characters"));
    }
    Ok(Json(app.engine.request_requirements_changes(&key, &body.feedback).await?))
}

pub async fn override_reviews(
    State(app): State<Arc<App>>,
    Path(key): Path<String>,
    Body422(body): Body422<crate::engine::approvals::ApprovalRequest>,
) -> ApiResult {
    validate_approval(&body)?;
    Ok(Json(
        app.engine
            .override_reviews(&key, body.reason.as_deref().unwrap_or(""), body.expected_sha.as_deref().unwrap_or(""))
            .await?,
    ))
}

pub async fn discard_worktree_drift(
    State(app): State<Arc<App>>,
    Path(key): Path<String>,
    Body422(body): Body422<WorktreeCleanup>,
) -> ApiResult {
    if !(40..=64).contains(&body.expected_sha.len()) {
        return Err(ApiError::new(422, "expectedSha: must be 40 to 64 characters"));
    }
    Ok(Json(app.engine.discard_worktree_drift(&key, &body.expected_sha).await?))
}

pub async fn retry_worktree_setup(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.retry_worktree_setup(&jira_key).await?))
}

pub async fn cancel_task(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.cancel_task(&jira_key).await?))
}

pub async fn archive_task(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.archive_task(&jira_key).await?))
}

pub async fn unarchive_task(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.unarchive_task(&jira_key).await?))
}

pub async fn delete_preview(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.delete_preview(&jira_key).await?))
}

#[derive(Deserialize)]
pub struct DeleteTask {
    #[serde(rename = "removeWorktree")]
    remove_worktree: bool,
    #[serde(rename = "removeBranch")]
    remove_branch: bool,
    #[serde(rename = "removeTaskFolder")]
    remove_task_folder: bool,
    /// The branch tip the preview showed; required to remove the worktree or branch.
    #[serde(rename = "expectedSha")]
    expected_sha: Option<String>,
}

pub async fn delete_task(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<DeleteTask>,
) -> ApiResult {
    if body.expected_sha.as_ref().is_some_and(|s| !(40..=64).contains(&s.len())) {
        return Err(ApiError::new(422, "expectedSha: must be 40 to 64 characters"));
    }
    let result = app
        .engine
        .delete_task(
            &jira_key,
            body.remove_worktree,
            body.remove_branch,
            body.remove_task_folder,
            body.expected_sha.as_deref(),
        )
        .await?;
    Ok(Json(result))
}

pub async fn worktree_cleanup_preview(State(app): State<Arc<App>>, Path(jira_key): Path<String>) -> ApiResult {
    Ok(Json(app.engine.worktree_cleanup_preview(&jira_key).await?))
}

/// `GET /api/tasks/{key}/findings/{id}/code`: the code a review finding points at.
pub async fn finding_code(State(app): State<Arc<App>>, Path((jira_key, finding_id)): Path<(String, i64)>) -> ApiResult {
    Ok(Json(app.engine.finding_code(&jira_key, finding_id).await?))
}

#[derive(Deserialize)]
pub struct WorktreeCleanup {
    #[serde(rename = "expectedSha")]
    expected_sha: String,
    /// Skip the reviewed-or-merged branch check (the human says it was merged elsewhere).
    #[serde(default)]
    force: bool,
}

pub async fn cleanup_worktree(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<WorktreeCleanup>,
) -> ApiResult {
    if !(40..=64).contains(&body.expected_sha.len()) {
        return Err(ApiError::new(422, "expectedSha: must be 40 to 64 characters"));
    }
    Ok(Json(app.engine.cleanup_worktree(&jira_key, &body.expected_sha, body.force).await?))
}

/// `POST /api/tasks/{key}/nodes/{node}/run`. The body is optional.
pub async fn run_node(
    State(app): State<Arc<App>>,
    Path((jira_key, node_key)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let mut request: RunNodeRequest = if body.iter().all(u8::is_ascii_whitespace) {
        RunNodeRequest::default()
    } else {
        let value: Value =
            serde_json::from_slice(&body).map_err(|e| ApiError::new(422, format!("Invalid JSON body: {e}")))?;
        if value.is_null() {
            RunNodeRequest::default()
        } else {
            serde_path_to_error::deserialize(value)
                .map_err(|e| ApiError::new(422, format!("{}: {}", e.path(), e.inner())))?
        }
    };
    if let Some(approval) = request.metered_approval.take() {
        request.metered_approval = Some(approval.validated().map_err(|problem| ApiError::new(422, problem))?);
    }
    Ok(Json(app.engine.run_node(&jira_key, &node_key, request, false).await?))
}

#[derive(Deserialize)]
pub struct NodeQuery {
    node: Option<String>,
}

/// `POST /api/tasks/{key}/zellij/open?node=`: the embedded terminal view.
pub async fn open_zellij(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Query(query): Query<NodeQuery>,
) -> ApiResult {
    Ok(Json(app.engine.open_zellij(&jira_key, query.node.as_deref()).await?))
}

/// `POST /api/tasks/{key}/zellij/window?node=`: an interactive terminal window.
pub async fn open_zellij_window(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Query(query): Query<NodeQuery>,
) -> ApiResult {
    Ok(Json(app.engine.open_zellij_window(&jira_key, query.node.as_deref()).await?))
}

/// `POST /api/tasks/{key}/reveal/{target}`: the worktree or agent cwd in the file explorer.
pub async fn reveal_path(State(app): State<Arc<App>>, Path((jira_key, target)): Path<(String, String)>) -> ApiResult {
    Ok(Json(app.engine.reveal_path(&jira_key, &target).await?))
}

/// `PUT /api/tasks/{key}/nodes/{node}/config`: the node's own harness, model and reasoning.
pub async fn set_node_config(
    State(app): State<Arc<App>>,
    Path((jira_key, node_key)): Path<(String, String)>,
    Body422(body): Body422<crate::engine::lifecycle::AgentOverride>,
) -> ApiResult {
    Ok(Json(app.engine.set_node_override(&jira_key, &node_key, &body).await?))
}

#[derive(Deserialize)]
pub struct Recovery {
    #[serde(default)]
    retry: bool,
}

/// `POST /api/tasks/{key}/nodes/{node}/recover`: Resume (or, with `retry`, relaunch) an
/// interrupted node.
pub async fn recover_node(
    State(app): State<Arc<App>>,
    Path((jira_key, node_key)): Path<(String, String)>,
    Body422(body): Body422<Recovery>,
) -> ApiResult {
    Ok(Json(app.engine.recover_node(&jira_key, &node_key, body.retry).await?))
}

/// `POST /api/tasks/{key}/questions/{uid}/answer`: free text (`answer`) or a path of 0-based
/// choice indexes (`path`, branch then follow-up option), as `QuestionAnswerRequest` accepts them.
pub async fn answer_question(
    State(app): State<Arc<App>>,
    Path((jira_key, question_uid)): Path<(String, String)>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let uid = uuid::Uuid::parse_str(&question_uid)
        .map_err(|_| ApiError::new(422, "question_uid: Input should be a valid UUID"))?
        .to_string();
    let answer = match body.get("answer") {
        None | Some(Value::Null) => None,
        Some(Value::String(answer)) => {
            if !(1..=agents::MAX_ANSWER_LENGTH).contains(&answer.chars().count()) {
                return Err(ApiError::new(422, "answer: must be 1 to 4000 characters"));
            }
            Some(agents::clean_answer(answer).map_err(|problem| ApiError::new(422, problem))?)
        }
        Some(_) => return Err(ApiError::new(422, "answer: must be a string")),
    };
    let path = match body.get("path") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) if (1..=2).contains(&items.len()) => Some(
            items
                .iter()
                .map(|item| item.as_u64().filter(|_| !item.is_f64()).map(|index| index as usize))
                .collect::<Option<Vec<usize>>>()
                .ok_or_else(|| ApiError::new(422, "path: must be 0-based choice indexes"))?,
        ),
        Some(_) => return Err(ApiError::new(422, "path: must list 1 or 2 choice indexes")),
    };
    match (answer, path) {
        (Some(answer), None) => Ok(Json(app.engine.answer_question(&jira_key, &uid, &answer, None).await?)),
        (None, Some(path)) => Ok(Json(app.engine.answer_question_path(&jira_key, &uid, &path, None).await?)),
        _ => Err(ApiError::new(422, "Send either answer or path")),
    }
}

#[derive(Deserialize)]
pub struct QuestionsQuery {
    open: Option<String>,
}

/// `GET /api/questions?open=true`: every question a human can answer now, across tasks.
pub async fn open_questions(State(app): State<Arc<App>>, Query(query): Query<QuestionsQuery>) -> ApiResult {
    // FastAPI's bool: true, 1, yes, on, t or y (any case) is true; anything else is refused here.
    let open = query.open.as_deref().map(str::to_ascii_lowercase);
    if open.is_some_and(|open| !matches!(open.as_str(), "true" | "1" | "yes" | "on" | "t" | "y")) {
        return Err(ApiError::new(422, "Only open questions are listed: use open=true"));
    }
    Ok(Json(Value::Array(app.engine.open_questions()?)))
}

/// `GET /api/tasks/{key}/terminal/{role}`: the agent's pane as an NDJSON stream of screens.
pub async fn stream_terminal(
    State(app): State<Arc<App>>,
    headers: axum::http::HeaderMap,
    Path((jira_key, role)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    use tokio_stream::StreamExt;
    let cookies = headers.get(axum::http::header::COOKIE).and_then(|c| c.to_str().ok());
    let (generation, mut human_session) = app
        .human
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .stream_access(cookies)
        .ok_or_else(|| ApiError::new(403, "Pairing required"))?;
    let (session, pane_id) = app.engine.terminal_target(&jira_key, &role).await?;
    let lines: std::pin::Pin<Box<dyn tokio_stream::Stream<Item = String> + Send>> =
        match app.engine.terminals.subscribe(&session, &pane_id) {
            Ok(events) => Box::pin(tokio_stream::wrappers::ReceiverStream::new(events).map(|event| event.to_ndjson())),
            Err(error) => Box::pin(tokio_stream::once(PaneEvent::Error(error.0).to_ndjson())),
        };
    let pane_header = axum::http::HeaderValue::from_str(&terminal_binding(&pane_id))
        .map_err(|_| ApiError::new(500, "Invalid terminal pane identity"))?;
    let revoked = async move {
        loop {
            if *human_session.borrow() != generation || human_session.changed().await.is_err() {
                break;
            }
        }
    };
    let lines = futures_util::StreamExt::take_until(lines, revoked);
    let body = axum::body::Body::from_stream(lines.map(Ok::<_, std::convert::Infallible>));
    let mut response = (
        [
            (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
            (
                axum::http::HeaderName::from_static("x-terminal-input"),
                if app.engine.terminals.backend() == crate::terminal::Backend::Embedded {
                    "enabled"
                } else {
                    "disabled"
                },
            ),
            (axum::http::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        body,
    )
        .into_response();
    response.headers_mut().insert("x-terminal-pane", pane_header);
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalInput {
    input: String,
    pane_id: String,
    #[serde(default)]
    submit: bool,
}

/// Explicit human terminal input; uses the same task/role resolution as the screen stream.
pub async fn input_terminal(
    State(app): State<Arc<App>>,
    Path((jira_key, role)): Path<(String, String)>,
    Body422(body): Body422<TerminalInput>,
) -> ApiResult {
    if body.input.is_empty() || body.input.len() > 4096 {
        return Err(ApiError::new(422, "input: must be 1 to 4096 bytes"));
    }
    let (session, pane_id) = app.engine.terminal_target(&jira_key, &role).await?;
    if terminal_binding(&pane_id) != body.pane_id {
        return Err(ApiError::new(409, "The terminal pane changed; reconnect before sending input"));
    }
    if app.engine.terminals.backend() != crate::terminal::Backend::Embedded {
        return Err(ApiError::new(409, "Use the external terminal window for this backend"));
    }
    let result = if body.submit {
        app.engine.terminals.write_line(&session, &pane_id, &body.input).await
    } else {
        app.engine.terminals.write_input(&session, &pane_id, body.input.as_bytes()).await
    };
    result.map_err(|error| ApiError::new(409, error.0))?;
    Ok(Json(json!({"ok": true})))
}

// A process nonce prevents a stale paired browser from writing to a reused pty ID after restart.
fn terminal_binding(pane_id: &str) -> String {
    static INSTANCE: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();
    format!("{}:{pane_id}", INSTANCE.get_or_init(uuid::Uuid::new_v4))
}

#[cfg(test)]
mod terminal_input_tests {
    use super::terminal_binding;

    #[test]
    fn terminal_binding_is_stable_and_includes_process_nonce_and_pane() {
        let first = terminal_binding("pty_1");
        assert_eq!(first, terminal_binding("pty_1"));
        assert_ne!(first, terminal_binding("pty_2"));
        let (nonce, pane) = first.split_once(':').unwrap();
        assert!(uuid::Uuid::parse_str(nonce).is_ok());
        assert_eq!(pane, "pty_1");
    }
}
