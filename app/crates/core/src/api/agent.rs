//! Agent-facing routes flowctl calls (`/api/tasks/{key}/agent/*`), ported from
//! `backend/agentflow/api/workflow.py`. The permissions middleware has already checked the run's
//! credential against the body's role and run id; these validate the body as Python's request
//! models did (422 naming the field) and call the engine.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Map, Value};

use super::{ApiError, ApiResult, App, Body422};
use crate::engine::agents::{clean_ask, finding_dump};

fn invalid(field: &str, problem: &str) -> ApiError {
    ApiError::new(422, format!("{field}: {problem}"))
}

/// Pydantic's lax `int`: an integer, a whole float, a bool or a numeric string.
fn lax_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => {
            n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15).map(|f| f as i64))
        }
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The fields every agent request carries: `role`, and `runId` (or `run_id`).
struct Caller {
    role: String,
    run_id: Option<i64>,
}

fn caller(body: &Map<String, Value>) -> Result<Caller, ApiError> {
    let role = match body.get("role") {
        Some(Value::String(role)) => role.clone(),
        Some(_) => return Err(invalid("role", "must be a string")),
        None => return Err(invalid("role", "field required")),
    };
    let run_id = match body.get("runId").or_else(|| body.get("run_id")) {
        None | Some(Value::Null) => None,
        Some(value) => Some(lax_int(value).ok_or_else(|| invalid("runId", "must be an integer"))?),
    };
    Ok(Caller { role, run_id })
}

fn text<'a>(body: &'a Map<String, Value>, field: &str) -> Result<&'a str, ApiError> {
    match body.get(field) {
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(invalid(field, "must be a string")),
        None => Err(invalid(field, "field required")),
    }
}

pub async fn question(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    let raw = text(&body, "message")?;
    let (message, choices, decision) = clean_ask(raw, body.get("choices"), body.get("context"), body.get("risk"))
        .map_err(|problem| ApiError::new(422, problem))?;
    Ok(Json(app.engine.agent_decision(&jira_key, &who.role, who.run_id, &message, &choices, decision).await?))
}

pub async fn heartbeat(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    let phase = match body.get("phase") {
        None => "working",
        Some(Value::String(phase)) => phase,
        Some(_) => return Err(invalid("phase", "must be a string")),
    };
    Ok(Json(app.engine.agent_heartbeat(&jira_key, &who.role, who.run_id, phase).await?))
}

pub async fn running(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    Ok(Json(app.engine.agent_running(&jira_key, &who.role, who.run_id).await?))
}

pub async fn complete(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    let result = match body.get("result") {
        Some(Value::Object(result)) => result.clone(),
        Some(_) => return Err(invalid("result", "must be an object")),
        None => return Err(invalid("result", "field required")),
    };
    Ok(Json(app.engine.agent_complete(&jira_key, &who.role, who.run_id, result).await?))
}

pub async fn finding(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    let Some(raw) = body.get("finding") else { return Err(invalid("finding", "field required")) };
    let finding = finding_dump(raw).map_err(|problem| ApiError::new(422, problem))?;
    Ok(Json(app.engine.agent_finding(&jira_key, &who.role, who.run_id, finding).await?))
}

pub async fn fail(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    let message = text(&body, "message")?;
    Ok(Json(app.engine.agent_fail(&jira_key, &who.role, who.run_id, message).await?))
}

pub async fn exited(
    State(app): State<Arc<App>>,
    Path(jira_key): Path<String>,
    Body422(body): Body422<Map<String, Value>>,
) -> ApiResult {
    let who = caller(&body)?;
    let code = match body.get("exitCode").or_else(|| body.get("exit_code")) {
        Some(value) => lax_int(value).ok_or_else(|| invalid("exitCode", "must be an integer"))?,
        None => return Err(invalid("exitCode", "field required")),
    };
    Ok(Json(app.engine.agent_exited(&jira_key, &who.role, who.run_id, code).await?))
}
