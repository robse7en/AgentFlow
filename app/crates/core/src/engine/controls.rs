//! Human-only accounting corrections, serialized with task mutations and launch policy.
use super::store::TaskState;
use super::usage::UsageReport;
use super::{Engine, WorkflowError};
use crate::db::table::{self, Tracked};
use crate::db::{Json, RunEvidenceRow, new_uid};
use serde_json::{Value, json};

impl Engine {
    pub async fn set_task_budget(&self, key: &str, amount: Option<f64>) -> Result<Value, WorkflowError> {
        if amount.is_some_and(|n| !n.is_finite() || n <= 0.0) {
            return Err(WorkflowError::new("amount: must be positive and finite", 422));
        }
        let lock = self.lock(key);
        let held = lock.lock().await;
        let launching = self.launch_lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        state.task.api_budget_usd = amount;
        state.log("Human updated ticket API budget", None, "info", Some(json!({"amountUsd": amount})));
        self.db.session(|tx| state.flush(tx))?;
        drop(launching);
        drop(held);
        self.publish(key)
    }

    pub async fn import_usage(
        &self,
        key: &str,
        run: i64,
        report: &UsageReport,
        billing: Option<&str>,
    ) -> Result<Value, WorkflowError> {
        if report.scope != "run" {
            return Err(WorkflowError::new("Import usage for this run only, not cumulative session totals", 422));
        }
        if billing.is_some_and(|s| !["metered", "subscription", "unknown"].contains(&s)) {
            return Err(WorkflowError::new("Invalid billing mode", 422));
        }
        let lock = self.lock(key);
        let held = lock.lock().await;
        let mut state = self.db.session(|tx| TaskState::load(tx, key))?;
        if !state.runs.iter().any(|r| r.id == run) {
            return Err(WorkflowError::not_found("Run not found"));
        }
        let mut evidence = self
            .db
            .session(|tx| table::select_one::<RunEvidenceRow>(tx, "WHERE run_id = ?1", [run]))?
            .map(Tracked::loaded)
            .unwrap_or_else(|| {
                Tracked::new(RunEvidenceRow {
                    uid: new_uid(),
                    run_id: run,
                    requirements_digest: None,
                    verification: Json(Value::Null),
                    usage: Json(Value::Null),
                    confirmed_model: None,
                    billing_mode: "unknown".into(),
                })
            });
        evidence.usage = Json(Value::Object(report.record("human-imported-provider-report")));
        evidence.confirmed_model = Some(report.model.clone());
        if let Some(billing) = billing {
            evidence.billing_mode = billing.into();
        }
        state.log("Human imported provider usage", None, "info", Some(json!({"runId": run, "source": report.source})));
        self.db.session(|tx| {
            evidence.flush(tx)?;
            state.flush(tx)
        })?;
        drop(held);
        self.publish(key)
    }
}
