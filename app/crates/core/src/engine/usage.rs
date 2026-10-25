//! Usage collection from an explicitly configured provider-report command, ported from
//! `backend/agentflow/workflow/usage.py`.
//!
//! A command naming one of the bundled scripts (`usage_claude.py`, `usage_codex.py`,
//! `usage_copilot.py`, as the example config and the setup wizard write them) runs the Rust port
//! of that script in-process ([`crate::collectors`]); config.json is left as it is, so the Python
//! backend keeps reading it.
//!
//! Any other command runs as a process. Difference: Python ran a leading `python` as its own
//! interpreter; the Rust backend has none, so there `python` is looked up on PATH. Relative script
//! arguments still resolve against the AgentFlow install (the directory holding `scripts/`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;

use crate::collectors;
use crate::harness::as_posix;
use crate::process;

/// EX_TEMPFAIL, as returned by scripts/usage_copilot.py: not all of the run's usage is written yet.
pub const PENDING_EXIT: i32 = 75;
const REPORT_LIMIT: usize = 16384;
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A validated provider usage report (Python's `UsageReport`). Token counts are None when the
/// provider reports cost only.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageReport {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub model: String,
    pub source: String,
    pub cost: Option<f64>,
    /// "billed" (the provider metered it) or "estimated" (tokens priced at list rates).
    pub cost_kind: Option<String>,
    /// "run" or "session".
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UsageError {
    /// The provider has not written all of this run's usage yet; collecting again later can
    /// succeed. Carries the usage so far when the collector printed one.
    Pending(Option<Box<UsageReport>>),
    /// Anything else: unreadable, invalid, failed or timed out.
    Failed(String),
}

fn failed(message: impl Into<String>) -> UsageError {
    UsageError::Failed(message.into())
}

/// A strict non-negative integer (pydantic `int`, strict, ge=0): no bools, no floats.
fn count(data: &Map<String, Value>, keys: [&str; 2]) -> Result<Option<i64>, UsageError> {
    match keys.iter().find_map(|k| data.get(*k)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => match n.as_i64() {
            Some(v) if v >= 0 => Ok(Some(v)),
            _ => Err(failed(format!("{} must be at least 0", keys[0]))),
        },
        Some(_) => Err(failed(format!("{} must be an integer", keys[0]))),
    }
}

fn text(data: &Map<String, Value>, key: &str, max: usize) -> Result<String, UsageError> {
    match data.get(key) {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= max => Ok(s.clone()),
        Some(Value::String(_)) => Err(failed(format!("{key} must be 1 to {max} characters"))),
        _ => Err(failed(format!("{key} is required"))),
    }
}

impl UsageReport {
    /// `UsageReport.model_validate`: field aliases or names, no other keys.
    pub fn validate(value: &Value) -> Result<UsageReport, UsageError> {
        let Some(data) = value.as_object() else { return Err(failed("A usage report must be a JSON object")) };
        const KNOWN: [&str; 13] = [
            "inputTokens",
            "input_tokens",
            "outputTokens",
            "output_tokens",
            "cachedInputTokens",
            "cached_input_tokens",
            "model",
            "source",
            "costUsd",
            "cost",
            "costKind",
            "cost_kind",
            "scope",
        ];
        if let Some(extra) = data.keys().find(|k| !KNOWN.contains(&k.as_str())) {
            return Err(failed(format!("{extra}: extra inputs are not permitted")));
        }
        let cost = match data.get("costUsd").or_else(|| data.get("cost")) {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => match n.as_f64() {
                Some(v) if v.is_finite() && v >= 0.0 => Some(v),
                _ => return Err(failed("costUsd must be a finite number of at least 0")),
            },
            Some(_) => return Err(failed("costUsd must be a number")),
        };
        let cost_kind = match data.get("costKind").or_else(|| data.get("cost_kind")) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s == "billed" || s == "estimated" => Some(s.clone()),
            Some(_) => return Err(failed("costKind must be billed or estimated")),
        };
        let scope = match data.get("scope") {
            None => "run".to_string(),
            Some(Value::String(s)) if s == "run" || s == "session" => s.clone(),
            Some(_) => return Err(failed("scope must be run or session")),
        };
        let mut report = UsageReport {
            input_tokens: count(data, ["inputTokens", "input_tokens"])?,
            output_tokens: count(data, ["outputTokens", "output_tokens"])?,
            cached_input_tokens: count(data, ["cachedInputTokens", "cached_input_tokens"])?,
            model: text(data, "model", 128)?,
            source: text(data, "source", 200)?,
            cost,
            cost_kind,
            scope,
        };
        report.check_counts()?;
        Ok(report)
    }

    /// The model validator: both token counts or neither, cached within input, and a cost kind
    /// only with a cost (provider-reported costs default to billed).
    fn check_counts(&mut self) -> Result<(), UsageError> {
        if self.input_tokens.is_none() != self.output_tokens.is_none() {
            return Err(failed("Report both inputTokens and outputTokens, or neither"));
        }
        match (self.input_tokens, self.cached_input_tokens) {
            (None, Some(_)) => return Err(failed("cachedInputTokens requires inputTokens")),
            (None, None) if self.cost.is_none() => return Err(failed("A usage report needs token counts or a cost")),
            (Some(_), None) => self.cached_input_tokens = Some(0),
            (Some(input), Some(cached)) if cached > input => {
                return Err(failed("Cached input tokens must be included in inputTokens"));
            }
            _ => {}
        }
        if self.cost.is_none() {
            self.cost_kind = None;
        } else if self.cost_kind.is_none() {
            self.cost_kind = Some("billed".into());
        }
        Ok(())
    }

    /// `model_dump(by_alias=True)`.
    pub fn dump(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("inputTokens".into(), json!(self.input_tokens));
        map.insert("outputTokens".into(), json!(self.output_tokens));
        map.insert("cachedInputTokens".into(), json!(self.cached_input_tokens));
        map.insert("model".into(), json!(self.model));
        map.insert("source".into(), json!(self.source));
        map.insert("costUsd".into(), json!(self.cost));
        map.insert("costKind".into(), json!(self.cost_kind));
        map.insert("scope".into(), json!(self.scope));
        map
    }

    /// The stored usage record.
    pub fn record(&self, provenance: &str) -> Map<String, Value> {
        let mut map = self.dump();
        let total = self.input_tokens.zip(self.output_tokens).map(|(i, o)| i + o);
        map.insert("totalTokens".into(), json!(total));
        map.insert("provenance".into(), json!(provenance));
        map
    }
}

/// The directory holding AgentFlow's `scripts/`: an ancestor of this executable in a source
/// checkout (`app/target/<profile>/`).
fn install_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        exe.ancestors().find(|dir| dir.join("scripts").join("usage_claude.py").is_file()).map(Path::to_path_buf)
    })
    .as_deref()
}

/// A relative argument naming a file in the AgentFlow install resolves to it.
pub fn resolve_usage_command(argv: &[String]) -> Vec<String> {
    let mut resolved = vec![argv[0].clone()];
    for arg in &argv[1..] {
        let candidate = install_root().filter(|_| !Path::new(arg).is_absolute()).map(|root| root.join(arg));
        match candidate.filter(|path| path.is_file()) {
            Some(path) => resolved.push(as_posix(&path)),
            None => resolved.push(arg.clone()),
        }
    }
    resolved
}

/// Run the collector. Legacy collectors report runs; bundled ones report session totals.
pub async fn collect_usage(
    argv: &[String],
    cwd: &Path,
    run_id: i64,
    session_id: Option<&str>,
) -> Result<UsageReport, UsageError> {
    if argv.is_empty() {
        return Err(failed("No usage command"));
    }
    if let Some(collector) = collectors::bundled(argv) {
        return collect_bundled(collector, collectors::CollectorEnv::from_process(), session_id).await;
    }
    let argv = resolve_usage_command(argv);
    let env = HashMap::from([
        ("AGENTFLOW_RUN".to_string(), run_id.to_string()),
        ("AGENTFLOW_SESSION_ID".to_string(), session_id.unwrap_or_default().to_string()),
    ]);
    let mut child = process::spawn_captured_group(&argv, cwd, &env).map_err(|e| failed(e.0))?;
    let mut stdout = child.stdout.take().ok_or_else(|| failed("no stdout"))?;
    let read = async {
        let mut data = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stdout.read(&mut chunk).await.map_err(|e| failed(e.to_string()))?;
            if read == 0 {
                break;
            }
            data.extend_from_slice(&chunk[..read]);
            if data.len() > REPORT_LIMIT {
                return Err(failed("Usage report exceeds 16 KB"));
            }
        }
        let status = child.wait().await.map_err(|e| failed(e.to_string()))?;
        let parse = |data: &[u8]| -> Result<UsageReport, UsageError> {
            let value: Value = serde_json::from_slice(data).map_err(|e| failed(e.to_string()))?;
            UsageReport::validate(&value)
        };
        if status.code() == Some(PENDING_EXIT) {
            let partial = if data.iter().all(u8::is_ascii_whitespace) { None } else { Some(Box::new(parse(&data)?)) };
            return Err(UsageError::Pending(partial));
        }
        if !status.success() {
            return Err(failed("Usage collector failed"));
        }
        parse(&data)
    };
    let outcome = tokio::time::timeout(COLLECT_TIMEOUT, read).await;
    if let Ok(None) = child.try_wait() {
        process::terminate_process_tree(&mut child).await;
    }
    outcome.unwrap_or_else(|_| Err(failed("Usage collector timed out")))
}

/// Where the engine gets a run's usage report: the configured command ([`CommandUsage`]), or a
/// test's fake (Python's tests replaced `collect_usage` with a mock).
#[async_trait::async_trait]
pub trait UsageSource: Send + Sync {
    async fn collect(
        &self,
        argv: &[String],
        cwd: &Path,
        run_id: i64,
        session_id: Option<&str>,
    ) -> Result<UsageReport, UsageError>;
}

/// The real source: [`collect_usage`].
pub struct CommandUsage;

#[async_trait::async_trait]
impl UsageSource for CommandUsage {
    async fn collect(
        &self,
        argv: &[String],
        cwd: &Path,
        run_id: i64,
        session_id: Option<&str>,
    ) -> Result<UsageReport, UsageError> {
        collect_usage(argv, cwd, run_id, session_id).await
    }
}

/// Run a bundled collector in-process, with what the script's exit and output would have meant:
/// a report, pending (with the totals so far when printed), or failed.
pub async fn collect_bundled(
    collector: collectors::Collector,
    env: collectors::CollectorEnv,
    session_id: Option<&str>,
) -> Result<UsageReport, UsageError> {
    let session_id = session_id.unwrap_or_default().to_string();
    let work = tokio::task::spawn_blocking(move || collectors::run(collector, &env, &session_id));
    // The work itself is bounded (file reads, and a download with its own deadline); this keeps
    // the engine's wait at the same limit as a collector process.
    let outcome = match tokio::time::timeout(COLLECT_TIMEOUT, work).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => return Err(failed("Usage collector failed")),
        Err(_) => return Err(failed("Usage collector timed out")),
    };
    match outcome {
        collectors::Outcome::Report(value) => UsageReport::validate(&value),
        collectors::Outcome::Pending(None) => Err(UsageError::Pending(None)),
        collectors::Outcome::Pending(Some(value)) => {
            Err(UsageError::Pending(Some(Box::new(UsageReport::validate(&value)?))))
        }
        collectors::Outcome::Unavailable => Err(failed("Usage collector failed")),
    }
}

/// Subtract an immutable launch baseline, never a mutable last-collection cursor.
pub fn run_usage(report: &UsageReport, baseline: Option<&Map<String, Value>>) -> Result<UsageReport, UsageError> {
    if report.scope == "run" {
        return Ok(report.clone());
    }
    let Some(baseline) = baseline else { return Err(failed("Session usage has no verified launch baseline")) };
    let mut data = report.clone();
    let base = |key: &str| -> Option<Option<i64>> {
        // Outer None: the key is absent (counts as 0). Inner None: present but null.
        baseline.get(key).map(|v| v.as_i64())
    };
    let keys = ["inputTokens", "outputTokens", "cachedInputTokens"];
    if report.input_tokens.is_some() && keys.iter().any(|k| matches!(base(k), Some(None))) {
        // A cost-only baseline cannot be subtracted from token counters.
        data.input_tokens = None;
        data.output_tokens = None;
        data.cached_input_tokens = None;
    } else if report.input_tokens.is_some() {
        let fields = [&mut data.input_tokens, &mut data.output_tokens, &mut data.cached_input_tokens];
        for (field, key) in fields.into_iter().zip(keys) {
            let value = field.unwrap_or(0) - base(key).flatten().unwrap_or(0);
            if value < 0 {
                return Err(failed("Session usage counter decreased"));
            }
            *field = Some(value);
        }
        data.cached_input_tokens = data.cached_input_tokens.min(data.input_tokens);
    }
    let mut previous_cost = match baseline.get("costUsd") {
        None => (baseline.is_empty()).then_some(0.0),
        Some(value) => value.as_f64(),
    };
    if !baseline.is_empty() && baseline.get("costKind").and_then(Value::as_str) != report.cost_kind.as_deref() {
        previous_cost = None; // billed and estimated counters cannot be subtracted from each other
    }
    data.cost = report.cost.zip(previous_cost).map(|(cost, previous)| cost - previous);
    if data.cost.is_some_and(|c| c < 0.0) {
        return Err(failed("Session cost counter decreased"));
    }
    data.scope = "run".into();
    data.check_counts()?;
    Ok(data)
}

/// The usage baseline written at launch: `{}` for a fresh conversation, the session's totals so
/// far for a resumed one, or None (usage unknown) when that can't be measured.
pub async fn prepare_baseline(
    source: &dyn UsageSource,
    argv: Option<&[String]>,
    cwd: &Path,
    run_id: i64,
    session_id: Option<&str>,
    resumed: bool,
) -> Value {
    if !resumed {
        return json!({});
    }
    let (Some(argv), Some(session_id)) = (argv.filter(|a| !a.is_empty()), session_id.filter(|s| !s.is_empty())) else {
        return Value::Null;
    };
    match source.collect(argv, cwd, run_id, Some(session_id)).await {
        Ok(report) => Value::Object(report.dump()),
        // The previous process is gone, so what it wrote is final even if its turn was cut off.
        Err(UsageError::Pending(Some(report))) => Value::Object(report.dump()),
        Err(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(value: Value) -> Result<UsageReport, UsageError> {
        UsageReport::validate(&value)
    }

    #[test]
    fn validation_follows_the_pydantic_model() {
        let ok = report(json!({"inputTokens": 10, "outputTokens": 2, "model": "m", "source": "s"})).unwrap();
        assert_eq!(ok.cached_input_tokens, Some(0));
        assert_eq!(ok.cost_kind, None);
        let billed = report(json!({"costUsd": 1, "model": "m", "source": "s"})).unwrap();
        assert_eq!((billed.cost, billed.cost_kind.as_deref()), (Some(1.0), Some("billed")));
        for bad in [
            json!({"inputTokens": 1, "model": "m", "source": "s"}),
            json!({"inputTokens": 1.0, "outputTokens": 1, "model": "m", "source": "s"}),
            json!({"inputTokens": true, "outputTokens": 1, "model": "m", "source": "s"}),
            json!({"model": "m", "source": "s"}),
            json!({"inputTokens": 1, "outputTokens": 1, "cachedInputTokens": 2, "model": "m", "source": "s"}),
            json!({"costUsd": 1, "model": "m", "source": "s", "extra": 1}),
            json!({"costUsd": -1, "model": "m", "source": "s"}),
            json!({"costUsd": 1, "model": "", "source": "s"}),
        ] {
            assert!(report(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn records_keep_pythons_key_order() {
        let r = report(json!({"inputTokens": 10, "outputTokens": 2, "model": "m", "source": "s"})).unwrap();
        let keys: Vec<String> = r.record("p").keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "inputTokens",
                "outputTokens",
                "cachedInputTokens",
                "model",
                "source",
                "costUsd",
                "costKind",
                "scope",
                "totalTokens",
                "provenance"
            ]
        );
    }
}
