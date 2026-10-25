//! Real processes: stop_processes_in kills only what works inside the given directory.
//! Port of backend/tests/test_worktree_processes.py (same test names), plus the guards the Rust
//! version adds.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use agentflow_core::process::worktree::stop_processes_in;

/// A single-process sleeper whose own pid holds `cwd` (no launcher that forks a child).
fn sleeper(cwd: &Path) -> Child {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("ping");
        c.args(["-n", "120", "127.0.0.1"]);
        c
    } else {
        let mut c = Command::new("sleep");
        c.arg("120");
        c
    };
    command.current_dir(cwd).stdout(Stdio::null()).spawn().unwrap()
}

struct Folders {
    _root: tempfile::TempDir,
    worktree: PathBuf,
    inside: PathBuf,
    outside: PathBuf,
}

fn folders() -> Folders {
    let root = tempfile::tempdir().unwrap();
    let base = agentflow_core::paths::resolve(root.path());
    let (worktree, outside) = (base.join("DE-1"), base.join("DE-10"));
    let inside = worktree.join("nested");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    Folders { _root: root, worktree, inside, outside }
}

#[cfg(windows)]
#[tokio::test]
async fn test_kills_processes_working_inside_the_directory_only() {
    let f = folders();
    let mut held = sleeper(&f.inside);
    // A sibling whose name shares the prefix: must survive.
    let mut neighbour = sleeper(&f.outside);
    let stopped = stop_processes_in(&f.worktree).await;
    let neighbour_alive = neighbour.try_wait().unwrap().is_none();
    let _ = held.kill();
    let _ = neighbour.kill();
    let _ = held.wait();
    let _ = neighbour.wait();

    let stopped = stopped.unwrap();
    assert_eq!(stopped.iter().map(|p| p.pid).collect::<Vec<_>>(), [held.id()]);
    assert!(agentflow_core::paths::same_path(&stopped[0].cwd, &f.inside));
    assert!(neighbour_alive, "a process in the sibling DE-10 was killed");
    std::fs::remove_dir_all(&f.worktree).expect("nothing holds the worktree any more");
}

/// A process started in the worktree's 8.3 short form (which the port accepts as a worktree
/// path) is still found when cleanup passes the long form.
#[cfg(windows)]
#[tokio::test]
async fn kills_a_process_that_entered_through_an_8_3_short_name() {
    let f = folders();
    let long = f.worktree.join("a long nested directory");
    std::fs::create_dir(&long).unwrap();
    let output = Command::new("cmd")
        .args(["/c", "for", "%I", "in"])
        .arg(format!("(\"{}\")", long.display()))
        .args(["do", "@echo", "%~sI"])
        .output()
        .unwrap();
    let short = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if short.is_empty() || short.eq_ignore_ascii_case(&long.to_string_lossy()) {
        eprintln!("no 8.3 name on this volume; skipped");
        return;
    }
    assert!(short.contains('~'), "expected a short form, got {short}");
    let mut held = sleeper(Path::new(&short));
    let mut neighbour = sleeper(&f.outside);
    let stopped = stop_processes_in(&f.worktree).await;
    let neighbour_alive = neighbour.try_wait().unwrap().is_none();
    let _ = held.kill();
    let _ = neighbour.kill();
    let _ = held.wait();
    let _ = neighbour.wait();

    let stopped = stopped.unwrap();
    assert_eq!(stopped.iter().map(|p| p.pid).collect::<Vec<_>>(), [held.id()]);
    // The PEB kept the short spelling, so only the resolved comparison could have matched it.
    assert!(stopped[0].cwd.contains('~'), "the process recorded {}", stopped[0].cwd);
    assert!(neighbour_alive, "a process in the sibling DE-10 was killed");
}

#[tokio::test]
async fn test_finds_nothing_in_an_unused_directory() {
    let f = folders();
    assert_eq!(stop_processes_in(&f.worktree).await.unwrap(), []);
}

#[tokio::test]
async fn refuses_a_filesystem_root() {
    let root = std::env::temp_dir().ancestors().last().unwrap().to_path_buf();
    let error = stop_processes_in(&root).await.unwrap_err().to_string();
    assert!(error.contains("filesystem root"), "{error}");
}

#[tokio::test]
async fn refuses_a_missing_directory_or_a_file() {
    let f = folders();
    assert!(stop_processes_in(&f.worktree.join("missing")).await.is_err());
    let file = f.worktree.join("file.txt");
    std::fs::write(&file, "x").unwrap();
    assert!(stop_processes_in(&file).await.is_err());
}

// ------------------------------------------------------------------------------ test_questions_terminal.py

/// A window opened on a session breaks away from the hosting job, so it survives the app's host.
#[cfg(windows)]
#[test]
fn test_spawn_detached_can_break_away_from_the_hosting_job() {
    assert_eq!(agentflow_core::process::detached_flags(true, true), 0x0000_0200 | 0x0000_0008 | 0x0100_0000);
}

#[cfg(windows)]
#[test]
fn test_detached_process_can_keep_windows_console_association() {
    assert_eq!(agentflow_core::process::detached_flags(false, false), 0x0000_0200);
}

/// NUL std handles make zellij's first pane exit and take the server down with it, so the hidden
/// console's stdio is left alone; only the window is hidden.
#[cfg(windows)]
#[test]
fn test_spawn_hidden_console_hides_the_window_and_leaves_stdio_alone() {
    use agentflow_core::process::hidden::{creation_flags, startup_window};
    const NEW_CONSOLE: u32 = 0x10;
    const NEW_PROCESS_GROUP: u32 = 0x200;
    const BREAKAWAY: u32 = 0x0100_0000;
    const UNICODE_ENVIRONMENT: u32 = 0x400;
    assert_eq!(creation_flags(true), NEW_CONSOLE | NEW_PROCESS_GROUP | BREAKAWAY | UNICODE_ENVIRONMENT);
    assert_eq!(creation_flags(false), NEW_CONSOLE | NEW_PROCESS_GROUP | UNICODE_ENVIRONMENT);
    const STARTF_USESHOWWINDOW: u32 = 0x1;
    assert_eq!(startup_window(), (STARTF_USESHOWWINDOW, 0), "hidden, and no STARTF_USESTDHANDLES");
}
