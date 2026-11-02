//! Cost estimates against Python's own output for the same inputs (contract/golden/estimates-unit,
//! recorded by contract/record_python_goldens.py), plus the Python unit tests ported by name.

use std::path::Path;

use agentflow_core::engine::estimates;
use serde_json::{Value, json};

fn golden() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/estimates-unit/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn every_estimate_matches_python() {
    let cases = golden();
    assert!(cases.len() >= 15, "golden estimates-unit looks truncated");
    let mut failures = Vec::new();
    for case in cases {
        let actual = match case["function"].as_str().unwrap() {
            "estimate" => {
                estimates::estimate(case["samples"].as_array().unwrap(), case["billingMode"].as_str().unwrap())
            }
            "route" => estimates::route(case["stages"].as_array().unwrap().clone()),
            other => panic!("unknown function {other}"),
        };
        // Compared as text too: 0.5 and 0.50000000000001, or 2 and 2.0, must not pass as equal.
        let (actual_text, expected_text) = (actual.to_string(), case["result"].to_string());
        if actual != case["result"] || actual_text != expected_text {
            failures.push(format!("{}:\n  python {}\n  rust   {}", case["name"], case["result"], actual));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn usd(cost: f64) -> Value {
    json!({"costUsd": cost, "totalTokens": null})
}

#[test]
fn test_workers_share_one_stage() {
    assert_eq!(estimates::stage_key("worker_3f2a9c"), "worker");
    assert_eq!(estimates::stage_key("worker_b"), "worker");
    assert_eq!(estimates::stage_key("implementer"), "implementer");
}

#[test]
fn test_not_enough_history_gives_no_numbers() {
    let result = estimates::estimate(&[usd(1.0), usd(2.0)], "metered");
    assert_eq!(
        result,
        json!({"unit": "usd", "runs": 2, "enough": false, "typical": null, "high": null, "costKind": null})
    );
}

#[test]
fn test_typical_and_high_are_nearest_rank_median_and_90th_percentile() {
    let samples: Vec<Value> = [0.9, 0.1, 0.5, 0.3, 0.7].into_iter().map(usd).collect();
    let result = estimates::estimate(&samples, "metered");
    assert_eq!((result["typical"].as_f64(), result["high"].as_f64()), (Some(0.5), Some(0.9)));
    assert_eq!(result["costKind"], "billed");
    let ten: Vec<Value> = (1..=10).map(|n| usd(n as f64)).collect();
    let result = estimates::estimate(&ten, "unknown");
    assert_eq!((result["typical"].as_f64(), result["high"].as_f64()), (Some(5.0), Some(9.0)));
}

#[test]
fn test_only_the_most_recent_complete_samples_count() {
    let mut samples = vec![usd(1.0); 20];
    samples.extend(vec![usd(100.0); 5]);
    assert_eq!(estimates::estimate(&samples, "metered")["high"].as_f64(), Some(1.0));
    let mut partial = vec![json!({"costUsd": 50.0, "partial": true}); 3];
    partial.extend(vec![usd(1.0); 3]);
    assert_eq!(estimates::estimate(&partial, "metered")["high"].as_f64(), Some(1.0));
    let sparse = [Value::Null, json!({"costUsd": null}), usd(1.0), usd(2.0), usd(3.0)];
    assert_eq!(estimates::estimate(&sparse, "metered")["runs"], 3);
}

#[test]
fn test_subscription_is_measured_in_tokens_and_list_prices_are_labelled() {
    let samples: Vec<Value> = [1000, 3000, 2000]
        .into_iter()
        .map(|n| json!({"costUsd": 0.4, "costKind": "estimated", "totalTokens": n}))
        .collect();
    assert_eq!(
        estimates::estimate(&samples, "subscription"),
        json!({"unit": "tokens", "runs": 3, "enough": true, "typical": 2000, "high": 3000, "costKind": null})
    );
    assert_eq!(estimates::estimate(&samples, "metered")["costKind"], "estimated");
    let mixed = [samples[0].clone(), samples[1].clone(), usd(0.4)];
    assert_eq!(estimates::estimate(&mixed, "metered")["costKind"], "mixed");
}
