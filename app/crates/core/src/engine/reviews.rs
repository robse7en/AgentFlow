//! Review findings and explicit rulings, recorded before consolidation.
use super::result_models::normalize;
use super::snapshot::workspace;
use super::states::NodeState;
use super::store::TaskState;
use super::{Engine, review_loop, spec};
use crate::db::table::{self, Tracked};
use crate::db::{DbTime, ReviewFindingRow};
use serde_json::{Map, Value, json};

impl Engine {
    pub(crate) async fn complete_review(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), String> {
        let result = normalize(raw, "review")?;
        let expected = state.runs[run]
            .expected_sha
            .clone()
            .or_else(|| state.nodes[node].checkpoint_sha.clone())
            .or_else(|| state.task.current_sha.clone())
            .unwrap_or_default();
        let summary = result["summary"].as_str().unwrap_or("");
        if result["status"] != "completed" {
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(summary.into()));
            state.set_state(
                node,
                NodeState::Failed,
                Some(&format!("Reviewer reported {}", result["status"].as_str().unwrap_or(""))),
                Some(summary.into()),
            );
            return Ok(());
        }
        let reported = result["reviewedSha"].as_str().unwrap_or("").trim();
        if expected.is_empty() || reported != expected {
            let error = format!(
                "Review state mismatch: expected SHA {}, reviewer reported {}. Rerun the review.",
                expected.chars().take(10).collect::<String>(),
                if reported.is_empty() { "none".into() } else { reported.chars().take(10).collect::<String>() }
            );
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(error.clone()));
            state.set_state(node, NodeState::Failed, Some("Review state mismatch"), Some(error));
            return Ok(());
        }
        let findings =
            review_loop::reported_findings(result["findings"].as_array().unwrap(), &self.pending_findings(state, run));
        let code = state.nodes[node].key == spec::CODE_REVIEW;
        let (kind, prefix) = if code { ("code-review", "CODE") } else { ("security-review", "SEC") };
        let (_, prior) = self.previous_review(state, node, state.runs[run].id).map_err(|e| e.to_string())?;
        let mut prior = prior.unwrap_or_default();
        let rows = self.findings(state.task.id).map_err(|e| e.to_string())?;
        let mut number = rows
            .iter()
            .filter(|f| f.kind == kind)
            .filter_map(|f| f.finding_id.strip_prefix(&format!("{prefix}-")).and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let mut blocking = 0;
        let mut stored = Vec::new();
        let mut new_rows = Vec::new();
        for mut finding in findings.iter().cloned() {
            let previous = prior
                .iter()
                .filter(|f| finding.get("priorId").and_then(Value::as_str) == Some(&f.finding_id))
                .collect::<Vec<_>>();
            let (id, occurrence, prior_id) = if let Some(last) = previous.last() {
                (last.finding_id.clone(), previous.iter().map(|r| r.occurrence).max().unwrap() + 1, Some(last.id))
            } else {
                let id = format!("{prefix}-{number:03}");
                number += 1;
                (id, 1, None)
            };
            let is_blocking = super::agents::finding_is_blocking(&finding);
            blocking += usize::from(is_blocking);
            let text = |key: &str| finding.get(key).and_then(Value::as_str).map(String::from);
            let row = ReviewFindingRow {
                id: 0,
                task_id: state.task.id,
                node_run_id: Some(state.runs[run].id),
                finding_id: id.clone(),
                kind: kind.into(),
                iteration: state.task.review_iteration,
                reviewed_sha: Some(expected.clone()),
                severity: text("severity").unwrap_or("medium".into()),
                blocking: is_blocking,
                file: text("file"),
                line: finding.get("line").and_then(Value::as_i64),
                title: text("title")
                    .filter(|s| !s.is_empty())
                    .or_else(|| text("description").filter(|s| !s.is_empty()).map(|s| s.chars().take(80).collect()))
                    .unwrap_or(id.clone()),
                description: text("description"),
                recommendation: text("recommendation"),
                status: "open".into(),
                prior_finding_id: prior_id,
                occurrence,
                resolution: None,
                response: None,
                response_note: None,
                created_at: DbTime::now(),
                resolved_at: None,
            };
            new_rows.push(row);
            finding.insert("id".into(), json!(id));
            finding.insert("type".into(), json!(kind));
            finding.insert("blocking".into(), json!(is_blocking));
            finding.insert("occurrence".into(), json!(occurrence));
            stored.push(Value::Object(finding));
        }
        let mut updates = Vec::new();
        for ruling in result["priorFindings"].as_array().unwrap() {
            for row in prior.iter_mut().filter(|r| ruling["id"].as_str() == Some(&r.finding_id)) {
                let mut row = Tracked::loaded(row.clone());
                row.resolution = ruling["evidence"].as_str().map(|s| s.trim().into());
                if ["fixed", "withdrawn"].contains(&ruling["status"].as_str().unwrap_or("")) {
                    row.status = if ruling["status"] == "fixed" { "resolved" } else { "withdrawn" }.into();
                    row.resolved_at = Some(DbTime::now());
                }
                updates.push(row);
            }
        }
        let verdict = if blocking > 0 { "changes_required" } else { "approved" };
        if state.runs[run].run_dir.is_some() {
            let path = workspace(&state.task).findings_dir.join(format!(
                "{}-{:02}.json",
                if code { "review" } else { "security" },
                state.task.review_iteration
            ));
            crate::workspace::write_json(&path, &json!({"reviewedSha": expected, "verdict": verdict, "summary": summary, "findings": stored, "priorFindings": result["priorFindings"]})).map_err(|e| e.to_string())?;
            self.artifact(state, Some(state.runs[run].id), kind, &path)?;
        }
        // Validate and write the result artifact before changing finding lineage. A failed write
        // must not leave findings from a failed run or silently resolve its predecessor's issues.
        self.db
            .session(|tx| {
                for row in &mut new_rows {
                    table::insert(tx, row)?;
                }
                for row in &mut updates {
                    row.flush(tx)?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .map_err(|e| e.to_string())?;
        TaskState::finish_run(&mut state.runs[run], NodeState::Completed, None);
        state.set_state(
            node,
            NodeState::Completed,
            Some(&format!("{} finding(s), {blocking} blocking — {verdict}", findings.len())),
            None,
        );
        let other = state
            .node_index(if code { spec::SECURITY_REVIEW } else { spec::CODE_REVIEW })
            .map_err(|e| e.to_string())?;
        if state.nodes[other].state == "COMPLETED" && state.nodes[other].iteration == state.task.review_iteration {
            let consolidate = state.node_index(spec::CONSOLIDATE).map_err(|e| e.to_string())?;
            state.set_state(consolidate, NodeState::Ready, None, None);
            self.consolidate(state, consolidate).await?;
        }
        Ok(())
    }
}
