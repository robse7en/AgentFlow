//! Structured completion contracts, aliases, defaults and validation.
use serde_json::{Map, Value, json};

fn field(raw: &Map<String, Value>, alias: &str) -> Option<Value> {
    let mut snake = String::new();
    for c in alias.chars() {
        if c.is_uppercase() {
            snake.push('_');
            snake.extend(c.to_lowercase());
        } else {
            snake.push(c);
        }
    }
    raw.get(alias).or_else(|| raw.get(&snake)).cloned()
}

/// Pydantic's declared field ordering/defaults, with alias input and preserved extra fields.
pub fn normalize(raw: &Map<String, Value>, kind: &str) -> Result<Map<String, Value>, String> {
    let fields: Vec<(&str, Value, &str)> = match kind {
        "implementation" => vec![
            ("status", json!("completed"), "status"),
            ("summary", json!(""), "str"),
            ("filesChanged", json!([]), "strings"),
            ("tests", Value::Null, "tests"),
            ("assumptions", json!([]), "strings"),
            ("knownIssues", json!([]), "strings"),
            ("findingResponses", json!([]), "responses"),
            ("checkpointSha", Value::Null, "optional"),
        ],
        "review" => vec![
            ("status", json!("completed"), "review_status"),
            ("reviewedSha", Value::Null, "optional"),
            ("verdict", Value::Null, "verdict"),
            ("summary", json!(""), "str"),
            ("findings", json!([]), "findings"),
            ("priorFindings", json!([]), "rulings"),
        ],
        _ => vec![
            ("status", json!("completed"), "review_status"),
            ("taskSummary", json!(""), "str"),
            ("complexity", Value::Null, "optional"),
            ("complexityScore", Value::Null, "integer"),
            ("originalScope", Value::Null, "optional"),
            ("implementationPerformed", Value::Null, "optional"),
            ("filesAffected", json!([]), "strings"),
            ("keyFindings", json!([]), "strings"),
            ("reviewFindings", Value::Null, "optional"),
            ("fixesMade", Value::Null, "optional"),
            ("testsPerformed", Value::Null, "optional"),
            ("knownLimitations", json!([]), "strings"),
            ("outstandingUncertainty", json!([]), "strings"),
            ("regressionRisk", Value::Null, "optional"),
            ("securityRisk", Value::Null, "optional"),
            ("overallRisk", json!("MEDIUM"), "risk"),
            ("riskFactors", json!([]), "strings"),
            ("humanReviewRecommendation", json!(""), "str"),
            ("requirementsVerified", Value::Null, "boolean"),
        ],
    };
    let mut result = Map::new();
    for (name, default, ty) in &fields {
        let mut value = field(raw, name).unwrap_or_else(|| default.clone());
        let valid = match *ty {
            "str" => value.is_string(),
            "optional" => value.is_null() || value.is_string(),
            "strings" => value.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            "status" => value.as_str().is_some_and(|s| ["completed", "failed", "blocked"].contains(&s)),
            "review_status" => value.as_str().is_some_and(|s| ["completed", "failed"].contains(&s)),
            "verdict" => {
                value.is_null() || value.as_str().is_some_and(|s| ["approved", "changes_required"].contains(&s))
            }
            "integer" => value.is_null() || value.as_i64().is_some(),
            "boolean" => value.is_null() || value.is_boolean(),
            "risk" => {
                if let Some(s) = value.as_str() {
                    value = json!(s.to_uppercase());
                }
                value.as_str().is_some_and(|s| ["LOW", "MEDIUM", "HIGH"].contains(&s))
            }
            "tests" => {
                if value.is_null() {
                    true
                } else {
                    let data = value.as_object().ok_or("tests: must be an object")?;
                    let mut tests = Map::new();
                    for name in ["executed", "passed", "failed"] {
                        let v = data.get(name).cloned().unwrap_or(Value::Null);
                        if !v.is_null() && v.as_i64().is_none_or(|n| n < 0) {
                            return Err(format!("tests.{name}: must be nonnegative"));
                        }
                        tests.insert(name.into(), v);
                    }
                    let notes = data.get("notes").cloned().unwrap_or(Value::Null);
                    if !notes.is_null() && !notes.is_string() {
                        return Err("tests.notes: must be a string".into());
                    }
                    tests.insert("notes".into(), notes);
                    for (k, v) in data {
                        if !tests.contains_key(k) {
                            tests.insert(k.clone(), v.clone());
                        }
                    }
                    if let Some(executed) = tests["executed"].as_i64()
                        && i128::from(tests["passed"].as_i64().unwrap_or(0))
                            + i128::from(tests["failed"].as_i64().unwrap_or(0))
                            > i128::from(executed)
                    {
                        return Err("Passed and failed counts exceed executed tests".into());
                    }
                    value = Value::Object(tests);
                    true
                }
            }
            "findings" => {
                let list = value.as_array().ok_or("findings: must be a list")?;
                let normalized = list.iter().map(super::agents::finding_dump).collect::<Result<Vec<_>, _>>()?;
                value = json!(normalized);
                true
            }
            "responses" | "rulings" => {
                let list = value.as_array().ok_or_else(|| format!("{name}: must be a list"))?;
                let note = if *ty == "responses" { "rationale" } else { "evidence" };
                let action = if *ty == "responses" { "action" } else { "status" };
                let mut normalized = Vec::new();
                for item in list {
                    let mut item = item.as_object().cloned().ok_or_else(|| format!("{name}: must be objects"))?;
                    if !item.get("id").is_some_and(Value::is_string) || !item.get(action).is_some_and(Value::is_string)
                    {
                        return Err(format!("{name}: id and {action} must be strings"));
                    }
                    item.entry(note).or_insert(json!(""));
                    if !item[note].is_string() {
                        return Err(format!("{name}.{note}: must be a string"));
                    }
                    normalized.push(Value::Object(item));
                }
                value = json!(normalized);
                true
            }
            _ => false,
        };
        if !valid {
            return Err(format!("{name}: invalid {ty}"));
        }
        result.insert((*name).into(), value);
    }
    for (key, value) in raw {
        if !fields.iter().any(|(alias, _, _)| {
            let snake = alias
                .chars()
                .flat_map(|c| if c.is_uppercase() { vec!['_', c.to_ascii_lowercase()] } else { vec![c] })
                .collect::<String>();
            key == alias || *key == snake
        }) {
            result.insert(key.clone(), value.clone());
        }
    }
    Ok(result)
}
