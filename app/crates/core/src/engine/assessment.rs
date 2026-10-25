//! Final assessment artifacts and the history the architect receives.
use super::result_models::normalize;
use super::snapshot::workspace;
use super::states::NodeState;
use super::store::TaskState;
use super::{Engine, WorkflowError, spec};
use crate::db::{Json, ReviewFindingRow};
use serde_json::{Map, Value, json};

impl Engine {
    pub(crate) async fn complete_final(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), String> {
        let mut data = normalize(raw, "final")?;
        if data["status"] != "completed" {
            let summary = data["taskSummary"].as_str().unwrap_or("");
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(summary.into()));
            state.set_state(node, NodeState::Failed, Some("Architect could not verify"), Some(summary.into()));
            return Ok(());
        }
        data.insert("finalSha".into(), json!(state.task.current_sha));
        data.insert("reviewIterations".into(), json!(state.task.review_iteration));
        state.runs[run].result = Json(json!(data));
        let dir = workspace(&state.task).final_dir;
        let path = dir.join("assessment.json");
        crate::workspace::write_json(&path, &json!(data)).map_err(|e| e.to_string())?;
        self.artifact(state, Some(state.runs[run].id), "final-assessment", &path)?;
        if !dir.join("assessment.md").exists() {
            crate::pyfile::write_text(&dir.join("assessment.md"), &assessment_markdown(&data))
                .map_err(|e| e.to_string())?;
        }
        TaskState::finish_run(&mut state.runs[run], NodeState::Completed, None);
        let risk = data["overallRisk"].as_str().unwrap();
        state.set_state(node, NodeState::Completed, Some(&format!("Assessment received. Risk {risk}")), None);
        state.info(format!("Final assessment received. Risk: {risk}"));
        self.db.session(|tx| state.flush(tx)).map_err(|e| e.to_string())?;
        let blockers = self.current_final_blockers(state.key()).map_err(|e| e.to_string())?;
        let ready = state.node_index(spec::READY_FOR_PR).map_err(|e| e.to_string())?;
        let message = if blockers.is_empty() {
            "Confirm the assessment to mark Ready for PR".into()
        } else {
            format!("Blocked: {}", blockers.join("; "))
        };
        state.set_state(ready, NodeState::WaitingForApproval, Some(&message), None);
        Ok(())
    }

    pub(crate) fn history(&self, state: &TaskState) -> Result<Value, WorkflowError> {
        let runs = state.runs.iter().filter_map(|r| {
            let key = state.run_node_key(r)?;
            if ![spec::IMPLEMENTER, spec::CODE_REVIEW, spec::SECURITY_REVIEW].contains(&key) { return None; }
            Some(json!({"node": key, "iteration": r.iteration, "state": r.state, "harness": r.harness,
                "summary": r.result.0.get("summary"), "filesChanged": r.result.0.get("filesChanged"), "tests": r.result.0.get("tests"),
                "knownIssues": r.result.0.get("knownIssues"), "verdict": r.result.0.get("verdict"), "reviewedSha": r.result.0.get("reviewedSha")}))
        }).collect::<Vec<_>>();
        Ok(
            json!({"iterations": state.task.review_iteration, "finalSha": state.task.current_sha, "runs": runs, "findings": self.findings(state.task.id)?.iter().map(history_finding).collect::<Vec<_>>()}),
        )
    }
}

pub fn history_finding(f: &ReviewFindingRow) -> Value {
    if f.status == "superseded" {
        return json!({"id": f.finding_id, "iteration": f.iteration, "file": f.file, "line": f.line, "title": f.title, "status": f.status});
    }
    if ["resolved", "withdrawn"].contains(&f.status.as_str()) {
        let mut data = json!({"id": f.finding_id, "iteration": f.iteration, "severity": f.severity, "blocking": f.blocking, "file": f.file, "line": f.line, "title": f.title, "status": f.status, "resolution": f.resolution});
        if let Some(response) = &f.response {
            data["implementerResponse"] = json!({"action": response, "rationale": f.response_note});
        }
        return data;
    }
    json!({"id": f.finding_id, "type": f.kind, "iteration": f.iteration, "severity": f.severity, "blocking": f.blocking, "file": f.file, "line": f.line, "title": f.title, "description": f.description, "recommendation": f.recommendation, "status": f.status})
}

fn assessment_markdown(data: &Map<String, Value>) -> String {
    let mut lines = vec!["# Final assessment".to_string(), String::new()];
    for (key, value) in data {
        if key == "status" {
            continue;
        }
        let title = key.chars().flat_map(|c| if c.is_uppercase() { vec![' ', c] } else { vec![c] }).collect::<String>();
        let title = title.trim().to_lowercase();
        let mut chars = title.chars();
        let title = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
        lines.push(format!("## {title}"));
        if let Some(items) = value.as_array() {
            if items.is_empty() {
                lines.push("- (none)".into());
            }
            lines.extend(items.iter().map(|v| format!("- {}", v.as_str().unwrap_or(""))));
        } else {
            lines.push(match value {
                Value::Null => "(none)".into(),
                Value::String(s) if s.is_empty() => "(none)".into(),
                Value::String(s) => s.clone(),
                Value::Bool(b) => if *b { "True" } else { "False" }.into(),
                v => v.to_string(),
            });
        }
        lines.push(String::new());
    }
    lines.join("\n")
}
