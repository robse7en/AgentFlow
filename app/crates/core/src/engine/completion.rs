//! Implementation checkpoints, independent evidence, review lineage, consolidation and assessment.
use super::snapshot::harness_cwd;
use super::states::NodeState;
use super::store::TaskState;
use super::{Engine, WorkflowError, spec, verification};
use crate::db::table::{self, Tracked};
use crate::db::{DbTime, Json, RunEvidenceRow, TaskArtifactRow};
use serde_json::{Map, Value, json};
use std::path::Path;

pub use super::assessment::history_finding;
pub use super::result_models::normalize;

impl Engine {
    pub(crate) async fn on_ready(&self, state: &mut TaskState, node: usize) -> Result<(), WorkflowError> {
        if state.nodes[node].execution_mode == "automatic"
            && state.nodes[node].kind == "agent"
            && let Err(error) = self.start_agent_run(state, node, None).await
        {
            if let Some(run) = state.runs.iter_mut().rev().find(|r| {
                r.node_id == state.nodes[node].id && NodeState::parse(&r.state).is_some_and(NodeState::is_active)
            }) {
                TaskState::finish_run(run, NodeState::Failed, Some(error.to_string()));
            }
            state.set_state(node, NodeState::Failed, Some("Automatic start failed"), Some(error.to_string()));
        }
        Ok(())
    }

    pub(crate) async fn complete_implementation(
        &self,
        state: &mut TaskState,
        node: usize,
        run: usize,
        raw: &Map<String, Value>,
    ) -> Result<(), String> {
        let mut result = normalize(raw, "implementation")?;
        state.runs[run].result = Json(json!(result));
        let summary = result["summary"].as_str().unwrap_or("").to_string();
        if result["status"] != "completed" {
            TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(summary.clone()));
            state.set_state(
                node,
                NodeState::Failed,
                Some(&format!("Implementer reported {}", result["status"].as_str().unwrap_or(""))),
                Some(summary),
            );
            return Ok(());
        }
        self.check_worker_integration(state).await?;
        let (sha, committed) = match self
            .git
            .checkpoint(
                Path::new(&state.task.worktree_path),
                &format!("AgentFlow checkpoint: {} iteration {}", state.key(), state.task.review_iteration),
                self.config().workflow.commit_checkpoints,
            )
            .await
        {
            Ok(value) => value,
            Err(error) => {
                TaskState::finish_run(&mut state.runs[run], NodeState::Failed, Some(error.to_string()));
                state.set_state(node, NodeState::Failed, Some("Checkpoint failed"), Some(error.to_string()));
                return Ok(());
            }
        };
        state.task.current_sha = Some(sha.clone());
        let mut evidence = self
            .db
            .session(|tx| table::select_one::<RunEvidenceRow>(tx, "WHERE run_id = ?1", [state.runs[run].id]))
            .map_err(|e| e.to_string())?
            .map(Tracked::loaded)
            .unwrap_or_else(|| {
                Tracked::new(RunEvidenceRow {
                    uid: crate::db::new_uid(),
                    run_id: state.runs[run].id,
                    requirements_digest: None,
                    verification: Json(Value::Null),
                    usage: Json(Value::Null),
                    confirmed_model: None,
                    billing_mode: "unknown".into(),
                })
            });
        self.db
            .session(|tx| {
                evidence.flush(tx)?;
                state.flush(tx)
            })
            .map_err(|e| e.to_string())?;
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        self.verifications.lock().unwrap_or_else(|p| p.into_inner()).insert(state.key().to_string(), cancel);
        let outcome = verification::verify_commands(
            &self.config().workflow.verification_commands,
            &harness_cwd(&state.task),
            &sha,
            receiver,
        )
        .await;
        self.verifications.lock().unwrap_or_else(|p| p.into_inner()).remove(state.key());
        evidence.verification = Json(match outcome {
            Ok(value) => value,
            Err(error) if error == "Independent verification was cancelled" => return Err(error),
            Err(error) => json!({"sha": sha, "passed": false, "error": error}),
        });
        self.db.session(|tx| evidence.flush(tx)).map_err(|e| e.to_string())?;
        let path = Path::new(&state.task.worktree_path);
        if self.git.head_sha(path).await.map_err(|e| e.to_string())? != sha
            || self.git.is_dirty(path).await.map_err(|e| e.to_string())?
        {
            return Err("Verification changed the checkpoint; inspect changes and run completion again".into());
        }
        state.nodes[node].checkpoint_sha = Some(sha.clone());
        state.nodes[node].iteration = state.task.review_iteration;
        result.insert("checkpointSha".into(), json!(sha));
        state.runs[run].result = Json(json!(result));
        if let Some(dir) = &state.runs[run].run_dir {
            crate::workspace::write_json(&Path::new(dir).join("result.json"), &json!(result))
                .map_err(|e| e.to_string())?;
        }
        TaskState::finish_run(&mut state.runs[run], NodeState::Completed, None);
        if let (Some(given), _) = self.fix_round_findings(state).map_err(|e| e.to_string())? {
            let mut rows = self.findings(state.task.id).map_err(|e| e.to_string())?;
            for response in result["findingResponses"].as_array().unwrap() {
                for f in &given {
                    if f["id"] == response["id"]
                        && let Some(row) = rows.iter_mut().find(|r| Some(r.id) == f["dbId"].as_i64())
                    {
                        let mut tracked = Tracked::loaded(row.clone());
                        tracked.response = response["action"].as_str().map(String::from);
                        tracked.response_note = response["rationale"].as_str().map(|s| s.trim().into());
                        self.db.session(|tx| tracked.flush(tx)).map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        state.set_state(
            node,
            NodeState::Completed,
            Some(&if summary.is_empty() {
                "Implementation complete".to_string()
            } else {
                summary.chars().take(200).collect::<String>()
            }),
            None,
        );
        state.log(
            format!(
                "Checkpoint SHA {}{}",
                &sha[..sha.len().min(10)],
                if committed { " (committed)" } else { " (HEAD, nothing to commit)" }
            ),
            Some(spec::IMPLEMENTER),
            "info",
            Some(json!({"sha": sha})),
        );
        for key in [spec::CODE_REVIEW, spec::SECURITY_REVIEW] {
            let review = state.node_index(key).map_err(|e| e.to_string())?;
            state.nodes[review].iteration = state.task.review_iteration;
            state.nodes[review].checkpoint_sha = Some(sha.clone());
            state.nodes[review].message = None;
            state.set_state(review, NodeState::Ready, None, None);
        }
        let consolidate = state.node_index(spec::CONSOLIDATE).map_err(|e| e.to_string())?;
        state.nodes[consolidate].iteration = state.task.review_iteration;
        state.set_state(consolidate, NodeState::Pending, None, None);
        for key in [spec::CODE_REVIEW, spec::SECURITY_REVIEW] {
            let review = state.node_index(key).map_err(|e| e.to_string())?;
            self.on_ready(state, review).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn artifact(&self, state: &TaskState, run: Option<i64>, kind: &str, path: &Path) -> Result<(), String> {
        let mut artifact = TaskArtifactRow {
            id: 0,
            task_id: state.task.id,
            node_run_id: run,
            kind: kind.into(),
            path: crate::paths::py_str(path),
            created_at: DbTime::now(),
        };
        self.db.session(|tx| table::insert(tx, &mut artifact)).map_err(|e| e.to_string())
    }
}
