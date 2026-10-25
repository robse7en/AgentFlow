//! Shared review input, lineage lookup and resubmission contract.
use super::snapshot::{finding_dict, workspace};
use super::store::TaskState;
use super::{Engine, WorkflowError, review_loop, spec};
use crate::db::table;
use crate::db::{NodeRunRow, ReviewFindingRow};
use serde_json::{Map, Value};
use std::path::Path;

type FixFindings = (Option<Vec<Value>>, Option<String>);

impl Engine {
    pub(crate) fn findings(&self, task_id: i64) -> Result<Vec<ReviewFindingRow>, WorkflowError> {
        Ok(self.db.session(|tx| table::select(tx, "WHERE task_id = ?1 ORDER BY id", [task_id]))?)
    }

    pub(crate) fn previous_review(
        &self,
        state: &TaskState,
        node: usize,
        run_id: i64,
    ) -> Result<(Option<NodeRunRow>, Option<Vec<ReviewFindingRow>>), WorkflowError> {
        let previous = state
            .runs
            .iter()
            .rev()
            .find(|r| r.node_id == state.nodes[node].id && r.id != run_id && r.state == "COMPLETED")
            .map(|r| r.row.clone());
        let rows = match previous.as_ref().filter(|r| r.expected_sha.as_deref().is_some_and(|s| !s.is_empty())) {
            Some(run) => {
                Some(self.findings(state.task.id)?.into_iter().filter(|f| f.node_run_id == Some(run.id)).collect())
            }
            None => None,
        };
        Ok((previous, rows))
    }

    pub(crate) fn fix_round_findings(&self, state: &TaskState) -> Result<FixFindings, WorkflowError> {
        if state.task.review_iteration <= 1 {
            return Ok((None, None));
        }
        let path = workspace(&state.task)
            .findings_dir
            .join(format!("consolidated-{:02}.json", state.task.review_iteration - 1));
        if !path.exists() {
            return Ok((None, None));
        }
        let data = crate::workspace::read_json(&path).map_err(|e| WorkflowError::new(e.to_string(), 500))?;
        Ok((
            Some(data["findings"].as_array().cloned().unwrap_or_default()),
            data["reviewedSha"].as_str().map(String::from),
        ))
    }

    pub(crate) fn pending_findings(&self, state: &TaskState, run: usize) -> Vec<Value> {
        state.runs[run]
            .run_dir
            .as_deref()
            .and_then(|d| crate::workspace::read_json(&Path::new(d).join("findings-pending.json")).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    pub(crate) fn completion_rejection(
        &self,
        state: &TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), WorkflowError> {
        if raw.get("status").and_then(Value::as_str).unwrap_or("completed") != "completed" {
            return Ok(());
        }
        match state.nodes[node].key.as_str() {
            spec::CODE_REVIEW | spec::SECURITY_REVIEW => {
                let (_, rows) = self.previous_review(state, node, state.runs[run].id)?;
                let ids = rows
                    .as_ref()
                    .map(|rows| review_loop::blocking_ids(&rows.iter().map(finding_dict).collect::<Vec<_>>()));
                let reported = review_loop::reported_findings(
                    raw.get("findings").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default(),
                    &self.pending_findings(state, run),
                );
                let problems = review_loop::review_problems(raw, ids.as_deref(), &reported);
                if !problems.is_empty() {
                    return Err(WorkflowError::bad_request(review_loop::review_rejection(&problems, ids.as_deref())));
                }
            }
            spec::IMPLEMENTER => {
                if let (Some(given), _) = self.fix_round_findings(state)? {
                    let problems = review_loop::response_problems(raw, &given);
                    if !problems.is_empty() {
                        return Err(WorkflowError::bad_request(review_loop::response_rejection(&problems, &given)));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
