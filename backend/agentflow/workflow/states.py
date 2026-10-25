"""Enumerations shared by engine, API and UI."""

from __future__ import annotations

from enum import StrEnum


class NodeState(StrEnum):
    PENDING = "PENDING"
    READY = "READY"
    RUNNING = "RUNNING"
    WAITING_FOR_USER = "WAITING_FOR_USER"
    WAITING_FOR_APPROVAL = "WAITING_FOR_APPROVAL"
    COMPLETED = "COMPLETED"
    FAILED = "FAILED"
    SKIPPED = "SKIPPED"
    CANCELLED = "CANCELLED"
    INTERRUPTED = "INTERRUPTED"


class TaskStatus(StrEnum):
    ACTIVE = "ACTIVE"
    NEEDS_HUMAN_REVIEW = "NEEDS_HUMAN_REVIEW"
    READY_FOR_PR = "READY_FOR_PR"
    PR_CREATED = "PR_CREATED"
    MERGED = "MERGED"
    FAILED = "FAILED"
    CANCELLED = "CANCELLED"


class NodeKind(StrEnum):
    AGENT = "agent"
    APPROVAL = "approval"
    SYSTEM = "system"
    TERMINAL = "terminal"


class ExecutionMode(StrEnum):
    MANUAL = "manual"
    AUTOMATIC = "automatic"
    APPROVAL_REQUIRED = "approval_required"


class Role(StrEnum):
    ARCHITECT = "architect"
    ORCHESTRATOR = "orchestrator"
    IMPLEMENTER = "implementer"
    CODE_REVIEWER = "code_reviewer"
    SECURITY_REVIEWER = "security_reviewer"


ROLE_LABELS = {
    Role.ORCHESTRATOR: "Orchestrator",
    Role.ARCHITECT: "Architect",
    Role.IMPLEMENTER: "Implementer",
    Role.CODE_REVIEWER: "Code Review",
    Role.SECURITY_REVIEWER: "Security",
}

# Run directories under <task>/runs/
ROLE_RUN_DIRS = {
    Role.ORCHESTRATOR: "orchestrator",
    Role.ARCHITECT: "architect",
    Role.IMPLEMENTER: "implementer",
    Role.CODE_REVIEWER: "code-review",
    Role.SECURITY_REVIEWER: "security-review",
}

# Nodes whose state means "an agent process is expected to report back".
ACTIVE_STATES = {NodeState.RUNNING, NodeState.WAITING_FOR_USER}

# Task reached Ready for PR (or further along); no more nodes, approvals or agent
# signals are accepted from this point on, regardless of PR/merge progress.
POST_READY_STATUSES = {TaskStatus.READY_FOR_PR, TaskStatus.PR_CREATED, TaskStatus.MERGED}

# Nothing more will happen to the task: it may be archived or deleted.
FINISHED_STATUSES = {TaskStatus.MERGED, TaskStatus.CANCELLED, TaskStatus.FAILED}
