//! The single fixed workflow, ported from `backend/agentflow/workflow/default_workflow.py`.
//!
//! Node keys are stable identifiers used by the engine, the API and the Vue graph. Edges are
//! informational (drawn by the UI); transitions are implemented in the engine.

use super::states::{NodeKind, Role};
use serde_json::{Map, Value};

use crate::config::{Config, ExecutionMode};

pub const ARCHITECT: &str = "architect";
pub const REQUIREMENTS_GATE: &str = "requirements_gate";
pub const IMPLEMENTER: &str = "implementer";
pub const ORCHESTRATOR: &str = "orchestrator";
pub const CODE_REVIEW: &str = "code_review";
pub const SECURITY_REVIEW: &str = "security_review";
pub const CONSOLIDATE: &str = "consolidate";
pub const FINAL_ARCHITECT: &str = "final_architect";
pub const READY_FOR_PR: &str = "ready_for_pr";

pub const EDGES: [(&str, &str); 10] = [
    (ARCHITECT, REQUIREMENTS_GATE),
    (REQUIREMENTS_GATE, ORCHESTRATOR),
    (ORCHESTRATOR, IMPLEMENTER),
    (IMPLEMENTER, CODE_REVIEW),
    (IMPLEMENTER, SECURITY_REVIEW),
    (CODE_REVIEW, CONSOLIDATE),
    (SECURITY_REVIEW, CONSOLIDATE),
    (CONSOLIDATE, IMPLEMENTER), // fix loop
    (CONSOLIDATE, FINAL_ARCHITECT),
    (FINAL_ARCHITECT, READY_FOR_PR),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSpec {
    pub key: &'static str,
    pub kind: NodeKind,
    pub label: &'static str,
    pub role: Option<Role>,
    pub execution_mode: ExecutionMode,
    pub description: &'static str,
}

/// The workflow's nodes in display order.
pub fn nodes(config: &Config) -> Vec<NodeSpec> {
    let manual = config.workflow.default_execution_mode;
    let node = |key, kind, label, role, execution_mode, description| NodeSpec {
        key,
        kind,
        label,
        role,
        execution_mode,
        description,
    };
    vec![
        node(
            ARCHITECT,
            NodeKind::Agent,
            "Architect",
            Some(Role::Architect),
            manual,
            "Jira intake and discovery through the harness' Jira MCP; produces requirements.",
        ),
        node(
            REQUIREMENTS_GATE,
            NodeKind::Approval,
            "Requirements Approval",
            None,
            ExecutionMode::ApprovalRequired,
            "Human approves requirements, assumptions and the implementation plan.",
        ),
        node(
            ORCHESTRATOR,
            NodeKind::Agent,
            "Orchestrator",
            Some(Role::Orchestrator),
            ExecutionMode::Automatic,
            "Splits the approved scope into focused subtasks; AgentFlow dispatches up to three workers. \
             New tasks give this stage to the Architect unless a separate Orchestrator is configured.",
        ),
        node(
            IMPLEMENTER,
            NodeKind::Agent,
            "Implementer",
            Some(Role::Implementer),
            ExecutionMode::Automatic,
            "Implements the approved plan (or fixes review findings) and reports a checkpoint.",
        ),
        node(
            CODE_REVIEW,
            NodeKind::Agent,
            "Code Review",
            Some(Role::CodeReviewer),
            ExecutionMode::Automatic,
            "Inspect-only code review of the checkpoint SHA.",
        ),
        node(
            SECURITY_REVIEW,
            NodeKind::Agent,
            "Security Review",
            Some(Role::SecurityReviewer),
            ExecutionMode::Automatic,
            "Inspect-only security review of the checkpoint SHA.",
        ),
        node(
            CONSOLIDATE,
            NodeKind::System,
            "Consolidate Findings",
            None,
            ExecutionMode::Automatic,
            "Merges both reviews; loops back to the Implementer or continues to final verification.",
        ),
        node(
            FINAL_ARCHITECT,
            NodeKind::Agent,
            "Final Architect Verification",
            Some(Role::Architect),
            ExecutionMode::Automatic,
            "The persistent Architect verifies the result and produces the risk assessment.",
        ),
        node(
            READY_FOR_PR,
            NodeKind::Terminal,
            "Ready for PR",
            None,
            ExecutionMode::ApprovalRequired,
            "Terminal state. No PR is created automatically.",
        ),
    ]
}

pub const ARCHITECT_PLANNING_LABEL: &str = "Architect Planning";

/// Who plans worker subtasks on a new Parallel task. The Architect does, resuming the conversation
/// it explored the repository in, unless a separate Orchestrator is configured globally
/// (`agents.orchestrator`) or for this task. Stored tasks keep the role their node was created with.
pub fn planner_role(config: &Config, overrides: &Map<String, Value>) -> Role {
    let task_override = overrides
        .get(Role::Orchestrator.as_str())
        .and_then(Value::as_object)
        .is_some_and(|fields| fields.values().any(|value| !value.is_null() && value.as_str() != Some("")));
    if config.agents.orchestrator.is_some() || task_override { Role::Orchestrator } else { Role::Architect }
}

/// The stored string for an execution mode (`manual`, `automatic`, `approval_required`).
pub fn mode_str(mode: ExecutionMode) -> String {
    serde_json::to_value(mode).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}
