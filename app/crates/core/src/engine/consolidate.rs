//! Merge both reviews, choose a fix round or escalate to the human.
use super::snapshot::{finding_dict, workspace};
use super::states::NodeState;
use super::store::TaskState;
use super::{Engine, spec};
use crate::db::DbTime;
use crate::db::table::Tracked;
use serde_json::json;

impl Engine {
    pub(crate) async fn consolidate(&self, state: &mut TaskState, node: usize) -> Result<(), String> {
        state.set_state(node, NodeState::Running, None, None);
        let iteration = state.task.review_iteration;
        let all = self.findings(state.task.id).map_err(|e| e.to_string())?;
        let findings = all.iter().filter(|f| f.iteration == iteration).collect::<Vec<_>>();
        for old in all.iter().filter(|f| f.iteration < iteration && f.status == "open") {
            let mut row = Tracked::loaded(old.clone());
            row.status = "superseded".into();
            row.resolved_at = Some(DbTime::now());
            self.db.session(|tx| row.flush(tx)).map_err(|e| e.to_string())?;
        }
        let blocking = findings.iter().filter(|f| f.blocking).collect::<Vec<_>>();
        let path = workspace(&state.task).findings_dir.join(format!("consolidated-{iteration:02}.json"));
        crate::workspace::write_json(&path, &json!({"iteration": iteration, "reviewedSha": state.task.current_sha, "blockingCount": blocking.len(), "findings": findings.iter().map(|f| finding_dict(f)).collect::<Vec<_>>()})).map_err(|e| e.to_string())?;
        self.artifact(state, None, "consolidated", &path)?;
        if !blocking.is_empty() {
            let implementer = state.node_index(spec::IMPLEMENTER).map_err(|e| e.to_string())?;
            let at_max = iteration >= state.task.max_review_iterations;
            let repeated =
                blocking.iter().filter(|f| f.occurrence >= 3).map(|f| f.finding_id.as_str()).collect::<Vec<_>>();
            let upheld = blocking
                .iter()
                .filter(|f| {
                    f.prior_finding_id
                        .is_some_and(|id| all.iter().any(|p| p.id == id && p.response.as_deref() == Some("disputed")))
                })
                .map(|f| f.finding_id.as_str())
                .collect::<Vec<_>>();
            let mut reasons = Vec::new();
            if at_max {
                reasons.push(format!("Maximum review iterations reached ({})", state.task.max_review_iterations));
            }
            if !repeated.is_empty() {
                reasons.push(format!("Raised 3 or more times without being fixed: {}", repeated.join(", ")));
            }
            if !upheld.is_empty() {
                reasons.push(format!("Disputed by the implementer and upheld by the reviewer: {}", upheld.join(", ")));
            }
            if !reasons.is_empty() {
                state.task.status = "NEEDS_HUMAN_REVIEW".into();
                let message = format!(
                    "{}. Remaining blocking findings: {}",
                    reasons.join(". "),
                    blocking.iter().map(|f| f.finding_id.as_str()).collect::<Vec<_>>().join(", ")
                );
                state.task.status_message = Some(message.clone());
                state.set_state(
                    node,
                    NodeState::Completed,
                    Some(&format!(
                        "Iteration {iteration}: {} blocking, {}",
                        blocking.len(),
                        if at_max { "max iterations reached" } else { "escalated for a human decision" }
                    )),
                    None,
                );
                state.log(message, None, "error", None);
                state.nodes[implementer].iteration = iteration + 1;
                state.set_state(implementer, NodeState::Ready, Some("Human decision required before continuing"), None);
                return Ok(());
            }
            state.task.review_iteration = iteration + 1;
            state.nodes[implementer].iteration = iteration + 1;
            state.set_state(
                node,
                NodeState::Completed,
                Some(&format!("Iteration {iteration}: {} blocking finding(s) → Implementer fixes", blocking.len())),
                None,
            );
            state.set_state(
                implementer,
                NodeState::Ready,
                Some(&format!("Fix {} finding(s) from iteration {iteration}", findings.len())),
                None,
            );
            self.on_ready(state, implementer).await.map_err(|e| e.to_string())?;
        } else {
            for finding in findings.iter().filter(|f| f.status == "open") {
                let mut row = Tracked::loaded((*finding).clone());
                row.status = "accepted".into();
                self.db.session(|tx| row.flush(tx)).map_err(|e| e.to_string())?;
            }
            state.set_state(
                node,
                NodeState::Completed,
                Some(&format!("Iteration {iteration}: no blocking findings — reviews passed")),
                None,
            );
            state.info("Code Review passed and Security Review passed");
            let final_node = state.node_index(spec::FINAL_ARCHITECT).map_err(|e| e.to_string())?;
            state.nodes[final_node].checkpoint_sha = state.task.current_sha.clone();
            state.nodes[final_node].iteration = iteration;
            state.set_state(final_node, NodeState::Ready, None, None);
            self.on_ready(state, final_node).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
