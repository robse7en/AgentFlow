//! Independently execute configured argv with bounded output and process-tree cancellation.
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

const LIMIT: usize = 16000;
const HEAD: usize = 2000;
const TRUNCATED: &[u8] = b"\n[... output truncated: only the start and the end are kept ...]\n";

fn bounded(head: &[u8], tail: &[u8], truncated: bool) -> String {
    if !truncated {
        return String::from_utf8_lossy(head).into_owned();
    }
    let keep = LIMIT - HEAD - TRUNCATED.len();
    let bytes = [&head[..HEAD], TRUNCATED, &tail[tail.len().saturating_sub(keep)..]].concat();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Cancellation is observed while draining and waiting, and kills descendants before returning.
pub async fn verify_commands(
    commands: &[Vec<String>],
    cwd: &Path,
    sha: &str,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Value, String> {
    let mut results = Vec::new();
    for argv in commands {
        if argv.is_empty() || argv.iter().any(String::is_empty) {
            return Err("Verification commands must be nonempty argv arrays".into());
        }
        match run_command(argv, cwd, Duration::from_secs(300), &mut cancel).await {
            Ok((code, output)) => results.push(json!({"command": argv, "exitCode": code, "output": output})),
            Err(error) if error == CANCELLED => return Err("Independent verification was cancelled".into()),
            Err(error) => return Err(error),
        }
    }
    let passed = !results.is_empty() && results.iter().all(|r| r["exitCode"] == 0);
    Ok(json!({"sha": sha, "commands": results, "passed": passed}))
}

/// What [`run_command`] returns when `cancel` fired (or its sender is gone).
pub const CANCELLED: &str = "Cancelled";

/// Run one argv without a shell, with the allowlisted environment, stdout and stderr merged and
/// kept within [`LIMIT`]. Returns the exit code and output, or `OSError`, `TimeoutError` or
/// [`CANCELLED`], after stopping the whole process tree.
pub async fn run_command(
    argv: &[String],
    cwd: &Path,
    timeout: Duration,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(i32, String), String> {
    if *cancel.borrow() {
        return Err(CANCELLED.into());
    }
    #[cfg(windows)]
    let job = crate::process::job::Job::kill_on_close().map_err(|_| "OSError".to_string())?;
    let (mut child, mut stdout) = crate::process::spawn_verification(argv, cwd).map_err(|_| "OSError".to_string())?;
    #[cfg(windows)]
    if let Some(pid) = child.id()
        && job.assign(pid).is_err()
    {
        crate::process::terminate_process_tree(&mut child).await;
        return Err("OSError".into());
    }
    let drain = async {
        let mut head = Vec::new();
        let mut tail = Vec::new();
        let mut truncated = false;
        let mut chunk = [0u8; 8192];
        loop {
            let n = stdout.read(&mut chunk).await.map_err(|_| "OSError")?;
            if n == 0 {
                break;
            }
            truncated |= head.len() + n > LIMIT;
            head.extend_from_slice(&chunk[..n.min(LIMIT - head.len())]);
            tail.extend_from_slice(&chunk[..n]);
            if tail.len() > LIMIT {
                tail.drain(..tail.len() - LIMIT);
            }
        }
        let code = child.wait().await.map_err(|_| "OSError")?.code().unwrap_or(-1);
        Ok::<_, String>((code, bounded(&head, &tail, truncated)))
    };
    let outcome = tokio::select! {
        result = tokio::time::timeout(timeout, drain) => {
            result.unwrap_or_else(|_| Err("TimeoutError".into()))
        }
        _ = cancel.changed() => Err(CANCELLED.into()),
    };
    if outcome.is_err() {
        #[cfg(windows)]
        job.terminate(1);
        crate::process::terminate_process_tree(&mut child).await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_output_still_reports_truncation() {
        let repeated = vec![b'x'; LIMIT];
        let shortened = bounded(&repeated, &repeated, true);
        assert_eq!(shortened.len(), LIMIT);
        assert!(shortened.contains("output truncated"));
        assert_eq!(bounded(&repeated, &repeated, false), "x".repeat(LIMIT));
    }
}
