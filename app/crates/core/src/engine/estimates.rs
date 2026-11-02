//! What a stage usually costs, from the usage of its past runs, ported from
//! `backend/agentflow/workflow/estimates.py`. Display only: an estimate never feeds a budget, a
//! launch reserve or a launch decision.
//!
//! A stage's samples are the usage records of finished (COMPLETED or FAILED: a failed run is paid
//! for too) runs with the same stage, harness and model, most recent first. Workers share one
//! stage. Checked against Python's output in golden estimates-unit: values are picked from the
//! sorted samples by nearest rank, never interpolated or averaged, and totals add up in workflow
//! order.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::rules::{cost_kind, py_sum};
use crate::collectors::truthy;

/// The most recent runs a stage's estimate looks at.
pub const ESTIMATE_SAMPLES: usize = 20;
/// Fewer than this and the estimate says there is not enough history.
pub const ESTIMATE_MIN_RUNS: usize = 3;
pub const WORKER_STAGE: &str = "worker";
/// A review round that finds something to fix runs these again.
pub const FIX_ROUND_STAGES: [&str; 3] = ["implementer", "code_review", "security_review"];

pub fn stage_key(node_key: &str) -> &str {
    if node_key.starts_with("worker_") { WORKER_STAGE } else { node_key }
}

/// Nearest rank: the smallest value at or above `fraction` of the sorted values.
fn rank(values: &[Value], fraction: f64) -> Value {
    values[(fraction * values.len() as f64).ceil() as usize - 1].clone()
}

/// The typical (median) and high (90th percentile) cost of one run of a stage, from its samples
/// (usage records, most recent first). A subscription harness is measured in tokens, since its
/// runs cost nothing extra; anything else in USD, labelled with whether the figures are
/// provider-billed, list-price estimates, or a mix. Partial usage records are skipped.
pub fn estimate(samples: &[Value], billing_mode: &str) -> Value {
    let complete = samples.iter().filter(|s| truthy(Some(s)) && !truthy(s.get("partial")));
    let (unit, metric) = if billing_mode == "subscription" { ("tokens", "totalTokens") } else { ("usd", "costUsd") };
    let used: Vec<&Value> =
        complete.filter(|s| s.get(metric).is_some_and(|v| !v.is_null())).take(ESTIMATE_SAMPLES).collect();
    let mut values: Vec<Value> = used.iter().map(|s| s[metric].clone()).collect();
    values.sort_by(|a, b| a.as_f64().unwrap_or(0.0).total_cmp(&b.as_f64().unwrap_or(0.0)));
    let kinds: BTreeSet<String> =
        if unit == "usd" { used.iter().filter_map(|s| cost_kind(Some(s))).collect() } else { BTreeSet::new() };
    let enough = values.len() >= ESTIMATE_MIN_RUNS;
    let cost_kind = match kinds.len() {
        _ if !enough => Value::Null,
        0 => Value::Null,
        1 => json!(kinds.into_iter().next()),
        _ => json!("mixed"),
    };
    json!({
        "unit": unit, "runs": values.len(), "enough": enough,
        "typical": if enough { rank(&values, 0.5) } else { Value::Null },
        "high": if enough { rank(&values, 0.9) } else { Value::Null },
        "costKind": cost_kind,
    })
}

fn totals(stages: &[Value], keys: &[&str]) -> Value {
    let mut result = Map::new();
    for unit in ["usd", "tokens"] {
        let chosen: Vec<&Value> = stages
            .iter()
            .filter(|s| s["key"].as_str().is_some_and(|k| keys.contains(&k)))
            .map(|s| &s["estimate"])
            .filter(|e| e["enough"] == json!(true) && e["unit"] == json!(unit))
            .collect();
        let total = if chosen.is_empty() {
            Value::Null
        } else {
            json!({"typical": py_sum(chosen.iter().map(|e| &e["typical"])),
                   "high": py_sum(chosen.iter().map(|e| &e["high"]))})
        };
        result.insert(unit.into(), total);
    }
    Value::Object(result)
}

/// A route's estimate from its stages (`{key, label, estimate}`, in workflow order): one pass
/// through every stage but the workers, plus what each worker and each fix round adds, since how
/// many there will be is not known upfront. Stages without enough history are named in `missing`
/// and left out of the totals.
pub fn route(stages: Vec<Value>) -> Value {
    let keys: Vec<&str> = stages.iter().filter_map(|s| s["key"].as_str()).collect();
    let one_pass: Vec<&str> = keys.iter().copied().filter(|k| *k != WORKER_STAGE).collect();
    let fix_round: Vec<&str> = keys.iter().copied().filter(|k| FIX_ROUND_STAGES.contains(k)).collect();
    let per_worker = if keys.contains(&WORKER_STAGE) { totals(&stages, &[WORKER_STAGE]) } else { Value::Null };
    let missing: Vec<Value> =
        stages.iter().filter(|s| s["estimate"]["enough"] != json!(true)).map(|s| s["label"].clone()).collect();
    let (one_pass, per_fix_round) = (totals(&stages, &one_pass), totals(&stages, &fix_round));
    json!({
        "stages": stages, "onePass": one_pass, "perWorker": per_worker, "perFixRound": per_fix_round,
        "missing": missing,
    })
}
