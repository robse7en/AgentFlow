//! flowctl — the structured channel between agents and AgentFlow.
//!
//! A port of `backend/agentflow/cli/flowctl.py` with the same commands, arguments, messages and
//! exit codes (1 for a reported failure, 2 for a usage error). Agents run it inside their terminal;
//! task, role and run id come from the environment the launcher writes:
//!
//! ```text
//! flowctl question --message "Should this apply to all tenants?"
//! flowctl question --file - <<'EOF'
//! {"message": "...", "risk": "medium", "choices": [{"label": "...", "recommended": true}, ...]}
//! EOF
//! flowctl running
//! flowctl complete --file - <<'EOF'
//! {"status": "completed", "summary": "..."}
//! EOF
//! flowctl complete --file <run dir>/submission.json   (a result too long for a heredoc)
//! flowctl finding --id CODE-001 --severity high --title "..." --file src/x.cs --line 12 --description "..."
//! flowctl fail --message "Cannot continue because ..."
//! flowctl status
//! ```
//!
//! It talks HTTP to the local backend only; it never touches the database.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value, json};

const DEFAULT_API: &str = "http://127.0.0.1:8765";

#[derive(Parser)]
#[command(name = "flowctl", about = "Signal AgentFlow from inside an agent session.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct Common {
    /// Jira key (default: $AGENTFLOW_TASK)
    #[arg(long)]
    task: Option<String>,
    /// architect | orchestrator | implementer | code_reviewer | security_reviewer (default: $AGENTFLOW_AGENT)
    #[arg(long)]
    agent: Option<String>,
    /// Node run id (default: $AGENTFLOW_RUN)
    #[arg(long)]
    run: Option<i64>,
    /// Backend URL (default: $AGENTFLOW_API or http://127.0.0.1:8765)
    #[arg(long)]
    api: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Show workflow state (all tasks, or the current task)
    Status {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        json: bool,
    },
    /// Ask the human a question; node becomes WAITING_FOR_USER
    #[command(group(clap::ArgGroup::new("form").required(true).args(["message", "file"])))]
    Question {
        #[command(flatten)]
        common: Common,
        /// The question (a flat question, with optional --choice)
        #[arg(long, short = 'm')]
        message: Option<String>,
        /// Decision-tree question as a JSON file, or - to read it from stdin
        #[arg(long, short = 'f')]
        file: Option<String>,
        /// Selectable answer (repeat up to eight times)
        #[arg(long = "choice")]
        choices: Vec<String>,
    },
    /// Report agent responsiveness without changing workflow state
    Heartbeat {
        #[command(flatten)]
        common: Common,
        #[arg(long, default_value = "working", value_parser = ["working", "investigating", "implementing", "testing", "reviewing"])]
        phase: String,
    },
    /// Questions answered; node is RUNNING again
    Running {
        #[command(flatten)]
        common: Common,
    },
    /// Submit the structured completion result
    Complete {
        #[command(flatten)]
        common: Common,
        /// JSON result file, or - to read JSON from stdin
        #[arg(long, short = 'f')]
        file: Option<String>,
        /// Inline JSON result
        #[arg(long)]
        json: Option<String>,
        /// Short summary (when no file/json is given)
        #[arg(long)]
        summary: Option<String>,
        /// Reviewed SHA (reviewers, when no file/json is given)
        #[arg(long)]
        sha: Option<String>,
        #[arg(long, value_parser = ["approved", "changes_required"])]
        verdict: Option<String>,
    },
    /// Record one review finding incrementally
    Finding {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, default_value = "medium", value_parser = ["info", "low", "medium", "high", "critical"])]
        severity: String,
        #[arg(long, overrides_with = "no_blocking")]
        blocking: bool,
        #[arg(long, overrides_with = "blocking")]
        no_blocking: bool,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        line: Option<i64>,
        /// Last line of a finding that spans several lines
        #[arg(long)]
        end_line: Option<i64>,
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        recommendation: Option<String>,
        /// Id of your previous-round blocking finding this re-raises
        #[arg(long)]
        prior_id: Option<String>,
    },
    /// Report that the agent cannot complete its task
    Fail {
        #[command(flatten)]
        common: Common,
        #[arg(long, short = 'm')]
        message: String,
    },
    /// (internal) launcher reports the harness process exit code
    Exited {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        code: i64,
    },
}

/// A failure flowctl reports on stderr before exiting with status 1 (Python's `sys.exit(message)`).
struct Fail(String);

type Result<T> = std::result::Result<T, Fail>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(Fail(message.into()))
}

/// The common arguments after applying the environment defaults.
struct Target {
    task: Option<String>,
    agent: Option<String>,
    run: Option<i64>,
    api: String,
}

impl Target {
    fn from(common: Common) -> Self {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let run = common.run.or_else(|| {
            env("AGENTFLOW_RUN").filter(|v| v.chars().all(|c| c.is_ascii_digit())).and_then(|v| v.parse().ok())
        });
        let api = common.api.or_else(|| env("AGENTFLOW_API")).unwrap_or_else(|| DEFAULT_API.to_owned());
        Target {
            task: common.task.filter(|v| !v.is_empty()).or_else(|| env("AGENTFLOW_TASK")),
            agent: common.agent.filter(|v| !v.is_empty()).or_else(|| env("AGENTFLOW_AGENT")),
            run,
            api: api.trim_end_matches('/').to_owned(),
        }
    }

    fn require(&self) -> Result<()> {
        if self.task.is_none() {
            return fail("flowctl: missing --task (or $AGENTFLOW_TASK)");
        }
        if self.agent.is_none() {
            return fail("flowctl: missing --agent (or $AGENTFLOW_AGENT)");
        }
        Ok(())
    }

    fn task(&self) -> &str {
        self.task.as_deref().unwrap_or_default()
    }

    fn agent(&self) -> &str {
        self.agent.as_deref().unwrap_or_default()
    }

    /// The bearer token for this run, from the credential file the launcher points at.
    fn authorization(&self) -> Result<String> {
        let path = std::env::var("AGENTFLOW_CREDENTIAL_FILE").ok().filter(|v| !v.is_empty());
        let (Some(path), Some(run)) = (path, self.run) else {
            return fail("flowctl: scoped credential file and explicit run ID required; restart older agent runs");
        };
        let credential: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or_else(|| Fail(format!("flowctl: cannot read credential file {path}")))?;
        if credential.get("runId").and_then(Value::as_i64) == Some(run)
            && let Some(token) = credential.get("token").and_then(Value::as_str)
        {
            return Ok(format!("Bearer {token}"));
        }
        fail("flowctl: no credential for this run; use --run from the current prompt")
    }

    /// POST to `/api/tasks/{task}/agent/{path}`. `on_response` runs once the backend has answered
    /// at all, even to reject the request; a transport failure never reaches it.
    fn post(&self, path: &str, payload: Map<String, Value>, on_response: impl FnOnce()) -> Result<Value> {
        let url = format!("{}/api/tasks/{}/agent/{path}", self.api, self.task());
        let mut body = Map::new();
        body.insert("role".into(), json!(self.agent));
        body.insert("runId".into(), json!(self.run));
        body.extend(payload);
        let authorization = self.authorization()?;
        let mut agent =
            ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(30)).timeout_write(Duration::from_secs(30));
        if path != "complete" {
            // A completion can wait on verification commands, so only its read is unbounded.
            agent = agent.timeout_read(Duration::from_secs(30));
        }
        let response =
            match agent.build().post(&url).set("Authorization", &authorization).send_json(Value::Object(body)) {
                Ok(response) => response,
                Err(ureq::Error::Status(_, response)) => response,
                Err(ureq::Error::Transport(error)) => {
                    return fail(format!("flowctl: cannot reach AgentFlow at {}: {error}", self.api));
                }
            };
        on_response();
        let status = response.status();
        let text = response.into_string().unwrap_or_default();
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        if status >= 400 {
            let detail = match parsed.as_ref().and_then(|body| body.get("detail")) {
                Some(Value::String(detail)) => detail.clone(),
                Some(detail) => detail.to_string(),
                None => text,
            };
            return fail(format!("flowctl: {path} rejected ({status}): {detail}"));
        }
        parsed.ok_or_else(|| Fail(format!("flowctl: {path} returned a response that is not JSON")))
    }
}

/// Python's `str()` of a JSON value, for the human-readable output.
fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

/// Python truthiness of an optional JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Bool(true)) => true,
    }
}

fn status(target: Target, as_json: bool) -> Result<()> {
    if target.task.is_none() {
        return fail("flowctl: agents can only read their assigned task");
    }
    let authorization = target.authorization()?;
    let url = format!("{}/api/tasks/{}", target.api, target.task());
    let response = match ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build()
        .get(&url)
        .set("Authorization", &authorization)
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => return fail(format!("flowctl: task {} not found", target.task())),
        Err(ureq::Error::Status(code, _)) => return fail(format!("flowctl: status rejected ({code})")),
        Err(ureq::Error::Transport(error)) => {
            return fail(format!("flowctl: cannot reach AgentFlow at {}: {error}", target.api));
        }
    };
    let snapshot: Value = response.into_json().map_err(|e| Fail(format!("flowctl: invalid status response: {e}")))?;
    println!(
        "Task {}: {} (iteration {}/{})",
        text(snapshot.get("jiraKey")),
        text(snapshot.get("status")),
        text(snapshot.get("reviewIteration")),
        text(snapshot.get("maxReviewIterations")),
    );
    if truthy(snapshot.get("statusMessage")) {
        println!("  {}", text(snapshot.get("statusMessage")));
    }
    for node in snapshot.get("nodes").and_then(Value::as_array).into_iter().flatten() {
        let extra =
            if truthy(node.get("message")) { format!(" — {}", text(node.get("message"))) } else { String::new() };
        println!("  {:<30} {:<22}{extra}", text(node.get("label")), text(node.get("state")));
        if truthy(node.get("error")) {
            println!("      error: {}", text(node.get("error")));
        }
    }
    if as_json {
        println!("{}", pyjson::dumps(&snapshot));
    }
    Ok(())
}

fn question(target: Target, message: Option<String>, file: Option<String>, choices: Vec<String>) -> Result<()> {
    target.require()?;
    let payload = match file {
        Some(file) => {
            if !choices.is_empty() {
                return fail("flowctl: --choice cannot be combined with --file; put the choices in the JSON");
            }
            // A decision tree (see the engine's `decisions`): one JSON object, read as UTF-8.
            let bytes = if file == "-" {
                let mut input = Vec::new();
                std::io::stdin()
                    .read_to_end(&mut input)
                    .map_err(|e| Fail(format!("flowctl: cannot read stdin: {e}")))?;
                input
            } else {
                match std::fs::read(&file) {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return fail(format!("flowctl: question file not found: {file}"));
                    }
                    Err(e) => return fail(format!("flowctl: cannot read {file}: {e}")),
                }
            };
            let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
            let raw =
                std::str::from_utf8(bytes).map_err(|e| Fail(format!("flowctl: question file is not UTF-8: {e}")))?;
            let parsed: Value =
                serde_json::from_str(raw).map_err(|e| Fail(format!("flowctl: question is not valid JSON: {e}")))?;
            let Value::Object(mut payload) = parsed else { return fail("flowctl: question must be a JSON object") };
            for identity in ["role", "runId", "run_id"] {
                payload.remove(identity);
            }
            payload
        }
        None => object([("message", json!(message.unwrap_or_default())), ("choices", json!(choices))]),
    };
    let result = target.post("question", payload, || {})?;
    println!(
        "Question recorded. Node is now {}. Wait for the human to reply in AgentFlow, then run: flowctl running",
        text(result.get("state"))
    );
    Ok(())
}

fn heartbeat(target: Target, phase: String) -> Result<()> {
    target.require()?;
    target.post("heartbeat", object([("phase", json!(phase))]), || {})?;
    println!("Agent heartbeat recorded.");
    Ok(())
}

fn running(target: Target) -> Result<()> {
    target.require()?;
    let result = target.post("running", Map::new(), || {})?;
    println!("Marked running ({}).", text(result.get("state")));
    Ok(())
}

struct Completion {
    file: Option<String>,
    json: Option<String>,
    summary: Option<String>,
    sha: Option<String>,
    verdict: Option<String>,
}

fn complete(target: Target, args: Completion) -> Result<()> {
    target.require()?;
    let mut result: Value = match args.file.as_deref() {
        // The usual path for a short result, e.g. `flowctl complete --file - <<'EOF' ... EOF`.
        // A long result exceeds Windows' ~32K command-line limit once a harness passes the whole
        // heredoc as one argument, so it comes through --file submission.json instead.
        Some("-") => {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| Fail(format!("flowctl: cannot read stdin: {e}")))?;
            serde_json::from_str(&input).map_err(|e| Fail(format!("flowctl: stdin is not valid JSON: {e}")))?
        }
        Some(file) if !file.is_empty() => {
            let path = Path::new(file);
            if !path.exists() {
                return fail(format!("flowctl: result file not found: {file}"));
            }
            let bytes = std::fs::read(path).map_err(|e| Fail(format!("flowctl: cannot read {file}: {e}")))?;
            let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
            serde_json::from_slice(bytes).map_err(|e| Fail(format!("flowctl: {file} is not valid JSON: {e}")))?
        }
        _ => match args.json.as_deref().filter(|j| !j.is_empty()) {
            Some(inline) => {
                serde_json::from_str(inline).map_err(|e| Fail(format!("flowctl: --json is not valid JSON: {e}")))?
            }
            None => {
                let mut result =
                    object([("status", json!("completed")), ("summary", json!(args.summary.unwrap_or_default()))]);
                if let Some(sha) = args.sha.filter(|s| !s.is_empty()) {
                    result.insert("reviewedSha".into(), json!(sha));
                }
                if let Some(verdict) = args.verdict {
                    result.insert("verdict".into(), json!(verdict));
                }
                Value::Object(result)
            }
        },
    };
    let Some(fields) = result.as_object_mut() else {
        return fail("flowctl: result must be a JSON object");
    };
    // An orchestrator's work plan has no status field and the backend forbids extra ones.
    if target.agent() != "orchestrator" && !fields.contains_key("status") {
        fields.insert("status".into(), json!("completed"));
    }
    // Persisted outside the repo, next to credential.json, before the network call: if the backend
    // is unreachable at this exact instant, the finished result survives for AgentFlow to pick up
    // on its next startup. Any response at all - even a rejection - means there is nothing left
    // worth preserving, so it's cleared on any response, not just success.
    let pending = run_file("pending-complete.json");
    if let Some(pending) = &pending {
        let record = object([("role", json!(target.agent)), ("runId", json!(target.run)), ("result", result.clone())]);
        std::fs::write(pending, pyjson::dumps(&Value::Object(record)))
            .map_err(|e| Fail(format!("flowctl: cannot write {}: {e}", pending.display())))?;
    }
    let clear_pending = || {
        if let Some(pending) = &pending {
            let _ = std::fs::remove_file(pending);
        }
    };
    let response = target.post("complete", object([("result", result)]), clear_pending)?;
    if truthy(response.get("error")) {
        return fail(format!(
            "flowctl: completion REJECTED — {}\nNode state: {}; task status: {}.",
            text(response.get("error")),
            text(response.get("state")),
            text(response.get("taskStatus")),
        ));
    }
    // Kept on a rejection so the agent can fix it and resubmit. Only the designated file is
    // removed: --file can name any path, and a JSON object like package.json would parse.
    if let (Some(file), Some(submission)) = (args.file.as_deref().filter(|f| *f != "-"), run_file("submission.json"))
        && same_file(Path::new(file), &submission)
    {
        let _ = std::fs::remove_file(submission);
    }
    println!(
        "Completion accepted. Node state: {}; task status: {}.",
        text(response.get("state")),
        text(response.get("taskStatus"))
    );
    Ok(())
}

/// A file in this run's directory, which also holds credential.json.
fn run_file(name: &str) -> Option<PathBuf> {
    let credential = std::env::var("AGENTFLOW_CREDENTIAL_FILE").ok().filter(|v| !v.is_empty())?;
    Some(Path::new(&credential).parent()?.join(name))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

struct Finding {
    id: Option<String>,
    severity: String,
    blocking: Option<bool>,
    file: Option<String>,
    line: Option<i64>,
    end_line: Option<i64>,
    title: String,
    description: Option<String>,
    recommendation: Option<String>,
    prior_id: Option<String>,
}

fn finding(target: Target, f: Finding) -> Result<()> {
    target.require()?;
    let fields = [
        ("id", f.id.map(Value::from)),
        ("severity", Some(Value::from(f.severity))),
        ("blocking", f.blocking.map(Value::from)),
        ("file", f.file.map(Value::from)),
        ("line", f.line.map(Value::from)),
        ("endLine", f.end_line.map(Value::from)),
        ("title", Some(Value::from(f.title))),
        ("description", f.description.map(Value::from)),
        ("recommendation", f.recommendation.map(Value::from)),
        ("priorId", f.prior_id.map(Value::from)),
    ];
    let finding: Map<String, Value> = fields.into_iter().filter_map(|(k, v)| Some((k.to_owned(), v?))).collect();
    let response = target.post("finding", object([("finding", Value::Object(finding))]), || {})?;
    println!(
        "Finding recorded ({} pending for this run). Include them all in your final result or complete without findings to use these.",
        text(response.get("count"))
    );
    Ok(())
}

fn fail_run(target: Target, message: String) -> Result<()> {
    target.require()?;
    let response = target.post("fail", object([("message", json!(message))]), || {})?;
    println!("Failure recorded. Node state: {}.", text(response.get("state")));
    Ok(())
}

fn exited(target: Target, code: i64) -> Result<()> {
    target.require()?;
    // The backend being down at shutdown is not worth a failure exit.
    if let Err(Fail(message)) = target.post("exited", object([("exitCode", json!(code))]), || {}) {
        eprintln!("{message}");
    }
    Ok(())
}

fn object<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn main() -> ExitCode {
    let outcome = match Cli::parse().command {
        Command::Status { common, json } => status(Target::from(common), json),
        Command::Question { common, message, file, choices } => question(Target::from(common), message, file, choices),
        Command::Heartbeat { common, phase } => heartbeat(Target::from(common), phase),
        Command::Running { common } => running(Target::from(common)),
        Command::Complete { common, file, json, summary, sha, verdict } => {
            complete(Target::from(common), Completion { file, json, summary, sha, verdict })
        }
        Command::Finding {
            common,
            id,
            severity,
            blocking,
            no_blocking,
            file,
            line,
            end_line,
            title,
            description,
            recommendation,
            prior_id,
        } => {
            let blocking = if blocking {
                Some(true)
            } else if no_blocking {
                Some(false)
            } else {
                None
            };
            finding(
                Target::from(common),
                Finding { id, severity, blocking, file, line, end_line, title, description, recommendation, prior_id },
            )
        }
        Command::Fail { common, message } => fail_run(Target::from(common), message),
        Command::Exited { common, code } => exited(Target::from(common), code),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
