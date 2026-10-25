//! Project routes, ported from `backend/agentflow/api/projects.py`, and the task's project
//! (`PUT /api/tasks/{key}/project` in `api/tasks.py`).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use super::{ApiError, ApiResult, App, Body422};
use crate::engine::projects::{ProjectRequest, nullable};

pub async fn list_projects(State(app): State<Arc<App>>) -> ApiResult {
    Ok(Json(Value::Array(app.engine.list_projects()?)))
}

pub async fn create_project(
    State(app): State<Arc<App>>,
    Body422(request): Body422<ProjectRequest>,
) -> Result<Response, ApiError> {
    let request = request.validated().map_err(|problem| ApiError::new(422, problem))?;
    Ok((StatusCode::CREATED, Json(app.engine.create_project(request).await?)).into_response())
}

pub async fn update_project(
    State(app): State<Arc<App>>,
    Path(project_id): Path<i64>,
    Body422(request): Body422<ProjectRequest>,
) -> ApiResult {
    let request = request.validated().map_err(|problem| ApiError::new(422, problem))?;
    Ok(Json(app.engine.update_project(project_id, request).await?))
}

pub async fn archive_project(State(app): State<Arc<App>>, Path(project_id): Path<i64>) -> ApiResult {
    Ok(Json(app.engine.set_project_archived(project_id, true).await?))
}

pub async fn unarchive_project(State(app): State<Arc<App>>, Path(project_id): Path<i64>) -> ApiResult {
    Ok(Json(app.engine.set_project_archived(project_id, false).await?))
}

pub async fn delete_project(State(app): State<Arc<App>>, Path(project_id): Path<i64>) -> ApiResult {
    Ok(Json(app.engine.delete_project(project_id).await?))
}

#[derive(Deserialize)]
pub struct TaskProject {
    #[serde(rename = "projectId", deserialize_with = "nullable")]
    project_id: Option<i64>,
}

pub async fn set_task_project(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<TaskProject>,
) -> ApiResult {
    Ok(Json(app.engine.set_task_project(&jira_key, body.project_id).await?))
}
