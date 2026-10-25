//! Over-the-air updates from the update server a release build was configured with
//! (`build-desktop-installer.ps1 -UpdateUrl`). Builds without one never check.
use crate::desktop_controls::{self, Purpose};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_updater::{Update, UpdaterExt};

/// Delay before the first check, so it does not compete with startup.
const FIRST_CHECK: Duration = Duration::from_secs(60);
/// How often a running desktop checks again.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether this build was packaged with an update server.
pub fn configured<R: tauri::Runtime>(context: &tauri::Context<R>) -> bool {
    context.config().plugins.0.contains_key("updater")
}

#[derive(Default)]
struct Updates {
    /// Set while a check, download or offer is in progress.
    busy: AtomicBool,
    /// A version the user chose not to install now; periodic checks do not offer it again.
    declined: Mutex<Option<String>>,
}

/// Checks periodically while a configured instance is open. Setup never offers an update.
pub fn start(app: &tauri::AppHandle) {
    if !app.config().plugins.0.contains_key("updater") {
        return;
    }
    app.manage(Updates::default());
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(FIRST_CHECK).await;
            // Until first-run setup opens an instance, look again shortly.
            if handle.try_state::<super::Desktop>().is_none() {
                continue;
            }
            check(&handle, false).await;
            tokio::time::sleep(CHECK_INTERVAL - FIRST_CHECK).await;
        }
    });
}

/// Settings' Check for updates: reports the result even when there is nothing to install.
pub fn check_now(handle: &tauri::AppHandle) {
    if handle.try_state::<Updates>().is_none() {
        let app = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            desktop_controls::tell(
                desktop_controls::owner(&app),
                Purpose::Update,
                "This build of AgentFlow has no update server configured.",
            );
        });
        return;
    }
    let app = handle.clone();
    tauri::async_runtime::spawn(async move { check(&app, true).await });
}

async fn check(app: &tauri::AppHandle, manual: bool) {
    let Some(updates) = app.try_state::<Updates>() else { return };
    if app.try_state::<super::Desktop>().is_none() || updates.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let found = find(app).await;
    if let Ok(Some((update, _))) = &found
        && !manual
        && updates.declined.lock().unwrap_or_else(|p| p.into_inner()).as_deref() == Some(update.version.as_str())
    {
        updates.busy.store(false, Ordering::SeqCst);
        return;
    }
    let message = match found {
        Ok(Some((update, bytes))) => {
            let handle = app.clone();
            // Closing blocks on the engine's shutdown, which must not run inside an async task.
            if app.run_on_main_thread(move || offer(&handle, update, bytes)).is_err() {
                updates.busy.store(false, Ordering::SeqCst);
            }
            return;
        }
        Ok(None) => format!("AgentFlow {} is up to date.", app.package_info().version),
        Err(error) => {
            agentflow_core::diagnostics::event("update_check_failed", serde_json::json!({ "error": error }));
            format!("AgentFlow could not check for updates:\n\n{error}")
        }
    };
    updates.busy.store(false, Ordering::SeqCst);
    if manual {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            desktop_controls::tell(desktop_controls::owner(&handle), Purpose::Update, &message);
        });
    }
}

/// A newer version, already downloaded and its signature verified.
async fn find(app: &tauri::AppHandle) -> Result<Option<(Update, Vec<u8>)>, String> {
    let Some(update) = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    Ok(Some((update, bytes)))
}

fn offer(app: &tauri::AppHandle, update: Update, bytes: Vec<u8>) {
    let (Some(updates), Some(desktop)) = (app.try_state::<Updates>(), app.try_state::<super::Desktop>()) else {
        return;
    };
    let owner = desktop_controls::owner(app);
    // The installer restarts AgentFlow with this process's arguments. After Change instance
    // they would reopen setup, so such a session is reopened by hand instead.
    let restart = std::env::args_os().len() == 1;
    let notes = update.body.as_deref().map(str::trim).filter(|notes| !notes.is_empty());
    let message = format!(
        "AgentFlow {} is ready to install. You have {}.{}\n\nInstall it now? AgentFlow closes while the installer runs{}. Task history, settings and files are kept.\n\nChoose No to keep working; AgentFlow offers it again when you choose Check for updates in Settings.",
        update.version,
        update.current_version,
        notes.map(|notes| format!("\n\n{notes}")).unwrap_or_default(),
        if restart { " and then reopens" } else { "; reopen it from the Start menu afterwards" },
    );
    if !desktop_controls::ask(owner, Purpose::Update, &message) {
        *updates.declined.lock().unwrap_or_else(|p| p.into_inner()) = Some(update.version.clone());
        updates.busy.store(false, Ordering::SeqCst);
        return;
    }
    if !desktop_controls::close(app, &desktop, Purpose::Update) {
        updates.busy.store(false, Ordering::SeqCst);
        return;
    }
    // Agents are stopped and the local service is closed. On success the installer starts and
    // this process exits without returning.
    if let Err(error) = update.restart_after_install(restart).install(bytes) {
        desktop_controls::tell(
            owner,
            Purpose::Update,
            &format!("The update could not start:\n\n{error}\n\nAgentFlow will close. Reopen it to keep working."),
        );
    }
    app.exit(0);
}
