"""SQLAlchemy ORM models. The schema is intentionally small."""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any
from uuid import uuid4

from sqlalchemy import JSON, Boolean, DateTime, ForeignKey, Integer, String, Text, UniqueConstraint
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


class Base(DeclarativeBase):
    pass


class RequirementsApproval(Base):
    __tablename__ = "requirements_approvals"

    uid: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid4()))
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), unique=True)
    digest: Mapped[str] = mapped_column(String(64))
    documents: Mapped[dict] = mapped_column(JSON)
    approved_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)


class RunEvidence(Base):
    __tablename__ = "run_evidence"

    uid: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid4()))
    run_id: Mapped[int] = mapped_column(ForeignKey("node_runs.id"), unique=True)
    requirements_digest: Mapped[str | None] = mapped_column(String(64), nullable=True)
    verification: Mapped[dict | None] = mapped_column(JSON, nullable=True)
    usage: Mapped[dict | None] = mapped_column(JSON, nullable=True)
    confirmed_model: Mapped[str | None] = mapped_column(String(128), nullable=True)
    billing_mode: Mapped[str] = mapped_column(String(16), default="unknown", server_default="unknown")


class RunHeartbeat(Base):
    """Latest authenticated agent report per attempt; never workflow evidence."""

    __tablename__ = "run_heartbeats"

    uid: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid4()))
    run_id: Mapped[int] = mapped_column(ForeignKey("node_runs.id", ondelete="CASCADE"), unique=True)
    received_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))
    phase: Mapped[str] = mapped_column(String(32))


class DeletedRunSpend(Base):
    """What a deleted task's runs cost, so deleting a task never frees daily API budget.

    No foreign keys: the task and run it describes are gone. `cost_usd` is the provider-billed
    metered cost, or None when that was unknown (which still blocks launches under a daily limit)."""

    __tablename__ = "deleted_run_spend"

    id: Mapped[int] = mapped_column(primary_key=True)
    jira_key: Mapped[str] = mapped_column(String(64))
    run_id: Mapped[int] = mapped_column(Integer)
    charged_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), index=True)
    billing_mode: Mapped[str] = mapped_column(String(16))
    cost_usd: Mapped[float | None] = mapped_column(nullable=True)


class Project(Base):
    """A named group of tasks, with defaults the New Task form copies when the project is picked."""

    __tablename__ = "projects"

    id: Mapped[int] = mapped_column(primary_key=True)
    # The URL token; set from the name at creation and kept through renames.
    slug: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    name: Mapped[str] = mapped_column(String(80), unique=True)
    color: Mapped[str] = mapped_column(String(16))
    description: Mapped[str | None] = mapped_column(Text, nullable=True)
    default_repository_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    default_project_relative_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    default_base_branch: Mapped[str | None] = mapped_column(String(255), nullable=True)
    # role -> {"harness": ..., "model": ..., "reasoning": ...}, as on Task.agent_overrides
    agent_overrides: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)
    # Hidden from the pickers; its tasks keep it.
    archived_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow, onupdate=utcnow)


class Task(Base):
    __tablename__ = "tasks"

    id: Mapped[int] = mapped_column(primary_key=True)
    jira_key: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    api_budget_usd: Mapped[float | None] = mapped_column(nullable=True)
    jira_title: Mapped[str | None] = mapped_column(Text, nullable=True)
    repository_path: Mapped[str] = mapped_column(Text)
    repository_root: Mapped[str] = mapped_column(Text)
    project_relative_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    base_branch: Mapped[str] = mapped_column(String(255))
    branch_name: Mapped[str] = mapped_column(String(255))
    worktree_path: Mapped[str] = mapped_column(Text)
    zellij_session_name: Mapped[str] = mapped_column(String(255))
    task_dir: Mapped[str] = mapped_column(Text)
    additional_context: Mapped[str | None] = mapped_column(Text, nullable=True)
    status: Mapped[str] = mapped_column(String(32), default="ACTIVE")
    status_message: Mapped[str | None] = mapped_column(Text, nullable=True)
    review_iteration: Mapped[int] = mapped_column(Integer, default=1)
    max_review_iterations: Mapped[int] = mapped_column(Integer, default=5)
    max_requirements_revisions: Mapped[int] = mapped_column(Integer, default=3)
    current_sha: Mapped[str | None] = mapped_column(String(64), nullable=True)
    pr_url: Mapped[str | None] = mapped_column(Text, nullable=True)
    pr_created_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    merged_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    # Set when the human undoes a PR status; GitHub PR sync then leaves the task alone until they
    # mark one by hand again.
    pr_sync_paused: Mapped[bool] = mapped_column(Boolean, default=False, server_default="0")
    # Head commit of the PR as GitHub merged it. Commits added after final review (PR feedback)
    # are safe to clean up once the branch tip is exactly this commit.
    merged_head_sha: Mapped[str | None] = mapped_column(String(64), nullable=True)
    # The branch the recorded PR targets, as GitHub last reported it (None: not looked up). The UI
    # warns when it isn't base_branch, the branch the task started from.
    pr_base_branch: Mapped[str | None] = mapped_column(String(255), nullable=True)
    # Hidden from the dashboard and chat task lists; set only on a finished task, and undoable.
    archived_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    # Deleting a project clears this first (WorkflowEngine.projects.delete); nothing cascades.
    project_id: Mapped[int | None] = mapped_column(ForeignKey("projects.id"), nullable=True, index=True)
    # role -> {"harness": ..., "model": ..., "reasoning": ...} (task-level overrides)
    agent_overrides: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow, onupdate=utcnow)

    nodes: Mapped[list["WorkflowNode"]] = relationship(back_populates="task", cascade="all, delete-orphan")
    sessions: Mapped[list["AgentSession"]] = relationship(back_populates="task", cascade="all, delete-orphan")
    findings: Mapped[list["ReviewFinding"]] = relationship(back_populates="task", cascade="all, delete-orphan")
    events: Mapped[list["WorkflowEvent"]] = relationship(back_populates="task", cascade="all, delete-orphan")
    artifacts: Mapped[list["TaskArtifact"]] = relationship(back_populates="task", cascade="all, delete-orphan")
    questions: Mapped[list["AgentQuestion"]] = relationship(back_populates="task", cascade="all, delete-orphan")


class WorkflowNode(Base):
    __tablename__ = "workflow_nodes"

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    key: Mapped[str] = mapped_column(String(64))
    kind: Mapped[str] = mapped_column(String(32))  # agent | approval | system | terminal
    label: Mapped[str] = mapped_column(String(128))
    role: Mapped[str | None] = mapped_column(String(64), nullable=True)
    state: Mapped[str] = mapped_column(String(32), default="PENDING")
    execution_mode: Mapped[str] = mapped_column(String(32), default="manual")
    iteration: Mapped[int] = mapped_column(Integer, default=0)
    checkpoint_sha: Mapped[str | None] = mapped_column(String(64), nullable=True)
    message: Mapped[str | None] = mapped_column(Text, nullable=True)
    error: Mapped[str | None] = mapped_column(Text, nullable=True)
    # node-level agent override (harness/model/reasoning)
    config_override: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow, onupdate=utcnow)

    task: Mapped[Task] = relationship(back_populates="nodes")
    runs: Mapped[list["NodeRun"]] = relationship(back_populates="node", cascade="all, delete-orphan")


class NodeRun(Base):
    __tablename__ = "node_runs"

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    node_id: Mapped[int] = mapped_column(ForeignKey("workflow_nodes.id"), index=True)
    run_number: Mapped[int] = mapped_column(Integer, default=1)
    iteration: Mapped[int] = mapped_column(Integer, default=0)
    state: Mapped[str] = mapped_column(String(32), default="RUNNING")
    harness: Mapped[str | None] = mapped_column(String(64), nullable=True)
    model: Mapped[str | None] = mapped_column(String(128), nullable=True)
    reasoning: Mapped[str | None] = mapped_column(String(32), nullable=True)
    run_dir: Mapped[str | None] = mapped_column(Text, nullable=True)
    prompt_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    result_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    expected_sha: Mapped[str | None] = mapped_column(String(64), nullable=True)
    result: Mapped[dict[str, Any] | None] = mapped_column(JSON, nullable=True)
    exit_code: Mapped[int | None] = mapped_column(Integer, nullable=True)
    error: Mapped[str | None] = mapped_column(Text, nullable=True)
    started_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    node: Mapped[WorkflowNode] = relationship(back_populates="runs")


class AgentSession(Base):
    __tablename__ = "agent_sessions"

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    role: Mapped[str] = mapped_column(String(64))
    harness: Mapped[str] = mapped_column(String(64))
    model: Mapped[str | None] = mapped_column(String(128), nullable=True)
    reasoning: Mapped[str | None] = mapped_column(String(32), nullable=True)
    zellij_tab_name: Mapped[str] = mapped_column(String(128))
    zellij_tab_id: Mapped[int | None] = mapped_column(Integer, nullable=True)
    zellij_pane_id: Mapped[str | None] = mapped_column(String(32), nullable=True)
    harness_session_id: Mapped[str | None] = mapped_column(String(128), nullable=True)
    status: Mapped[str] = mapped_column(String(32), default="running")  # running | exited | stopped
    # Live classification of the pane's rendered screen, from ScreenTracker (docs/agent-state-tracking-plan.md).
    # None until a subscribe stream has classified at least one frame; idle | working | blocked | unknown thereafter.
    screen_state: Mapped[str | None] = mapped_column(String(16), nullable=True)
    screen_state_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    current_run_id: Mapped[int | None] = mapped_column(Integer, nullable=True)
    started_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    ended_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    task: Mapped[Task] = relationship(back_populates="sessions")


class ReviewFinding(Base):
    __tablename__ = "review_findings"

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    node_run_id: Mapped[int | None] = mapped_column(Integer, nullable=True)
    finding_id: Mapped[str] = mapped_column(String(32))  # CODE-001 / SEC-001
    type: Mapped[str] = mapped_column(String(32))  # code-review | security-review
    iteration: Mapped[int] = mapped_column(Integer)
    reviewed_sha: Mapped[str | None] = mapped_column(String(64), nullable=True)
    severity: Mapped[str] = mapped_column(String(16))
    blocking: Mapped[bool] = mapped_column(Boolean, default=False)
    file: Mapped[str | None] = mapped_column(Text, nullable=True)
    line: Mapped[int | None] = mapped_column(Integer, nullable=True)
    title: Mapped[str] = mapped_column(Text)
    description: Mapped[str | None] = mapped_column(Text, nullable=True)
    recommendation: Mapped[str | None] = mapped_column(Text, nullable=True)
    status: Mapped[str] = mapped_column(String(16), default="open")  # open | resolved | withdrawn | superseded | accepted
    # Lineage: a re-raised finding keeps its id and points at the row it continues.
    prior_finding_id: Mapped[int | None] = mapped_column(Integer, nullable=True)
    occurrence: Mapped[int] = mapped_column(Integer, default=1)
    resolution: Mapped[str | None] = mapped_column(Text, nullable=True)  # reviewer's evidence for its verdict
    response: Mapped[str | None] = mapped_column(String(16), nullable=True)  # implementer: fixed | disputed | deferred
    response_note: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    resolved_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    # Declared last so a fresh table matches one upgraded with ALTER TABLE.
    end_line: Mapped[int | None] = mapped_column(Integer, nullable=True)  # last line of a multi-line finding

    task: Mapped[Task] = relationship(back_populates="findings")


class WorkflowEvent(Base):
    __tablename__ = "workflow_events"

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    timestamp: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    level: Mapped[str] = mapped_column(String(16), default="info")
    node_key: Mapped[str | None] = mapped_column(String(64), nullable=True)
    message: Mapped[str] = mapped_column(Text)
    data: Mapped[dict[str, Any] | None] = mapped_column(JSON, nullable=True)

    task: Mapped[Task] = relationship(back_populates="events")


class TaskArtifact(Base):
    __tablename__ = "task_artifacts"

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    node_run_id: Mapped[int | None] = mapped_column(Integer, nullable=True)
    kind: Mapped[str] = mapped_column(String(64))
    path: Mapped[str] = mapped_column(Text)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)

    task: Mapped[Task] = relationship(back_populates="artifacts")


class AgentQuestion(Base):
    __tablename__ = "agent_questions"

    id: Mapped[int] = mapped_column(primary_key=True)
    uid: Mapped[str] = mapped_column(String(36), unique=True, index=True, default=lambda: str(uuid4()))
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    node_run_id: Mapped[int | None] = mapped_column(Integer, nullable=True)
    role: Mapped[str] = mapped_column(String(64))
    message: Mapped[str] = mapped_column(Text)
    choices: Mapped[list[str]] = mapped_column(JSON, default=list)
    answered: Mapped[bool] = mapped_column(Boolean, default=False)
    answer: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow)
    answered_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    # A decision tree's `{context, risk, choices}` as validated (workflow.decisions); NULL for a
    # flat question. Declared last, so a fresh table and an upgraded one have the same columns.
    decision: Mapped[dict[str, Any] | None] = mapped_column(JSON, nullable=True)

    task: Mapped[Task] = relationship(back_populates="questions")


class AgentAccess(Base):
    """Additive table: existing databases retain their task and run identities."""

    __tablename__ = "agent_access"

    id: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid4()))
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    run_id: Mapped[int] = mapped_column(ForeignKey("node_runs.id"), unique=True)
    role: Mapped[str] = mapped_column(String(64))
    token_hash: Mapped[str] = mapped_column(String(64), unique=True)


class ProcessLease(Base):
    """Bind exit reports to the launched session, even when it serves later runs."""

    __tablename__ = "process_leases"

    id: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid4()))
    session_id: Mapped[int] = mapped_column(ForeignKey("agent_sessions.id"), unique=True)
    launch_run_id: Mapped[int] = mapped_column(ForeignKey("node_runs.id"), unique=True)


class ImplementationWorkItem(Base):
    """Durable worker identity and briefing; lifecycle remains on its workflow node."""

    __tablename__ = "implementation_work_items"

    uid: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid4()))
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    node_id: Mapped[int] = mapped_column(ForeignKey("workflow_nodes.id"), unique=True)
    plan_key: Mapped[str] = mapped_column(String(64))
    brief: Mapped[str] = mapped_column(Text)
    owned_paths: Mapped[list[str]] = mapped_column(JSON)
    acceptance_criteria: Mapped[list[str]] = mapped_column(JSON)
    dependencies: Mapped[list[str]] = mapped_column(JSON)
    worktree_path: Mapped[str] = mapped_column(Text)
    branch_name: Mapped[str] = mapped_column(Text)
    base_sha: Mapped[str] = mapped_column(String(64))
    prepared: Mapped[bool] = mapped_column(Boolean, default=False)


class ChatThread(Base):
    """Where a task's chat notifications go, and which conditions were already announced.

    `notified` maps each currently-true condition key to the message that announced it, so a
    restart does not repeat a DM and a condition that clears and returns is announced again."""

    __tablename__ = "chat_threads"
    __table_args__ = (UniqueConstraint("task_id", "platform"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    task_id: Mapped[int] = mapped_column(ForeignKey("tasks.id"), index=True)
    platform: Mapped[str] = mapped_column(String(32))
    channel: Mapped[str | None] = mapped_column(String(64), nullable=True)
    thread_id: Mapped[str | None] = mapped_column(String(64), nullable=True)
    title: Mapped[str | None] = mapped_column(Text, nullable=True)
    notified: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utcnow, onupdate=utcnow)
