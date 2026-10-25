//! The installed harness CLIs (Claude Code, Codex, Copilot) in embedded terminals.
//!
//! Ignored: these start the user's real CLIs, which write their own state and logs (e.g. a project
//! entry for the temp directory in ~/.claude.json). Each CLI starts without a prompt, so no model
//! work begins, in a fresh temp directory it has never trusted. Run on request with:
//!
//!     AGENTFLOW_LIVE_HARNESSES=claude,codex,copilot cargo test --test live_harnesses -- --ignored --nocapture
//!
//! Checks per CLI: it renders through ConPTY into the screen model, the adapter classifies its first
//! screen (an untrusted directory should show a trust dialog: blocked), Ctrl-C reaches it, a
//! resize reaches the screen model, and closing the tab leaves no process working in the directory.
#![cfg(windows)]

use std::time::{Duration, Instant};

use agentflow_core::config::Config;
use agentflow_core::harness::{HarnessAdapter, ScreenState};
use agentflow_core::process::worktree::stop_processes_in;
use agentflow_core::terminal::embedded::EmbeddedTerminals;

/// Rows shown per screen in the report: enough to see a dialog, not the whole pane.
const REPORT_ROWS: usize = 14;

fn tail(rows: &[String]) -> String {
    let non_blank: Vec<&String> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
    non_blank[non_blank.len().saturating_sub(REPORT_ROWS)..]
        .iter()
        .map(|r| format!("    | {}", r.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The classification once the screen has settled (unchanged for a second), or the last one.
async fn settled_state(
    terminals: &EmbeddedTerminals,
    pane: &str,
    adapter: &HarnessAdapter,
    timeout: Duration,
) -> (ScreenState, Vec<String>) {
    let deadline = Instant::now() + timeout;
    let mut last = Vec::new();
    let mut stable_since = Instant::now();
    loop {
        let rows = terminals.screen("live", pane).unwrap_or_default();
        if rows != last {
            last = rows;
            stable_since = Instant::now();
        }
        let state = adapter.classify_screen(&last);
        let drawn = last.iter().any(|r| !r.trim().is_empty());
        if (drawn && stable_since.elapsed() > Duration::from_secs(1) && state != ScreenState::Unknown)
            || Instant::now() > deadline
        {
            return (state, last);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts the user's real harness CLIs; set AGENTFLOW_LIVE_HARNESSES"]
async fn installed_harnesses_run_in_embedded_terminals() {
    let wanted = std::env::var("AGENTFLOW_LIVE_HARNESSES").expect("AGENTFLOW_LIVE_HARNESSES=claude,codex,copilot");
    let config = Config::default();
    let terminals = EmbeddedTerminals::from_config(&config.zellij);
    terminals.ensure_session("live");
    for key in wanted.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        let harness = config.harnesses.get(key).unwrap_or_else(|| panic!("no default harness {key}")).clone();
        let adapter = HarnessAdapter::new(key, harness);
        if agentflow_core::process::resolve_executable(&adapter.config.command).is_none() {
            println!("\n== {key}: not installed, skipped");
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let cwd = agentflow_core::paths::resolve(dir.path());
        let mut argv = adapter.base_argv();
        if key == "codex" {
            // Codex refuses to start its shared daemon from an elevated process and suggests this.
            argv.push("--no-daemon".into());
        }
        let started = Instant::now();
        let (tab, pane) = terminals.new_tab("live", key, &cwd, Some(&argv)).unwrap();
        let pane = pane.unwrap();

        let (state, rows) = settled_state(&terminals, &pane, &adapter, Duration::from_secs(45)).await;
        println!(
            "\n== {key}: first screen after {:.1}s: {}\n{}",
            started.elapsed().as_secs_f64(),
            state.as_str(),
            tail(&rows)
        );

        let info = terminals.pane_state("live", &pane).unwrap().unwrap();
        if info.exited {
            println!("   exited by itself with status {:?}", info.exit_status);
            terminals.close_tab("live", tab).unwrap();
            continue;
        }

        if key == "copilot" && state == ScreenState::Blocked {
            // Typed lines must submit in a real TUI: the engine types Copilot's exit command this
            // way. Enter picks the highlighted "1. Yes" (trust for this session only), then
            // write_line types `/exit` and presses Enter.
            terminals.write_input("live", &pane, b"\r").unwrap();
            let (state, rows) = settled_state(&terminals, &pane, &adapter, Duration::from_secs(45)).await;
            println!("   after accepting the dialog: {}\n{}", state.as_str(), tail(&rows));
            terminals.write_line("live", &pane, adapter.kind.exit_command().unwrap()).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let info = loop {
                let info = terminals.pane_state("live", &pane).unwrap().unwrap();
                if info.exited || Instant::now() > deadline {
                    break info;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            };
            let rows = terminals.screen("live", &pane).unwrap_or_default();
            println!(
                "   after write_line(\"/exit\"): exited={} status={:?}\n{}",
                info.exited,
                info.exit_status,
                tail(&rows)
            );
            assert!(info.exited, "/exit typed with write_line did not exit Copilot");
            terminals.close_tab("live", tab).unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(stop_processes_in(&cwd).await.unwrap().is_empty(), "{key} left processes behind");
            continue;
        }

        terminals.resize("live", &pane, 40, 120).unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        let resized = terminals.screen("live", &pane).unwrap();
        println!("   resize to 40x120: screen model has {} rows of {} cols", resized.len(), 120);
        assert_eq!(resized.len(), 40);

        terminals.write_input("live", &pane, b"\x03").unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let info = terminals.pane_state("live", &pane).unwrap().unwrap();
        let rows = terminals.screen("live", &pane).unwrap_or_default();
        println!(
            "   after Ctrl-C: exited={} status={:?} classified {}\n{}",
            info.exited,
            info.exit_status,
            adapter.classify_screen(&rows).as_str(),
            tail(&rows)
        );

        terminals.close_tab("live", tab).unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let leftovers = stop_processes_in(&cwd).await.unwrap();
        println!(
            "   after closing the tab: {} process(es) still working in the directory {:?}",
            leftovers.len(),
            leftovers.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert!(leftovers.is_empty(), "{key} left processes behind");
    }
}
