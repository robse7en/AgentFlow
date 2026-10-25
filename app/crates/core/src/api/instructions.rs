//! Operator APIs. Existing human-session middleware rejects agent credentials on all routes.
use super::{ApiResult, App, Body422};
use crate::engine::instruction_ui::{ApplyUpdate, PreviewQuery, SourceSave};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use std::sync::Arc;

pub async fn project(State(app): State<Arc<App>>, Path(id): Path<i64>) -> ApiResult {
    Ok(Json(app.engine.project_instructions(id).await?))
}
pub async fn save_project(
    State(app): State<Arc<App>>,
    Path((id, role)): Path<(i64, String)>,
    Body422(body): Body422<SourceSave>,
) -> ApiResult {
    Ok(Json(app.engine.save_project_instruction(id, &role, body).await?))
}
pub async fn task(
    State(app): State<Arc<App>>,
    Path(key): Path<String>,
    Query(query): Query<PreviewQuery>,
) -> ApiResult {
    Ok(Json(app.engine.task_instructions(&key, query).await?))
}
pub async fn save_task(
    State(app): State<Arc<App>>,
    Path((key, role)): Path<(String, String)>,
    Body422(body): Body422<SourceSave>,
) -> ApiResult {
    Ok(Json(app.engine.save_task_instruction(&key, &role, body).await?))
}
pub async fn apply(
    State(app): State<Arc<App>>,
    Path(key): Path<String>,
    Body422(body): Body422<ApplyUpdate>,
) -> ApiResult {
    Ok(Json(app.engine.apply_instructions(&key, body).await?))
}
