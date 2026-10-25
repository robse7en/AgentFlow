//! Subprocess helpers, ported from `backend/agentflow/services/process.py`.
//!
//! Every child gets a cleaned environment ([`clean_env`]): only the variables a CLI needs to run,
//! plus what the caller passes explicitly, so credentials in the backend's environment never
//! reach git, harnesses or verification commands.
//!
//! `spawn_hidden_console`, which only the zellij backend uses, is in [`hidden`] (Windows).

#[cfg(windows)]
pub mod hidden;
#[cfg(windows)]
pub mod job;
pub mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ProcessError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub argv: Vec<String>,
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandResult {
    pub fn ok(&self) -> bool {
        self.returncode == 0
    }

    /// The result, or an error naming `what` (default: the executable) with its output.
    pub fn check(self, what: Option<&str>) -> Result<CommandResult, ProcessError> {
        if self.ok() {
            return Ok(self);
        }
        let detail = if self.stderr.is_empty() { &self.stdout } else { &self.stderr };
        Err(ProcessError(format!(
            "{} failed (exit {}): {}",
            what.unwrap_or(&self.argv[0]),
            self.returncode,
            detail.trim()
        )))
    }
}

/// Environment variables a child may inherit from the backend (compared case-insensitively).
pub const SAFE_ENV_NAMES: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMDATA",
    "LANG",
    "LC_ALL",
    "TERM",
    "COLORTERM",
    "USERNAME",
    "USER",
    "LOGNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    // Lets the Claude usage collector find a relocated Copilot-proxy ledger.
    "COPILOT_API_HOME",
];

/// The backend's safe variables plus `extra` (which wins).
pub fn clean_env(extra: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> =
        std::env::vars().filter(|(name, _)| SAFE_ENV_NAMES.contains(&name.to_uppercase().as_str())).collect();
    env.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

/// The executable plus its leading subcommand words (`zellij action new-tab`). Stops at the first
/// flag or value, so arbitrary arguments never reach error messages.
pub fn describe(argv: &[String]) -> String {
    let mut words = vec![argv[0].as_str()];
    for arg in argv.iter().skip(1).take(2) {
        let mut chars = arg.chars();
        let word =
            chars.next().is_some_and(|c| c.is_ascii_lowercase()) && chars.all(|c| c.is_ascii_lowercase() || c == '-');
        if !word {
            break;
        }
        words.push(arg);
    }
    words.join(" ")
}

/// Locate an executable by path or on PATH (honouring PATHEXT on Windows); `~` and environment
/// variables in `command` are expanded first.
pub fn resolve_executable(command: &str) -> Option<PathBuf> {
    let expanded = expand_user(&expand_vars(command));
    let path = Path::new(&expanded);
    if path.is_absolute() || expanded.contains('/') || expanded.contains(std::path::MAIN_SEPARATOR) {
        if path.is_file() {
            return Some(path.to_path_buf());
        }
        if cfg!(windows) {
            return [".exe", ".cmd", ".bat", ".ps1"]
                .iter()
                .map(|ext| path.with_extension(&ext[1..]))
                .find(|candidate| candidate.is_file());
        }
        return None;
    }
    which(&expanded)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        let pathext: Vec<String> = pathext.split(';').filter(|e| !e.is_empty()).map(str::to_lowercase).collect();
        // A name that already has a PATHEXT extension is looked up as is; otherwise only with one.
        if pathext.iter().any(|ext| name.to_lowercase().ends_with(ext.as_str())) {
            vec![String::new()]
        } else {
            pathext
        }
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() && is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    true
}

/// `~` or `~/...` with the home directory (`os.path.expanduser`).
pub fn expand_user(value: &str) -> String {
    match value.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            std::env::home_dir().map_or_else(|| value.to_string(), |home| format!("{}{rest}", home.display()))
        }
        _ => value.to_string(),
    }
}

/// `$NAME`, `${NAME}` and (on Windows) `%NAME%`; unknown variables are left as written, as
/// Python's `os.path.expandvars` does.
fn expand_vars(value: &str) -> String {
    let lookup = |name: &str| std::env::var(name).ok();
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find(['$', '%']) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let (name, consumed) = if let Some(braced) = tail.strip_prefix("${") {
            braced.find('}').map_or((None, 1), |end| (Some(&braced[..end]), end + 3))
        } else if let Some(dollar) = tail.strip_prefix('$') {
            let end = dollar.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(dollar.len());
            if end == 0 { (None, 1) } else { (Some(&dollar[..end]), end + 1) }
        } else if cfg!(windows) {
            let percent = &tail[1..];
            percent
                .find('%')
                .map_or((None, 1), |end| if end == 0 { (None, 2) } else { (Some(&percent[..end]), end + 2) })
        } else {
            (None, 1)
        };
        match name.and_then(lookup) {
            Some(found) => out.push_str(&found),
            None => out.push_str(&tail[..consumed]),
        }
        rest = &tail[consumed..];
    }
    out.push_str(rest);
    out
}

/// Windows: no console window for a captured child. A GUI-subsystem app (the Tauri shell) would
/// otherwise flash one for every git or verification command.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Run a command to completion, capturing its output. On timeout the process is killed and the
/// error names the command without its arguments.
pub async fn run(
    argv: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
    timeout: Duration,
    input: Option<&str>,
) -> Result<CommandResult, ProcessError> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(clean_env(env))
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|e| ProcessError(format!("{}: cannot start: {e}", describe(argv))))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        // Written concurrently with reading output, so a large input cannot deadlock on a full pipe.
        let text = text.as_bytes().to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&text).await;
        });
    }
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(CommandResult {
            argv: argv.to_vec(),
            returncode: exit_code(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Ok(Err(e)) => Err(ProcessError(format!("{}: {e}", describe(argv)))),
        // Dropping the future dropped the child, which kill_on_drop kills.
        Err(_) => Err(ProcessError(format!("{} timed out after {:?}s", describe(argv), timeout.as_secs_f64()))),
    }
}

/// The exit code, or (POSIX) minus the signal that killed the process, as Python reports it. Never
/// 0 for a killed process: a verification command killed by a signal must not count as passed.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    status.code().unwrap_or(-1)
}

/// Windows creation flags for [`spawn_detached`]: its own process group, optionally without the
/// backend's console, and optionally outside the job the backend runs in (a terminal's job would
/// otherwise kill it when that terminal closes, if the job allows breakaway).
#[cfg(windows)]
pub fn detached_flags(detach_from_console: bool, breakaway_from_job: bool) -> u32 {
    let mut flags = CREATE_NEW_PROCESS_GROUP;
    if detach_from_console {
        flags |= DETACHED_PROCESS;
    }
    if breakaway_from_job {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    flags
}

/// Start a background process with no stdio, in its own process group (session on POSIX).
pub fn spawn_detached(
    argv: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
    detach_from_console: bool,
    breakaway_from_job: bool,
) -> Result<Child, ProcessError> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(clean_env(env))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    command.creation_flags(detached_flags(detach_from_console, breakaway_from_job));
    #[cfg(unix)]
    {
        let _ = (detach_from_console, breakaway_from_job);
        command.process_group(0);
    }
    let spawned = command.spawn();
    // A hosting job that forbids breakaway refuses the flag (see hidden::spawn_hidden_console);
    // starting inside that job beats not starting.
    #[cfg(windows)]
    let spawned = match spawned {
        Err(error) if breakaway_from_job && error.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!("cannot break away from the hosting job; {} will end with it", describe(argv));
            command.creation_flags(detached_flags(detach_from_console, false)).spawn()
        }
        other => other,
    };
    spawned.map_err(|e| ProcessError(format!("{}: cannot start: {e}", describe(argv))))
}

/// Start a child in its own process group with a clean environment, stdout piped and no other
/// stdio (a usage collector). Stop it with [`terminate_process_tree`].
pub fn spawn_captured_group(argv: &[String], cwd: &Path, env: &HashMap<String, String>) -> Result<Child, ProcessError> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(clean_env(env))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    #[cfg(unix)]
    command.process_group(0);
    command.spawn().map_err(|e| ProcessError(format!("{}: cannot start: {e}", describe(argv))))
}

/// Verification shares one pipe between stdout and stderr, retaining their write order.
pub fn spawn_verification(argv: &[String], cwd: &Path) -> Result<(Child, tokio::fs::File), ProcessError> {
    let (reader, writer) = std::io::pipe().map_err(|e| ProcessError(e.to_string()))?;
    let stderr = writer.try_clone().map_err(|e| ProcessError(e.to_string()))?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(clean_env(&HashMap::new()))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(stderr)
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn().map_err(|e| ProcessError(e.to_string()))?;
    // Command retains its copies of the writers; close them so EOF follows the child's exit.
    drop(command);
    #[cfg(windows)]
    let file = std::fs::File::from(std::os::windows::io::OwnedHandle::from(reader));
    #[cfg(unix)]
    let file = std::fs::File::from(std::os::fd::OwnedFd::from(reader));
    Ok((child, tokio::fs::File::from_std(file)))
}

/// Stop a process and everything it started (our own verifier or usage collector).
pub async fn terminate_process_tree(child: &mut Child) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    let Some(pid) = child.id() else { return };
    #[cfg(windows)]
    {
        let killer = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .status();
        let _ = tokio::time::timeout(Duration::from_secs(10), killer).await;
        if let Ok(None) = child.try_wait() {
            let _ = child.start_kill();
        }
    }
    #[cfg(unix)]
    {
        // The child leads its own process group (spawn_detached), so this reaches all of it.
        let _ = std::process::Command::new("kill").args(["-KILL", &format!("-{pid}")]).status();
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn test_process_errors_name_the_subcommand_but_not_its_arguments() {
        assert_eq!(describe(&argv(&["zellij", "action", "new-tab", "--name", "Architect"])), "zellij action new-tab");
        assert_eq!(describe(&argv(&["zellij", "list-sessions", "--no-formatting"])), "zellij list-sessions");
        assert_eq!(describe(&argv(&["git", "-C", "D:/repo", "status"])), "git");
    }

    #[test]
    fn clean_env_drops_unknown_variables_and_keeps_explicit_ones() {
        // SAFETY: tests in this module do not read these variables concurrently.
        unsafe {
            std::env::set_var("UNRELATED_API_TOKEN", "secret");
            std::env::set_var("AGENTFLOW_SLACK_BOT_TOKEN", "xoxb-secret");
        }
        let env = clean_env(&HashMap::from([("EXPLICIT_TOKEN".to_string(), "synthetic".to_string())]));
        assert!(!env.contains_key("UNRELATED_API_TOKEN"));
        assert!(!env.keys().any(|k| k.starts_with("AGENTFLOW_SLACK")));
        assert_eq!(env["EXPLICIT_TOKEN"], "synthetic");
        assert!(env.keys().any(|k| k.eq_ignore_ascii_case("PATH")));
    }

    #[cfg(windows)]
    #[test]
    fn detached_flags_match_the_python_backend() {
        assert_eq!(detached_flags(true, true), 0x0000_0200 | 0x0000_0008 | 0x0100_0000);
        assert_eq!(detached_flags(false, false), 0x0000_0200);
    }

    #[test]
    fn resolves_executables_on_path_and_by_path() {
        let git = resolve_executable("git").expect("git on PATH");
        assert!(git.is_file());
        assert_eq!(resolve_executable(&git.to_string_lossy()), Some(git.clone()));
        assert_eq!(resolve_executable("no-such-tool-agentflow"), None);
        #[cfg(windows)]
        {
            // A path without its extension finds the .exe, as the Python backend does.
            let bare = git.with_extension("");
            assert_eq!(resolve_executable(&bare.to_string_lossy()), Some(git));
        }
    }

    #[test]
    fn expands_variables_like_python() {
        // SAFETY: see above.
        unsafe { std::env::set_var("AGENTFLOW_TEST_DIR", "x") };
        assert_eq!(expand_vars("$AGENTFLOW_TEST_DIR/a"), "x/a");
        assert_eq!(expand_vars("${AGENTFLOW_TEST_DIR}/a"), "x/a");
        assert_eq!(expand_vars("$NO_SUCH_VAR_AF/a"), "$NO_SUCH_VAR_AF/a");
        assert_eq!(expand_vars("100$"), "100$");
        #[cfg(windows)]
        assert_eq!(expand_vars("%AGENTFLOW_TEST_DIR%\\a and 50%"), "x\\a and 50%");
    }

    #[tokio::test]
    async fn run_captures_output_exit_code_and_input() {
        let echo = if cfg!(windows) { argv(&["cmd", "/c", "findstr", "x"]) } else { argv(&["grep", "x"]) };
        let result = run(&echo, None, &HashMap::new(), Duration::from_secs(30), Some("axb\nnope\n")).await.unwrap();
        assert!(result.ok());
        assert_eq!(result.stdout.trim(), "axb");
        let missing = run(&echo, None, &HashMap::new(), Duration::from_secs(30), Some("nope\n")).await.unwrap();
        assert_eq!(missing.returncode, 1);
        let error = missing.check(Some("search")).unwrap_err().to_string();
        assert!(error.starts_with("search failed (exit 1)"), "{error}");
    }

    #[tokio::test]
    async fn run_times_out_naming_only_the_command() {
        let sleep = if cfg!(windows) { argv(&["ping", "-n", "30", "127.0.0.1"]) } else { argv(&["sleep", "30"]) };
        let error = run(&sleep, None, &HashMap::new(), Duration::from_millis(300), None).await.unwrap_err();
        assert_eq!(error.0, format!("{} timed out after 0.3s", sleep[0]));
    }
}
