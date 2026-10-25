//! Pure rules over loaded rows, ported from `backend/agentflow/workflow/`: `attention.py`,
//! `budgets.py`, the requirements digest and check of `assurance.py`, `usage.cost_kind`, and the
//! engine's usage summary.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::states::{NodeState, TaskStatus};
use crate::config::CostControls;
use crate::db::{
    AgentQuestionRow, DbTime, DeletedRunSpendRow, NodeRunRow, RequirementsApprovalRow, RunEvidenceRow, TaskRow,
    WorkflowNodeRow,
};

/// Python's `datetime.isoformat()` of a naive value plus "Z", as attention items write it.
fn naive_iso_z(time: &DbTime) -> String {
    let iso = time.isoformat();
    format!("{}Z", iso.trim_end_matches("+00:00"))
}

fn state_in(state: &str, states: &[NodeState]) -> bool {
    NodeState::parse(state).is_some_and(|s| states.contains(&s))
}

/// Dashboard actions, including parallel nodes a single phase label would hide.
pub fn attention_items(
    task: &TaskRow,
    nodes: &[&WorkflowNodeRow],
    runs: &[&NodeRunRow],
    questions: &[&AgentQuestionRow],
) -> Vec<Value> {
    let status = TaskStatus::parse(&task.status);
    if status.is_some_and(|s| s.is_post_ready() || s == TaskStatus::Cancelled) {
        return Vec::new();
    }
    const ACTIONS: [(&str, &str); 5] = [
        ("WAITING_FOR_USER", "Answer questions"),
        ("WAITING_FOR_APPROVAL", "Review approval"),
        ("FAILED", "Review failure"),
        ("INTERRUPTED", "Recover agent"),
        ("READY", "Start stage"),
    ];
    let mut current: HashMap<i64, &NodeRunRow> = HashMap::new();
    for run in runs {
        if current.get(&run.node_id).is_none_or(|c| run.id > c.id) {
            current.insert(run.node_id, run);
        }
    }
    let mut items: Vec<(usize, String, Value)> = Vec::new();
    for node in nodes {
        let Some(priority) = ACTIONS.iter().position(|(state, _)| *state == node.state) else { continue };
        let run = current.get(&node.id);
        let pending: Vec<&&AgentQuestionRow> = questions
            .iter()
            .filter(|q| {
                !q.answered && run.is_some_and(|r| Some(r.id) == q.node_run_id && r.state == "WAITING_FOR_USER")
            })
            .collect();
        let since = pending.iter().map(|q| q.created_at).min().unwrap_or(node.updated_at);
        let since = naive_iso_z(&since);
        items.push((
            priority,
            since.clone(),
            json!({"nodeKey": node.key, "label": node.label, "state": node.state, "since": since,
                   "questions": pending.len(), "action": ACTIONS[priority].1}),
        ));
    }
    items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    items.into_iter().map(|(_, _, item)| item).collect()
}

// ---------------------------------------------------------------------------------- usage

/// Stored records predating costKind carry provider-reported costs.
pub fn cost_kind(record: Option<&Value>) -> Option<String> {
    let record = record.filter(|r| !r.is_null())?;
    if record.get("costUsd").is_none_or(Value::is_null) {
        return None;
    }
    Some(record.get("costKind").and_then(Value::as_str).filter(|k| !k.is_empty()).unwrap_or("billed").to_string())
}

/// Python's `sum()` of JSON numbers: an integer while every addend is one, else a float.
pub fn py_sum<'a>(values: impl IntoIterator<Item = &'a Value>) -> Value {
    let (mut int, mut float, mut is_float) = (0i64, 0.0f64, false);
    for value in values {
        match value.as_i64() {
            Some(n) if !is_float && value.is_i64() => int += n,
            _ => {
                if !is_float {
                    float = int as f64;
                    is_float = true;
                }
                float += value.as_f64().unwrap_or(0.0);
            }
        }
    }
    if is_float { json!(float) } else { json!(int) }
}

/// What a set of runs used and cost (the engine's `_usage_summary`), or None for no runs.
pub fn usage_summary(runs: &[&NodeRunRow], evidence: &HashMap<i64, &RunEvidenceRow>) -> Option<Value> {
    if runs.is_empty() {
        return None;
    }
    let usage = |run: &NodeRunRow| evidence.get(&run.id).map(|e| &e.usage.0).filter(|u| !u.is_null());
    let reports: Vec<&Value> = runs.iter().filter_map(|r| usage(r)).collect();
    let cost = |r: &&Value| r.get("costUsd").filter(|c| !c.is_null()).cloned();
    let costs: Vec<Value> = reports.iter().filter_map(cost).collect();
    let mode = |run: &NodeRunRow| evidence.get(&run.id).map_or("unknown".to_string(), |e| e.billing_mode.clone());
    let metered: Vec<&&NodeRunRow> = runs.iter().filter(|r| mode(r) == "metered").collect();
    let api_costs: Vec<Value> = metered
        .iter()
        .filter_map(|r| usage(r))
        .filter(|u| cost_kind(Some(u)).as_deref() == Some("billed"))
        .filter_map(|u| cost(&u))
        .collect();
    let of_kind = |kind: &str| -> Vec<Value> {
        reports.iter().filter(|r| cost_kind(Some(r)).as_deref() == Some(kind)).filter_map(cost).collect()
    };
    let (billed, estimated) = (of_kind("billed"), of_kind("estimated"));
    let tokened: Vec<&&Value> = reports.iter().filter(|r| r.get("totalTokens").is_some_and(|t| !t.is_null())).collect();
    let total = |key: &str| py_sum(tokened.iter().map(|r| r.get(key).unwrap_or(&Value::Null)));
    let sum_or_null = |values: &[Value]| if values.is_empty() { Value::Null } else { py_sum(values) };
    let modes: Vec<String> = runs.iter().map(|r| mode(r)).collect();
    Some(json!({
        "runs": runs.len(), "runsWithUsage": reports.len(), "runsWithTokens": tokened.len(),
        "runsWithCost": costs.len(), "meteredRuns": metered.len(),
        "meteredRunsWithCost": api_costs.len(),
        "subscriptionRuns": modes.iter().filter(|m| *m == "subscription").count(),
        "unknownBillingRuns": modes.iter().filter(|m| *m == "unknown").count(),
        "apiCostUsd": if !api_costs.is_empty() { py_sum(&api_costs) } else if metered.is_empty() { json!(0) } else { Value::Null },
        "inputTokens": total("inputTokens"),
        "outputTokens": total("outputTokens"),
        "cachedInputTokens": total("cachedInputTokens"),
        "totalTokens": total("totalTokens"),
        "costUsd": sum_or_null(&costs),
        "runsWithBilledCost": billed.len(), "billedCostUsd": sum_or_null(&billed),
        "runsWithEstimatedCost": estimated.len(), "estimatedCostUsd": sum_or_null(&estimated),
    }))
}

// ---------------------------------------------------------------------------------- budgets

/// (billing mode, provider-billed metered cost or None) for a run's evidence row.
pub fn run_charge(record: Option<&RunEvidenceRow>) -> (String, Option<f64>) {
    let mode = record.map_or("unknown".to_string(), |r| r.billing_mode.clone());
    // List-price estimates are for display only; a limit needs the provider's own figure.
    let cost = record
        .filter(|_| mode == "metered")
        .filter(|r| cost_kind(Some(&r.usage.0)).as_deref() == Some("billed"))
        .and_then(|r| r.usage.0.get("costUsd").and_then(Value::as_f64));
    (mode, cost)
}

/// The whole run is charged to one UTC day: that of completion, or start while active.
pub fn charged_at(run: &NodeRunRow) -> DbTime {
    run.completed_at.unwrap_or(run.started_at)
}

const BUSY: [NodeState; 3] = [NodeState::Running, NodeState::WaitingForUser, NodeState::Interrupted];

fn spend(runs: &[&NodeRunRow], evidence: &HashMap<i64, &RunEvidenceRow>, reserve: f64) -> Map<String, Value> {
    let (mut known, mut reserved, mut unknown) = (0.0f64, 0.0f64, 0i64);
    for run in runs {
        let (mode, cost) = run_charge(evidence.get(&run.id).copied());
        if mode == "subscription" {
            continue;
        }
        if let Some(cost) = cost {
            known += cost;
        }
        if state_in(&run.state, &BUSY) {
            reserved += reserve;
        } else if cost.is_none() {
            unknown += 1;
        }
    }
    let mut group = Map::new();
    group.insert("knownUsd".into(), json!(known));
    group.insert("reservedUsd".into(), json!(reserved));
    group.insert("unknownRuns".into(), json!(unknown));
    group
}

fn today() -> chrono::NaiveDate {
    chrono::Utc::now().date_naive()
}

fn daily(
    runs: &[&NodeRunRow],
    evidence: &HashMap<i64, &RunEvidenceRow>,
    controls: &CostControls,
    deleted: &[DeletedRunSpendRow],
) -> Map<String, Value> {
    let today = today();
    let todays: Vec<&NodeRunRow> =
        runs.iter().copied().filter(|r| charged_at(r).0.date() == today || state_in(&r.state, &BUSY)).collect();
    let mut group = spend(&todays, evidence, controls.launch_reserve_usd.get());
    // Runs of deleted tasks still count (deleting a task must not free budget or hide unknown cost).
    for row in deleted {
        if row.charged_at.0.date() != today || row.billing_mode == "subscription" {
            continue;
        }
        match row.cost_usd {
            Some(cost) => group["knownUsd"] = json!(group["knownUsd"].as_f64().unwrap_or(0.0) + cost),
            None => group["unknownRuns"] = json!(group["unknownRuns"].as_i64().unwrap_or(0) + 1),
        }
    }
    group.insert("limitUsd".into(), json!(controls.daily_budget_usd));
    group
}

fn warn(mut group: Map<String, Value>, controls: &CostControls) -> Value {
    let limit = group["limitUsd"].as_f64().filter(|l| *l != 0.0);
    let spent = group["knownUsd"].as_f64().unwrap_or(0.0) + group["reservedUsd"].as_f64().unwrap_or(0.0);
    let warning = limit.is_some_and(|limit| spent >= limit * controls.warning_percent as f64 / 100.0);
    group.insert("warning".into(), json!(warning));
    Value::Object(group)
}

/// The rows budget_status reads for one task, loaded by [`load_spending`].
pub struct SpendingRows {
    pub runs: Vec<NodeRunRow>,
    pub evidence: Vec<RunEvidenceRow>,
    pub deleted: Vec<DeletedRunSpendRow>,
}

impl SpendingRows {
    pub fn spending(&self) -> Spending<'_> {
        Spending {
            runs: self.runs.iter().collect(),
            evidence: self.evidence.iter().map(|e| (e.run_id, e)).collect(),
            deleted: &self.deleted,
        }
    }
}

/// Exactly what budget_status can count for `task_id` on `day` (UTC), rather than every run and
/// evidence row ever recorded (AF-AUDIT-005): the task's runs over its whole life, other runs
/// charged that day or still in flight (they hold a reservation across midnight), the evidence of
/// those runs, and that day's spend of deleted tasks. budget_status filters the same way, so
/// loading this superset changes no total.
pub fn load_spending(
    tx: &rusqlite::Transaction,
    task_id: i64,
    day: chrono::NaiveDate,
) -> rusqlite::Result<SpendingRows> {
    use crate::db::table;
    // Stored times are "YYYY-MM-DD HH:MM:SS.ffffff", so a day is a text range.
    let (start, end) = (day.to_string(), day.succ_opt().unwrap_or(day).to_string());
    let runs = "task_id = ?1 OR state IN ('RUNNING', 'WAITING_FOR_USER', 'INTERRUPTED') \
                OR (COALESCE(completed_at, started_at) >= ?2 AND COALESCE(completed_at, started_at) < ?3)";
    let params = rusqlite::params![task_id, start, end];
    Ok(SpendingRows {
        runs: table::select(tx, &format!("WHERE {runs} ORDER BY id"), params)?,
        evidence: table::select(tx, &format!("WHERE run_id IN (SELECT id FROM node_runs WHERE {runs})"), params)?,
        deleted: table::select(tx, "WHERE charged_at >= ?1 AND charged_at < ?2", rusqlite::params![start, end])?,
    })
}

/// The UTC day budgets are counted for.
pub fn budget_day() -> chrono::NaiveDate {
    today()
}

/// All runs, evidence and deleted spend, as budget_status needs them.
pub struct Spending<'a> {
    pub runs: Vec<&'a NodeRunRow>,
    pub evidence: HashMap<i64, &'a RunEvidenceRow>,
    pub deleted: &'a [DeletedRunSpendRow],
}

/// The task's and today's API spend against their limits (budgets.budget_status).
pub fn budget_status(spending: &Spending<'_>, task: Option<&TaskRow>, controls: &CostControls) -> Value {
    let task_runs: Vec<&NodeRunRow> =
        spending.runs.iter().copied().filter(|r| task.is_some_and(|t| r.task_id == t.id)).collect();
    let mut task_group = spend(&task_runs, &spending.evidence, controls.launch_reserve_usd.get());
    let limit = task.and_then(|t| t.api_budget_usd).filter(|l| *l != 0.0).or(controls.task_budget_usd);
    task_group.insert("limitUsd".into(), json!(limit));
    json!({
        "task": warn(task_group, controls),
        "daily": warn(daily(&spending.runs, &spending.evidence, controls, spending.deleted), controls),
        "warningPercent": controls.warning_percent,
        "launchReserveUsd": controls.launch_reserve_usd,
        "dayUtc": today().to_string(),
    })
}

/// How many of these runs leave today's API spend incomplete (what launch_blocker refuses on).
pub fn unknown_cost_runs_today(
    runs: &[&NodeRunRow],
    evidence: &HashMap<i64, &RunEvidenceRow>,
    controls: &CostControls,
) -> i64 {
    daily(runs, evidence, controls, &[])["unknownRuns"].as_i64().unwrap_or(0)
}

/// Why a metered launch must not start under the configured limits, or None.
pub fn launch_blocker(status: &Value) -> Option<String> {
    for (key, label) in [("task", "Ticket"), ("daily", "Daily")] {
        let group = &status[key];
        let Some(limit) = group["limitUsd"].as_f64() else { continue };
        if group["unknownRuns"].as_i64().unwrap_or(0) != 0 {
            return Some(format!(
                "{label} API spend is incomplete; import missing run costs before starting another metered agent"
            ));
        }
        let planned = group["knownUsd"].as_f64().unwrap_or(0.0)
            + group["reservedUsd"].as_f64().unwrap_or(0.0)
            + status["launchReserveUsd"].as_f64().unwrap_or(0.0);
        if planned > limit {
            return Some(format!(
                "{label} API budget reached or insufficient launch reserve; adjust the budget or use a subscription harness"
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------------- requirements

/// sha256 of `json.dumps(documents, sort_keys=True)`.
pub fn documents_digest(documents: &indexmap::IndexMap<String, String>) -> String {
    let mut sorted: Vec<(&String, &String)> = documents.iter().collect();
    sorted.sort();
    let map: Map<String, Value> = sorted.into_iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let text = pyjson::dumps_compact(&Value::Object(map));
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// assurance.check_requirements: the approval must exist and match the current documents.
pub fn check_requirements(
    approval: Option<&RequirementsApprovalRow>,
    documents: &indexmap::IndexMap<String, String>,
) -> Result<(), String> {
    let Some(approval) = approval else {
        return Err(
            "Requirements need approval under the current policy: this task predates the requirements-approval \
                    gate and has no recorded approval. Open the Requirements tab and click \"Approve requirements\" to \
                    record it, then retry."
                .into(),
        );
    };
    if documents_digest(documents) != approval.digest {
        return Err("Approved requirements changed; restore the approved documents or create a new task".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_keep_python_number_types() {
        assert_eq!(py_sum(&[json!(1), json!(2)]), json!(3));
        assert_eq!(py_sum(&[json!(1), json!(0.5)]), json!(1.5));
        assert_eq!(py_sum(&[]), json!(0));
    }

    #[test]
    fn digest_matches_python() {
        // Python: hashlib.sha256(json.dumps({"b": "é", "a": "x"}, sort_keys=True).encode()).hexdigest()
        let documents: indexmap::IndexMap<String, String> =
            [("b".to_string(), "é".to_string()), ("a".to_string(), "x".to_string())].into_iter().collect();
        assert_eq!(documents_digest(&documents), "19def9e57837b918b5a7fad82a9a8a2ac5874f2659bb083932f99d0a6579efa0");
    }
}
