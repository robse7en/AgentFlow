//! The bundled usage collectors against what the Python scripts produced on the same fixture homes
//! (contract/golden/usage, recorded by contract/record_python_goldens.py), plus the script tests of
//! `test_usage_accounting.py` ported by name. No test reads the real home or the network: every
//! environment is a temp directory with a downloader that refuses.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agentflow_core::collectors::{self, Collector, CollectorEnv, Downloader, Outcome, claude, codex, copilot, pricing};
use agentflow_core::engine::usage::{self, UsageError, UsageReport};
use serde_json::{Value, json};

fn golden() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/usage/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn offline(home: &Path) -> CollectorEnv {
    CollectorEnv::at(home.to_path_buf(), None, Arc::new(|| None))
}

fn lay_out(home: &Path, case: &Value) {
    for (rel, text) in case["files"].as_object().unwrap() {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text.as_str().unwrap().as_bytes()).unwrap();
    }
    for (rel, mtime) in case.get("mtimes").and_then(Value::as_object).into_iter().flatten() {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.as_u64().unwrap());
        std::fs::File::options().write(true).open(home.join(rel)).unwrap().set_modified(when).unwrap();
    }
    if let Some(rows) = case.get("ledger").and_then(Value::as_array) {
        ledger(home, rows);
    }
}

fn ledger(home: &Path, rows: &[Value]) {
    let database = home.join(".local/share/copilot-api/copilot-api.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("CREATE TABLE token_usage_events (session_id TEXT, total_cost_nanos INTEGER, cost_currency TEXT)", [])
        .unwrap();
    for row in rows {
        let row = row.as_array().unwrap();
        connection
            .execute(
                "INSERT INTO token_usage_events VALUES (?1, ?2, ?3)",
                rusqlite::params![row[0].as_str(), row[1].as_i64(), row[2].as_str()],
            )
            .unwrap();
    }
}

fn collector(name: &str) -> Collector {
    match name {
        "claude" => Collector::Claude,
        "codex" => Collector::Codex,
        "copilot" => Collector::Copilot,
        other => panic!("no collector {other}"),
    }
}

/// The script's exit code and printed report for an outcome.
fn as_script(outcome: Outcome) -> (i64, Value) {
    match outcome {
        Outcome::Report(report) => (0, report),
        Outcome::Pending(report) => (75, report.unwrap_or(Value::Null)),
        Outcome::Unavailable => (1, Value::Null),
    }
}

#[test]
fn every_collector_matches_python() {
    let tmp = tempfile::tempdir().unwrap();
    let cases = golden();
    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let home = tmp.path().join(name);
        std::fs::create_dir_all(&home).unwrap();
        lay_out(&home, case);
        let outcome = collectors::run(
            collector(case["collector"].as_str().unwrap()),
            &offline(&home),
            case["session"].as_str().unwrap(),
        );
        let (exit, report) = as_script(outcome);
        // Exact equality: costs must come out of the same arithmetic in the same order.
        if exit != case["exit"].as_i64().unwrap() || report != case["report"] {
            failures
                .push(format!("{name}: rust exit {exit} {report}\n  python exit {} {}", case["exit"], case["report"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} collector cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn the_engine_runs_bundled_scripts_in_process_and_validates_their_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let cases = golden();
    let case = |name: &str| cases.iter().find(|c| c["name"] == name).unwrap().clone();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let collect = |name: &str| {
        let case = case(name);
        let home = tmp.path().join(name);
        std::fs::create_dir_all(&home).unwrap();
        lay_out(&home, &case);
        let kind = collector(case["collector"].as_str().unwrap());
        runtime.block_on(usage::collect_bundled(kind, offline(&home), case["session"].as_str()))
    };
    let report = collect("claude-basic").unwrap();
    assert_eq!((report.scope.as_str(), report.cost_kind.as_deref()), ("session", Some("estimated")));
    match collect("claude-open-turn") {
        Err(UsageError::Pending(Some(report))) => assert_eq!(report.input_tokens, Some(17070)),
        other => panic!("{other:?}"),
    }
    assert_eq!(collect("copilot-resumed-turn-is-pending"), Err(UsageError::Pending(None)));
    assert!(matches!(collect("codex-no-token-count"), Err(UsageError::Failed(_))));
}

// ------------------------------------------------------------------------------ the price catalog

fn counting_download(body: Option<&'static str>) -> (Arc<AtomicUsize>, Downloader) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    (
        calls,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            body.map(|b| b.as_bytes().to_vec())
        }),
    )
}

fn own_cache(home: &Path) -> PathBuf {
    home.join(".cache/agentflow/models-dev-api.json")
}

#[test]
fn the_proxy_catalog_comes_first_and_an_empty_one_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let (calls, download) = counting_download(Some(r#"{"downloaded": {}}"#));
    let env = CollectorEnv::at(home.to_path_buf(), Some(home.join("proxy").to_string_lossy().into_owned()), download);
    std::fs::create_dir_all(home.join("proxy")).unwrap();
    std::fs::write(home.join("proxy/models-dev-api.json"), r#"{"proxy": {}}"#).unwrap();
    assert!(pricing::load_catalog(&env).unwrap().contains_key("proxy"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    std::fs::write(home.join("proxy/models-dev-api.json"), "{}").unwrap();
    assert!(pricing::load_catalog(&env).unwrap().contains_key("downloaded"));
    // The download is cached through a temp file, in Python's location and format.
    assert_eq!(std::fs::read_to_string(own_cache(home)).unwrap(), r#"{"downloaded": {}}"#);
    assert!(!own_cache(home).with_extension("tmp").exists());
}

#[test]
fn a_fresh_own_copy_is_used_and_a_stale_one_only_when_the_download_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(own_cache(home).parent().unwrap()).unwrap();
    std::fs::write(own_cache(home), r#"{"own": {}}"#).unwrap();
    let (calls, download) = counting_download(Some(r#"{"downloaded": {}}"#));
    let env = CollectorEnv::at(home.to_path_buf(), None, download);
    assert!(pricing::load_catalog(&env).unwrap().contains_key("own"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let eight_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::options().write(true).open(own_cache(home)).unwrap().set_modified(eight_days_ago).unwrap();
    assert!(pricing::load_catalog(&env).unwrap().contains_key("downloaded"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    std::fs::write(own_cache(home), r#"{"own": {}}"#).unwrap();
    std::fs::File::options().write(true).open(own_cache(home)).unwrap().set_modified(eight_days_ago).unwrap();
    for body in [None, Some("[1, 2]"), Some("not json")] {
        let (_, download) = counting_download(body);
        let env = CollectorEnv::at(home.to_path_buf(), None, download);
        assert!(pricing::load_catalog(&env).unwrap().contains_key("own"), "{body:?}");
    }
}

/// The real models.dev download (network): `cargo test --test usage_collectors -- --ignored`.
#[test]
#[ignore]
fn live_catalog_download() {
    let bytes = pricing::download_catalog().expect("models.dev reachable within the deadline");
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(catalog.get("anthropic").is_some() && catalog.get("openai").is_some());
}

#[test]
fn copilot_api_home_comes_from_the_environment_unless_empty() {
    let home = PathBuf::from("H");
    let env = CollectorEnv::at(home.clone(), Some(String::new()), Arc::new(|| None));
    assert_eq!(env.copilot_api_home, home.join(".local").join("share").join("copilot-api"));
    let env = CollectorEnv::at(home, Some("X".into()), Arc::new(|| None));
    assert_eq!(env.copilot_api_home, PathBuf::from("X"));
}

// ------------------------------------------------------------------------------ test_usage_accounting.py

const CATALOG: &str = r#"{"openai": {"models": {"gpt-test": {"cost": {
    "input": 2, "output": 10, "cache_read": 0.2, "cache_write": 2.5,
    "tiers": [{"input": 4, "output": 15, "cache_read": 0.4, "cache_write": 5,
               "tier": {"type": "context", "size": 272000}}]}}}},
    "anthropic": {"models": {"claude-test-5-5": {"cost": {
        "input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5}}}}}"#;

/// A fake home with the Copilot proxy's cached price catalog, so no test touches the network.
fn home() -> (tempfile::TempDir, CollectorEnv) {
    let tmp = tempfile::tempdir().unwrap();
    let proxy = tmp.path().join(".local/share/copilot-api");
    std::fs::create_dir_all(&proxy).unwrap();
    std::fs::write(proxy.join("models-dev-api.json"), CATALOG).unwrap();
    let env = offline(tmp.path());
    (tmp, env)
}

fn write_lines(path: &Path, events: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, events.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
}

fn copilot_events(env: &CollectorEnv, session: &str, events: &[Value]) -> PathBuf {
    let path = env.home.join(".copilot/session-state").join(session).join("events.jsonl");
    write_lines(&path, events);
    path
}

fn copilot_shutdown(nano_aiu: Option<i64>, per_model: Option<i64>) -> Value {
    let mut metrics = json!({"usage": {"inputTokens": 100, "outputTokens": 20, "cacheReadTokens": 40}});
    if let Some(per_model) = per_model {
        metrics["totalNanoAiu"] = json!(per_model);
    }
    let mut data = json!({"modelMetrics": {"gpt-test": metrics}});
    if let Some(nano_aiu) = nano_aiu {
        data["totalNanoAiu"] = json!(nano_aiu);
    }
    json!({"type": "session.shutdown", "data": data})
}

fn copilot_checkpoint(nano_aiu: i64) -> Value {
    json!({"type": "session.usage_checkpoint", "data": {"totalNanoAiu": nano_aiu}})
}

fn copilot_turn() -> Vec<Value> {
    vec![
        json!({"type": "user.message", "data": {}}),
        json!({"type": "assistant.turn_start", "data": {}}),
        json!({"type": "assistant.message", "data": {"model": "gpt-test"}}),
        json!({"type": "assistant.turn_end", "data": {}}),
    ]
}

fn events(parts: &[&[Value]]) -> Vec<Value> {
    parts.concat()
}

fn cost(path: &Path) -> f64 {
    match copilot::collect_cost(path).unwrap() {
        copilot::Cost::Billed(report) => report["costUsd"].as_f64().unwrap(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn test_codex_session_lookup_requires_exact_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let env = offline(tmp.path());
    let folder = tmp.path().join(".codex/sessions/2026/09/22");
    for identity in ["first", "second"] {
        write_lines(
            &folder.join(format!("{identity}.jsonl")),
            &[json!({"type": "session_meta", "payload": {"id": identity, "source": "cli", "cwd": "same-directory"}})],
        );
    }
    assert_eq!(codex::find_session_file(&env, "first").unwrap().unwrap().file_name().unwrap(), "first.jsonl");
    assert_eq!(codex::find_session_file(&env, "missing").unwrap(), None);
    assert_eq!(codex::find_session_file(&env, "").unwrap(), None);
}

#[test]
fn test_copilot_reports_billed_ai_credits_cumulatively_without_tokens() {
    let (_tmp, env) = home();
    copilot_events(
        &env,
        "session",
        &events(&[
            &[json!({"type": "session.start"})],
            &copilot_turn(),
            &[copilot_shutdown(Some(125_000_000_000), None)],
        ]),
    );
    let Outcome::Report(printed) = collectors::run(Collector::Copilot, &env, "session") else { panic!() };
    let current = UsageReport::validate(&printed).unwrap();
    assert_eq!(
        (current.cost, current.cost_kind.as_deref(), current.model.as_str()),
        (Some(1.25), Some("billed"), "gpt-test")
    );
    assert_eq!(current.input_tokens, None);
    assert_eq!(current.record("fixture")["totalTokens"], Value::Null);
    let empty = serde_json::Map::new();
    assert_eq!(usage::run_usage(&current, Some(&empty)).unwrap().cost, Some(1.25));
}

#[test]
fn test_copilot_sums_per_model_credits_without_a_session_total() {
    let (_tmp, env) = home();
    let path = copilot_events(&env, "s", &[copilot_shutdown(None, Some(50_000_000_000))]);
    assert_eq!(cost(&path), 0.5);
}

#[test]
fn test_copilot_reads_the_checkpoint_written_after_each_turn() {
    let (_tmp, env) = home();
    let path = copilot_events(
        &env,
        "s",
        &events(&[
            &[json!({"type": "session.start", "data": {"selectedModel": "gpt-test"}})],
            &copilot_turn(),
            &[copilot_checkpoint(634_300_000), json!({"type": "hook.end"})],
        ]),
    );
    assert_eq!(cost(&path), 0.006343);
}

#[test]
fn test_copilot_turn_after_the_latest_snapshot_is_pending() {
    let (_tmp, env) = home();
    // The agent's own `flowctl complete` runs mid-turn, before that turn's checkpoint exists.
    copilot_events(
        &env,
        "session",
        &events(&[
            &[json!({"type": "session.start"})],
            &copilot_turn(),
            &[copilot_checkpoint(1), json!({"type": "user.message"}), json!({"type": "assistant.turn_start"})],
        ]),
    );
    assert_eq!(collectors::run(Collector::Copilot, &env, "session"), Outcome::Pending(None));
}

#[test]
fn test_resumed_copilot_turn_is_pending_not_the_previous_shutdown() {
    let (_tmp, env) = home();
    // Reporting the prior shutdown here would equal the resumed run's baseline: a permanent $0.
    copilot_events(
        &env,
        "session",
        &events(&[
            &[
                json!({"type": "session.start"}),
                copilot_shutdown(Some(1_000_000_000), None),
                json!({"type": "session.resume"}),
            ],
            &copilot_turn(),
        ]),
    );
    assert_eq!(collectors::run(Collector::Copilot, &env, "session"), Outcome::Pending(None));
}

#[test]
fn test_resumed_copilot_session_before_any_turn_measures_the_baseline() {
    let (_tmp, env) = home();
    let path = copilot_events(
        &env,
        "s",
        &events(&[
            &[json!({"type": "session.start"})],
            &copilot_turn(),
            &[copilot_shutdown(Some(2_000_000_000), None), json!({"type": "session.resume"})],
        ]),
    );
    assert_eq!(cost(&path), 0.02);
}

#[test]
fn test_copilot_snapshot_without_credits_is_unavailable() {
    let (_tmp, env) = home();
    copilot_events(
        &env,
        "session",
        &[
            copilot_shutdown(Some(2_000_000_000), None),
            json!({"type": "session.resume"}),
            json!({"type": "session.shutdown", "data": {"modelMetrics": {}}}),
        ],
    );
    assert_eq!(collectors::run(Collector::Copilot, &env, "session"), Outcome::Unavailable);
}

fn claude_transcript(env: &CollectorEnv, session: &str, model: &str, usage: Value) -> PathBuf {
    let path = env.home.join(".claude/projects/project").join(format!("{session}.jsonl"));
    write_lines(&path, &[json!({"type": "assistant", "message": {"id": "m1", "model": model, "usage": usage}})]);
    path
}

fn printed(env: &CollectorEnv, collector: Collector, session: &str) -> Value {
    match collectors::run(collector, env, session) {
        Outcome::Report(report) => report,
        other => panic!("{other:?}"),
    }
}

#[test]
fn test_claude_through_copilot_proxy_reports_billed_session_cost() {
    let (tmp, env) = home();
    claude_transcript(&env, "session", "gpt-test", json!({"input_tokens": 10, "output_tokens": 5}));
    ledger(
        tmp.path(),
        &[
            json!(["session", 2_000_000_000i64, "USD"]),
            json!(["session", 500_000_000, "USD"]),
            json!(["other", 9_000_000_000i64, "USD"]),
        ],
    );
    let report = printed(&env, Collector::Claude, "session");
    assert_eq!((report["costUsd"].as_f64(), report["costKind"].as_str()), (Some(2.5), Some("billed")));
}

#[test]
fn test_claude_proxy_rows_without_cost_fall_back_to_estimate() {
    let (tmp, env) = home();
    claude_transcript(&env, "session", "gpt-test", json!({"input_tokens": 1000, "output_tokens": 100}));
    ledger(tmp.path(), &[json!(["session", 2_000_000_000i64, "USD"]), json!(["session", null, null])]);
    let report = printed(&env, Collector::Claude, "session");
    assert_eq!(report["costKind"], "estimated");
    assert!((report["costUsd"].as_f64().unwrap() - (1000.0 * 2.0 + 100.0 * 10.0) / 1e6).abs() < 1e-12);
}

#[test]
fn test_claude_estimate_prices_long_context_and_one_hour_cache_writes() {
    let (_tmp, env) = home();
    let path = claude_transcript(&env, "long", "gpt-test[1m]", json!({"input_tokens": 300_000, "output_tokens": 10}));
    let (long, _) = claude::collect(&env, &path).unwrap();
    assert!((long.unwrap()["costUsd"].as_f64().unwrap() - (300_000.0 * 4.0 + 10.0 * 15.0) / 1e6).abs() < 1e-12);
    let path = claude_transcript(
        &env,
        "cache",
        "claude-test-5.5",
        json!({"input_tokens": 0, "output_tokens": 0,
        "cache_creation_input_tokens": 300, "cache_creation": {"ephemeral_1h_input_tokens": 100}}),
    );
    let (cached, _) = claude::collect(&env, &path).unwrap();
    assert!((cached.unwrap()["costUsd"].as_f64().unwrap() - (200.0 * 5.0 + 100.0 * 4.0 * 2.0) / 1e6).abs() < 1e-12);
}

#[test]
fn test_codex_tokens_are_priced_as_an_estimate() {
    let (_tmp, env) = home();
    let rollout = env.home.join("rollout.jsonl");
    write_lines(
        &rollout,
        &[
            json!({"type": "turn_context", "payload": {"model": "gpt-test"}}),
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {
                "input_tokens": 1000, "cached_input_tokens": 600, "output_tokens": 50}}}}),
        ],
    );
    let report = codex::collect(&env, &rollout).unwrap().unwrap();
    assert_eq!(report["costKind"], "estimated");
    assert!((report["costUsd"].as_f64().unwrap() - (400.0 * 2.0 + 600.0 * 0.2 + 50.0 * 10.0) / 1e6).abs() < 1e-12);
}

// ------------------------------------------------------------------------------ test_security_improvements.py
// Commands that are not bundled scripts still run as a process.

/// A script that prints `lines` (a .cmd on Windows, sh elsewhere), as a usage command.
fn printing_command(dir: &Path, lines: &[String]) -> Vec<String> {
    if cfg!(windows) {
        let script = dir.join("collector.cmd");
        let body: String = lines.iter().map(|line| format!("@echo {line}\r\n")).collect();
        std::fs::write(&script, body).unwrap();
        vec![script.to_string_lossy().into_owned()]
    } else {
        let script = dir.join("collector.sh");
        let body: String = lines.iter().map(|line| format!("printf '%s\\n' '{line}'\n")).collect();
        std::fs::write(&script, body).unwrap();
        vec!["sh".into(), script.to_string_lossy().into_owned()]
    }
}

#[tokio::test]
async fn test_usage_collector_collects_provider_report_and_model() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = json!({"inputTokens": 100, "outputTokens": 20, "cachedInputTokens": 30,
                         "model": "actual-model", "source": "synthetic provider report"});
    let command = printing_command(tmp.path(), &[payload.to_string()]);
    let report = usage::collect_usage(&command, tmp.path(), 1, None).await.unwrap();
    assert_eq!(report.record("collector")["totalTokens"], 120);
    assert_eq!(report.model, "actual-model");
    assert_eq!(report.cost, None);
    let mut invalid = payload.clone();
    invalid["cachedInputTokens"] = json!(101);
    assert!(UsageReport::validate(&invalid).is_err());
}

#[tokio::test]
async fn test_usage_collector_rejects_oversized_report() {
    let tmp = tempfile::tempdir().unwrap();
    let command = printing_command(tmp.path(), &vec!["x".repeat(100); 200]);
    match usage::collect_usage(&command, tmp.path(), 1, None).await {
        Err(UsageError::Failed(message)) => assert!(message.contains("16 KB"), "{message}"),
        other => panic!("{other:?}"),
    }
}

fn claude_turn(path: &Path, message_id: &str, stop_reason: &str, output_tokens: i64) {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let line = json!({"type": "assistant", "message": {"id": message_id, "model": "gpt-test", "stop_reason": stop_reason,
                      "usage": {"input_tokens": 100, "output_tokens": output_tokens}}});
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

#[test]
fn test_claude_turn_inside_a_tool_call_reports_usage_so_far_as_pending() {
    let (_tmp, env) = home();
    let path = env.home.join(".claude/projects/project/session.jsonl");
    // The agent's `flowctl complete` is a tool call: its turn continues after it returns.
    claude_turn(&path, "m1", "tool_use", 10);
    match collectors::run(Collector::Claude, &env, "session") {
        Outcome::Pending(Some(report)) => assert_eq!(report["outputTokens"], 10),
        other => panic!("{other:?}"),
    }
    claude_turn(&path, "m2", "end_turn", 5);
    assert_eq!(printed(&env, Collector::Claude, "session")["outputTokens"], 15);
}
