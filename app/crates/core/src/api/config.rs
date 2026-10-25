//! Configuration and system status routes, ported from `backend/agentflow/api/config.py` (the
//! directory browser is in [`super::fs`]).
//!
//! Edits read config.json fresh, change one setting, validate the whole document as Python did and
//! write it back as `json.dumps(raw, indent=2) + "\n"` in text mode, so the file stays the one the
//! Python backend reads.

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{ApiError, ApiResult, App, Body422};
use crate::config::{BillingMode, Config, CostControls, Role, jira_startup_problem, load_config};
use crate::harness::HarnessRegistry;
use crate::paths::py_str;
use crate::terminal::{Backend, zellij::ZellijService};

/// `sys.platform`'s value for this build.
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// Tool availability for `/api/config`'s `system`, as build_state collected it, plus `terminal`:
/// the backend agents actually run in. zellij is still probed for the Settings page, but only
/// matters when it is that backend; the embedded one needs nothing installed.
pub async fn system_status(config: &Config, backend: Backend) -> Value {
    let git = crate::git::GitService::default();
    let zellij = ZellijService::new(config.zellij.clone());
    let zellij = json!({"available": zellij.executable().is_some(), "version": zellij.version().await,
                        "command": config.zellij.command});
    json!({
        "git": {"available": git.executable().is_some(), "version": git.version().await},
        "terminal": terminal_status(backend, &zellij),
        "zellij": zellij,
    })
}

/// `system.terminal` for `backend`, given `system.zellij`.
pub fn terminal_status(backend: Backend, zellij: &Value) -> Value {
    match backend {
        Backend::Embedded => json!({"backend": "embedded", "available": true, "version": "built-in"}),
        Backend::Zellij => json!({"backend": "zellij", "available": zellij["available"], "version": zellij["version"]}),
    }
}

/// Detect every configured harness (build_state).
pub async fn detected_harnesses(config: &Config) -> HarnessRegistry {
    let mut registry = HarnessRegistry::new(config);
    registry.detect_all().await;
    for (key, caps) in &registry.capabilities {
        match &caps.message {
            None if caps.available => tracing::info!("harness {key:<10} available"),
            message => tracing::info!("harness {key:<10} unavailable ({})", message.as_deref().unwrap_or_default()),
        }
    }
    registry
}

pub fn system_info(app: &App) -> Value {
    let engine = &app.engine;
    let config = engine.config();
    let at = &engine.locations;
    let harnesses = engine.harnesses();
    let mut agents = serde_json::to_value(&config.agents).expect("agents serialize");
    agents["orchestrator"] =
        serde_json::to_value(config.agents.for_role(Role::Orchestrator)).expect("agent serializes");
    let harness_list: Vec<Value> = harnesses
        .capabilities
        .values()
        .map(|caps| {
            let configured = config.harnesses.get(&caps.key);
            json!({
                "key": caps.key, "displayName": caps.display_name, "type": caps.kind, "enabled": caps.enabled,
                "available": caps.available, "command": caps.command, "resolvedPath": caps.resolved_path,
                "version": caps.version, "message": caps.message, "supportsModel": caps.supports_model,
                "supportsReasoning": caps.supports_reasoning, "supportsResume": caps.supports_resume,
                "defaultModel": configured.and_then(|h| h.default_model.clone()),
                "automaticTrustWorkspaces": configured.map(|h| h.trusted_repositories.clone()).unwrap_or_default(),
                "billingMode": configured.map_or(BillingMode::Unknown, |h| h.billing_mode),
                "inheritedEnvironment": "allowlist; additional credentials require explicit harness env configuration",
                "availableModels": configured.map(|h| h.available_models.clone()).unwrap_or_default(),
            })
        })
        .collect();
    let jira = &config.jira;
    let filled = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
    json!({
        "configPath": py_str(&app.config_file),
        "dataDirectory": py_str(&config.data_dir(at)),
        "databasePath": py_str(&config.database_file(at)),
        "worktreeDirectory": py_str(&config.worktree_dir(at)),
        "taskDirectory": py_str(&config.task_dir(at)),
        "defaultRepositoryDirectory": config.workspace.default_repository_directory,
        "jiraEnabled": jira.enabled && filled(&jira.base_url) && filled(&jira.api_token),
        "jiraBaseUrl": jira.base_url,
        "github": config.github,
        "platform": platform(),
        // The UI's Settings page shows this as the backend runtime.
        "python": format!("AgentFlow {} (Rust)", crate::VERSION),
        "system": app.system.read().unwrap_or_else(|p| p.into_inner()).clone(),
        "zellij": config.zellij,
        "workflow": config.workflow,
        "agents": agents,
        "harnesses": harness_list,
    })
}

pub async fn get_config(State(app): State<Arc<App>>) -> Json<Value> {
    Json(system_info(&app))
}

pub async fn list_harnesses(State(app): State<Arc<App>>) -> Json<Value> {
    Json(system_info(&app)["harnesses"].clone())
}

pub async fn detect_harnesses(State(app): State<Arc<App>>) -> Json<Value> {
    let registry = detected_harnesses(&app.engine.config()).await;
    app.engine.set_harnesses(Arc::new(registry));
    Json(system_info(&app)["harnesses"].clone())
}

/// Re-read config.json. Settings that need a restart are refused; the rest take effect at once.
/// Holds the settings lock, so a reload never swaps in a file read before a save that finished.
pub async fn reload(State(app): State<Arc<App>>) -> ApiResult {
    let _settings = app.settings.lock().await;
    let engine = &app.engine;
    let current = engine.config();
    let at = &engine.locations;
    let mut new = load_config(&app.config_file, at).map_err(|e| ApiError::new(400, e.to_string()))?;
    let configured_server = app.configured_server.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(problem) = jira_startup_problem(&new) {
        return Err(ApiError::new(400, problem));
    }
    if new.database_file(at) != current.database_file(at)
        || new.data_dir(at) != current.data_dir(at)
        || new.server != *configured_server.as_ref().unwrap_or(&current.server)
    {
        return Err(ApiError::new(409, "Restart AgentFlow to change database, data directory, host or port"));
    }
    if new.zellij != current.zellij {
        return Err(ApiError::new(409, "Restart AgentFlow to change Zellij configuration"));
    }
    if new.slack != current.slack {
        return Err(ApiError::new(409, "Restart AgentFlow to change Slack configuration"));
    }
    let registry = detected_harnesses(&new).await;
    let system = system_status(&new, engine.terminals.backend()).await;
    // The engine and its locks stay: requests may still be finishing on it.
    if configured_server.is_some() {
        new.server = current.server.clone();
    }
    engine.set_config(Arc::new(new));
    engine.set_harnesses(Arc::new(registry));
    *app.system.write().unwrap_or_else(|p| p.into_inner()) = system;
    Ok(Json(system_info(&app)))
}

// ------------------------------------------------------------------------------ edits

fn read_raw(path: &Path) -> Result<Map<String, Value>, ApiError> {
    let text = std::fs::read_to_string(path).map_err(|e| ApiError::new(500, format!("Cannot read config: {e}")))?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(raw)) => Ok(raw),
        _ => Err(ApiError::new(500, "config.json is not a JSON object")),
    }
}

/// `raw.setdefault(key, {})`, as an object.
fn section<'a>(raw: &'a mut Map<String, Value>, key: &str) -> Result<&'a mut Map<String, Value>, ApiError> {
    raw.entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| ApiError::new(500, format!("{key} is not an object")))
}

fn validated(raw: &Map<String, Value>, status: u16, detail: Option<&str>) -> Result<Config, ApiError> {
    Config::from_value(Value::Object(raw.clone()))
        .map_err(|problems| ApiError::new(status, detail.map_or_else(|| problems.join("\n"), String::from)))
}

/// `json.dumps(raw, indent=2) + "\n"`, in text mode.
fn dumped(raw: &Map<String, Value>) -> String {
    pyjson::dumps(&Value::Object(raw.clone())) + "\n"
}

/// Replace config.json atomically: write a temporary file beside it, then rename it over. A failed
/// write leaves the previous file as it was (and the temporary file removed).
fn write_config(path: &Path, raw: &Map<String, Value>) -> Result<(), ApiError> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = path.with_file_name(format!("{name}.{}.tmp", super::auth::token_hex(4)));
    crate::pyfile::write_text(&temporary, &dumped(raw)).and_then(|()| std::fs::rename(&temporary, path)).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        ApiError::new(500, format!("Cannot write config: {e}"))
    })
}

/// Change the live configuration in place, as Python mutated its config object.
fn update_config(app: &App, change: impl FnOnce(&mut Config)) {
    let mut config = (*app.engine.config()).clone();
    change(&mut config);
    app.engine.set_config(Arc::new(config));
}

#[derive(Deserialize)]
pub struct DefaultRepository {
    #[serde(default)]
    path: Option<String>,
}

/// Persist the repository directory the New Task form should prefill.
pub async fn set_default_repository(
    State(app): State<Arc<App>>,
    Body422(body): Body422<DefaultRepository>,
) -> ApiResult {
    // Python: `payload.path.strip() if payload.path else None` (whitespace-only becomes "").
    let path = body.path.filter(|p| !p.is_empty()).map(|p| p.trim().to_string());
    let _settings = app.settings.lock().await;
    let mut raw = read_raw(&app.config_file)?;
    section(&mut raw, "workspace")?.insert("defaultRepositoryDirectory".into(), json!(path));
    validated(&raw, 400, None)?;
    write_config(&app.config_file, &raw)?;
    update_config(&app, |config| config.workspace.default_repository_directory = path);
    Ok(Json(system_info(&app)))
}

#[derive(Deserialize)]
pub struct ExchangeRate {
    #[serde(rename = "usdToNzdRate")]
    rate: f64,
}

/// Persist the fixed USD→NZD rate used to display run costs (not a live FX lookup).
pub async fn set_exchange_rate(State(app): State<Arc<App>>, Body422(body): Body422<ExchangeRate>) -> ApiResult {
    if body.rate.is_nan() || body.rate <= 0.0 {
        return Err(ApiError::new(422, "usdToNzdRate: must be greater than 0"));
    }
    let _settings = app.settings.lock().await;
    let mut raw = read_raw(&app.config_file)?;
    section(&mut raw, "workflow")?.insert("usdToNzdRate".into(), json!(body.rate));
    validated(&raw, 400, None)?;
    write_config(&app.config_file, &raw)?;
    update_config(&app, |config| config.workflow.usd_to_nzd_rate = body.rate);
    Ok(Json(system_info(&app)))
}

#[derive(Deserialize)]
pub struct CostSettings {
    controls: CostControls,
    billing: indexmap::IndexMap<String, String>,
}

/// Human-only configuration; serialized with other settings saves, and with launches so a policy
/// change is atomic (settings lock first, then the launch lock).
pub async fn set_cost_controls(State(app): State<Arc<App>>, Body422(body): Body422<CostSettings>) -> ApiResult {
    let engine = &app.engine;
    let _settings = app.settings.lock().await;
    let _launches = engine.launch_lock.lock().await;
    let config = engine.config();
    let mut raw = read_raw(&app.config_file)?;
    section(&mut raw, "workflow")?.insert("costControls".into(), serde_json::to_value(&body.controls).unwrap());
    if !body.billing.is_empty() && !raw.contains_key("harnesses") {
        // The file relies on the default harnesses. Write them all, not just the one being changed:
        // a harnesses section replaces the defaults, and the agents still name them.
        raw.insert("harnesses".into(), serde_json::to_value(&config.harnesses).unwrap());
    }
    for (key, mode) in &body.billing {
        let Some(harness) = config.harnesses.get(key) else {
            return Err(ApiError::new(422, "Unknown harness"));
        };
        let harnesses = section(&mut raw, "harnesses")?;
        let entry = harnesses.entry(key.clone()).or_insert_with(|| serde_json::to_value(harness).unwrap());
        let entry = entry.as_object_mut().ok_or_else(|| ApiError::new(422, "Invalid cost controls or billing mode"))?;
        entry.insert("billingMode".into(), json!(mode));
    }
    let updated = validated(&raw, 422, Some("Invalid cost controls or billing mode"))?;
    write_config(&app.config_file, &raw)?;
    update_config(&app, |config| {
        config.workflow.cost_controls = updated.workflow.cost_controls.clone();
        for key in body.billing.keys() {
            if let (Some(live), Some(saved)) = (config.harnesses.get_mut(key), updated.harnesses.get(key)) {
                live.billing_mode = saved.billing_mode;
            }
        }
    });
    Ok(Json(system_info(&app)))
}
