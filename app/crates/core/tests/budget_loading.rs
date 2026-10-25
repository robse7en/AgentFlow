//! AF-AUDIT-005: a task's budget reads only the rows it can count (its own runs, today's and
//! in-flight runs, their evidence, today's deleted spend), with exactly the totals that loading
//! every row gives, across the audit's accounting cases.

mod engine_support;

use std::collections::HashMap;

use agentflow_core::config::CostControls;
use agentflow_core::db::table::{self, Tracked};
use agentflow_core::db::{DbTime, DeletedRunSpendRow, Json, NodeRunRow, RunEvidenceRow, TaskRow, new_uid};
use agentflow_core::engine::rules::{self, Spending};
use agentflow_core::engine::spec;
use chrono::{Duration, NaiveDate, NaiveDateTime};
use engine_support::{Harness, engine};
use serde_json::{Value, json};

fn at(day: NaiveDate, time: &str) -> DbTime {
    DbTime(NaiveDateTime::parse_from_str(&format!("{day} {time}"), "%Y-%m-%d %H:%M:%S%.f").unwrap())
}

/// Another task, so runs can belong to unrelated history.
fn other_task(harness: &Harness) -> i64 {
    let mut task = harness.load().task.row.clone();
    task.id = 0;
    task.jira_key = "OTHER-1".into();
    task.zellij_session_name = "agentflow-OTHER-1".into();
    let mut task = Tracked::<TaskRow>::new(task);
    harness.engine.db.session(|tx| task.flush(tx)).unwrap();
    task.id
}

struct Run<'a> {
    task: i64,
    state: &'a str,
    started: DbTime,
    completed: Option<DbTime>,
    /// (billing mode, billed cost)
    charge: Option<(&'a str, Option<f64>)>,
}

fn add(harness: &Harness, node_id: i64, run: Run<'_>) -> i64 {
    let mut row = NodeRunRow {
        id: 0,
        task_id: run.task,
        node_id,
        run_number: 1,
        iteration: 1,
        state: run.state.into(),
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
        started_at: run.started,
        completed_at: run.completed,
    };
    harness.engine.db.session(|tx| table::insert(tx, &mut row)).unwrap();
    if let Some((mode, cost)) = run.charge {
        let usage = match cost {
            Some(cost) => json!({"inputTokens": 1, "outputTokens": 1, "costUsd": cost, "costKind": "billed"}),
            None => Value::Null,
        };
        let mut evidence = RunEvidenceRow {
            uid: new_uid(),
            run_id: row.id,
            requirements_digest: None,
            verification: Json(Value::Null),
            usage: Json(usage),
            confirmed_model: None,
            billing_mode: mode.into(),
        };
        harness.engine.db.session(|tx| table::insert(tx, &mut evidence)).unwrap();
    }
    row.id
}

fn deleted(harness: &Harness, charged_at: DbTime, mode: &str, cost: Option<f64>) {
    let mut row = DeletedRunSpendRow {
        id: 0,
        jira_key: "GONE-1".into(),
        run_id: 999,
        charged_at,
        billing_mode: mode.into(),
        cost_usd: cost,
    };
    harness.engine.db.session(|tx| table::insert(tx, &mut row)).unwrap();
}

fn controls() -> CostControls {
    CostControls { task_budget_usd: Some(100.0), daily_budget_usd: Some(100.0), ..Default::default() }
}

/// Budget status from every row (as before the fix) and from the bounded load.
fn both(harness: &Harness, day: NaiveDate) -> (Value, Value, rules::SpendingRows) {
    let task = harness.load().task.row.clone();
    let (runs, evidence, gone): (Vec<NodeRunRow>, Vec<RunEvidenceRow>, Vec<DeletedRunSpendRow>) = harness
        .engine
        .db
        .session(|tx| -> rusqlite::Result<_> {
            Ok((table::select(tx, "", [])?, table::select(tx, "", [])?, table::select(tx, "", [])?))
        })
        .unwrap();
    let full = Spending {
        runs: runs.iter().collect(),
        evidence: evidence.iter().map(|e| (e.run_id, e)).collect::<HashMap<_, _>>(),
        deleted: &gone,
    };
    let full = rules::budget_status(&full, Some(&task), &controls());
    let rows = harness.engine.db.session(|tx| rules::load_spending(tx, task.id, day)).unwrap();
    let bounded = rules::budget_status(&rows.spending(), Some(&task), &controls());
    (full, bounded, rows)
}

#[tokio::test]
async fn the_bounded_load_counts_exactly_what_loading_everything_counts() {
    let harness = engine().await;
    let day = rules::budget_day();
    let yesterday = day - Duration::days(1);
    let mine = harness.load().task.id;
    let other = other_task(&harness);
    let node = harness.load().node(spec::IMPLEMENTER).unwrap().id;
    let completed = |task, start: DbTime, end: DbTime, charge| Run {
        task,
        state: "COMPLETED",
        started: start,
        completed: Some(end),
        charge,
    };
    // This task, across its life: an old metered run with a cost, an old one with unknown cost.
    add(
        &harness,
        node,
        completed(
            mine,
            at(yesterday - Duration::days(30), "10:00:00"),
            at(yesterday - Duration::days(30), "11:00:00"),
            Some(("metered", Some(2.0))),
        ),
    );
    add(
        &harness,
        node,
        completed(
            mine,
            at(yesterday - Duration::days(20), "10:00:00"),
            at(yesterday - Duration::days(20), "11:00:00"),
            Some(("metered", None)),
        ),
    );
    // UTC boundary: the last microsecond of yesterday is not today; midnight is.
    add(
        &harness,
        node,
        completed(other, at(yesterday, "23:00:00"), at(yesterday, "23:59:59.999999"), Some(("metered", Some(4.0)))),
    );
    add(&harness, node, completed(other, at(yesterday, "23:30:00"), at(day, "00:00:00"), Some(("metered", Some(8.0)))));
    // Started yesterday and still running or interrupted: reserved today across midnight.
    add(
        &harness,
        node,
        Run {
            task: other,
            state: "RUNNING",
            started: at(yesterday, "22:00:00"),
            completed: None,
            charge: Some(("metered", None)),
        },
    );
    add(
        &harness,
        node,
        Run { task: other, state: "INTERRUPTED", started: at(yesterday, "21:00:00"), completed: None, charge: None },
    );
    // Subscription and estimated costs today never count as billed API spend.
    add(&harness, node, completed(other, at(day, "01:00:00"), at(day, "02:00:00"), Some(("subscription", Some(16.0)))));
    // Deleted-task spend: today's counts (known and unknown), yesterday's does not.
    deleted(&harness, at(day, "03:00:00"), "metered", Some(32.0));
    deleted(&harness, at(day, "04:00:00"), "metered", None);
    deleted(&harness, at(yesterday, "05:00:00"), "metered", Some(64.0));

    let (full, bounded, _) = both(&harness, day);
    assert_eq!(bounded, full);
    assert_eq!(full["task"]["knownUsd"], 2.0);
    assert_eq!(full["task"]["unknownRuns"], 1);
    assert_eq!(full["daily"]["knownUsd"], 8.0 + 32.0);
    assert_eq!(full["daily"]["unknownRuns"], 1, "today's unknown deleted spend");
    assert_eq!(full["daily"]["reservedUsd"], 2.0, "both in-flight runs hold a reservation");
}

/// Old completed runs of other tasks, with their evidence, are not read for a task's budget.
#[tokio::test]
async fn unrelated_history_is_not_loaded() {
    let harness = engine().await;
    let day = rules::budget_day();
    let mine = harness.load().task.id;
    let other = other_task(&harness);
    let node = harness.load().node(spec::IMPLEMENTER).unwrap().id;
    let old = day - Duration::days(40);
    for _ in 0..200 {
        add(
            &harness,
            node,
            Run {
                task: other,
                state: "COMPLETED",
                started: at(old, "10:00:00"),
                completed: Some(at(old, "11:00:00")),
                charge: Some(("metered", Some(1.0))),
            },
        );
    }
    let own = add(
        &harness,
        node,
        Run {
            task: mine,
            state: "COMPLETED",
            started: at(old, "10:00:00"),
            completed: Some(at(old, "11:00:00")),
            charge: Some(("metered", Some(1.0))),
        },
    );
    let today = add(
        &harness,
        node,
        Run {
            task: other,
            state: "COMPLETED",
            started: at(day, "00:30:00"),
            completed: Some(at(day, "00:40:00")),
            charge: Some(("metered", Some(1.0))),
        },
    );
    deleted(&harness, at(old, "12:00:00"), "metered", Some(5.0));

    let (full, bounded, rows) = both(&harness, day);
    assert_eq!(bounded, full);
    let loaded: Vec<i64> = rows.runs.iter().map(|r| r.id).collect();
    assert_eq!(loaded, [own, today]);
    let evidence: Vec<i64> = rows.evidence.iter().map(|e| e.run_id).collect();
    assert_eq!(evidence.len(), 2);
    assert!(evidence.contains(&own) && evidence.contains(&today));
    assert!(rows.deleted.is_empty());
}
