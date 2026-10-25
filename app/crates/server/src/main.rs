//! `agentflow`: start the AgentFlow backend, ported from `backend/agentflow/main.py`.
//!
//! Same arguments as `python -m agentflow.main`, so the contract suite starts either the same way.
//! The pairing code is printed to stdout on the line the suite (and the human) reads it from.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use agentflow_core::api::{self, App, Startup};
use agentflow_core::config::{Locations, SKIP_JIRA_CHECK, jira_startup_problem, load_config};
use agentflow_core::diagnostics;
use agentflow_core::git::GitService;
use agentflow_core::terminal::embedded::EmbeddedTerminals;
use agentflow_core::terminal::zellij::ZellijService;
use agentflow_core::terminal::{Terminals, leftover_zellij};
use clap::Parser;
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "agentflow",
    about = "Start the AgentFlow backend",
    after_help = "Run `agentflow setup` to configure AgentFlow, or `agentflow doctor` to check it."
)]
struct Args {
    /// Path to config.json (defaults to config.json in the AgentFlow folder, or an existing
    /// ~/.agentflow/config.json while that has none)
    #[arg(long)]
    config: Option<String>,
    /// Override server.host
    #[arg(long)]
    host: Option<String>,
    /// Override server.port
    #[arg(long)]
    port: Option<u16>,
    /// Start without Jira configured (development and tests; same as AGENTFLOW_SKIP_JIRA_CHECK=1)
    #[arg(long)]
    skip_jira_check: bool,
    /// Where agents run: zellij sessions (as the Python backend), or terminals embedded in the app
    #[arg(long, value_enum, default_value_t = TerminalChoice::Embedded)]
    terminal: TerminalChoice,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TerminalChoice {
    Zellij,
    Embedded,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.skip_jira_check {
        // Through the environment, so in-app config reloads skip it too. Set before any thread
        // exists, which makes set_var sound.
        unsafe { std::env::set_var(SKIP_JIRA_CHECK, "1") };
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,agentflow::diagnostics=warn".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    let locations = Locations::from_env();
    let file = match &args.config {
        Some(path) => PathBuf::from(agentflow_core::process::expand_user(path)),
        None => locations.config_path(),
    };
    let mut config = match load_config(&file, &locations) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Configuration error:\n{error}");
            return ExitCode::from(2);
        }
    };
    if let Some(problem) = jira_startup_problem(&config) {
        eprintln!("{problem}\nConfig file: {}", file.display());
        return ExitCode::from(2);
    }
    if let Some(host) = &args.host {
        match serde_json::from_value(json!(host)) {
            Ok(host) => config.server.host = host,
            Err(_) => {
                eprintln!("AgentFlow must bind to a loopback address");
                return ExitCode::from(2);
            }
        }
    }
    if let Some(port) = args.port {
        config.server.port = i64::from(port);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    match runtime.block_on(serve(config, file, locations, args.terminal)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(
    config: agentflow_core::config::Config,
    config_file: PathBuf,
    locations: Locations,
    terminal: TerminalChoice,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let host = serde_json::to_value(config.server.host)?.as_str().unwrap_or("127.0.0.1").to_string();
    let port = u16::try_from(config.server.port)?;
    let api_base_url = api::api_base_url(&config);
    let checkout = locations.checkout.clone();
    let flowctl = std::env::current_exe()?.with_file_name(if cfg!(windows) { "flowctl.exe" } else { "flowctl" });
    // Opened first, as Python's lifespan did, so startup itself is recorded.
    if let Err(error) = diagnostics::open_log(&api::diagnostic_path(&config_file)) {
        tracing::warn!(%error, "diagnostic log unavailable");
    }
    diagnostics::install_panic_hook();
    diagnostics::event("application.starting", json!({"version": agentflow_core::VERSION}));
    let (terminals, leftover_zellij): (Arc<dyn Terminals>, _) = match terminal {
        TerminalChoice::Zellij => (Arc::new(ZellijService::new(config.zellij.clone())), None),
        TerminalChoice::Embedded => {
            (Arc::new(EmbeddedTerminals::from_config(&config.zellij)), leftover_zellij(&config.zellij))
        }
    };
    let app = App::start(Startup {
        config,
        config_file,
        locations,
        flowctl,
        probe_tools: true,
        terminals,
        leftover_zellij,
        git: Arc::new(GitService::default()),
    })
    .await?;
    let system = app.system.read().unwrap().clone();
    tracing::info!(
        "git: {} | zellij: {}",
        system["git"]["version"].as_str().unwrap_or("unavailable"),
        system["zellij"]["version"].as_str().unwrap_or("unavailable")
    );

    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    // The backend serves the built UI itself when frontend/dist exists; otherwise the Vite dev
    // server does.
    let dist = checkout.map(|c| c.join("frontend").join("dist")).filter(|d| d.join("index.html").is_file());
    let ui_base = if dist.is_some() { api_base_url } else { "http://127.0.0.1:5173".into() };
    let code = app.human.lock().unwrap().pairing_code.clone();
    println!("AgentFlow browser pairing code: {code}");
    println!("Open {ui_base}/#pair={code} to pair this browser.");
    tracing::info!("AgentFlow listening on {}", listener.local_addr()?);

    axum::serve(listener, api::router_with_ui(app.clone(), dist)).with_graceful_shutdown(shutdown_signal()).await?;
    // Stop background work (usage retries, screen trackers), as Python's lifespan did.
    app.shutdown();
    diagnostics::event("application.stopped", json!({}));
    diagnostics::close_log();
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
