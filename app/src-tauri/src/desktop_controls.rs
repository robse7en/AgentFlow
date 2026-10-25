//! Native controls remain available when the local web UI loses its connection.
use std::sync::atomic::Ordering;
use tauri::Manager;

/// How long closing waits for cancelled verification commands to stop.
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Passed with this process's id to the setup it restarts into.
const WAIT_FLAG: &str = "--wait-for-pid";
/// How long a restarted setup waits for the previous process to exit.
const WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

pub fn reconnect(handle: &tauri::AppHandle, target: &tauri::Url) {
    let Some(desktop) = handle.try_state::<super::Desktop>() else { return };
    let code = agentflow_core::api::auth::token_urlsafe(24);
    desktop.app.human.lock().unwrap_or_else(|p| p.into_inner()).pairing_code = code.clone();
    let mut url = target.clone();
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(Some(&format!("pair={code}")));
    let app = handle.clone();
    let _ = handle.run_on_main_thread(move || {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.navigate(url);
        }
    });
}

/// Restarts into setup to choose another instance. Runs after the navigation handler returns,
/// where a close request's dialog and shutdown run, rather than inside it.
pub fn change_instance(handle: &tauri::AppHandle) {
    let app = handle.clone();
    let _ = handle.run_on_main_thread(move || {
        let Some(desktop) = app.try_state::<super::Desktop>() else { return };
        if !close(&app, &desktop, Purpose::ChangeInstance) {
            return;
        }
        // The new process waits for this one to exit; until then the single-instance check would
        // hand it to this window.
        let restarted = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe).args(["--setup", WAIT_FLAG, &std::process::id().to_string()]).spawn()
        });
        if let Err(error) = restarted {
            tell(owner(&app), Purpose::ChangeInstance, &format!(
                "AgentFlow could not reopen setup: {error}\n\nStart agentflow-desktop.exe --setup from the installation folder."
            ));
        }
        app.exit(0);
    });
}

/// Links that open a new window (`target="_blank"`, `window.open`) go to the default browser, where
/// the human is signed in to GitHub and Jira, rather than a WebView2 pop-up on this app's own
/// profile. Anything but http(s) is dropped, so a page cannot start other protocol handlers.
pub fn open_new_window(url: &tauri::Url) -> tauri::webview::NewWindowResponse<tauri::Wry> {
    if matches!(url.scheme(), "http" | "https") {
        open_in_browser(url.as_str());
    }
    tauri::webview::NewWindowResponse::Deny
}

#[cfg(windows)]
fn open_in_browser(url: &str) {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            window: Owner,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> Owner;
    }
    let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    let file: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    // SW_SHOWNORMAL. No shell in between, so nothing in the URL is interpreted as a command.
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), operation.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
    }
}

#[cfg(not(windows))]
fn open_in_browser(_url: &str) {}

/// In a setup restarted by `change_instance`, waits for the previous process to exit first.
pub fn wait_for_previous_instance() {
    let pid = std::env::args().skip_while(|arg| arg != WAIT_FLAG).nth(1).and_then(|pid| pid.parse().ok());
    if let Some(pid) = pid {
        wait_for_exit(pid);
    }
}

/// Why the main window's work is being stopped.
#[derive(Clone, Copy)]
pub enum Purpose {
    Quit,
    ChangeInstance,
    Update,
}

impl Purpose {
    fn title(self) -> &'static str {
        match self {
            Purpose::Quit => "Stop agents and quit AgentFlow?",
            Purpose::ChangeInstance => "Change AgentFlow instance?",
            Purpose::Update => "Update AgentFlow?",
        }
    }
}

/// Whether the main window may close. With agents or verifications running, asks first; then
/// stops background work and admits no new agent. The dialog disables the window, and close
/// requests sent to it anyway are queued until this one is answered; should the dialog's nested
/// message loop ever deliver one sooner, it is refused rather than re-entered.
pub fn close(app: &tauri::AppHandle, desktop: &super::Desktop, purpose: Purpose) -> bool {
    if desktop.closing.swap(true, Ordering::SeqCst) {
        return false;
    }
    let owner = owner(app);
    let allowed = confirm(desktop, owner, purpose) && stop(desktop, owner, purpose);
    if !allowed {
        desktop.closing.store(false, Ordering::SeqCst);
    }
    allowed
}

const CHANGE_INSTANCE: &str = "AgentFlow will restart and open setup, where you can choose another config.json or create a new instance.\n\nThis instance's settings, task history and files stay where they are. Close the setup window to keep using it.";

fn confirm(desktop: &super::Desktop, owner: Owner, purpose: Purpose) -> bool {
    let labels = match desktop.app.engine.active_work_labels() {
        Ok(labels) if labels.is_empty() => None,
        Ok(labels) => Some(labels.join("\n")),
        Err(_) => Some("Active work could not be checked. Agents may still be running.".into()),
    };
    let message = match (purpose, labels) {
        // The update offer has already asked.
        (Purpose::Quit | Purpose::Update, None) => return true,
        (Purpose::ChangeInstance, None) => CHANGE_INSTANCE.into(),
        // Agents in zellij sessions keep running, and Resume reattaches them once AgentFlow is
        // reopened; only verification commands stop.
        (purpose, Some(labels)) if desktop.embedded.is_none() => {
            let (title, action) = match purpose {
                Purpose::Quit => ("Quit AgentFlow?", "quit"),
                Purpose::ChangeInstance => (purpose.title(), "restart into setup to choose another instance"),
                Purpose::Update => (purpose.title(), "install the update"),
            };
            return ask_titled(
                owner,
                title,
                &format!(
                    "AgentFlow has active work:\n\n{labels}\n\nAgents keep running in their zellij sessions but cannot report progress while AgentFlow is closed. When it reopens, choose Resume on each interrupted step to reattach its agent. Running verification commands will be stopped.\n\nChoose Yes to {action}, or No to keep working."
                ),
            );
        }
        (Purpose::Quit, Some(labels)) => format!(
            "AgentFlow has active work:\n\n{labels}\n\nStop agents and quit?\n\nChoose No to keep working. Choose Yes to stop agents and quit. Task history and files will be retained."
        ),
        (Purpose::ChangeInstance, Some(labels)) => format!(
            "AgentFlow has active work:\n\n{labels}\n\nStop agents and restart into setup to choose another instance?\n\nChoose No to keep working. Task history and files will be retained."
        ),
        (Purpose::Update, Some(labels)) => format!(
            "AgentFlow has active work:\n\n{labels}\n\nStop agents and install the update?\n\nChoose No to keep working. Task history and files will be retained."
        ),
    };
    ask(owner, purpose, &message)
}

fn stop(desktop: &super::Desktop, owner: Owner, purpose: Purpose) -> bool {
    // Taken only after the dialog, so a second close request cannot block on it.
    let admission = desktop.app.engine.launch_lock.blocking_lock();
    if !tauri::async_runtime::block_on(desktop.app.engine.shutdown_and_wait(STOP_TIMEOUT)) {
        drop(admission);
        tell(owner, purpose, "Verification commands are still stopping. Try again in a moment.");
        return false;
    }
    // Held until the process exits: no agent may start after the terminal sweep below.
    std::mem::forget(admission);
    desktop.shutdown();
    true
}

pub type Owner = *mut std::ffi::c_void;

pub fn owner(app: &tauri::AppHandle) -> Owner {
    #[cfg(windows)]
    if let Some(hwnd) = app.get_webview_window("main").and_then(|window| window.hwnd().ok()) {
        return hwnd.0 as Owner;
    }
    let _ = app;
    std::ptr::null_mut()
}

#[cfg(windows)]
fn message_box(owner: Owner, title: &str, message: &str, flags: u32) -> i32 {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(window: Owner, text: *const u16, caption: *const u16, flags: u32) -> i32;
    }
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    // Owned by the main window, so the page cannot start agents while it is open.
    unsafe { MessageBoxW(owner, text.as_ptr(), title.as_ptr(), flags) }
}

pub fn ask(owner: Owner, purpose: Purpose, message: &str) -> bool {
    ask_titled(owner, purpose.title(), message)
}

#[cfg(windows)]
fn ask_titled(owner: Owner, title: &str, message: &str) -> bool {
    // YES/NO, warning icon, No selected by default. Failure also keeps the app open.
    message_box(owner, title, message, 0x4 | 0x30 | 0x100) == 6
}

#[cfg(windows)]
pub fn tell(owner: Owner, purpose: Purpose, message: &str) {
    message_box(owner, purpose.title(), message, 0x30);
}

#[cfg(windows)]
fn wait_for_exit(pid: u32) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Owner;
        fn WaitForSingleObject(handle: Owner, milliseconds: u32) -> u32;
        fn CloseHandle(handle: Owner) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    unsafe {
        // Null when the process has already exited.
        let process = OpenProcess(SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            WaitForSingleObject(process, WAIT_LIMIT.as_millis() as u32);
            CloseHandle(process);
        }
    }
}

#[cfg(not(windows))]
fn ask_titled(_owner: Owner, _title: &str, _message: &str) -> bool {
    // This package ships on Windows. Do not silently stop work on other platforms.
    false
}

#[cfg(not(windows))]
pub fn tell(_owner: Owner, _purpose: Purpose, _message: &str) {}

#[cfg(not(windows))]
fn wait_for_exit(_pid: u32) {}
