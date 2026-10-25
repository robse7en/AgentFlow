//! A temporary loopback-only setup page. No native IPC or external requests are granted.
use agentflow_core::{
    api::auth::token_hex,
    config::{Locations, jira_startup_problem, read_config},
};
use axum::{
    Form, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::Manager;

pub type Origin = Arc<Mutex<String>>;

pub struct SetupShutdown(pub tokio::sync::watch::Sender<bool>);
impl Drop for SetupShutdown {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetupForm {
    pub token: String,
    pub mode: String,
    pub config_path: String,
    pub base_directory: String,
    pub closed: String,
    pub repository: String,
    pub harness: String,
    pub billing: String,
    pub jira_kind: String,
    pub jira_url: String,
    pub jira_email: String,
    pub jira_token: String,
    pub repair: String,
}

struct Setup {
    /// The default locations, without the saved selection: a new installation is created here.
    base: Locations,
    resources: PathBuf,
    handle: tauri::AppHandle,
    token: String,
    url: String,
    origin: Origin,
    message: String,
    saving: tokio::sync::Mutex<bool>,
    stop: tokio::sync::watch::Sender<bool>,
}

/// Setup runs on first launch, for a configuration that cannot be read, and when Jira is enabled
/// without its settings (the backend would refuse to start). Disabled Jira needs no setup.
pub fn required(at: &Locations) -> bool {
    std::env::args().any(|arg| arg == "--setup")
        || read_config(&at.config_path()).map_or(true, |config| jira_startup_problem(&config).is_some())
}

pub async fn start(
    base: Locations,
    resources: PathBuf,
    handle: tauri::AppHandle,
    message: String,
) -> Result<(String, Origin), super::Error> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let origin = Arc::new(Mutex::new(url.parse::<tauri::Url>()?.origin().ascii_serialization()));
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    handle.manage(SetupShutdown(stop.clone()));
    let setup = Arc::new(Setup {
        base,
        resources,
        handle,
        token: token_hex(32),
        url: url.clone(),
        origin: origin.clone(),
        message,
        saving: tokio::sync::Mutex::new(false),
        stop,
    });
    let router = Router::new()
        .route("/", get(page))
        .route(
            "/setup.css",
            get(|| async {
                ([("content-type", "text/css"), ("cache-control", "no-store")], include_str!("setup.css"))
            }),
        )
        .route("/setup", post(save))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(setup);
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stopped.changed().await;
            })
            .await;
    });
    Ok((url, origin))
}

fn host_matches(setup: &Setup, headers: &HeaderMap) -> bool {
    headers.get("host").and_then(|v| v.to_str().ok()) == setup.url.strip_prefix("http://")
}

async fn page(State(setup): State<Arc<Setup>>, headers: HeaderMap) -> Response {
    if !host_matches(&setup, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    render(&setup, &setup.message, None)
}

async fn save(State(setup): State<Arc<Setup>>, headers: HeaderMap, Form(form): Form<SetupForm>) -> Response {
    if !host_matches(&setup, &headers)
        || headers.get("origin").and_then(|v| v.to_str().ok()) != Some(setup.url.as_str())
        || form.token != setup.token
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut completed = setup.saving.lock().await;
    if *completed {
        return StatusCode::CONFLICT.into_response();
    }
    match launch(&setup, &form).await {
        Ok(()) => {
            *completed = true;
            setup.stop.send_replace(true);
            Html("Opening AgentFlow…").into_response()
        }
        Err(error) => render(&setup, &error.to_string(), Some(&form)),
    }
}

async fn launch(setup: &Setup, form: &SetupForm) -> Result<(), super::Error> {
    let at = choose(&setup.base, form)?;
    let config = read_config(&at.config_path())?;
    if let Some(problem) = jira_startup_problem(&config) {
        return Err(problem.into());
    }
    let (desktop, url) = super::backend(setup.resources.clone(), at.clone()).await?;
    // Save only a location pointer. If that fails, Desktop's Drop stops the newly started backend.
    super::instance::remember(&setup.base, &at)?;
    let url = url.parse::<tauri::Url>()?;
    let window = setup.handle.get_webview_window("main").ok_or("The setup window was closed.")?;
    *setup.origin.lock().unwrap_or_else(|p| p.into_inner()) = url.origin().ascii_serialization();
    setup.handle.manage(desktop);
    // Native navigation allows the new ephemeral origin without weakening form-action 'self'.
    window.navigate(url)?;
    Ok(())
}

/// The instance a submission selects. A new installation ignores the saved selection, so a wrong
/// choice of existing config.json is never overwritten by setting up afresh.
fn choose(base: &Locations, form: &SetupForm) -> Result<Locations, super::Error> {
    match form.mode.as_str() {
        "existing" if form.closed == "yes" => super::instance::existing(base, &form.config_path, &form.base_directory),
        "new" => super::instance::create(base, form),
        _ => Err("Choose a setup option and confirm the required checkbox.".into()),
    }
}

/// A fresh page offers the current instance, so choosing it again takes one submission.
fn current(base: &Locations) -> SetupForm {
    let at = super::instance::selected(base).unwrap_or_else(|_| base.clone());
    SetupForm {
        config_path: shown(&at.config_path()),
        base_directory: at.checkout.as_deref().map(shown).unwrap_or_default(),
        ..SetupForm::default()
    }
}

/// A path without the `\\?\` prefix that a saved selection's canonical drive paths carry.
fn shown(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => text.into_owned(),
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('{', "&#123;")
        .replace('}', "&#125;")
}

/// The page with a failed submission's choices, or the current instance, filled in.
fn form_html(form: &SetupForm) -> String {
    let values = [
        ("CONFIG_INPUT", form.config_path.as_str()),
        ("BASE_DIRECTORY", form.base_directory.as_str()),
        ("REPOSITORY", form.repository.as_str()),
        ("JIRA_URL", form.jira_url.as_str()),
        ("JIRA_EMAIL", form.jira_email.as_str()),
    ];
    let mut html = include_str!("setup.html").to_string();
    for (key, value) in values {
        html = html.replace(&format!("{{{{{key}}}}}"), &escape(value));
    }
    for (field, selected, options) in [
        ("HARNESS", form.harness.as_str(), &["claude", "codex", "copilot"][..]),
        ("BILLING", form.billing.as_str(), &["unknown", "subscription", "metered"][..]),
        ("JIRA_KIND", form.jira_kind.as_str(), &["cloud", "server"][..]),
    ] {
        for option in options {
            html =
                html.replace(&format!("{{{{{field}_{option}}}}}"), if selected == *option { "selected" } else { "" });
        }
    }
    // Secrets and confirmations are deliberately not echoed into a new response.
    html
}

fn render(setup: &Setup, message: &str, form: Option<&SetupForm>) -> Response {
    let tools = ["git", "claude", "codex", "copilot"]
        .map(|name| {
            let status = if agentflow_core::process::resolve_executable(name).is_some() { "found" } else { "missing" };
            format!("{name}: {status}")
        })
        .join("; ");
    let config_path = setup.base.config_path().to_string_lossy().into_owned();
    let html = form_html(form.unwrap_or(&current(&setup.base)))
        .replace("{{MESSAGE}}", &escape(message))
        .replace("{{TOKEN}}", &setup.token)
        .replace("{{CONFIG_PATH}}", &escape(&config_path))
        .replace("{{TOOLS}}", &escape(&tools));
    // Not no-referrer: under that policy WebView2 sends `Origin: null` with the form POST, which
    // `save` rejects. same-origin still sends nothing to any other origin.
    ([
        ("cache-control", "no-store"),
        ("content-security-policy", "default-src 'none'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "same-origin"),
    ], Html(html)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attributes of every `<input ...>` tag on the page.
    fn inputs(html: &str) -> Vec<&str> {
        html.split("<input").skip(1).map(|rest| &rest[..rest.find('>').unwrap()]).collect()
    }

    #[test]
    fn failed_setup_preserves_choices_but_never_echoes_secrets_or_confirmations() {
        let form = SetupForm {
            repository: r#"D:\a<&"{{TOKEN}}"#.into(),
            config_path: r"D:\old\config.json".into(),
            base_directory: r"D:\old".into(),
            harness: "codex".into(),
            billing: "subscription".into(),
            jira_kind: "server".into(),
            jira_url: "https://jira.example".into(),
            jira_email: "user@example.test".into(),
            jira_token: "SECRET_DO_NOT_ECHO".into(),
            closed: "yes".into(),
            repair: "yes".into(),
            ..SetupForm::default()
        };
        let html = form_html(&form);
        assert!(html.contains("a&lt;&amp;&quot;&#123;&#123;TOKEN&#125;&#125;"));
        assert!(html.contains("value=\"codex\" selected"));
        assert!(html.contains("value=\"subscription\" selected"));
        assert!(html.contains("value=\"server\" selected"));
        assert!(html.contains("user@example.test"));
        assert!(html.contains(r#"value="D:\old\config.json""#));
        assert!(!html.contains("SECRET_DO_NOT_ECHO"));
        assert!(inputs(&html).iter().all(|input| !input.contains("checked")), "{html}");
    }

    fn temp_locations() -> (tempfile::TempDir, Locations) {
        let temp = tempfile::Builder::new()
            .prefix("onboarding-test-")
            .tempdir_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
            .unwrap();
        let at =
            Locations { home: temp.path().join("home"), home_overridden: true, checkout: None, config_override: None };
        std::fs::create_dir_all(&at.home).unwrap();
        (temp, at)
    }

    /// Saves a selection of `other/config.json` (base directory `other`) and returns that file.
    fn select_other(temp: &std::path::Path, base: &Locations) -> PathBuf {
        let other = temp.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("config.json"), "{}").unwrap();
        let config = other.join("config.json");
        let at = super::super::instance::existing(base, config.to_str().unwrap(), other.to_str().unwrap()).unwrap();
        super::super::instance::remember(base, &at).unwrap();
        at.config_path()
    }

    #[test]
    fn a_fresh_page_offers_the_default_config_path() {
        let (_temp, base) = temp_locations();
        let html = form_html(&current(&base));
        let default = escape(&base.config_path().to_string_lossy());
        assert!(html.contains(&format!(r#"name="config_path" required value="{default}""#)), "{html}");
        // Only the placeholders render fills afterwards remain.
        let left: Vec<&str> = html.split("{{").skip(1).map(|rest| &rest[..rest.find("}}").unwrap()]).collect();
        assert!(left.iter().all(|name| ["MESSAGE", "TOKEN", "CONFIG_PATH", "TOOLS"].contains(name)), "{left:?}");
    }

    #[test]
    fn a_fresh_page_offers_the_selected_instance_again() {
        let (temp, base) = temp_locations();
        let selected = select_other(temp.path(), &base);
        let html = form_html(&current(&base));
        let config = escape(&shown(&selected));
        let directory = escape(&shown(selected.parent().unwrap()));
        assert!(!config.contains('?'), "{config}");
        assert!(html.contains(&format!(r#"name="config_path" required value="{config}""#)), "{html}");
        assert!(html.contains(&format!(r#"name="base_directory" value="{directory}""#)), "{html}");
    }

    #[test]
    fn a_new_installation_ignores_the_selected_instance() {
        let (temp, base) = temp_locations();
        let selected = select_other(temp.path(), &base);
        let form = SetupForm {
            mode: "new".into(),
            harness: "claude".into(),
            billing: "unknown".into(),
            repair: "yes".into(),
            ..SetupForm::default()
        };
        let at = choose(&base, &form).unwrap();
        assert_eq!(at.config_path(), base.config_path());
        assert!(read_config(&base.config_path()).is_ok());
        assert_eq!(std::fs::read_to_string(&selected).unwrap(), "{}");
    }

    #[test]
    fn setup_is_required_only_when_the_backend_could_not_start() {
        let (_temp, at) = temp_locations();
        let write = |json: &str| std::fs::write(at.config_path(), json).unwrap();
        assert!(required(&at), "first launch");
        write("{");
        assert!(required(&at), "unreadable configuration");
        write("{}");
        assert!(!required(&at), "Jira disabled");
        write(r#"{"jira": {"enabled": true, "baseUrl": "https://x.atlassian.net"}}"#);
        assert!(required(&at), "Jira enabled without a token");
    }
}
