//! `spawn_hidden_console` from `backend/agentflow/services/process.py` (Windows only): start a
//! process in its own new console whose window is never shown. Only the zellij backend needs it.
//!
//! Unlike [`super::spawn_detached`], stdio is left unredirected so the child uses its new console's
//! own handles: a zellij server started with NUL std handles passes them on to its first pane's
//! shell, which reads EOF and exits at once; the server then fails to reap it ("The parameter is
//! incorrect", os error 87) and dies within a second. The process also breaks away from the
//! hosting job, so it outlives whatever runs the backend.
//!
//! std's `Command` cannot hide the new console's window (`show_window` is unstable), so this calls
//! `CreateProcessW` itself, with the flags and STARTUPINFO Python's subprocess uses.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, STILL_ACTIVE};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT,
    CreateProcessW, GetExitCodeProcess, PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW, TerminateProcess,
};

use super::{ProcessError, clean_env};

/// What to run in the hidden console.
pub enum HiddenCommand<'a> {
    /// An executable and its arguments, quoted as Windows programs parse them.
    Argv(&'a [String]),
    /// A `cmd /c` line, which a `mode con ... & ...` chain needs. Passed to cmd as written (Python's
    /// shell mode), so an inner quoted path is not re-quoted into something cmd can't parse.
    Shell(&'a str),
}

/// A process started by [`spawn_hidden_console`]. Dropping it closes the handle; the process keeps
/// running.
pub struct HiddenConsole {
    process: HANDLE,
    pub pid: u32,
}

// SAFETY: a process handle may be used and closed from any thread.
unsafe impl Send for HiddenConsole {}
unsafe impl Sync for HiddenConsole {}

pub fn spawn_hidden_console(
    command: HiddenCommand<'_>,
    env: &HashMap<String, String>,
) -> Result<HiddenConsole, ProcessError> {
    let (application, command_line) = match command {
        HiddenCommand::Argv(argv) => {
            if argv.is_empty() {
                return Err(ProcessError("empty command".into()));
            }
            (None, argv.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" "))
        }
        HiddenCommand::Shell(line) => {
            let comspec = comspec();
            let command_line = format!("{comspec} /c \"{line}\"");
            (Some(comspec), command_line)
        }
    };
    let described = match command {
        HiddenCommand::Argv(argv) => super::describe(argv),
        HiddenCommand::Shell(_) => "cmd /c".to_string(),
    };
    let application: Option<Vec<u16>> = application.map(|a| wide(&a));
    let environment = environment_block(&clean_env(env));
    let flags = creation_flags(false);
    match create(application.as_deref(), &command_line, &environment, creation_flags(true)) {
        // The job hosting this process forbids breakaway (cargo runs tests in such a job; so can a
        // launcher). Python failed here; staying in that job is the only way to start at all, at
        // the cost of the process ending with the job.
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            tracing::warn!("cannot break away from the hosting job; {described} will end with it");
            create(application.as_deref(), &command_line, &environment, flags)
        }
        result => result,
    }
    .map_err(|error| ProcessError(format!("{described}: cannot start: {error}")))
}

/// The process creation flags: a new console in its own process group, outside the hosting job when
/// `breakaway` (Python's subprocess adds the Unicode environment flag itself).
pub fn creation_flags(breakaway: bool) -> u32 {
    let flags = CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
    if breakaway { flags | CREATE_BREAKAWAY_FROM_JOB } else { flags }
}

/// STARTUPINFO's (dwFlags, wShowWindow): the window is hidden, and stdio is left alone (no
/// STARTF_USESTDHANDLES), so the child uses its new console's own handles.
pub fn startup_window() -> (u32, u16) {
    (STARTF_USESHOWWINDOW, 0) // SW_HIDE
}

fn create(
    application: Option<&[u16]>,
    command_line: &str,
    environment: &[u16],
    flags: u32,
) -> std::io::Result<HiddenConsole> {
    let mut command_line = wide(command_line);
    // SAFETY: every pointer is to a live, NUL-terminated buffer or a zeroed, sized structure; the
    // thread handle is closed and the process handle is owned by the returned HiddenConsole.
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        (startup.dwFlags, startup.wShowWindow) = startup_window();
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            application.map_or(std::ptr::null(), <[u16]>::as_ptr),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            environment.as_ptr().cast(),
            std::ptr::null(),
            &startup,
            &mut info,
        );
        if created == 0 {
            return Err(std::io::Error::last_os_error());
        }
        CloseHandle(info.hThread);
        Ok(HiddenConsole { process: info.hProcess, pid: info.dwProcessId })
    }
}

impl HiddenConsole {
    /// The exit code, or None while the process runs (Python's `returncode`).
    pub fn exit_code(&self) -> Option<i64> {
        let mut code = 0u32;
        // SAFETY: the handle is valid for the lifetime of self.
        let ok = unsafe { GetExitCodeProcess(self.process, &mut code) };
        if ok == 0 || code == STILL_ACTIVE as u32 {
            return None;
        }
        Some(i64::from(code))
    }

    /// Wait up to `timeout` for the process to exit; whether it did.
    pub async fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.exit_code().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Stop the process and everything it started, as `terminate_process_tree` does.
    pub async fn terminate_tree(&self) {
        if self.exit_code().is_some() {
            return;
        }
        let argv: Vec<String> = ["taskkill", "/PID", &self.pid.to_string(), "/T", "/F"].map(String::from).into();
        let _ = super::run(&argv, None, &HashMap::new(), Duration::from_secs(10), None).await;
        if self.exit_code().is_none() {
            // SAFETY: the handle is valid for the lifetime of self.
            unsafe {
                TerminateProcess(self.process, 1);
            }
        }
        self.wait(Duration::from_secs(5)).await;
    }
}

impl Drop for HiddenConsole {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateProcessW and is closed exactly once.
        unsafe {
            CloseHandle(self.process);
        }
    }
}

/// `%ComSpec%` when it names an absolute path, else the system's cmd.exe (as Python resolves it).
fn comspec() -> String {
    std::env::var("ComSpec").ok().filter(|c| std::path::Path::new(c).is_absolute()).unwrap_or_else(|| {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        format!(r"{root}\System32\cmd.exe")
    })
}

/// One argument quoted exactly as Python's `subprocess.list2cmdline` quotes it (the MSVC runtime's
/// rules): surrounding quotes only for a space, a tab or an empty argument; a `"` is escaped
/// with a backslash, and backslashes are doubled only where they precede a quote.
pub fn quote_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty() || arg.contains([' ', '\t']);
    let mut out = String::new();
    if needs_quotes {
        out.push('"');
    }
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(if needs_quotes { backslashes * 2 } else { backslashes }));
    if needs_quotes {
        out.push('"');
    }
    out
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `NAME=value\0...\0\0`, sorted case-insensitively as Windows expects.
fn environment_block(env: &HashMap<String, String>) -> Vec<u16> {
    let mut entries: Vec<(&String, &String)> = env.iter().collect();
    entries.sort_by_key(|(name, _)| name.to_uppercase());
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(format!("{name}={value}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_like_list2cmdline() {
        // Each pair is (argument, subprocess.list2cmdline([argument])).
        let cases = [
            ("plain", "plain"),
            ("", "\"\""),
            ("two words", "\"two words\""),
            (r"C:\Program Files\zellij\zellij.exe", r#""C:\Program Files\zellij\zellij.exe""#),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r"trailing\ space\", r#""trailing\ space\\""#),
            (r"no\space\", r"no\space\"),
            (r#"a"b"#, r#"a\"b"#),
            (r#"a\"b"#, r#"a\\\"b"#),
        ];
        for (arg, expected) in cases {
            assert_eq!(quote_arg(arg), expected, "{arg}");
        }
    }

    #[tokio::test]
    async fn runs_hidden_and_reports_the_exit_code() {
        let argv: Vec<String> = ["cmd", "/c", "exit 5"].map(String::from).into();
        let child = spawn_hidden_console(HiddenCommand::Argv(&argv), &HashMap::new()).unwrap();
        assert!(child.wait(Duration::from_secs(10)).await);
        assert_eq!(child.exit_code(), Some(5));
    }

    #[tokio::test]
    async fn a_shell_line_runs_through_cmd_with_its_quotes_intact() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out file.txt");
        let line = format!("mode con: cols=120 lines=30 >nul & echo ran> \"{}\"", out.display());
        let child = spawn_hidden_console(HiddenCommand::Shell(&line), &HashMap::new()).unwrap();
        assert!(child.wait(Duration::from_secs(10)).await);
        assert_eq!(child.exit_code(), Some(0));
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "ran");
    }

    #[tokio::test]
    async fn terminate_tree_stops_a_running_process() {
        let argv: Vec<String> = ["ping", "-n", "60", "127.0.0.1"].map(String::from).into();
        let child = spawn_hidden_console(HiddenCommand::Argv(&argv), &HashMap::new()).unwrap();
        assert_eq!(child.exit_code(), None);
        child.terminate_tree().await;
        assert!(child.exit_code().is_some());
    }
}
