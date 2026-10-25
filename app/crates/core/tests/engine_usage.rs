//! Usage accounting, ported by name from backend/tests/test_usage_accounting.py: per-run usage from
//! session counters, the summaries, and how the engine collects usage (late exits, background
//! retries, and asking idle Copilot agents to exit before their tabs close). The collector itself
//! is a fake here, as Python's tests mocked `collect_usage`; the bundled collectors are checked in
//! usage_collectors.rs.

mod engine_support;

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentflow_core::db::table::{self, Tracked};
use agentflow_core::db::{AgentSessionRow, DbTime, Json, NodeRunRow, RunEvidenceRow, new_uid};
use agentflow_core::engine::rules;
use agentflow_core::engine::spec;
use agentflow_core::engine::usage::{self, UsageError, UsageReport, UsageSource};
use agentflow_core::terminal::PaneInfo;
use engine_support::{Harness, engine};
use serde_json::{Map, Value, json};

fn report(tokens: i64, cost: Option<f64>) -> UsageReport {
    UsageReport::validate(&json!({"inputTokens": tokens, "outputTokens": 10, "cachedInputTokens": 0, "model": "test",
                                  "source": "fixture", "scope": "session", "costUsd": cost}))
    .unwrap()
}

fn dumped(report: &UsageReport) -> Map<String, Value> {
    report.dump()
}

fn validate(value: Value) -> Result<UsageReport, UsageError> {
    UsageReport::validate(&value)
}

fn failed_with(result: Result<UsageReport, UsageError>, text: &str) {
    match result {
        Err(UsageError::Failed(message)) => assert!(message.contains(text), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn test_resumed_usage_is_repeatable_and_excludes_prior_run() {
    let baseline = dumped(&report(100, Some(1.0)));
    let current = report(150, Some(1.5));
    let first = usage::run_usage(&current, Some(&baseline)).unwrap();
    assert_eq!((first.input_tokens, first.output_tokens, first.cost), (Some(50), Some(0), Some(0.5)));
    assert_eq!(usage::run_usage(&current, Some(&baseline)).unwrap(), first);
}

#[test]
fn test_missing_baseline_does_not_claim_session_total_as_run_usage() {
    failed_with(usage::run_usage(&report(150, None), None), "baseline");
}

#[test]
fn test_fresh_session_has_zero_baseline() {
    assert_eq!(usage::run_usage(&report(150, None), Some(&Map::new())).unwrap().input_tokens, Some(150));
}

#[test]
fn test_custom_run_collector_remains_compatible() {
    let value = validate(json!({"inputTokens": 12, "outputTokens": 3, "model": "test", "source": "custom"})).unwrap();
    assert_eq!(usage::run_usage(&value, None).unwrap(), value);
}

#[test]
fn test_reset_cumulative_counter_is_unknown() {
    failed_with(usage::run_usage(&report(90, None), Some(&dumped(&report(100, None)))), "decreased");
}

#[test]
fn test_missing_cost_baseline_never_assigns_prior_spend_to_new_run() {
    let run = usage::run_usage(&report(150, Some(3.0)), Some(&dumped(&report(100, None)))).unwrap();
    assert_eq!(run.cost, None);
}

#[test]
fn test_cost_only_reports_need_a_cost_and_both_or_no_token_counts() {
    assert_eq!(validate(json!({"model": "m", "source": "s", "costUsd": 1})).unwrap().cached_input_tokens, None);
    failed_with(validate(json!({"model": "m", "source": "s"})), "token counts or a cost");
    failed_with(validate(json!({"inputTokens": 1, "model": "m", "source": "s", "costUsd": 1})), "or neither");
    let both = validate(json!({"inputTokens": 1, "outputTokens": 1, "model": "m", "source": "s"})).unwrap();
    assert_eq!(both.cached_input_tokens, Some(0));
}

#[test]
fn test_cost_only_session_usage_subtracts_the_cost_baseline() {
    let cost =
        |value: f64| validate(json!({"model": "m", "source": "s", "scope": "session", "costUsd": value})).unwrap();
    assert_eq!(usage::run_usage(&cost(3.0), Some(&dumped(&cost(1.0)))).unwrap().cost, Some(2.0));
    // Token counters cannot be subtracted from a cost-only baseline, so they become unknown.
    let tokens = validate(json!({"inputTokens": 5, "outputTokens": 5, "model": "m", "source": "s",
                                 "scope": "session", "costUsd": 3}))
    .unwrap();
    assert_eq!(usage::run_usage(&tokens, Some(&dumped(&cost(1.0)))).unwrap().input_tokens, None);
}

#[test]
fn test_billed_and_estimated_counters_are_never_subtracted_from_each_other() {
    let baseline = dumped(
        &validate(json!({"inputTokens": 100, "outputTokens": 10, "model": "m", "source": "s", "scope": "session",
                         "costUsd": 1, "costKind": "estimated"}))
        .unwrap(),
    );
    let current = validate(json!({"inputTokens": 150, "outputTokens": 10, "model": "m", "source": "s",
                                  "scope": "session", "costUsd": 3, "costKind": "billed"}))
    .unwrap();
    assert_eq!(usage::run_usage(&current, Some(&baseline)).unwrap().cost, None);
    let mut billed = baseline.clone();
    billed.insert("costKind".into(), json!("billed"));
    assert_eq!(usage::run_usage(&current, Some(&billed)).unwrap().cost, Some(2.0));
}

// ------------------------------------------------------------------------------ summaries

fn run_row(id: i64) -> NodeRunRow {
    NodeRunRow {
        id,
        task_id: 1,
        node_id: 1,
        run_number: 1,
        iteration: 1,
        state: "COMPLETED".into(),
        harness: None,
        model: None,
        reasoning: None,
        run_dir: None,
        prompt_path: None,
        result_path: None,
        expected_sha: None,
        result: Json(Value::Null),
        exit_code: None,
        error: None,
        started_at: DbTime::now(),
        completed_at: None,
    }
}

fn evidence_row(run_id: i64, billing_mode: &str, usage: Map<String, Value>) -> RunEvidenceRow {
    RunEvidenceRow {
        uid: new_uid(),
        run_id,
        requirements_digest: None,
        verification: Json(Value::Null),
        usage: Json(Value::Object(usage)),
        confirmed_model: None,
        billing_mode: billing_mode.into(),
    }
}

/// `report(tokens, cost)` as a stored record (Python's tests' `report(...).record("fixture")`).
fn record(tokens: i64, cost: Option<f64>) -> Map<String, Value> {
    report(tokens, cost).record("fixture")
}

fn summary(rows: &[(i64, &str, Map<String, Value>)]) -> Value {
    let runs: Vec<NodeRunRow> = rows.iter().map(|(id, _, _)| run_row(*id)).collect();
    let evidence: Vec<RunEvidenceRow> =
        rows.iter().map(|(id, mode, usage)| evidence_row(*id, mode, usage.clone())).collect();
    let run_refs: Vec<&NodeRunRow> = runs.iter().collect();
    let by_run: HashMap<i64, &RunEvidenceRow> = evidence.iter().map(|e| (e.run_id, e)).collect();
    rules::usage_summary(&run_refs, &by_run).unwrap()
}

#[test]
fn test_token_totals_cover_only_runs_that_report_tokens() {
    let cost_only = validate(json!({"model": "m", "source": "s", "costUsd": 2})).unwrap().record("fixture");
    let summary = summary(&[(0, "subscription", record(100, Some(1.0))), (1, "subscription", cost_only)]);
    assert_eq!((summary["runsWithUsage"].clone(), summary["runsWithTokens"].clone()), (json!(2), json!(1)));
    assert_eq!(summary["totalTokens"], 110);
    assert_eq!(summary["billedCostUsd"], 3.0);
}

#[test]
fn test_cost_coverage_is_separate_from_token_coverage_and_subscription_spend() {
    let rows: Vec<(i64, &str, Map<String, Value>)> = (0..3)
        .map(|i| (i, if i == 2 { "subscription" } else { "metered" }, record(100, (i != 1).then_some(1.0))))
        .collect();
    let summary = summary(&rows);
    assert_eq!(summary["runsWithUsage"], 3);
    assert_eq!(summary["meteredRuns"], 2);
    assert_eq!(summary["meteredRunsWithCost"], 1);
    assert_eq!(summary["subscriptionRuns"], 1);
    assert_eq!(summary["apiCostUsd"], 1.0);
    assert_eq!((summary["billedCostUsd"].clone(), summary["runsWithBilledCost"].clone()), (json!(2.0), json!(2)));
}

#[test]
fn test_estimates_are_shown_apart_from_billed_cost_and_api_spend() {
    let usage = |cost: f64, kind: &str| {
        validate(json!({"inputTokens": 10, "outputTokens": 1, "model": "m", "source": "s", "costUsd": cost, "costKind": kind}))
            .unwrap()
            .record("fixture")
    };
    let summary = summary(&[
        (0, "subscription", usage(2.0, "billed")),
        (1, "subscription", usage(3.0, "estimated")),
        (2, "metered", usage(4.0, "estimated")),
    ]);
    assert_eq!(summary["billedCostUsd"], 2.0);
    assert_eq!((summary["estimatedCostUsd"].clone(), summary["runsWithEstimatedCost"].clone()), (json!(7.0), json!(2)));
    assert_eq!((summary["meteredRunsWithCost"].clone(), summary["apiCostUsd"].clone()), (json!(0), Value::Null));
}

#[test]
fn test_additive_billing_migration_preserves_old_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let database = agentflow_core::db::Database::open(&path).unwrap();
    database.create_all().unwrap();
    database
        .session(|tx| {
            tx.execute_batch(
                "ALTER TABLE run_evidence DROP COLUMN billing_mode; ALTER TABLE tasks DROP COLUMN api_budget_usd;",
            )
        })
        .unwrap();
    database.create_all().unwrap();
    database.create_all().unwrap();
    let columns = |table: &str| -> Vec<String> {
        database
            .session(|tx| {
                let mut statement = tx.prepare(&format!("PRAGMA table_info({table})"))?;
                statement.query_map([], |row| row.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
    };
    assert!(columns("run_evidence").contains(&"billing_mode".to_string()));
    assert!(columns("tasks").contains(&"api_budget_usd".to_string()));
}

/// Difference: the Rust backend has no interpreter of its own, so `python` stays as written.
#[test]
fn test_usage_command_resolves_python_and_install_relative_scripts() {
    let argv = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let resolved = usage::resolve_usage_command(&argv(&["python", "scripts/usage_copilot.py", "--flag"]));
    assert_eq!(resolved[0], "python");
    assert!(
        resolved[1].ends_with("/scripts/usage_copilot.py") && Path::new(&resolved[1]).is_absolute(),
        "{resolved:?}"
    );
    assert!(Path::new(&resolved[1]).canonicalize().unwrap().starts_with(&root));
    assert_eq!(resolved[2], "--flag");
    let custom = argv(&["C:/tools/collector.exe", "report.json"]);
    assert_eq!(usage::resolve_usage_command(&custom), custom);
}

// ------------------------------------------------------------------------------ the engine collecting

/// Python's `AsyncMock(side_effect=[...])` for `collect_usage`: answers in order, counting calls.
#[derive(Default)]
struct FakeUsage {
    answers: Mutex<VecDeque<Result<UsageReport, UsageError>>>,
    calls: Mutex<usize>,
}

impl FakeUsage {
    fn answering(answers: Vec<Result<UsageReport, UsageError>>) -> Arc<FakeUsage> {
        Arc::new(FakeUsage { answers: Mutex::new(answers.into()), calls: Mutex::new(0) })
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl UsageSource for FakeUsage {
    async fn collect(&self, _: &[String], _: &Path, _: i64, _: Option<&str>) -> Result<UsageReport, UsageError> {
        *self.calls.lock().unwrap() += 1;
        self.answers.lock().unwrap().pop_front().unwrap_or(Err(UsageError::Failed("no more answers".into())))
    }
}

fn pending(report: Option<UsageReport>) -> Result<UsageReport, UsageError> {
    Err(UsageError::Pending(report.map(Box::new)))
}

fn copilot_cost(value: f64) -> UsageReport {
    validate(json!({"model": "gpt-test", "source": "copilot-cli-events", "costUsd": value})).unwrap()
}

/// A completed implementer run on `harness_key`, whose harness has a usage command.
fn completed_run(harness: &Harness, harness_key: &str) -> i64 {
    let run_id = harness.add_run(spec::IMPLEMENTER, "COMPLETED", None);
    harness.configure(|config| {
        config.harnesses.get_mut(harness_key).unwrap().usage_command = Some(vec!["collector".into()])
    });
    harness.with_task(|state| state.runs[0].harness = Some(harness_key.into()));
    run_id
}

fn evidence(harness: &Harness, run_id: i64) -> Option<RunEvidenceRow> {
    harness.engine.db.session(|tx| table::select_one(tx, "WHERE run_id = ?1", [run_id])).unwrap()
}

#[tokio::test]
async fn test_late_exit_does_not_overwrite_accepted_or_human_corrected_usage() {
    let harness = engine().await;
    let run_id = completed_run(&harness, "codex");
    let collector = FakeUsage::answering(vec![]);
    harness.engine.set_usage_source(collector.clone());
    let mut row = evidence_row(run_id, "unknown", report(100, None).record("configured-provider-collector"));
    harness.engine.db.session(|tx| table::insert(tx, &mut row)).unwrap();
    // The launcher's exit report collects usage for its run, which has accepted usage already.
    harness.engine.agent_exited("TEST-1", "implementer", Some(run_id), 0).await.unwrap();
    assert_eq!(collector.calls(), 0);
    assert_eq!(evidence(&harness, run_id).unwrap().usage.0["inputTokens"], 100);
}

#[tokio::test]
async fn test_pending_usage_is_collected_again_in_the_background() {
    let harness = engine().await;
    let run_id = completed_run(&harness, "copilot");
    harness.engine.set_usage_retry_delays(vec![Duration::ZERO, Duration::ZERO]);
    let collector = FakeUsage::answering(vec![pending(None), pending(None), Ok(copilot_cost(0.5))]);
    harness.engine.set_usage_source(collector.clone());
    harness.engine.agent_exited("TEST-1", "implementer", Some(run_id), 0).await.unwrap();
    assert!(evidence(&harness, run_id).is_none(), "pending usage is not recorded");
    harness.engine.join_usage_retry(run_id).await;
    let usage = evidence(&harness, run_id).unwrap().usage.0;
    assert_eq!((usage["costUsd"].clone(), usage["totalTokens"].clone()), (json!(0.5), Value::Null));
    assert_eq!(collector.calls(), 3);
    assert!(!harness.engine.usage_retries_pending());
}

#[tokio::test]
async fn test_partial_claude_usage_is_recorded_then_replaced_when_the_turn_ends() {
    let harness = engine().await;
    let run_id = completed_run(&harness, "claude");
    harness.engine.set_usage_retry_delays(vec![Duration::ZERO]);
    let so_far =
        validate(json!({"inputTokens": 100, "outputTokens": 10, "model": "m", "source": "s", "costUsd": 1})).unwrap();
    let last =
        validate(json!({"inputTokens": 150, "outputTokens": 20, "model": "m", "source": "s", "costUsd": 1.5})).unwrap();
    harness.engine.set_usage_source(FakeUsage::answering(vec![pending(Some(so_far)), Ok(last)]));
    harness.engine.agent_exited("TEST-1", "implementer", Some(run_id), 0).await.unwrap();
    let usage = evidence(&harness, run_id).unwrap().usage.0;
    assert_eq!((usage["outputTokens"].clone(), usage["partial"].clone()), (json!(10), json!(true)));
    harness.engine.join_usage_retry(run_id).await;
    let usage = evidence(&harness, run_id).unwrap().usage.0;
    assert_eq!((usage["outputTokens"].clone(), usage["costUsd"].clone()), (json!(20), json!(1.5)));
    assert!(usage.get("partial").is_none());
}

#[tokio::test]
async fn test_resumed_baseline_uses_the_usage_a_cut_off_turn_left() {
    let so_far =
        validate(json!({"inputTokens": 100, "outputTokens": 10, "model": "m", "source": "s", "scope": "session"}))
            .unwrap();
    let source = FakeUsage::answering(vec![pending(Some(so_far)), pending(None)]);
    let argv = vec!["collector".to_string()];
    let baseline =
        usage::prepare_baseline(source.as_ref(), Some(&argv), Path::new("."), 1, Some("session"), true).await;
    assert_eq!(baseline["outputTokens"], 10);
    let baseline =
        usage::prepare_baseline(source.as_ref(), Some(&argv), Path::new("."), 1, Some("session"), true).await;
    assert_eq!(baseline, Value::Null);
}

// Copilot CLI 1.0.89 screens, trimmed from a real pane.
const COPILOT_FOOTER_IDLE: &str =
    " ← open sidebar · / commands · ? help · tab next tab        GitHub Copilot • GPT-5.6 Luna";

fn copilot_screen(footer: &str) -> Vec<String> {
    let rule = "─".repeat(40);
    let mut screen = vec![
        " D:\\Dev\\AgentFlow [⎇ main]                     Session: 0.63 AIC used".to_string(),
        rule.clone(),
        "❯".to_string(),
        rule,
    ];
    screen.extend(footer.lines().map(String::from));
    screen
}

/// A running agent of `harness_key` on tab 7 serving `run_id`, whose pane shows `screen`.
fn agent_on_tab_seven(harness: &Harness, run_id: i64, harness_key: &str, screen: &[String]) {
    harness.with_task(|state| {
        state.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: state.task.id,
            role: "implementer".into(),
            harness: harness_key.into(),
            model: None,
            reasoning: None,
            zellij_tab_name: "Implementer".into(),
            zellij_tab_id: Some(7),
            zellij_pane_id: Some("terminal_7".into()),
            harness_session_id: None,
            status: "running".into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: Some(run_id),
            started_at: DbTime::now(),
            ended_at: None,
        }));
    });
    harness.terminals.add_pane(
        "agentflow-TEST-1",
        PaneInfo {
            pane_id: "terminal_7".into(),
            tab_id: 7,
            tab_name: "Implementer".into(),
            exited: false,
            exit_status: None,
            command: None,
        },
    );
    let rows: Vec<&str> = screen.iter().map(String::as_str).collect();
    harness.terminals.set_screen("terminal_7", &rows);
}

fn terminal_calls(harness: &Harness, names: &[&str]) -> Vec<String> {
    harness.terminals.calls().into_iter().filter(|c| names.iter().any(|n| c.starts_with(n))).collect()
}

#[tokio::test]
async fn test_stopping_an_idle_copilot_agent_exits_it_and_collects_its_cost() {
    let harness = engine().await;
    let run_id = completed_run(&harness, "copilot");
    agent_on_tab_seven(&harness, run_id, "copilot", &copilot_screen(COPILOT_FOOTER_IDLE));
    // Copilot writes its shutdown snapshot a moment after /exit.
    let collector = FakeUsage::answering(vec![pending(None), Ok(copilot_cost(0.25))]);
    harness.engine.set_usage_source(collector.clone());
    harness.engine.cancel_task("TEST-1").await.unwrap();
    assert_eq!(
        terminal_calls(&harness, &["write_line", "close_tab"]),
        ["write_line agentflow-TEST-1 terminal_7 /exit", "close_tab agentflow-TEST-1 7"]
    );
    assert_eq!(evidence(&harness, run_id).unwrap().usage.0["costUsd"], 0.25);
    assert!(!harness.engine.usage_retries_pending());
}

#[tokio::test]
async fn test_a_busy_or_prompting_copilot_agent_is_closed_without_typing() {
    for footer in [
        " ◉ Working · 141 B esc interrupt                                GitHub Copilot • GPT-5.6 Luna",
        "│ ❯ 1. Yes\n│   3. No (Esc)\n│ ↑/↓ to navigate · enter to select · esc to cancel",
        "something unrecognised",
    ] {
        let harness = engine().await;
        let run_id = completed_run(&harness, "copilot");
        agent_on_tab_seven(&harness, run_id, "copilot", &copilot_screen(footer));
        let collector = FakeUsage::answering(vec![]);
        harness.engine.set_usage_source(collector.clone());
        harness.engine.cancel_task("TEST-1").await.unwrap();
        assert!(terminal_calls(&harness, &["write_line"]).is_empty(), "{footer}");
        assert_eq!(collector.calls(), 0);
        assert_eq!(terminal_calls(&harness, &["close_tab"]), ["close_tab agentflow-TEST-1 7"]);
    }
}

#[tokio::test]
async fn test_a_worker_inside_its_completion_call_is_not_asked_to_exit() {
    let harness = engine().await;
    let run_id = completed_run(&harness, "copilot");
    agent_on_tab_seven(&harness, run_id, "copilot", &copilot_screen(COPILOT_FOOTER_IDLE));
    let mut state = harness.load();
    harness.engine.stop_sessions(&mut state, None, None, false).await.unwrap();
    assert!(terminal_calls(&harness, &["screen"]).is_empty());
    assert_eq!(terminal_calls(&harness, &["close_tab"]), ["close_tab agentflow-TEST-1 7"]);
}

#[tokio::test]
async fn test_claude_and_codex_tabs_are_closed_as_before() {
    for harness_key in ["claude", "codex"] {
        let harness = engine().await;
        let run_id = completed_run(&harness, harness_key);
        agent_on_tab_seven(&harness, run_id, harness_key, &copilot_screen(COPILOT_FOOTER_IDLE));
        let collector = FakeUsage::answering(vec![]);
        harness.engine.set_usage_source(collector.clone());
        harness.engine.cancel_task("TEST-1").await.unwrap();
        assert!(terminal_calls(&harness, &["screen", "write_line"]).is_empty(), "{harness_key}");
        assert_eq!(collector.calls(), 0);
        assert_eq!(terminal_calls(&harness, &["close_tab"]), ["close_tab agentflow-TEST-1 7"]);
    }
}
