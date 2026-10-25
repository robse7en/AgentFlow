//! Optional Windows shell around the same core and HTTP contract as the standalone backend.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod desktop_controls;
mod instance;
mod onboarding;
mod smoke;
mod updates;

use agentflow_core::{
    api::{self, App, Startup},
    config::{Host, Locations, jira_startup_problem, load_config},
    git::GitService,
    terminal::{Terminals, embedded::EmbeddedTerminals, leftover_zellij, zellij::ZellijService},
};
use std::{path::PathBuf, sync::Arc};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Desktop {
    app: Arc<App>,
    /// None when agents run in zellij (`zellij.useInDesktop`): those sessions outlive the app.
    embedded: Option<Arc<EmbeddedTerminals>>,
    stop: tokio::sync::watch::Sender<bool>,
    /// Set while a close request is being confirmed or carried out.
    closing: std::sync::atomic::AtomicBool,
    _instance_lock: std::fs::File,
}
impl Desktop {
    fn shutdown(&self) {
        self.stop.send_replace(true);
        self.app.shutdown();
        if let Some(embedded) = &self.embedded {
            for session in embedded.list_sessions() {
                embedded.kill_session(&session);
            }
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.shutdown();
    }
}
type Error = Box<dyn std::error::Error + Send + Sync>;

async fn backend(resources: PathBuf, locations: Locations) -> Result<(Desktop, String), Error> {
    let config_file = locations.config_path();
    let mut config = load_config(&config_file, &locations)?;
    agentflow_core::diagnostics::open_log(&api::diagnostic_path(&config_file))?;
    agentflow_core::diagnostics::install_panic_hook();
    let configured_server = config.server.clone();
    if let Some(problem) = jira_startup_problem(&config) {
        return Err(problem.into());
    }
    // A source web server must be closed before the desktop opens the same database.
    let configured_host = match config.server.host {
        Host::Ipv4Loopback => "127.0.0.1",
        Host::Ipv6Loopback => "::1",
        Host::Localhost => "localhost",
    };
    if tokio::time::timeout(
        std::time::Duration::from_millis(250),
        tokio::net::TcpStream::connect((configured_host, config.server.port as u16)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
    {
        return Err(
            "The configured web server port is in use. Close the old AgentFlow server before opening this instance."
                .into(),
        );
    }
    let instance_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.database_file(&locations).with_extension("desktop.lock"))?;
    instance_lock.try_lock().map_err(|_| "This database is already open in another AgentFlow desktop.")?;
    let use_zellij = config.zellij.use_in_desktop;
    let listener = if use_zellij {
        // Agents left running in zellij reach the app at the URL they were launched with, so
        // a reopened desktop must serve the same configured address.
        tokio::net::TcpListener::bind((configured_host, config.server.port as u16)).await?
    } else {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        config.server.host = Host::Ipv4Loopback;
        config.server.port = listener.local_addr()?.port().into();
        listener
    };
    let origin = api::api_base_url(&config);
    let ui = if resources.join("ui/index.html").is_file() {
        resources.join("ui")
    } else {
        locations.checkout.as_ref().ok_or("Desktop UI is missing")?.join("frontend/dist")
    };
    if !ui.join("index.html").is_file() {
        return Err("Build the Vue frontend before starting the desktop".into());
    }
    let executable = if cfg!(windows) { "flowctl.exe" } else { "flowctl" };
    let flowctl = if resources.join(executable).is_file() {
        resources.join(executable)
    } else {
        locations.checkout.as_ref().ok_or("flowctl is missing")?.join("app/target/debug").join(executable)
    };
    if !flowctl.is_file() {
        return Err("Build flowctl before starting the desktop".into());
    }
    let (terminals, embedded, leftover_zellij): (Arc<dyn Terminals>, _, _) = if use_zellij {
        let zellij = ZellijService::new(config.zellij.clone());
        if let Err(error) = zellij.require_subscribe_support().await {
            return Err(format!(
                "zellij.useInDesktop is set, but zellij cannot be used: {error}. Install Zellij 0.44 or newer, set zellij.command to its full path, or set zellij.useInDesktop to false."
            )
            .into());
        }
        (Arc::new(zellij), None, None)
    } else {
        let embedded = Arc::new(EmbeddedTerminals::from_config(&config.zellij));
        (embedded.clone(), Some(embedded), leftover_zellij(&config.zellij))
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
    *app.configured_server.lock().unwrap_or_else(|p| p.into_inner()) = Some(configured_server);
    // The existing Vue bootstrap consumes this single-use fragment and removes it. The token
    // stays in an HttpOnly cookie; neither the pairing code nor a bearer token enters HTTP logs.
    let code = app.human.lock().unwrap_or_else(|p| p.into_inner()).pairing_code.clone();
    let url = format!("{origin}/#pair={code}");
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let router = api::router_with_ui(app.clone(), Some(ui));
    tauri::async_runtime::spawn(async move {
        if axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stopped.changed().await;
            })
            .await
            .is_err()
        {
            eprintln!("AgentFlow's local API stopped unexpectedly");
        }
    });
    Ok((Desktop { app, embedded, stop, closing: Default::default(), _instance_lock: instance_lock }, url))
}

fn main() {
    desktop_controls::wait_for_previous_instance();
    let smoke = smoke::Smoke::from_args();
    let context = tauri::generate_context!();
    let mut builder = tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _, _| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));
    // The plugin cannot start without the update settings that only configured builds carry.
    if updates::configured(&context) {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    let application = builder
        .setup(move |app| {
            let resources = app.path().resource_dir()?;
            let base = Locations::from_env();
            let (locations, mut message) = match instance::selected(&base) {
                Ok(at) => (at, String::new()),
                Err(error) => (base.clone(), format!("Cannot open the saved instance: {error}")),
            };
            let webview_data = agentflow_core::config::read_config(&locations.config_path())
                .map(|config| config.data_dir(&locations)).unwrap_or_else(|_| locations.home.clone())
                .join("webview");
            let ready = if message.is_empty() && !onboarding::required(&locations) {
                match tauri::async_runtime::block_on(backend(resources.clone(), locations.clone())) {
                    Ok((desktop, url)) => {
                        app.manage(desktop);
                        Some(url)
                    }
                    Err(error) => { message = error.to_string(); None }
                }
            } else { None };
            let setup_mode = ready.is_none();
            let (url, origin) = if let Some(url) = ready {
                let origin = Arc::new(std::sync::Mutex::new(url.parse::<tauri::Url>()?.origin().ascii_serialization()));
                (url, origin)
            } else {
                // The default locations: setup offers the selected instance but creates new ones there.
                tauri::async_runtime::block_on(onboarding::start(
                    base, resources, app.handle().clone(), message,
                )).map_err(|error| error as Box<dyn std::error::Error>)?
            };
            let setup_url = url.clone();
            let url: tauri::Url = url.parse()?;
            let navigation_test = smoke.clone();
            let handle = app.handle().clone();
            // No native IPC capabilities are granted. Keep all navigation on this instance's
            // exact loopback origin so external pages cannot acquire the paired UI session.
            let mut window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url));
            if let Some(smoke) = &smoke { window = window.initialization_script(smoke.script()).additional_browser_args("--disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows"); }
            window
                .initialization_script("Object.defineProperty(window, '__AGENTFLOW_DESKTOP__', { value: true });")
                .title("AgentFlow")
                .inner_size(1280.0, 850.0)
                .min_inner_size(800.0, 600.0)
                .visible(smoke.is_none())
                .data_directory(webview_data)
                .on_navigation(move |url| {
                    if url.origin().ascii_serialization() != *origin.lock().unwrap_or_else(|p| p.into_inner()) {
                        return false;
                    }
                    if url.path() == "/__desktop/reconnect" {
                        desktop_controls::reconnect(&handle, url);
                        return false;
                    }
                    if url.path() == "/__desktop/change-instance" {
                        desktop_controls::change_instance(&handle);
                        return false;
                    }
                    if url.path() == "/__desktop/check-updates" {
                        updates::check_now(&handle);
                        return false;
                    }
                    navigation_test.as_ref().is_none_or(|test| test.navigation(&handle, url))
                })
                .on_new_window(|url, _| desktop_controls::open_new_window(&url))
                .build()?;
            if setup_mode && let Some(smoke) = &smoke { smoke.setup_ready(app.handle(), &setup_url)?; }
            if smoke.is_none() { updates::start(app.handle()); }
            Ok(())
        })
        .build(context)
        .expect("AgentFlow desktop startup failed");
    application.run(|app, event| {
        // Confirm on the window's close request: by ExitRequested the window is already gone.
        if let tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::CloseRequested { api, .. }, .. } =
            &event
            && label == "main"
            && let Some(desktop) = app.try_state::<Desktop>()
            && !desktop_controls::close(app, &desktop, desktop_controls::Purpose::Quit)
        {
            api.prevent_close();
        }
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(desktop) = app.try_state::<Desktop>() {
                desktop.shutdown();
            }
            if let Some(setup) = app.try_state::<onboarding::SetupShutdown>() {
                setup.0.send_replace(true);
            }
        }
    });
}
