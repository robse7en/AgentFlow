"""The single fixed MVP workflow.

Node keys are stable identifiers used by the engine, the API and the Vue graph.
Edges are informational (drawn by the UI); transitions are implemented in the engine.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from ..config import Config
from .states import ExecutionMode, NodeKind, Role


@dataclass(frozen=True)
class NodeSpec:
    key: str
    kind: NodeKind
    label: str
    role: Role | None = None
    execution_mode: ExecutionMode = ExecutionMode.MANUAL
    description: str = ""


@dataclass(frozen=True)
class WorkflowSpec:
    nodes: list[NodeSpec]
    edges: list[tuple[str, str]] = field(default_factory=list)

    def node(self, key: str) -> NodeSpec:
        for node in self.nodes:
            if node.key == key:
                return node
        raise KeyError(key)


ARCHITECT = "architect"
REQUIREMENTS_GATE = "requirements_gate"
IMPLEMENTER = "implementer"
ORCHESTRATOR = "orchestrator"
CODE_REVIEW = "code_review"
SECURITY_REVIEW = "security_review"
CONSOLIDATE = "consolidate"
FINAL_ARCHITECT = "final_architect"
READY_FOR_PR = "ready_for_pr"

EDGES = [
    (ARCHITECT, REQUIREMENTS_GATE),
    (REQUIREMENTS_GATE, ORCHESTRATOR),
    (ORCHESTRATOR, IMPLEMENTER),
    (IMPLEMENTER, CODE_REVIEW),
    (IMPLEMENTER, SECURITY_REVIEW),
    (CODE_REVIEW, CONSOLIDATE),
    (SECURITY_REVIEW, CONSOLIDATE),
    (CONSOLIDATE, IMPLEMENTER),  # fix loop
    (CONSOLIDATE, FINAL_ARCHITECT),
    (FINAL_ARCHITECT, READY_FOR_PR),
]


ARCHITECT_PLANNING_LABEL = "Architect Planning"


def planner_role(config: Config, overrides: dict[str, dict] | None) -> Role:
    """Who plans worker subtasks on a new Parallel task. The Architect does, resuming the conversation
    it explored the repository in, unless a separate Orchestrator is configured globally
    (agents.orchestrator) or for this task. Stored tasks keep the role their node was created with."""
    task_override = (overrides or {}).get(Role.ORCHESTRATOR) or {}
    if config.agents.orchestrator is not None or any(value not in (None, "") for value in task_override.values()):
        return Role.ORCHESTRATOR
    return Role.ARCHITECT


def build_default_workflow(config: Config) -> WorkflowSpec:
    manual = ExecutionMode(config.workflow.default_execution_mode)
    gate_mode = ExecutionMode.APPROVAL_REQUIRED
    final_mode = ExecutionMode.APPROVAL_REQUIRED
    nodes = [
        NodeSpec(ARCHITECT, NodeKind.AGENT, "Architect", Role.ARCHITECT, manual,
                 "Jira intake and discovery through the harness' Jira MCP; produces requirements."),
        NodeSpec(REQUIREMENTS_GATE, NodeKind.APPROVAL, "Requirements Approval", None, gate_mode,
                 "Human approves requirements, assumptions and the implementation plan."),
        NodeSpec(ORCHESTRATOR, NodeKind.AGENT, "Orchestrator", Role.ORCHESTRATOR, ExecutionMode.AUTOMATIC,
                 "Splits the approved scope into focused subtasks; AgentFlow dispatches up to three workers. "
                 "New tasks give this stage to the Architect unless a separate Orchestrator is configured."),
        NodeSpec(IMPLEMENTER, NodeKind.AGENT, "Implementer", Role.IMPLEMENTER, ExecutionMode.AUTOMATIC,
                 "Implements the approved plan (or fixes review findings) and reports a checkpoint."),
        NodeSpec(CODE_REVIEW, NodeKind.AGENT, "Code Review", Role.CODE_REVIEWER, ExecutionMode.AUTOMATIC,
                 "Inspect-only code review of the checkpoint SHA."),
        NodeSpec(SECURITY_REVIEW, NodeKind.AGENT, "Security Review", Role.SECURITY_REVIEWER, ExecutionMode.AUTOMATIC,
                 "Inspect-only security review of the checkpoint SHA."),
        NodeSpec(CONSOLIDATE, NodeKind.SYSTEM, "Consolidate Findings", None, ExecutionMode.AUTOMATIC,
                 "Merges both reviews; loops back to the Implementer or continues to final verification."),
        NodeSpec(FINAL_ARCHITECT, NodeKind.AGENT, "Final Architect Verification", Role.ARCHITECT, ExecutionMode.AUTOMATIC,
                 "The persistent Architect verifies the result and produces the risk assessment."),
        NodeSpec(READY_FOR_PR, NodeKind.TERMINAL, "Ready for PR", None, final_mode,
                 "Terminal state. No PR is created automatically."),
    ]
    return WorkflowSpec(nodes=nodes, edges=EDGES)
