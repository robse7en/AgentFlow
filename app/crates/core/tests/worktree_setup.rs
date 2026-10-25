//! Worktree setup against real Git: what is copied, what is refused, and how commands end
//! (engine::worktree_setup::run_setup). The engine's gating is in engine_worktree_setup.rs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agentflow_core::config::WorktreeSetup;
use agentflow_core::engine::worktree_setup::run_setup;
use agentflow_core::git::GitService;

struct Fixture {
    _dir: tempfile::TempDir,
    folder: PathBuf,
    repo: PathBuf,
    tree: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(cwd).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

impl Fixture {
    /// A repository ignoring `.env`, `local/`, `out.txt`, `linked`, `nested/` and `secret.json`
    /// (which is nevertheless tracked), and a worktree of it.
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let folder = agentflow_core::paths::resolve(dir.path());
        let repo = folder.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let ignore = folder.join("ignore");
        std::fs::write(&ignore, "").unwrap();
        for (key, value) in [
            ("core.excludesFile", ignore.to_string_lossy().into_owned()),
            ("core.hooksPath", folder.join("hooks").to_string_lossy().into_owned()),
            ("commit.gpgSign", "false".into()),
            ("user.name", "Test".into()),
            ("user.email", "test@localhost".into()),
        ] {
            git(&repo, &["config", key, &value]);
        }
        std::fs::write(repo.join(".gitignore"), ".env\nlocal/\nout.txt\nlinked\nnested/\nsecret.json\n").unwrap();
        std::fs::write(repo.join("file.txt"), "preserved\n").unwrap();
        std::fs::write(repo.join("secret.json"), "{}\n").unwrap();
        git(&repo, &["add", ".gitignore", "file.txt"]);
        git(&repo, &["add", "-f", "secret.json"]);
        git(&repo, &["commit", "-qm", "fixture"]);
        let tree = folder.join("tree");
        git(&repo, &["worktree", "add", "-q", "-b", "task", tree.to_str().unwrap()]);
        Fixture { _dir: dir, folder, repo, tree }
    }

    async fn run(&self, setup: &WorktreeSetup) -> Result<(), String> {
        let (_cancel, cancelled) = tokio::sync::watch::channel(false);
        self.run_with(setup, cancelled).await
    }

    async fn run_with(
        &self,
        setup: &WorktreeSetup,
        cancelled: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), String> {
        run_setup(&GitService::default(), &self.repo, &self.tree, setup, &self.log(), cancelled).await
    }

    fn log(&self) -> PathBuf {
        self.folder.join("setup.log")
    }
}

fn shell(script: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".into(), "/c".into(), script.into()]
    } else {
        vec!["sh".into(), "-c".into(), script.into()]
    }
}

fn copying(paths: &[&str]) -> WorktreeSetup {
    WorktreeSetup { copy: paths.iter().map(|p| p.to_string()).collect(), ..WorktreeSetup::default() }
}

fn running(commands: Vec<Vec<String>>) -> WorktreeSetup {
    WorktreeSetup { commands, ..WorktreeSetup::default() }
}

/// A directory link: a junction on Windows (no privilege needed), a symlink elsewhere.
fn link_directory(link: &Path, target: &Path) {
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[tokio::test]
async fn copies_ignored_files_and_directories_then_runs_commands() {
    let f = Fixture::new();
    std::fs::write(f.repo.join(".env"), "TOKEN=local\n").unwrap();
    std::fs::create_dir_all(f.repo.join("local").join("deep")).unwrap();
    std::fs::write(f.repo.join("local").join("deep").join("settings.json"), "{\"a\": 1}\n").unwrap();
    let setup = WorktreeSetup {
        copy: vec![".env".into(), "./local".into()],
        commands: vec![shell("echo built> out.txt")],
        timeout_seconds: 60,
    };

    f.run(&setup).await.unwrap();

    assert_eq!(std::fs::read_to_string(f.tree.join(".env")).unwrap(), "TOKEN=local\n");
    assert_eq!(
        std::fs::read_to_string(f.tree.join("local").join("deep").join("settings.json")).unwrap(),
        "{\"a\": 1}\n"
    );
    assert!(f.tree.join("out.txt").is_file());
    let log = std::fs::read_to_string(f.log()).unwrap();
    assert!(log.contains("Copied .env") && log.contains("Copied ./local") && log.contains("$ "), "{log}");
    // The copies are independent of the checkout.
    std::fs::write(f.tree.join(".env"), "TOKEN=changed\n").unwrap();
    assert_eq!(std::fs::read_to_string(f.repo.join(".env")).unwrap(), "TOKEN=local\n");
    // Running again over its own output is fine.
    f.run(&setup).await.unwrap();
}

#[tokio::test]
async fn only_ignored_untracked_destinations_are_copied() {
    let f = Fixture::new();
    std::fs::write(f.repo.join("notes.txt"), "not ignored\n").unwrap();

    let error = f.run(&copying(&["notes.txt"])).await.unwrap_err();
    assert!(error.contains("notes.txt is not ignored"), "{error}");
    assert!(!f.tree.join("notes.txt").exists());

    // Ignored by a pattern but tracked: copying would replace a committed file.
    std::fs::write(f.repo.join("secret.json"), "{\"local\": true}\n").unwrap();
    let error = f.run(&copying(&["secret.json"])).await.unwrap_err();
    assert!(error.contains("secret.json is not ignored"), "{error}");
    assert_eq!(std::fs::read_to_string(f.tree.join("secret.json")).unwrap().trim(), "{}");

    let error = f.run(&copying(&[".env"])).await.unwrap_err();
    assert_eq!(error, "Setup copy source is missing: .env");
    let error = f.run(&copying(&["../repo/.env"])).await.unwrap_err();
    assert!(error.contains("stay inside the repository"), "{error}");
}

#[tokio::test]
async fn links_never_lead_outside_the_repository() {
    let f = Fixture::new();
    let outside = f.folder.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("credentials"), "secret\n").unwrap();

    link_directory(&f.repo.join("linked"), &outside);
    let error = f.run(&copying(&["linked"])).await.unwrap_err();
    assert_eq!(error, "Setup copy source linked resolves outside the repository");

    std::fs::create_dir(f.repo.join("nested")).unwrap();
    std::fs::write(f.repo.join("nested").join("plain.txt"), "fine\n").unwrap();
    link_directory(&f.repo.join("nested").join("inner"), &outside);
    let error = f.run(&copying(&["nested"])).await.unwrap_err();
    assert!(error.contains("links are not copied"), "{error}");
    assert!(!f.tree.join("nested").join("inner").join("credentials").exists());

    // A destination directory that is itself a link out of the worktree.
    std::fs::create_dir(f.repo.join("local")).unwrap();
    std::fs::write(f.repo.join("local").join("x.txt"), "x\n").unwrap();
    link_directory(&f.tree.join("local"), &outside);
    let error = f.run(&copying(&["local/x.txt"])).await.unwrap_err();
    assert_eq!(error, "Setup copy destination local/x.txt resolves outside the worktree");
    assert!(!outside.join("x.txt").exists());
}

#[tokio::test]
async fn a_failing_command_or_a_changed_tree_fails_setup() {
    let f = Fixture::new();
    let error = f.run(&running(vec![shell("exit 3")])).await.unwrap_err();
    assert!(error.ends_with("exited with code 3"), "{error}");
    assert!(std::fs::read_to_string(f.log()).unwrap().contains("Failed: "));

    let error = f.run(&running(vec![shell("echo changed> file.txt")])).await.unwrap_err();
    assert!(error.starts_with("Setup changed files that Git tracks or does not ignore (file.txt)"), "{error}");

    let error = f.run(&running(vec![vec!["agentflow-no-such-program".into()]])).await.unwrap_err();
    assert_eq!(error, "`agentflow-no-such-program` could not be started");
}

#[tokio::test]
async fn commands_stop_at_the_timeout_or_on_cancel() {
    let f = Fixture::new();
    let slow = if cfg!(windows) { shell("ping -n 30 127.0.0.1 > nul") } else { shell("sleep 30") };
    let started = std::time::Instant::now();
    let error = f.run(&WorktreeSetup { commands: vec![slow.clone()], timeout_seconds: 1, ..Default::default() }).await;
    assert!(error.unwrap_err().ends_with("did not finish within 1 seconds"));
    assert!(started.elapsed() < Duration::from_secs(20));

    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.send_replace(true);
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(cancel);
    });
    let started = std::time::Instant::now();
    assert_eq!(f.run_with(&running(vec![slow]), cancelled).await.unwrap_err(), "Cancelled");
    assert!(started.elapsed() < Duration::from_secs(20));
}
