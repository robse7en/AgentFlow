use agentflow_core::chat::ask;
use std::{collections::HashMap, time::Duration};
#[test]
fn test_the_cli_runs_with_every_tool_off_and_never_the_harness_args() {
    let claude = ask::argv("claude", "fake-cli", Some("model"));
    assert!(claude.windows(2).any(|p| p == ["--tools", ""]));
    assert!(claude.windows(2).any(|p| p == ["--setting-sources", ""]));
    assert!(claude.contains(&"--strict-mcp-config".into()));
    let copilot = ask::argv("copilot", "fake-cli", None);
    assert!(copilot.windows(2).any(|p| p == ["--available-tools", "agentflow_no_tool"]));
    assert!(!copilot.iter().any(|a| a == "--allow-all-tools"));
}
#[test]
fn malformed_tool_requests_and_error_flags_never_become_an_answer() {
    assert!(ask::copilot_answer("{\"type\":\"assistant.message\",\"data\":{\"content\":\"private\",\"toolRequests\":{\"tool\":\"read\"}}}\n{\"type\":\"result\",\"exitCode\":0}").is_none());
    assert!(ask::claude_answer("{\"is_error\":\"failure\",\"result\":\"private\"}").is_none());
}
#[cfg(windows)]
#[tokio::test]
async fn isolated_cli_receives_prompt_on_stdin_and_explicit_environment() {
    let dir = tempfile::tempdir().unwrap();
    let args = [
        "powershell.exe",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$text=[Console]::In.ReadToEnd(); [Console]::Out.Write($env:ASK_TEST_VALUE + ':' + $text)",
    ]
    .map(str::to_owned);
    let result = ask::run_cli(
        &args,
        dir.path(),
        &HashMap::from([("ASK_TEST_VALUE".into(), "explicit".into())]),
        Duration::from_secs(10),
        "Unicode é, data not code $(private)",
    )
    .await
    .unwrap();
    assert_eq!(result.returncode, 0);
    assert!(result.stdout.starts_with("explicit:Unicode"));
    assert!(result.stdout.contains("$(private)"));
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}
#[cfg(windows)]
async fn process_gone(pid: u32) {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    tokio::time::timeout(Duration::from_secs(5), async move {
        loop {
            // SAFETY: the PID came from the test child; each queried handle is closed immediately.
            let alive = unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if handle.is_null() {
                    false
                } else {
                    let mut status = 0;
                    let running = GetExitCodeProcess(handle, &mut status) != 0 && status == 259;
                    CloseHandle(handle);
                    running
                }
            };
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[cfg(windows)]
async fn tree_timeout(cancel: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pid");
    let args = ["powershell.exe".into(),"-NoProfile".into(),"-NonInteractive".into(),"-Command".into(),"$child=Start-Process ping.exe -ArgumentList '-n 60 127.0.0.1' -WindowStyle Hidden -PassThru; [IO.File]::WriteAllText($env:ASK_TEST_PID, [string]$child.Id); Start-Sleep -Seconds 60".into()];
    let env = HashMap::from([("ASK_TEST_PID".into(), path.to_string_lossy().into_owned())]);
    let cwd = dir.path().to_path_buf();
    let job = tokio::spawn(async move {
        ask::run_cli(&args, &cwd, &env, Duration::from_secs(if cancel { 30 } else { 2 }), "").await
    });
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(pid) = text.parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    if cancel {
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
    } else {
        assert!(job.await.unwrap().unwrap_err().starts_with("No answer within"));
    }
    process_gone(pid).await;
}
#[cfg(windows)]
#[tokio::test]
async fn test_a_timed_out_cli_is_stopped_with_everything_it_started() {
    tree_timeout(false).await;
}
#[cfg(windows)]
#[tokio::test]
async fn cli_cancellation_kills_descendants() {
    tree_timeout(true).await;
}
