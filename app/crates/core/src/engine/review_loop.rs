//! Explicit implementer responses and reviewer rulings. Rejections leave the run active.
use super::agents::finding_dump;
use serde_json::{Map, Value};

fn text(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty())
}
fn entries<'a>(raw: &'a Map<String, Value>, key: &str) -> (Vec<&'a Value>, Vec<String>) {
    match raw.get(key) {
        None | Some(Value::Null) => (vec![], vec![]),
        Some(Value::Array(a)) if a.iter().all(Value::is_object) => (a.iter().collect(), vec![]),
        _ => (vec![], vec![format!("\"{key}\" must be a list of objects")]),
    }
}

pub fn blocking_ids(given: &[Value]) -> Vec<String> {
    let mut ids = Vec::new();
    for f in given {
        if f["blocking"] == true
            && let Some(id) = f["id"].as_str().filter(|s| !s.is_empty())
            && !ids.iter().any(|s| s == id)
        {
            ids.push(id.to_string());
        }
    }
    ids
}

pub fn review_problems(
    raw: &Map<String, Value>,
    prior: Option<&[String]>,
    reported: &[Map<String, Value>],
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut reraised = Vec::new();
    for f in reported {
        let Some(id) = f.get("priorId").and_then(Value::as_str).filter(|s| !s.is_empty()) else { continue };
        match prior {
            None => problems.push(format!("finding \"{}\" has priorId {id}, but there is no previous review to re-raise from; remove priorId", f.get("title").and_then(Value::as_str).unwrap_or(""))),
            Some(ids) if !ids.iter().any(|s| s == id) => problems.push(format!("priorId {id} is not one of your previous blocking findings ({})", if ids.is_empty() { "there were none".into() } else { ids.join(", ") })),
            Some(_) if reraised.contains(&id) => problems.push(format!("{id} is re-raised more than once; report it as a single finding (findings recorded with flowctl finding are already included)")),
            Some(_) => {
                reraised.push(id);
                if f.get("blocking") != Some(&Value::Bool(true)) { problems.push(format!("re-raised {id} must keep \"blocking\": true; if it no longer needs to block, give it status \"withdrawn\" with evidence instead of re-raising it")); }
            }
        }
    }
    let Some(prior) = prior else { return problems };
    let (verdicts, shape) = entries(raw, "priorFindings");
    problems.extend(shape);
    let mut ruled = Vec::new();
    for verdict in verdicts {
        let id = verdict["id"].as_str().unwrap_or("");
        if !prior.iter().any(|s| s == id) {
            problems.push(format!(
                "priorFindings entry {} is not one of your previous blocking findings",
                super::launch::py_repr(id)
            ));
            continue;
        }
        if ruled.iter().any(|(old, _)| *old == id) {
            problems.push(format!("{id} has more than one priorFindings entry"));
            continue;
        }
        let status = verdict["status"].as_str().unwrap_or("");
        ruled.push((id, status));
        if !["fixed", "not_fixed", "withdrawn"].contains(&status) {
            problems.push(format!(
                "{id}: status must be one of fixed, not_fixed, withdrawn (got {})",
                super::launch::py_repr(status)
            ));
        }
        if !text(verdict.get("evidence")) {
            problems.push(format!("{id}: evidence is required (the file:line or test that shows it)"));
        }
    }
    let missing = prior.iter().filter(|id| !ruled.iter().any(|(r, _)| r == id)).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        problems.push(format!("no verdict for prior blocking finding(s): {}", missing.join(", ")));
    }
    for (id, status) in ruled {
        if status == "not_fixed" && !reraised.contains(&id) {
            problems.push(format!(
                "{id} is \"not_fixed\" but is not re-reported in \"findings\" with \"priorId\": \"{id}\""
            ));
        } else if ["fixed", "withdrawn"].contains(&status) && reraised.contains(&id) {
            problems
                .push(format!("{id} is \"{status}\" but is also re-raised; use \"not_fixed\" if it is still broken"));
        }
    }
    problems
}

pub fn response_problems(raw: &Map<String, Value>, given: &[Value]) -> Vec<String> {
    let blocking = blocking_ids(given);
    let (responses, mut problems) = entries(raw, "findingResponses");
    let mut answered = Vec::new();
    for response in responses {
        let id = response["id"].as_str();
        if id.is_none() || !given.iter().any(|f| f["id"].as_str() == id) {
            problems.push(format!("findingResponses entry {} is not a finding from the last review", response["id"]));
            continue;
        }
        let id = id.unwrap();
        if answered.contains(&id) {
            problems.push(format!("{id} has more than one findingResponses entry"));
            continue;
        }
        answered.push(id);
        let action = response["action"].as_str().unwrap_or("");
        if !["fixed", "disputed", "deferred"].contains(&action) {
            problems.push(format!(
                "{id}: action must be one of fixed, disputed, deferred (got {})",
                super::launch::py_repr(action)
            ));
        } else if action == "deferred" && blocking.iter().any(|s| s == id) {
            problems.push(format!(
                "{id} is blocking, so it cannot be \"deferred\": fix it, or dispute it with a rationale"
            ));
        }
        if !text(response.get("rationale")) {
            problems
                .push(format!("{id}: rationale is required (what you changed and where, or why the finding is wrong)"));
        }
    }
    let missing = blocking.iter().filter(|id| !answered.iter().any(|r| r == id)).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        problems.push(format!("no response for blocking finding(s): {}", missing.join(", ")));
    }
    problems
}

pub fn review_rejection(problems: &[String], prior: Option<&[String]>) -> String {
    let mut lines = vec![
        "Review result not accepted. This run is still active: fix the result and run flowctl complete again.".into(),
    ];
    lines.extend(problems.iter().map(|p| format!("- {p}")));
    if let Some(id) = prior.and_then(|ids| ids.first()) {
        lines.extend([
            "Every blocking finding from your previous review needs exactly one verdict in \"priorFindings\", e.g.:".into(),
            format!("  \"priorFindings\": [{{\"id\": \"{id}\", \"status\": \"fixed | not_fixed | withdrawn\", \"evidence\": \"the file:line or test that shows it\"}}]"),
            "A \"not_fixed\" finding must also be re-reported in \"findings\" with \"priorId\": \"<old id>\" and \"blocking\": true.".into(),
        ]);
    }
    lines.join("\n")
}

pub fn response_rejection(problems: &[String], given: &[Value]) -> String {
    let ids = blocking_ids(given);
    let id = ids.first().map(String::as_str).unwrap_or("CODE-001");
    let mut lines = vec![
        "Implementation result not accepted. This run is still active: fix the result and run flowctl complete again."
            .into(),
    ];
    lines.extend(problems.iter().map(|p| format!("- {p}")));
    lines.extend([
        "Every blocking finding needs exactly one entry in \"findingResponses\", e.g.:".into(),
        format!("  \"findingResponses\": [{{\"id\": \"{id}\", \"action\": \"fixed | disputed\", \"rationale\": \"what changed and where, or why the finding is wrong\"}}]"),
        "\"deferred\" is allowed only for non-blocking findings.".into(),
    ]);
    lines.join("\n")
}

/// Normalize findings and remove only exact duplicates, including incremental reports.
pub fn reported_findings(reported: &[Value], pending: &[Value]) -> Vec<Map<String, Value>> {
    let mut findings = Vec::new();
    for item in reported.iter().chain(pending) {
        if let Ok(finding) = finding_dump(item)
            && !findings.contains(&finding)
        {
            findings.push(finding);
        }
    }
    findings
}
