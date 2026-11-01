//! The loopback HTTP API, ported from `backend/agentflow/main.py` and `backend/agentflow/api/`:
//! the UI routes, `/api/ws`, and the agent routes flowctl calls. Same paths, status codes and JSON
//! shapes as the Python backend (checked by `contract/` with `CONTRACT_TARGET=rust`).
//!
//! Middleware runs in Starlette's order, outermost first: request ids and diagnostics, CORS for the
//! Vite dev server, permissions (human session or agent credential), the trusted-Host check.
//!
//! **Difference:** a request body that fails validation gets a 422 whose `detail` is a readable
//! string, where FastAPI sent pydantic's error list (the UI shows `detail` as text either way).

pub mod agent;
pub mod auth;
pub mod config;
pub mod fs;
pub mod instructions;
pub mod projects;
pub mod tasks;
pub mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::diagnostics;
use crate::engine::{Engine, WorkflowError};

/// Everything the routes share. The engine owns the configuration and harness registry, which a
/// reload swaps.
pub struct App {
    pub engine: Arc<Engine>,
    pub config_file: PathBuf,
    pub human: Mutex<auth::HumanAccess>,
    /// `{"git": {...}, "terminal": {...}, "zellij": {...}}`: tool availability, refreshed on reload.
    pub system: RwLock<Value>,
    /// Serializes every read-modify-write of config.json and every reload, so concurrent saves of
    /// different settings cannot lose one another (AF-AUDIT-001). Taken before the engine's
    /// launch lock, never after it.
    pub settings: tokio::sync::Mutex<()>,
    pub diagnostic_path: PathBuf,
    pub chat: Mutex<Option<crate::chat::service::ChatHandle>>,
    /// Desktop pins a loopback port at runtime; reload compares the file against its original server.
    pub configured_server: Mutex<Option<crate::config::ServerConfig>>,
}

/// An error response: `{"detail": ..., "code"?: ...}`.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub detail: String,
    pub code: Option<&'static str>,
}

/// What starting the backend needs (main.py's build_state and create_app).
pub struct Startup {
    pub config: crate::config::Config,
    pub config_file: PathBuf,
    pub locations: crate::config::Locations,
    /// The flowctl executable agents call.
    pub flowctl: PathBuf,
    /// Run each harness CLI (and git and zellij) to report versions, as the Python backend did at
    /// startup. Off in tests, which must not start the user's CLIs.
    pub probe_tools: bool,
    /// Where agents run (embedded pseudoconsoles or zellij).
    pub terminals: Arc<dyn crate::terminal::Terminals>,
    /// See [`crate::engine::Engine::leftover_zellij`].
    pub leftover_zellij: Option<Arc<dyn crate::terminal::Terminals>>,
    /// The git the engine uses ([`crate::git::GitService`] outside tests).
    pub git: Arc<dyn crate::vcs::Git>,
}

/// Where the diagnostic log goes: `.agentflow/logs/agentflow.jsonl` beside config.json.
pub fn diagnostic_path(config_file: &std::path::Path) -> PathBuf {
    crate::paths::resolve(config_file)
        .parent()
        .map(|dir| dir.join(".agentflow").join("logs").join("agentflow.jsonl"))
        .unwrap_or_else(|| PathBuf::from("agentflow.jsonl"))
}

impl App {
    /// Open the database, detect tools and harnesses, and build the engine and the app state.
    pub async fn start(startup: Startup) -> Result<Arc<App>, Box<dyn std::error::Error + Send + Sync>> {
        let Startup { config, config_file, locations, flowctl, probe_tools, terminals, leftover_zellij, git } = startup;
        let diagnostic_path = diagnostic_path(&config_file);
        let db = Arc::new(crate::db::Database::open(&config.database_file(&locations))?);
        db.create_all()?;
        let (harnesses, system) = if probe_tools {
            (config::detected_harnesses(&config).await, config::system_status(&config, terminals.backend()).await)
        } else {
            let mut harnesses = crate::harness::HarnessRegistry::new(&config);
            harnesses.assume_capabilities();
            let zellij = json!({"available": false, "version": null, "command": config.zellij.command});
            let terminal = config::terminal_status(terminals.backend(), &zellij);
            (harnesses, json!({"git": {"available": true, "version": null}, "terminal": terminal, "zellij": zellij}))
        };
        let data_dir = config.data_dir(&locations);
        let engine = Engine::new(crate::engine::EngineSetup {
            api_base_url: api_base_url(&config),
            locations,
            config: Arc::new(config),
            harnesses: Arc::new(harnesses),
            db,
            git,
            terminals,
            leftover_zellij,
            worktree_killer: Arc::new(crate::process::worktree::SystemKiller),
            broadcaster: Arc::new(crate::events::Broadcaster::default()),
            flowctl,
        })?;
        // Runs a restart interrupted wait for a human (Resume or Retry); nothing is relaunched.
        engine.recover().await?;
        engine.start_pr_sync();
        let chat = crate::chat::service::start_chat(engine.clone());
        Ok(Arc::new(App {
            engine,
            config_file,
            human: Mutex::new(auth::HumanAccess::new(data_dir.join("browser-session"))),
            system: RwLock::new(system),
            settings: tokio::sync::Mutex::new(()),
            diagnostic_path,
            chat: Mutex::new(chat),
            configured_server: Mutex::new(None),
        }))
    }
}

impl App {
    /// App state around an engine built elsewhere (tests drive an engine with fakes through the
    /// routes this way). Not paired; `system` reports nothing probed.
    pub fn around(engine: Arc<Engine>, config_file: PathBuf) -> Arc<App> {
        let data_dir = engine.config().data_dir(&engine.locations);
        Arc::new(App {
            human: Mutex::new(auth::HumanAccess::new(data_dir.join("browser-session"))),
            diagnostic_path: diagnostic_path(&config_file),
            config_file,
            system: RwLock::new(json!({})),
            settings: tokio::sync::Mutex::new(()),
            engine,
            chat: Mutex::new(None),
            configured_server: Mutex::new(None),
        })
    }

    pub fn shutdown(&self) {
        self.chat.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.engine.shutdown();
    }
}

/// `http://host:port`, with an IPv6 host in brackets.
pub fn api_base_url(config: &crate::config::Config) -> String {
    let host = serde_json::to_value(config.server.host).ok().and_then(|v| v.as_str().map(String::from));
    let host = host.unwrap_or_else(|| "127.0.0.1".into());
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    format!("http://{host}:{}", config.server.port)
}

impl ApiError {
    pub fn new(status: u16, detail: impl Into<String>) -> Self {
        ApiError { status, detail: detail.into(), code: None }
    }
}

impl From<WorkflowError> for ApiError {
    fn from(error: WorkflowError) -> Self {
        ApiError { status: error.status, detail: error.message, code: error.code }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"detail": self.detail});
        if let Some(code) = self.code {
            body["code"] = json!(code);
        }
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, axum::Json(body)).into_response()
    }
}

pub type ApiResult<T = axum::Json<Value>> = Result<T, ApiError>;

/// A JSON body that must deserialize into `T`; anything else is a 422 naming the problem.
pub struct Body422<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body422<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(request, state).await.map_err(|e| ApiError::new(400, e.to_string()))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| ApiError::new(422, format!("Invalid JSON body: {e}")))?;
        serde_path_to_error::deserialize(value).map(Body422).map_err(|e| {
            let path = e.path().to_string();
            ApiError::new(422, if path == "." { e.inner().to_string() } else { format!("{path}: {}", e.inner()) })
        })
    }
}

pub fn router(app: Arc<App>) -> Router {
    router_with_ui(app, None)
}

/// Serve an optional Vue build through the same loopback middleware as the API.
pub fn router_with_ui(app: Arc<App>, dist: Option<PathBuf>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/pair", post(pair))
        .route("/api/auth/session", get(|| async { axum::Json(json!({"ok": true})) }))
        .route("/api/diagnostics", get(diagnostic_log))
        .route("/api/ws", get(websocket))
        .route("/api/config", get(config::get_config))
        .route("/api/jira/search", get(tasks::jira_search))
        .route("/api/config/reload", post(config::reload))
        .route("/api/config/default-repository", put(config::set_default_repository))
        .route("/api/config/exchange-rate", put(config::set_exchange_rate))
        .route("/api/config/cost-controls", put(config::set_cost_controls))
        .route("/api/fs/list", get(fs::list_directory))
        .route("/api/harnesses", get(config::list_harnesses))
        .route("/api/harnesses/detect", post(config::detect_harnesses))
        .route("/api/tasks", get(tasks::list_tasks).post(tasks::create_task))
        .route("/api/tasks/validate-repository", post(tasks::validate_repository))
        .route("/api/tasks/{jira_key}", get(tasks::get_task))
        .route("/api/tasks/{jira_key}/instructions", get(instructions::task))
        .route("/api/tasks/{jira_key}/instructions/{role}", put(instructions::save_task))
        .route("/api/tasks/{jira_key}/instructions/apply", post(instructions::apply))
        .route("/api/projects/{project_id}/instructions", get(instructions::project))
        .route("/api/projects/{project_id}/instructions/{role}", put(instructions::save_project))
        .route("/api/tasks/{jira_key}/cancel", post(tasks::cancel_task))
        .route("/api/tasks/{jira_key}/archive", post(tasks::archive_task))
        .route("/api/tasks/{jira_key}/unarchive", post(tasks::unarchive_task))
        .route("/api/tasks/{jira_key}/project", put(projects::set_task_project))
        .route("/api/projects", get(projects::list_projects).post(projects::create_project))
        .route("/api/projects/{project_id}", patch(projects::update_project).delete(projects::delete_project))
        .route("/api/projects/{project_id}/archive", post(projects::archive_project))
        .route("/api/projects/{project_id}/unarchive", post(projects::unarchive_project))
        .route("/api/tasks/{jira_key}/delete-preview", get(tasks::delete_preview))
        .route("/api/tasks/{jira_key}/delete", post(tasks::delete_task))
        .route("/api/tasks/{jira_key}/worktree/cleanup-preview", get(tasks::worktree_cleanup_preview))
        .route("/api/tasks/{jira_key}/worktree/cleanup", post(tasks::cleanup_worktree))
        .route("/api/tasks/{jira_key}/findings/{finding_id}/code", get(tasks::finding_code))
        .route("/api/tasks/{jira_key}/nodes/{node_key}/run", post(tasks::run_node))
        .route("/api/tasks/{jira_key}/nodes/{node_key}/recover", post(tasks::recover_node))
        .route("/api/tasks/{jira_key}/nodes/{node_key}/config", put(tasks::set_node_config))
        .route("/api/tasks/{jira_key}/nodes/{node_key}/approve", post(tasks::approve_node))
        .route("/api/tasks/{jira_key}/nodes/{node_key}/request-changes", post(tasks::request_changes))
        .route("/api/tasks/{jira_key}/review-override", post(tasks::override_reviews))
        .route("/api/tasks/{jira_key}/worktree/discard-drift", post(tasks::discard_worktree_drift))
        .route("/api/tasks/{jira_key}/worktree/setup/retry", post(tasks::retry_worktree_setup))
        .route("/api/tasks/{jira_key}/budget", put(tasks::set_task_budget))
        .route("/api/tasks/{jira_key}/runs/{run_id}/usage", put(tasks::import_usage))
        .route("/api/tasks/{jira_key}/pr/created", post(tasks::mark_pr_created))
        .route("/api/tasks/{jira_key}/pr/created/revert", post(tasks::revert_pr_created))
        .route("/api/tasks/{jira_key}/pr/merged", post(tasks::mark_pr_merged))
        .route("/api/tasks/{jira_key}/pr/merged/revert", post(tasks::revert_pr_merged))
        .route("/api/tasks/{jira_key}/pr/open", post(tasks::open_pr))
        .route("/api/tasks/{jira_key}/pr/retarget", post(tasks::retarget_pr))
        .route("/api/tasks/{jira_key}/zellij/open", post(tasks::open_zellij))
        .route("/api/tasks/{jira_key}/zellij/window", post(tasks::open_zellij_window))
        .route("/api/tasks/{jira_key}/reveal/{target}", post(tasks::reveal_path))
        .route("/api/tasks/{jira_key}/questions/{question_uid}/answer", post(tasks::answer_question))
        .route("/api/questions", get(tasks::open_questions))
        .route("/api/tasks/{jira_key}/terminal/{role}", get(tasks::stream_terminal))
        .route("/api/tasks/{jira_key}/terminal/{role}/input", post(tasks::input_terminal))
        .route("/api/tasks/{jira_key}/agent/question", post(agent::question))
        .route("/api/tasks/{jira_key}/agent/heartbeat", post(agent::heartbeat))
        .route("/api/tasks/{jira_key}/agent/running", post(agent::running))
        .route("/api/tasks/{jira_key}/agent/complete", post(agent::complete))
        .route("/api/tasks/{jira_key}/agent/finding", post(agent::finding))
        .route("/api/tasks/{jira_key}/agent/fail", post(agent::fail))
        .route("/api/tasks/{jira_key}/agent/exited", post(agent::exited))
        .fallback(move |request: Request| ui::serve(dist.clone(), request))
        .layer(axum::middleware::from_fn(operation_scope))
        .layer(axum::middleware::from_fn(trusted_host))
        .layer(axum::middleware::from_fn_with_state(app.clone(), permissions))
        .layer(axum::middleware::from_fn(cors))
        .layer(axum::middleware::from_fn(diagnostic_requests))
        .with_state(app)
}

async fn health() -> axum::Json<Value> {
    axum::Json(json!({"ok": true, "version": crate::VERSION}))
}

// ---------------------------------------------------------------------------------- middleware

/// A request id on every response (X-Request-ID) and every event the request causes, a
/// `request.completed` event, and a 500 that names the id instead of the error when a handler
/// panics.
async fn diagnostic_requests(request: Request, next: Next) -> Response {
    let reference = uuid::Uuid::new_v4().to_string();
    let started = Instant::now();
    let method = request.method().to_string();
    let route = request.extensions().get::<MatchedPath>().map(|p| p.as_str().to_string());
    let handled = tokio::spawn(diagnostics::REQUEST_ID.scope(reference.clone(), next.run(request))).await;
    diagnostics::REQUEST_ID
        .scope(reference.clone(), async {
            let mut response = match handled {
                Ok(response) => response,
                Err(_) => {
                    let frames = diagnostics::take_panic_frames(&reference);
                    diagnostics::failure("request.failed", json!({}), "panic", frames);
                    let detail = format!("Internal error. Diagnostic reference: {reference}");
                    ApiError::new(500, detail).into_response()
                }
            };
            diagnostics::event(
                "request.completed",
                json!({
                    "method": method,
                    "route": route.as_deref().unwrap_or("unmatched"),
                    "status": response.status().as_u16(),
                    "durationMs": started.elapsed().as_millis() as u64,
                }),
            );
            if let Ok(value) = HeaderValue::from_str(&reference) {
                response.headers_mut().insert("x-request-id", value);
            }
            response
        })
        .await
}

/// Run the request as the UI's operation (X-Operation-Id), so the engine's progress steps reach
/// the toast that started it. An invalid id is ignored rather than refused: progress is optional.
async fn operation_scope(request: Request, next: Next) -> Response {
    let operation = request
        .headers()
        .get("x-operation-id")
        .and_then(|v| v.to_str().ok())
        .filter(|id| crate::events::valid_operation_id(id))
        .map(String::from);
    match operation {
        Some(id) => crate::events::OPERATION_ID.scope(id, next.run(request)).await,
        None => next.run(request).await,
    }
}

const DEV_ORIGINS: [&str; 2] = ["http://127.0.0.1:5173", "http://localhost:5173"];

/// Starlette's CORSMiddleware for the Vite dev server origins: any method and header, no
/// credentials.
async fn cors(request: Request, next: Next) -> Response {
    let origin = request.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok()).map(String::from);
    let preflight = request.method() == Method::OPTIONS
        && request.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
        && origin.is_some();
    let allowed = origin.as_deref().is_some_and(|o| DEV_ORIGINS.contains(&o));
    if preflight {
        if !allowed {
            return (StatusCode::BAD_REQUEST, "Disallowed CORS origin").into_response();
        }
        let mut response = (StatusCode::OK, "OK").into_response();
        let headers = response.headers_mut();
        let requested = request.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS).cloned();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_str(origin.as_deref().unwrap()).unwrap());
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"),
        );
        headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        if let Some(requested) = requested {
            headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested);
        }
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        return response;
    }
    let mut response = next.run(request).await;
    if allowed && let Some(origin) = origin.and_then(|o| HeaderValue::from_str(&o).ok()) {
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response.headers_mut().append(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

fn host(headers: &HeaderMap) -> String {
    headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default().to_string()
}

fn origin(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::ORIGIN).and_then(|o| o.to_str().ok())
}

/// Who may call what: pairing only from the local UI; agent credentials only on their own task's
/// read and agent routes; everything else needs the paired browser session, and changes need the
/// local UI's Origin too.
async fn permissions(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if !path.starts_with("/api/") || request.method() == Method::OPTIONS || path == "/api/health" {
        return next.run(request).await;
    }
    match check_permissions(&app, request, &path).await {
        Ok(request) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

async fn check_permissions(app: &App, request: Request, path: &str) -> Result<Request, ApiError> {
    let headers = request.headers().clone();
    let host = host(&headers);
    if path == "/api/auth/pair" {
        if !auth::allowed_origin(origin(&headers), &host) {
            return Err(ApiError::new(403, "Use the local browser UI"));
        }
        return Ok(request);
    }
    let authorization = headers.get(header::AUTHORIZATION).and_then(|a| a.to_str().ok());
    if path.contains("/agent/") || authorization.is_some() {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let method = request.method().clone();
        let readable = parts.len() == 3 && method == Method::GET;
        let callback = parts.len() == 5 && parts[3] == "agent" && method == Method::POST;
        if parts.len() < 2 || parts[..2] != ["api", "tasks"] || !(readable || callback) {
            return Err(ApiError::new(403, "Agents cannot perform human actions"));
        }
        let token = authorization.unwrap_or_default();
        let token = token.strip_prefix("Bearer ").unwrap_or(token).to_string();
        let key = parts[2].to_string();
        if method != Method::POST {
            auth::authorize_agent(&app.engine, &key, &token, None).await?;
            return Ok(request);
        }
        // The body is checked against the credential, then handed on to the route unchanged.
        let (head, body) = request.into_parts();
        let bytes = axum::body::to_bytes(body, 16 * 1024 * 1024)
            .await
            .map_err(|_| ApiError::new(413, "Request body too large"))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| ApiError::new(422, "Expected a JSON object"))?;
        if !value.is_object() {
            return Err(ApiError::new(422, "Expected a JSON object"));
        }
        auth::authorize_agent(&app.engine, &key, &token, Some(&value)).await?;
        return Ok(Request::from_parts(head, Body::from(bytes)));
    }
    let cookies = headers.get(header::COOKIE).and_then(|c| c.to_str().ok());
    if !app.human.lock().unwrap_or_else(|p| p.into_inner()).authenticated(cookies) {
        return Err(ApiError::new(401, "Pair this browser using the code in the backend terminal"));
    }
    let reading = request.method() == Method::GET || request.method() == Method::HEAD;
    if !reading && !auth::allowed_origin(origin(&headers), &host) {
        return Err(ApiError::new(403, "Human actions require the local browser UI"));
    }
    Ok(request)
}

/// Starlette's TrustedHostMiddleware: only loopback names in the Host header.
async fn trusted_host(request: Request, next: Next) -> Response {
    let host = host(request.headers());
    let name = host.split(':').next().unwrap_or_default();
    if !["localhost", "127.0.0.1", "[::1]"].contains(&name) {
        return (StatusCode::BAD_REQUEST, "Invalid host header").into_response();
    }
    next.run(request).await
}

// ---------------------------------------------------------------------------------- routes

async fn pair(State(app): State<Arc<App>>, body: Bytes) -> Response {
    let code = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("code").and_then(Value::as_str).map(String::from));
    let Some(code) = code else {
        return ApiError::new(422, "Expected a pairing code").into_response();
    };
    let token = app.human.lock().unwrap_or_else(|p| p.into_inner()).pair(&code);
    match token {
        Ok(token) => {
            let cookie =
                format!("{}={token}; HttpOnly; Max-Age={}; Path=/; SameSite=strict", auth::COOKIE, 400 * 24 * 3600);
            let mut response = axum::Json(json!({"ok": true})).into_response();
            response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
            response
        }
        Err(error) => error.into_response(),
    }
}

/// The diagnostic log, read whole before responding so its length can't change mid-response (this
/// very request's `request.completed` line is appended to the same file).
async fn diagnostic_log(State(app): State<Arc<App>>) -> Response {
    let body = std::fs::read(&app.diagnostic_path).unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"agentflow.jsonl\""),
        ],
        body,
    )
        .into_response()
}

/// Pushed task snapshots for the paired UI. Frames from the UI are keep-alive pings and ignored.
async fn websocket(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let cookies = headers.get(header::COOKIE).and_then(|c| c.to_str().ok());
    let access = app.human.lock().unwrap_or_else(|p| p.into_inner()).stream_access(cookies);
    let Some((generation, session)) = access.filter(|_| auth::allowed_origin(origin(&headers), &host(&headers))) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let messages = app.engine.broadcaster.connect();
    upgrade.on_upgrade(move |socket| pump(socket, messages, generation, session))
}

const SOCKET_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Cancel a blocked write when pairing is revoked. Already written bytes cannot be recalled;
/// dropping the socket after cancellation prevents further queued frames from being sent.
async fn session_write<F, T>(write: F, generation: u64, session: &mut tokio::sync::watch::Receiver<u64>) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(write);
    let deadline = tokio::time::sleep(SOCKET_WRITE_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        if *session.borrow() != generation {
            return None;
        }
        tokio::select! {
            biased;
            changed = session.changed() => {
                if changed.is_err() {
                    return None;
                }
            }
            _ = &mut deadline => return None,
            result = &mut write => return Some(result),
        }
    }
}

async fn close_socket(socket: &mut WebSocket) {
    let _ = tokio::time::timeout(SOCKET_WRITE_TIMEOUT, socket.send(Message::Close(None))).await;
}

/// Forward broadcasts until the socket closes, falls behind, stalls, or pairing is revoked.
async fn pump(
    mut socket: WebSocket,
    mut messages: tokio::sync::broadcast::Receiver<Arc<str>>,
    generation: u64,
    mut session: tokio::sync::watch::Receiver<u64>,
) {
    use tokio::sync::broadcast::error::RecvError;
    let revoked = |session: &tokio::sync::watch::Receiver<u64>| *session.borrow() != generation;
    loop {
        if revoked(&session) {
            return;
        }
        tokio::select! {
            biased;
            changed = session.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            outgoing = messages.recv() => match outgoing {
                Ok(text) => {
                    if revoked(&session) {
                        return;
                    }
                    if !matches!(session_write(
                        socket.send(Message::Text(text.as_ref().into())),
                        generation,
                        &mut session,
                    ).await, Some(Ok(()))) {
                        return;
                    }
                }
                // Too far behind: drop the connection; the UI reconnects and reloads.
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => {
                    close_socket(&mut socket).await;
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn blocked_socket_write_times_out() {
        let (_sender, mut session) = tokio::sync::watch::channel(7);
        let start = tokio::time::Instant::now();
        let result = session_write(std::future::pending::<()>(), 7, &mut session).await;
        assert_eq!(result, None);
        assert_eq!(start.elapsed(), SOCKET_WRITE_TIMEOUT);
    }

    #[tokio::test]
    async fn pairing_revocation_cancels_pending_socket_write() {
        let (sender, mut session) = tokio::sync::watch::channel(7);
        let write = tokio::spawn(async move { session_write(std::future::pending::<()>(), 7, &mut session).await });
        tokio::task::yield_now().await;
        sender.send_replace(8);
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1), write).await.unwrap().unwrap(), None);
    }

    #[tokio::test]
    async fn revoked_pairing_does_not_poll_ready_write() {
        let (sender, mut session) = tokio::sync::watch::channel(7);
        sender.send_replace(8);
        let result = session_write(async { panic!("revoked write must not be polled") }, 7, &mut session).await;
        assert_eq!(result, None::<()>);
    }

    #[tokio::test]
    async fn valid_pairing_completes_socket_write() {
        let (_sender, mut session) = tokio::sync::watch::channel(7);
        assert_eq!(session_write(async { 42 }, 7, &mut session).await, Some(42));
    }

    /// Ported from backend/tests/test_diagnostics.py: a failing route answers 500 with a reference
    /// that is in the diagnostic log, and neither the response nor the log carries the error text or
    /// the query string.
    #[tokio::test]
    async fn test_internal_error_has_correlated_safe_diagnostic() {
        let _serial = diagnostics::TEST_LOG.lock().await;
        diagnostics::install_panic_hook();
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("agentflow.jsonl");
        diagnostics::open_log(&log).unwrap();
        async fn failing() -> &'static str {
            panic!("PRIVATE_PROMPT")
        }
        let router: Router = Router::new()
            .route("/api/tasks/{jira_key}", get(failing))
            .layer(axum::middleware::from_fn(diagnostic_requests));
        let request = Request::builder().uri("/api/tasks/TEST-1?secret=QUERY_SECRET").body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let reference = response.headers()["x-request-id"].to_str().unwrap().to_string();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let detail: Value = serde_json::from_slice(&body).unwrap();
        assert!(detail["detail"].as_str().unwrap().contains(&reference));
        let logs = std::fs::read_to_string(&log).unwrap();
        diagnostics::close_log();
        assert!(logs.contains(&reference) && logs.contains("request.failed"));
        let failed: Value = serde_json::from_str(logs.lines().find(|l| l.contains("request.failed")).unwrap()).unwrap();
        assert_eq!(failed["exceptionType"], "panic");
        assert!(failed["frames"][0]["file"].as_str().unwrap().ends_with("mod.rs"), "{failed}");
        assert!(!logs.contains("PRIVATE_PROMPT") && !logs.contains("QUERY_SECRET"));
        assert!(logs.contains("/api/tasks/{jira_key}"), "the route template, not the path");
    }
}
