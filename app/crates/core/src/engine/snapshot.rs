//! What the UI is sent about tasks, ported from the engine's `snapshot`, `list_tasks`,
//! `_task_summary`, `_final_blockers` and `resolve_agent_config`. Keys and their order follow the
//! Python dicts (contract/golden/shapes checks the shapes).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::estimates;
use super::rules;
use super::spec::{self, NodeSpec};
use super::states::{NodeKind, NodeState, TaskStatus};
use super::store::TaskState;
use super::{Engine, WorkflowError};
use crate::config::{BillingMode, Config, Role as ConfigRole};
use crate::db::table;
use crate::db::{
    AgentQuestionRow, AgentSessionRow, DbTime, ImplementationWorkItemRow, NodeRunRow, RequirementsApprovalRow,
    ReviewFindingRow, RunEvidenceRow, TaskRow, WorkflowEventRow, WorkflowNodeRow,
};
use crate::harness::HarnessRegistry;
use crate::paths::py_str;
use crate::workspace::TaskWorkspace;

/// Finished runs' usage records by (stage, harness, model), most recent first.
pub(crate) type EstimateSamples = HashMap<(String, Option<String>, Option<String>), Vec<Value>>;

/// `_iso`: a stored (UTC) time in isoformat with an offset, or null.
pub fn iso(time: Option<&DbTime>) -> Value {
    time.map_or(Value::Null, |t| json!(t.isoformat()))
}

/// Python's truthiness for a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn config_role(role: &str) -> Option<ConfigRole> {
    ConfigRole::ALL.into_iter().find(|r| r.as_str() == role)
}

/// The workspace of a task: `<task_dir's parent>/<key>`.
pub fn workspace(task: &TaskRow) -> TaskWorkspace {
    let task_dir = Path::new(&task.task_dir);
    TaskWorkspace::new(task_dir.parent().unwrap_or(task_dir), &task.jira_key)
}

/// Where the task's agents run: the worktree, or its project directory.
pub fn harness_cwd(task: &TaskRow) -> std::path::PathBuf {
    let root = Path::new(&task.worktree_path);
    match task.project_relative_path.as_deref().filter(|p| !p.is_empty()) {
        Some(rel) => root.join(rel),
        None => root.to_path_buf(),
    }
}

/// Agent configuration precedence: role assignment, Copilot routing, then task, stage and
/// per-run overrides. `override_layer` is a run's own override.
pub fn resolve_agent_config(
    config: &Config,
    harnesses: &HarnessRegistry,
    task: &TaskRow,
    nodes: &[&WorkflowNodeRow],
    node: &WorkflowNodeRow,
    override_layer: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let (node, override_layer) = if node.key.starts_with("worker_") {
        (nodes.iter().copied().find(|n| n.key == spec::IMPLEMENTER).unwrap_or(node), None)
    } else {
        (node, override_layer)
    };
    let role = node.role.clone().unwrap_or_default();
    let task_layer = task.agent_overrides.0.get(&role).and_then(Value::as_object).cloned();
    let node_layer = node.config_override.0.as_object().cloned();
    resolve_role_config(config, harnesses, &role, &[task_layer.as_ref(), node_layer.as_ref(), override_layer])
}

/// A role's configuration with the task, stage and request layers (any may be None) applied in
/// order.
pub fn resolve_role_config(
    config: &Config,
    harnesses: &HarnessRegistry,
    role: &str,
    layers: &[Option<&Map<String, Value>>],
) -> Map<String, Value> {
    let base = config.agents.for_role(config_role(role).unwrap_or(ConfigRole::Architect));
    let mut resolved = Map::new();
    resolved.insert("harness".into(), json!(base.harness));
    resolved.insert("model".into(), json!(base.model));
    resolved.insert("reasoning".into(), json!(base.reasoning));
    let mut explicit_model = false;
    let controls = &config.workflow.cost_controls;
    let copilot_role = config_role(role).is_some_and(|r| controls.copilot_roles.contains(&r));
    let mut route_reason = "Configured role assignment";
    if copilot_role {
        let preferred = config.harnesses.get("copilot");
        if let Some(preferred) =
            preferred.filter(|p| p.billing_mode == BillingMode::Subscription && harnesses.is_usable("copilot"))
        {
            resolved.insert("harness".into(), json!("copilot"));
            resolved.insert("model".into(), json!(preferred.default_model));
            route_reason = "Copilot preferred for this role";
        } else {
            route_reason = "Copilot preference unavailable; configured role fallback";
        }
    }
    let routed_harness = resolved["harness"].clone();
    for layer in layers.iter().copied().flatten() {
        if layer.is_empty() {
            continue;
        }
        route_reason = "Explicit task or stage configuration";
        for field in ["harness", "model", "reasoning"] {
            match layer.get(field) {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if s.is_empty() => {}
                Some(value) => {
                    resolved.insert(field.into(), value.clone());
                    if field == "model" {
                        explicit_model = true;
                    }
                }
            }
        }
    }
    if !explicit_model && resolved["harness"] != routed_harness {
        resolved.insert("model".into(), Value::Null);
    }
    let harness = resolved["harness"].as_str().map(String::from).unwrap_or_default();
    let harness_config = config.harnesses.get(&harness);
    if resolved["model"].is_null()
        && let Some(cfg) = harness_config
    {
        resolved.insert("model".into(), json!(cfg.default_model));
    }
    let billing = harness_config.map_or(BillingMode::Unknown, |h| h.billing_mode);
    resolved.insert("billingMode".into(), json!(billing));
    resolved.insert("routingReason".into(), json!(route_reason));
    resolved.insert(
        "requiresMeteredApproval".into(),
        json!((controls.require_metered_approval || copilot_role) && billing != BillingMode::Subscription),
    );
    resolved
}

/// `_task_summary`: the fields both the list and the full snapshot carry.
pub fn task_summary(task: &TaskRow, nodes: &[&WorkflowNodeRow]) -> Map<String, Value> {
    let in_states = |states: &[NodeState]| {
        nodes.iter().copied().find(|n| NodeState::parse(&n.state).is_some_and(|s| states.contains(&s)))
    };
    let active = in_states(&[NodeState::WaitingForUser, NodeState::WaitingForApproval])
        .or_else(|| in_states(&[NodeState::Running, NodeState::WaitingForUser]));
    let ready = in_states(&[NodeState::Ready]);
    let failed = in_states(&[NodeState::Failed]);
    let phase = if task.status == TaskStatus::Active.as_str() {
        match active.or(failed).or(ready) {
            Some(node) => format!("{}: {}", node.label, node.state),
            None => "ACTIVE".into(),
        }
    } else {
        task.status.clone()
    };
    let value = json!({
        "taskSource": if super::lifecycle::is_local_task(&task.jira_key) { "local" } else { "jira" },
        "jiraKey": task.jira_key, "jiraTitle": task.jira_title, "status": task.status,
        "statusMessage": task.status_message, "phase": phase,
        "repositoryRoot": task.repository_root, "projectRelativePath": task.project_relative_path,
        "baseBranch": task.base_branch, "branchName": task.branch_name, "worktreePath": task.worktree_path,
        "zellijSession": task.zellij_session_name, "reviewIteration": task.review_iteration,
        "maxReviewIterations": task.max_review_iterations,
        "maxRequirementsRevisions": task.max_requirements_revisions, "currentSha": task.current_sha,
        "prUrl": task.pr_url, "prCreatedAt": iso(task.pr_created_at.as_ref()), "mergedAt": iso(task.merged_at.as_ref()),
        "prBaseBranch": task.pr_base_branch, "prSyncPaused": task.pr_sync_paused, "archivedAt": iso(task.archived_at.as_ref()), "projectId": task.project_id,
        "createdAt": iso(Some(&task.created_at)), "updatedAt": iso(Some(&task.updated_at)),
    });
    match value {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

/// `_worktree_state`: filesystem stats, worth doing only once the task is Merged.
fn worktree_state(task: &TaskRow, items: &[&ImplementationWorkItemRow]) -> (Value, Value) {
    if task.status != TaskStatus::Merged.as_str() {
        return (Value::Null, Value::Null);
    }
    let workers = items.iter().filter(|i| Path::new(&i.worktree_path).exists()).count();
    (json!(Path::new(&task.worktree_path).exists()), json!(workers))
}

/// Everything a snapshot reads, loaded in one transaction.
struct Loaded {
    state: TaskState,
    approval: Option<RequirementsApprovalRow>,
    runs: Vec<NodeRunRow>,
    /// The task's runs and evidence, and other runs and spend only as far as budgets count them.
    spending: rules::SpendingRows,
    sessions: Vec<AgentSessionRow>,
    findings: Vec<ReviewFindingRow>,
    events: Vec<WorkflowEventRow>,
    questions: Vec<AgentQuestionRow>,
    items: Vec<ImplementationWorkItemRow>,
}

impl Engine {
    pub(crate) fn current_final_blockers(&self, key: &str) -> Result<Vec<String>, WorkflowError> {
        let loaded = self.load_snapshot(key)?;
        let documents =
            workspace(&loaded.state.task).requirement_files().map_err(|e| WorkflowError::new(e.to_string(), 500))?;
        Ok(final_blockers(&loaded.state.task, &loaded, &documents))
    }

    fn load_snapshot(&self, jira_key: &str) -> Result<Loaded, WorkflowError> {
        self.db.session(|tx| {
            let state = TaskState::load(tx, jira_key)?;
            let id = state.task.id;
            let mut events: Vec<WorkflowEventRow> =
                table::select(tx, "WHERE task_id = ?1 ORDER BY id DESC LIMIT 300", [id])?;
            events.reverse();
            Ok(Loaded {
                approval: table::select_one(tx, "WHERE task_id = ?1", [id])?,
                runs: table::select(tx, "WHERE task_id = ?1 ORDER BY id", [id])?,
                spending: rules::load_spending(tx, id, rules::budget_day())?,
                sessions: table::select(tx, "WHERE task_id = ?1 ORDER BY id", [id])?,
                findings: table::select(tx, "WHERE task_id = ?1 ORDER BY id", [id])?,
                events,
                questions: table::select(tx, "WHERE task_id = ?1 ORDER BY id", [id])?,
                items: table::select(tx, "WHERE task_id = ?1", [id])?,
                state,
            })
        })
    }

    /// The full task view the UI renders (and every task-changing route returns).
    /// Usage records of every finished run, by (stage, harness, model), most recent first: what
    /// `estimates::estimate` reads.
    pub(crate) fn estimate_samples(&self) -> Result<EstimateSamples, WorkflowError> {
        let rows = self.db.session(|tx| {
            let mut query = tx.prepare(
                "SELECT workflow_nodes.key, node_runs.harness, node_runs.model, run_evidence.usage \
                 FROM workflow_nodes JOIN node_runs ON node_runs.node_id = workflow_nodes.id \
                 JOIN run_evidence ON run_evidence.run_id = node_runs.id \
                 WHERE node_runs.state IN ('COMPLETED', 'FAILED') \
                 ORDER BY node_runs.completed_at DESC, node_runs.id DESC",
            )?;
            query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let mut groups = EstimateSamples::new();
        for (key, harness, model, usage) in rows {
            let usage: Option<Value> = usage.and_then(|text| serde_json::from_str(&text).ok());
            if let Some(usage) = usage.filter(Value::is_object) {
                groups.entry((estimates::stage_key(&key).to_string(), harness, model)).or_default().push(usage);
            }
        }
        Ok(groups)
    }

    /// What a new task would cost on a route (New Task form), from past runs of each of its
    /// stages with the agents it would get: the configured ones under the form's overrides.
    pub fn cost_estimate(
        &self,
        implementation_mode: Option<&str>,
        overrides: &indexmap::IndexMap<String, super::lifecycle::AgentOverride>,
    ) -> Result<Value, WorkflowError> {
        let config = self.config();
        let harnesses = self.harnesses();
        let mode = implementation_mode
            .map(String::from)
            .unwrap_or_else(|| json!(config.workflow.implementation_mode).as_str().unwrap_or_default().to_string());
        let layers: Map<String, Value> =
            overrides.iter().map(|(role, o)| (role.clone(), Value::Object(o.without_nones()))).collect();
        let planner = spec::planner_role(&config, &layers);
        let mut stages: Vec<(&str, &str, String)> = Vec::new(); // (stage, label, role), in workflow order
        for spec_node in spec::nodes(&config) {
            if spec_node.kind != NodeKind::Agent {
                continue;
            }
            if spec_node.key == spec::ORCHESTRATOR {
                if mode == "direct" {
                    continue;
                }
                let label = if planner == super::states::Role::Architect {
                    spec::ARCHITECT_PLANNING_LABEL
                } else {
                    spec_node.label
                };
                stages.push((spec_node.key, label, planner.as_str().to_string()));
                stages.push((estimates::WORKER_STAGE, "Worker", super::states::Role::Implementer.as_str().to_string()));
            } else {
                let role = spec_node.role.map(|r| r.as_str().to_string()).unwrap_or_default();
                stages.push((spec_node.key, spec_node.label, role));
            }
        }
        let samples = self.estimate_samples()?;
        let route = stages
            .into_iter()
            .map(|(key, label, role)| {
                let layer = layers.get(&role).and_then(Value::as_object);
                let cfg = resolve_role_config(&config, &harnesses, &role, &[layer]);
                let group = (
                    key.to_string(),
                    cfg["harness"].as_str().map(String::from),
                    cfg["model"].as_str().map(String::from),
                );
                let billing = cfg["billingMode"].as_str().unwrap_or_default();
                json!({"key": key, "label": label, "harness": cfg["harness"], "model": cfg["model"],
                       "billingMode": cfg["billingMode"],
                       "estimate": estimates::estimate(samples.get(&group).map_or(&[][..], Vec::as_slice), billing)})
            })
            .collect();
        let mut result = Map::new();
        result.insert("implementationMode".into(), json!(mode));
        if let Value::Object(route) = estimates::route(route) {
            result.extend(route);
        }
        Ok(Value::Object(result))
    }

    pub fn snapshot(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        let loaded = self.load_snapshot(jira_key)?;
        let config = self.config();
        let harnesses = self.harnesses();
        let task = &loaded.state.task.row;
        let nodes: Vec<&WorkflowNodeRow> = loaded.state.nodes.iter().map(|n| &n.row).collect();
        let node_key = |node_id: i64| nodes.iter().find(|n| n.id == node_id).map(|n| n.key.clone());
        let workspace = workspace(task);

        let runs: Vec<&NodeRunRow> = loaded.runs.iter().collect();
        let runs_by_id: HashMap<i64, &NodeRunRow> = runs.iter().map(|r| (r.id, *r)).collect();
        let task_run_ids: HashSet<i64> = runs.iter().map(|r| r.id).collect();
        let evidence_by_run: HashMap<i64, &RunEvidenceRow> = loaded
            .spending
            .evidence
            .iter()
            .filter(|e| task_run_ids.contains(&e.run_id))
            .map(|e| (e.run_id, e))
            .collect();
        let actionable = |q: &AgentQuestionRow| {
            !q.answered
                && q.node_run_id
                    .and_then(|id| runs_by_id.get(&id))
                    .is_some_and(|r| r.state == NodeState::WaitingForUser.as_str())
        };
        let mut open_questions: HashMap<i64, i64> = HashMap::new();
        for question in loaded.questions.iter().filter(|q| actionable(q)) {
            let owner = runs_by_id[&question.node_run_id.unwrap()].node_id;
            *open_questions.entry(owner).or_default() += 1;
        }

        let specs: Vec<NodeSpec> = spec::nodes(&config);
        let order = |key: &str| specs.iter().position(|s| s.key == key).unwrap_or(specs.len());
        let items: HashMap<i64, &ImplementationWorkItemRow> = loaded.items.iter().map(|i| (i.node_id, i)).collect();
        let samples = self.estimate_samples()?;
        let mut sorted = nodes.clone();
        sorted.sort_by_key(|n| order(&n.key));
        let mut node_values = Vec::new();
        for node in sorted {
            let node_runs: Vec<&NodeRunRow> = runs.iter().copied().filter(|r| r.node_id == node.id).collect();
            let last = node_runs.last();
            let effective = (node.kind == NodeKind::Agent.as_str()).then(|| {
                let mut effective = resolve_agent_config(&config, &harnesses, task, &nodes, node, None);
                let harness = effective["harness"].as_str().unwrap_or_default().to_string();
                let display = harnesses.capabilities.get(&harness).map_or(harness.clone(), |c| c.display_name.clone());
                effective.insert("harnessDisplayName".into(), json!(display));
                effective.insert("harnessAvailable".into(), json!(harnesses.is_usable(&harness)));
                Value::Object(effective)
            });
            let node_run_ids: HashSet<i64> = node_runs.iter().map(|r| r.id).collect();
            let node_findings: Vec<&ReviewFindingRow> =
                loaded.findings.iter().filter(|f| f.node_run_id.is_some_and(|id| node_run_ids.contains(&id))).collect();
            // The Implementer coordinator node has no run of its own while workers are active, so
            // it shows RUNNING whenever a worker is live (presentation only).
            let coordinating =
                node.key == spec::IMPLEMENTER && node.state == NodeState::Pending.as_str() && !items.is_empty();
            let worker_nodes = || nodes.iter().filter(|n| items.contains_key(&n.id));
            let live = [NodeState::Running, NodeState::WaitingForUser, NodeState::WaitingForApproval];
            let display_state = if coordinating && worker_nodes().any(|n| state_in(&n.state, &live)) {
                NodeState::Running.as_str().to_string()
            } else {
                node.state.clone()
            };
            let message = if coordinating {
                let done = worker_nodes().filter(|n| n.state == NodeState::Completed.as_str()).count();
                json!(format!("{done}/{} workers complete. Select to view workers.", items.len()))
            } else {
                json!(node.message)
            };
            let config_override =
                if truthy(Some(&node.config_override.0)) { node.config_override.0.clone() } else { json!({}) };
            let description = specs
                .iter()
                .find(|s| s.key == node.key)
                .map(|s| json!(s.description))
                .or_else(|| items.get(&node.id).map(|i| json!(i.brief)))
                .unwrap_or(Value::Null);
            let heartbeat = last
                .map(|run| {
                    let since = if node.updated_at.0 > run.started_at.0 { &node.updated_at } else { &run.started_at };
                    self.heartbeat_snapshot(run.id, since)
                })
                .transpose()?;
            node_values.push(json!({
                "key": node.key, "kind": node.kind, "label": node.label, "role": node.role, "state": node.state,
                "displayState": display_state,
                "executionMode": node.execution_mode, "iteration": node.iteration, "checkpointSha": node.checkpoint_sha,
                "message": message,
                "error": node.error, "configOverride": config_override,
                "effectiveConfig": effective,
                "openQuestions": if node.state == NodeState::WaitingForUser.as_str() { open_questions.get(&node.id).copied().unwrap_or(0) } else { 0 },
                "findingsCount": node_findings.len(), "blockingCount": node_findings.iter().filter(|f| f.blocking).count(),
                "runCount": node_runs.len(),
                "usage": rules::usage_summary(&node_runs, &evidence_by_run),
                "costEstimate": effective.as_ref().map(|cfg| {
                    let group = (
                        estimates::stage_key(&node.key).to_string(),
                        cfg["harness"].as_str().map(String::from),
                        cfg["model"].as_str().map(String::from),
                    );
                    estimates::estimate(
                        samples.get(&group).map_or(&[][..], Vec::as_slice),
                        cfg["billingMode"].as_str().unwrap_or_default(),
                    )
                }),
                "lastRun": last.map(|last| json!({
                    "id": last.id, "runNumber": last.run_number, "state": last.state, "harness": last.harness, "model": last.model,
                    "reasoning": last.reasoning, "runDir": last.run_dir, "promptPath": last.prompt_path, "resultPath": last.result_path,
                    "exitCode": last.exit_code, "error": last.error, "startedAt": iso(Some(&last.started_at)),
                    "completedAt": iso(last.completed_at.as_ref()), "result": last.result.0,
                    "heartbeat": heartbeat,
                })),
                "description": description,
                "worker": items.get(&node.id).map(|item| json!({
                    "uid": item.uid, "key": item.plan_key, "ownedPaths": item.owned_paths.0,
                    "acceptanceCriteria": item.acceptance_criteria.0, "dependencies": item.dependencies.0,
                    "worktreePath": item.worktree_path, "branchName": item.branch_name,
                })),
            }));
        }

        let assessment_path = workspace.final_dir.join("assessment.json");
        let assessment = if assessment_path.exists() {
            crate::workspace::read_json(&assessment_path).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let requirements = workspace.requirement_files().unwrap_or_default();
        let questions: Vec<&AgentQuestionRow> = loaded.questions.iter().collect();
        let spending = loaded.spending.spending();
        let item_refs: Vec<&ImplementationWorkItemRow> = loaded.items.iter().collect();
        let (worktree_exists, worker_count) = worktree_state(task, &item_refs);
        let blockers = final_blockers(task, &loaded, &requirements);
        let has_orchestrator = nodes.iter().any(|n| n.key == spec::ORCHESTRATOR);
        let mut edges: Vec<Value> = spec::EDGES
            .iter()
            .filter(|(s, t)| nodes.iter().any(|n| n.key == *s) && nodes.iter().any(|n| n.key == *t))
            .map(|(s, t)| json!({"source": s, "target": t}))
            .collect();
        if !has_orchestrator {
            edges.push(json!({"source": spec::REQUIREMENTS_GATE, "target": spec::IMPLEMENTER}));
        }
        let now = DbTime::now();

        let mut snapshot = task_summary(task, &nodes);
        let mut add = |key: &str, value: Value| {
            snapshot.insert(key.into(), value);
        };
        add("attention", Value::Array(rules::attention_items(task, &nodes, &runs, &questions)));
        add("budget", rules::budget_status(&spending, Some(task), &config.workflow.cost_controls));
        add("worktreeExists", worktree_exists);
        add("workerWorktreeCount", worker_count);
        add("blockers", json!(blockers));
        add("additionalContext", json!(task.additional_context));
        add("taskDir", json!(task.task_dir));
        add("harnessCwd", json!(py_str(&harness_cwd(task))));
        // Rust only, and only for a repository with setup configured (Python has no such key).
        if let Some(statuses) = self.setup_statuses(task) {
            add("worktreeSetup", json!(statuses));
        }
        add(
            "agentOverrides",
            if task.agent_overrides.0.is_null() { json!({}) } else { task.agent_overrides.0.clone() },
        );
        add(
            "zellij",
            json!({
                "session": task.zellij_session_name,
                "attachCommand": self.terminals.attach_command(&task.zellij_session_name),
                "webUrl": self.terminals.web_url(&task.zellij_session_name),
            }),
        );
        add("nodes", Value::Array(node_values));
        add("edges", Value::Array(edges));
        add(
            "sessions",
            Value::Array(
                loaded
                    .sessions
                    .iter()
                    .map(|s| {
                        let run = s.current_run_id.and_then(|id| runs_by_id.get(&id));
                        json!({
                            "id": s.id, "role": s.role, "harness": s.harness, "model": s.model, "reasoning": s.reasoning,
                            "tabName": s.zellij_tab_name, "tabId": s.zellij_tab_id, "paneId": s.zellij_pane_id,
                            "harnessSessionId": s.harness_session_id, "status": s.status,
                            "activity": session_display_status(s, &runs_by_id),
                            "screenState": if s.status == "running" { json!(s.screen_state) } else { Value::Null },
                            "screenStateAt": iso(s.screen_state_at.as_ref()),
                            "nodeKey": run.and_then(|r| node_key(r.node_id)),
                            "startedAt": iso(Some(&s.started_at)), "endedAt": iso(s.ended_at.as_ref()),
                        })
                    })
                    .collect(),
            ),
        );
        add("findings", Value::Array(loaded.findings.iter().map(finding_dict).collect()));
        add(
            "questions",
            Value::Array(
                loaded
                    .questions
                    .iter()
                    .map(|q| {
                        let run = q.node_run_id.and_then(|id| runs_by_id.get(&id));
                        question_dict(q, run.and_then(|r| node_key(r.node_id)), actionable(q))
                    })
                    .collect(),
            ),
        );
        add(
            "events",
            Value::Array(
                loaded
                    .events
                    .iter()
                    .map(|e| {
                        json!({"id": e.id, "timestamp": iso(Some(&e.timestamp)), "level": e.level,
                               "nodeKey": e.node_key, "message": e.message, "data": e.data.0})
                    })
                    .collect(),
            ),
        );
        add("requirements", json!(requirements));
        add("requirementsFeedback", json!(workspace.requirements_feedback().unwrap_or_default()));
        add("approvedRequirements", loaded.approval.as_ref().map_or(Value::Null, |a| a.documents.0.clone()));
        add("requirementsDigest", json!(loaded.approval.as_ref().map(|a| a.digest.clone())));
        add("requirementsDraftDigest", json!(rules::documents_digest(&requirements)));
        add(
            "executionEvidence",
            Value::Array(
                runs.iter()
                    .map(|run| {
                        let evidence = evidence_by_run.get(&run.id);
                        let end = run.completed_at.unwrap_or(now);
                        let seconds = (end.0 - run.started_at.0).num_microseconds().unwrap_or(0) as f64 / 1e6;
                        json!({
                            "runId": run.id, "nodeKey": node_key(run.node_id), "requestedModel": run.model,
                            "confirmedModel": evidence.and_then(|e| e.confirmed_model.clone()),
                            "usage": evidence.map_or(Value::Null, |e| e.usage.0.clone()),
                            "billingMode": evidence.map_or("unknown".to_string(), |e| e.billing_mode.clone()),
                            "verification": evidence.map_or(Value::Null, |e| e.verification.0.clone()),
                            "durationSeconds": seconds,
                            "iteration": run.iteration,
                        })
                    })
                    .collect(),
            ),
        );
        add("totalUsage", rules::usage_summary(&runs, &evidence_by_run).unwrap_or(Value::Null));
        add("assessment", assessment);
        add("assessmentMarkdown", json!(workspace.read_text("final/assessment.md", "").unwrap_or_default()));
        Ok(Value::Object(snapshot))
    }

    /// Every task (newest first) as the task list shows it. `archived`: exclude, include or only.
    pub async fn list_tasks(&self, archived: &str) -> Result<Vec<Value>, WorkflowError> {
        let filter = match archived {
            "include" => "",
            "only" => "WHERE archived_at IS NOT NULL",
            _ => "WHERE archived_at IS NULL",
        };
        let (tasks, nodes, runs, questions, evidence, items) = self.db.session(|tx| -> Result<_, WorkflowError> {
            Ok((
                table::select::<TaskRow>(tx, &format!("{filter} ORDER BY created_at DESC"), [])?,
                table::select::<WorkflowNodeRow>(tx, "ORDER BY id", [])?,
                table::select::<NodeRunRow>(tx, "ORDER BY id", [])?,
                table::select::<AgentQuestionRow>(tx, "WHERE answered = 0 ORDER BY id", [])?,
                table::select::<RunEvidenceRow>(tx, "", [])?,
                table::select::<ImplementationWorkItemRow>(tx, "", [])?,
            ))
        })?;
        for task in &tasks {
            if task.jira_title.is_none() {
                self.queue_jira_title(&task.jira_key);
            }
        }
        let evidence: HashMap<i64, &RunEvidenceRow> = evidence.iter().map(|e| (e.run_id, e)).collect();
        Ok(tasks
            .iter()
            .map(|task| {
                let task_nodes: Vec<&WorkflowNodeRow> = nodes.iter().filter(|n| n.task_id == task.id).collect();
                let task_runs: Vec<&NodeRunRow> = runs.iter().filter(|r| r.task_id == task.id).collect();
                let task_questions: Vec<&AgentQuestionRow> =
                    questions.iter().filter(|q| q.task_id == task.id).collect();
                let task_items: Vec<&ImplementationWorkItemRow> =
                    items.iter().filter(|i| i.task_id == task.id).collect();
                let mut summary = task_summary(task, &task_nodes);
                summary.insert(
                    "attention".into(),
                    Value::Array(rules::attention_items(task, &task_nodes, &task_runs, &task_questions)),
                );
                summary.insert("totalUsage".into(), rules::usage_summary(&task_runs, &evidence).unwrap_or(Value::Null));
                let (exists, workers) = worktree_state(task, &task_items);
                summary.insert("worktreeExists".into(), exists);
                summary.insert("workerWorktreeCount".into(), workers);
                Value::Object(summary)
            })
            .collect())
    }

    /// Every question a human can answer now (the snapshot's `actionable`: unanswered, its run
    /// still waiting for the reply) across tasks that are not archived, oldest first, with the task
    /// and agent it belongs to: the Decisions inbox.
    pub fn open_questions(&self) -> Result<Vec<Value>, WorkflowError> {
        let waiting = NodeState::WaitingForUser.as_str();
        let (questions, runs, nodes, tasks) = self.db.session(|tx| -> Result<_, WorkflowError> {
            Ok((
                table::select::<AgentQuestionRow>(tx, "WHERE answered = 0 ORDER BY created_at, id", [])?,
                table::select::<NodeRunRow>(tx, "WHERE state = ?1", [waiting])?,
                table::select::<WorkflowNodeRow>(tx, "", [])?,
                table::select::<TaskRow>(tx, "WHERE archived_at IS NULL", [])?,
            ))
        })?;
        Ok(questions
            .iter()
            .filter_map(|question| {
                let run = runs.iter().find(|r| Some(r.id) == question.node_run_id)?;
                let task = tasks.iter().find(|t| t.id == question.task_id)?;
                let node = nodes.iter().find(|n| n.id == run.node_id)?;
                let Value::Object(mut item) = question_dict(question, Some(node.key.clone()), true) else {
                    return None;
                };
                item.insert("jiraKey".into(), json!(task.jira_key));
                item.insert("jiraTitle".into(), json!(task.jira_title));
                item.insert("nodeLabel".into(), json!(node.label));
                item.insert("harness".into(), json!(run.harness));
                item.insert("model".into(), json!(run.model));
                Some(Value::Object(item))
            })
            .collect())
    }

    /// The task, then a broadcast of its snapshot to every connected UI.
    pub fn publish(&self, jira_key: &str) -> Result<Value, WorkflowError> {
        let snapshot = self.snapshot(jira_key)?;
        self.broadcaster.broadcast(&json!({"type": "task", "task": snapshot}));
        Ok(snapshot)
    }
}

fn question_dict(q: &AgentQuestionRow, node_key: Option<String>, actionable: bool) -> Value {
    json!({
        "uid": q.uid, "role": q.role, "message": q.message, "choices": q.choices.0, "decision": q.decision.0,
        "answered": q.answered, "answer": q.answer, "createdAt": iso(Some(&q.created_at)),
        "answeredAt": iso(q.answered_at.as_ref()), "runId": q.node_run_id,
        "nodeKey": node_key, "actionable": actionable,
    })
}

fn state_in(state: &str, states: &[NodeState]) -> bool {
    NodeState::parse(state).is_some_and(|s| states.contains(&s))
}

/// Persistent sessions stay alive between runs, so a running session reports its run's state.
fn session_display_status(session: &AgentSessionRow, runs: &HashMap<i64, &NodeRunRow>) -> String {
    if session.status != "running" {
        return session.status.clone();
    }
    match session.current_run_id.and_then(|id| runs.get(&id)) {
        Some(run) => run.state.to_lowercase(),
        None => "idle".into(),
    }
}

pub fn finding_dict(f: &ReviewFindingRow) -> Value {
    json!({
        "id": f.finding_id, "dbId": f.id, "type": f.kind, "iteration": f.iteration, "reviewedSha": f.reviewed_sha,
        "severity": f.severity, "blocking": f.blocking, "file": f.file, "line": f.line, "endLine": f.end_line,
        "title": f.title, "description": f.description, "recommendation": f.recommendation, "status": f.status,
        "occurrence": f.occurrence, "resolution": f.resolution, "response": f.response, "responseNote": f.response_note,
        "createdAt": iso(Some(&f.created_at)), "resolvedAt": iso(f.resolved_at.as_ref()),
    })
}

/// What stands between the task and Ready for PR (`_final_blockers`).
fn final_blockers(task: &TaskRow, loaded: &Loaded, documents: &indexmap::IndexMap<String, String>) -> Vec<String> {
    let mut blockers = Vec::new();
    if let Err(problem) = rules::check_requirements(loaded.approval.as_ref(), documents) {
        blockers.push(problem);
    }
    let key_of = |run: &NodeRunRow| loaded.state.nodes.iter().find(|n| n.id == run.node_id).map(|n| n.key.clone());
    let mut latest: HashMap<String, &NodeRunRow> = HashMap::new();
    for run in &loaded.runs {
        if let Some(key) = key_of(run) {
            latest.insert(key, run);
        }
    }
    let implementation = latest.get(spec::IMPLEMENTER);
    let verification = implementation
        .and_then(|run| loaded.spending.evidence.iter().find(|e| e.run_id == run.id))
        .map(|e| &e.verification.0)
        .filter(|v| !v.is_null());
    let verified = verification.is_some_and(|v| {
        v.get("sha").and_then(Value::as_str) == task.current_sha.as_deref() && truthy(v.get("passed"))
    });
    if !verified {
        blockers.push("Independent checkpoint verification is missing or failed".into());
    }
    let tests = implementation.and_then(|run| run.result.0.get("tests")).filter(|t| truthy(Some(t)));
    match tests {
        None => blockers.push("Test results are unverified".into()),
        Some(tests) => match tests.get("failed").filter(|f| !f.is_null()) {
            None => blockers.push("Test results are unverified".into()),
            Some(failed) if failed.as_f64().unwrap_or(0.0) > 0.0 => blockers.push("Tests failed".into()),
            Some(_) if !truthy(tests.get("executed")) => blockers.push("No executed tests were reported".into()),
            Some(_) => {}
        },
    }
    let assessment = latest.get(spec::FINAL_ARCHITECT).map(|run| &run.result.0).filter(|r| r.is_object());
    let field = |key: &str| assessment.and_then(|a| a.get(key));
    // Python's str(assessment.get(key, "")): a missing key is "", but a null is "None".
    let text = |key: &str| match field(key) {
        None => String::new(),
        Some(Value::Null) => "None".into(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    if field("requirementsVerified") != Some(&Value::Bool(true)) {
        blockers.push("Requirements are unverified".into());
    }
    if text("taskSummary").trim().is_empty() || text("testsPerformed").trim().is_empty() {
        blockers.push("Assessment evidence is incomplete".into());
    }
    for (key, label) in [("overallRisk", "Overall"), ("securityRisk", "Security"), ("regressionRisk", "Regression")] {
        if text(key).trim().to_uppercase().starts_with("HIGH") {
            blockers.push(format!("{label} risk is high"));
        }
    }
    let unanswered = loaded.questions.iter().any(|q| !q.answered);
    let discovery_open = latest.get(spec::ARCHITECT).is_some_and(|run| truthy(run.result.0.get("openQuestions")));
    if unanswered || discovery_open {
        blockers.push("Questions remain unanswered".into());
    }
    if truthy(field("outstandingUncertainty")) {
        blockers.push("Assessment contains outstanding uncertainty".into());
    }
    for key in [spec::CODE_REVIEW, spec::SECURITY_REVIEW] {
        let reviewed = latest.get(key).is_some_and(|review| {
            review.state == NodeState::Completed.as_str() && review.expected_sha == task.current_sha
        });
        if !reviewed {
            blockers.push(format!("{key}: current checkpoint has not been reviewed"));
        }
    }
    if loaded.findings.iter().any(|f| f.iteration == task.review_iteration && f.blocking) {
        blockers.push("Blocking review findings remain".into());
    }
    blockers
}
