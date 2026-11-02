"""Workflow engine: the single source of truth for task and node state.

Everything that changes state goes through this class. The Vue UI and flowctl only call into it.
"""

from __future__ import annotations

import asyncio
import logging
import shlex
import sys
import secrets
import json
import re
import shutil
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Literal
from uuid import uuid4

from sqlalchemy import delete, func, or_, select
from sqlalchemy.orm import Session, selectinload

from ..config import Config, GitHubConfig
from ..diagnostics import event as diagnostic_event
from ..database.database import Database
from ..database.models import (
    AgentQuestion,
    AgentAccess,
    ProcessLease,
    AgentSession,
    NodeRun,
    ReviewFinding,
    Task,
    TaskArtifact,
    WorkflowEvent,
    WorkflowNode,
    RunEvidence,
    RunHeartbeat,
    RequirementsApproval,
    ChatThread,
    DeletedRunSpend,
    ImplementationWorkItem,
    Project,
)
from ..harnesses.base import HarnessAdapter, LaunchSpec, SessionHandle
from ..harnesses.registry import HarnessRegistry
from ..services.events import Broadcaster
from ..services.git_workspace import BranchUnverified, GitError, GitService, WorktreeBusy
from ..services.github import GitHubCli, GitHubError, GitHubUnavailable, PullRequest, compare_url, is_pr_url, repo_from_remote
from ..services.jira_client import (
    JiraError, TicketSync, build_service as build_jira_service,
    manifest_generation as jira_manifest_generation, mark_prompted as mark_jira_prompted,
)
from ..services.process import ProcessError, SAFE_ENV_NAMES, spawn_detached
from ..services.worktree_processes import WorktreeHolder, find_holders, stop_processes_in
from ..services.workspace import TaskWorkspace
from ..services.zellij import EXITED as ZELLIJ_EXITED, ZellijError, ZellijService
from ..api.security import digest
from . import default_workflow as wf
from .models import is_local_task
from . import prompts, instructions
from . import workers, worker_prompts
from . import assurance
from . import review_loop
from . import finding_code
from .screen_tracking import ScreenTracker
from .usage import UsagePending, UsageReport, collect_usage, cost_kind as _cost_kind, prepare_baseline, run_usage
from .attention import attention_items
from .decisions import answer_line
from .projects import clean_overrides, next_color, project_dict, unique_slug
from .budgets import budget_status, charged_at, launch_blocker, run_charge, unknown_cost_runs_today
from .models import (
    AgentOverride,
    CreateTaskRequest,
    DiscoveryResult,
    FinalAssessment,
    Finding,
    ImplementationResult,
    ProjectRequest,
    ReviewResult,
    clean_answer,
    clean_pr_url,
    MAX_FEEDBACK_LENGTH,
)
from .states import ACTIVE_STATES, FINISHED_STATUSES, POST_READY_STATUSES, ROLE_LABELS, ROLE_RUN_DIRS, ExecutionMode, NodeKind, NodeState, Role, TaskStatus

log = logging.getLogger(__name__)

# Seconds between background attempts to collect usage a provider had not written yet (~4 min).
USAGE_RETRY_DELAYS = (5, 10, 15, 30, 60, 120)
# How long stopping an agent waits for a harness asked to exit to write its final usage.
EXIT_USAGE_WAIT_SECONDS = 10


class WorkflowError(Exception):
    """User-facing, actionable error (mapped to HTTP 400/409).

    `code` is a stable machine-readable tag for the few errors the UI reacts to with extra
    controls (e.g. offering to discard worktree drift), so it never has to match message text."""

    WORKTREE_DRIFT = "worktree_drift"
    # Worktree cleanup refused because the branch isn't reviewed or merged; the UI offers force.
    BRANCH_UNVERIFIED = "branch_unverified"

    def __init__(self, message: str, status_code: int = 400, code: str | None = None) -> None:
        super().__init__(message)
        self.status_code = status_code
        self.code = code


class NotFound(WorkflowError):
    def __init__(self, message: str) -> None:
        super().__init__(message, 404)


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _is_checkout(path: Path) -> bool:
    """Whether `path` is a git checkout of its own. A leftover directory whose `.git` is gone isn't:
    asking git there would answer for whatever repository encloses it (the data directory can sit
    inside one), and the previews would list that repository's files as ones about to be discarded."""
    return (path / ".git").exists()


def _describe_error(exc: Exception) -> str:
    """JiraError messages are written for humans; anything else is unexpected, so name its type."""
    return str(exc) if isinstance(exc, JiraError) else f"{type(exc).__name__}: {exc}"


def _iso(value: datetime | None) -> str | None:
    if value is None:
        return None
    if value.tzinfo is None:
        value = value.replace(tzinfo=timezone.utc)
    return value.isoformat()


def _attempt_time(value: datetime | None) -> str | None:
    """A run's timestamp as a retry briefing shows it (stored times are UTC)."""
    if value is None:
        return None
    if value.tzinfo is not None:
        value = value.astimezone(timezone.utc)
    return f"{value:%Y-%m-%d %H:%M} UTC"


def _recorded_start_sha(run_dir: str | None) -> str | None:
    """The HEAD an implementer run recorded at launch (its config.json `startSha`); None when it
    recorded none (runs launched before AgentFlow recorded it) or the file is unreadable."""
    if not run_dir:
        return None
    try:
        value = json.loads(Path(run_dir, "config.json").read_text(encoding="utf-8")).get("startSha")
    except (OSError, ValueError, AttributeError):
        return None
    return value if isinstance(value, str) and value else None


class WorkflowEngine:
    def __init__(
        self,
        config: Config,
        db: Database,
        git: GitService,
        zellij: ZellijService,
        harnesses: HarnessRegistry,
        broadcaster: Broadcaster,
        api_base_url: str,
    ) -> None:
        self.config = config
        self.db = db
        self.git = git
        self.zellij = zellij
        self.harnesses = harnesses
        self.broadcaster = broadcaster
        self.api_base_url = api_base_url
        self.spec = wf.build_default_workflow(config)
        self._locks: dict[str, asyncio.Lock] = {}
        self._launch_lock = asyncio.Lock()
        # Every project change and task assignment; taken after a task's lock, never before it.
        self._projects_lock = asyncio.Lock()
        self._verification_tasks: dict[str, asyncio.Task] = {}
        self._usage_retries: dict[int, asyncio.Task] = {}
        self.screen_tracker = ScreenTracker(zellij, db, self._publish)
        # gh for the human's PR actions (retarget); tests replace it. PR sync has its own.
        self.github: Callable[[GitHubConfig], GitHubCli] = lambda settings: GitHubCli(settings.command)
        self.bin_dir = config.data_dir / "bin"
        self._write_flowctl_shims()

    # ------------------------------------------------------------------------------
    # helpers
    # ------------------------------------------------------------------------------
    def _lock(self, jira_key: str) -> asyncio.Lock:
        return self._locks.setdefault(jira_key.upper(), asyncio.Lock())

    def _write_flowctl_shims(self) -> None:
        """Make `flowctl` resolvable inside agent tabs regardless of how AgentFlow was installed."""
        self.bin_dir.mkdir(parents=True, exist_ok=True)
        python = Path(sys.executable)
        (self.bin_dir / "flowctl.cmd").write_text(
            f'@echo off\r\n"{python}" -m agentflow.cli.flowctl %*\r\n', encoding="utf-8"
        )
        posix = self.bin_dir / "flowctl"
        posix.write_text(f'#!/bin/sh\nexec "{python.as_posix()}" -m agentflow.cli.flowctl "$@"\n', encoding="utf-8", newline="\n")
        try:
            posix.chmod(0o755)
        except OSError:
            pass

    def _load_task(self, session: Session, jira_key: str) -> Task:
        task = session.scalar(
            select(Task).where(Task.jira_key == jira_key.upper()).options(selectinload(Task.nodes))
        )
        if task is None:
            raise NotFound(f"Task {jira_key} not found")
        return task

    @staticmethod
    def _node(task: Task, key: str) -> WorkflowNode:
        for node in task.nodes:
            if node.key == key:
                return node
        raise NotFound(f"Node {key} not found")

    @staticmethod
    def _node_by_role(task: Task, role: str) -> WorkflowNode:
        """The node currently owning a role (final_architect vs architect share a role)."""
        candidates = [n for n in task.nodes if n.role == role]
        for node in candidates:
            if node.state in ACTIVE_STATES:
                return node
        for node in candidates:
            if node.state in (NodeState.READY, NodeState.FAILED):
                return node
        if candidates:
            return candidates[-1]
        raise NotFound(f"No node for role {role}")

    def _log(self, session: Session, task: Task, message: str, *, node_key: str | None = None,
             level: str = "info", data: dict[str, Any] | None = None) -> None:
        session.add(WorkflowEvent(task_id=task.id, message=message, node_key=node_key, level=level, data=data))
        log.info("[%s] %s", task.jira_key, message)

    def _set_state(self, session: Session, task: Task, node: WorkflowNode, state: NodeState,
                   message: str | None = None, error: str | None = None) -> None:
        if node.state != state or message is not None:
            node.state = state
            if message is not None:
                node.message = message
            node.error = error
            self._log(session, task, f"{node.label} → {state}" + (f": {message}" if message else ""),
                      node_key=node.key, level="error" if state == NodeState.FAILED else "info")

    def _open_question_count(self, session: Session, run_id: int) -> int:
        return session.scalar(select(func.count()).select_from(AgentQuestion).where(
            AgentQuestion.node_run_id == run_id, AgentQuestion.answered.is_(False)
        ))

    def workspace(self, task: Task) -> TaskWorkspace:
        return TaskWorkspace(Path(task.task_dir).parent, task.jira_key)

    def _harness_cwd(self, task: Task) -> Path:
        root = Path(task.worktree_path)
        return root / task.project_relative_path if task.project_relative_path else root

    @staticmethod
    def _working_dir(root: Path, relative: str | None) -> str | None:
        """`relative` as a POSIX path inside the repository at `root`; None for the root itself."""
        if not relative:
            return None
        candidate = (root / relative).resolve()
        if not candidate.is_dir():
            raise WorkflowError(f"Working directory does not exist: {candidate}")
        try:
            relative = candidate.relative_to(root.resolve()).as_posix()
        except ValueError as exc:
            raise WorkflowError("Working directory must be inside the repository") from exc
        return None if relative == "." else relative

    async def _publish(self, jira_key: str) -> dict[str, Any]:
        """Build the task snapshot once, broadcast it, and hand it back for the HTTP response."""
        snapshot = self.snapshot(jira_key)
        await self.broadcaster.broadcast({"type": "task", "task": snapshot})
        return snapshot

    def _ensure_screen_tracker(self, task: Task, agent_session: AgentSession) -> None:
        """Start (or confirm) live screen classification for a session already verified
        as the one to keep using - a fresh launch, a surviving session picked back up
        after a backend restart, or one a human just resumed."""
        if not agent_session.zellij_pane_id:
            return
        adapter = self.harnesses.get(agent_session.harness)
        self.screen_tracker.start(task.jira_key, agent_session.id, task.zellij_session_name,
                                  agent_session.zellij_pane_id, adapter)

    async def shutdown(self) -> None:
        for retry in list(self._usage_retries.values()):
            retry.cancel()
        await self.screen_tracker.stop_all()

    # ------------------------------------------------------------------------------
    # agent configuration precedence
    # ------------------------------------------------------------------------------
    def resolve_agent_config(self, task: Task, node: WorkflowNode, override: AgentOverride | None = None) -> dict[str, Any]:
        if node.key.startswith("worker_"):
            node = self._node(task, wf.IMPLEMENTER)
            override = None
        role = node.role or ""
        base = self.config.agents.for_role(role)
        resolved: dict[str, Any] = {"harness": base.harness, "model": base.model, "reasoning": base.reasoning}
        explicit_model = False
        controls = self.config.workflow.cost_controls
        preferred = self.config.harnesses.get("copilot")
        route_reason = "Configured role assignment"
        if role in controls.copilot_roles:
            if preferred and preferred.billing_mode == "subscription" and self.harnesses.is_usable("copilot"):
                resolved.update(harness="copilot", model=preferred.default_model)
                route_reason = "Copilot preferred for this role"
            else:
                route_reason = "Copilot preference unavailable; configured role fallback"
        routed_harness = resolved["harness"]
        for layer in (task.agent_overrides.get(role) if task.agent_overrides else None, node.config_override or None,
                      override.model_dump(exclude_none=True) if override else None):
            if layer:
                route_reason = "Explicit task or stage configuration"
                for field in ("harness", "model", "reasoning"):
                    if layer.get(field) not in (None, ""):
                        resolved[field] = layer[field]
                        if field == "model":
                            explicit_model = True
        if not explicit_model and resolved["harness"] != routed_harness:
            resolved["model"] = None
        if resolved["model"] is None:
            harness_cfg = self.config.harnesses.get(resolved["harness"])
            if harness_cfg:
                resolved["model"] = harness_cfg.default_model
        harness_cfg = self.config.harnesses.get(resolved["harness"])
        resolved["billingMode"] = harness_cfg.billing_mode if harness_cfg else "unknown"
        resolved["routingReason"] = route_reason
        resolved["requiresMeteredApproval"] = ((controls.require_metered_approval or role in controls.copilot_roles)
                                                 and resolved["billingMode"] != "subscription")
        return resolved

    # ------------------------------------------------------------------------------
    # task creation
    # ------------------------------------------------------------------------------
    async def validate_repository(self, path: str, base_branch: str | None = None, project_relative_path: str | None = None) -> dict[str, Any]:
        info = await self.git.inspect_repository(path)
        result: dict[str, Any] = {
            "ok": True,
            "root": info.root,
            "currentBranch": info.current_branch,
            "branches": info.branches,
            "checks": [{"ok": True, "message": "Git repository detected"}, {"ok": True, "message": f"Root: {info.root}"}],
        }
        if base_branch:
            # This runs on every keystroke pause in the New Task form. The branch list already
            # covers local heads and remote-tracking refs, so only a name unknown locally (new
            # on the remote, or a typo) is worth a fetch. Creation resolves the ref again anyway.
            if base_branch in info.branches:
                result["checks"].append({"ok": True, "message": f"Base branch: {base_branch}"})
            else:
                try:
                    await self.git.resolve_base_ref(info.root, base_branch)
                    result["checks"].append({"ok": True, "message": f"Base branch: {base_branch} (on origin)"})
                except GitError as exc:
                    result["ok"] = False
                    result["checks"].append({"ok": False, "message": str(exc)})
        if project_relative_path:
            project = (Path(info.root) / project_relative_path).resolve()
            if not project.is_dir() or Path(info.root).resolve() not in project.parents and project != Path(info.root).resolve():
                result["ok"] = False
                result["checks"].append({"ok": False, "message": f"Working directory not inside repository: {project_relative_path}"})
            else:
                result["checks"].append({"ok": True, "message": f"Working directory: {project_relative_path}"})
        return result

    async def create_task(self, req: CreateTaskRequest) -> dict[str, Any]:
        local = req.task_source == "local"
        jira_key = f"LOCAL-{str(uuid4()).upper()}" if local else req.jira_key
        context = req.additional_context
        if local:  # the prompt leads; blank extra context is dropped
            context = f"{req.prompt}\n\n{context}" if context and context.strip() else req.prompt
        async with self._lock(jira_key):
            with self.db.session() as session:
                existing = session.scalar(select(Task).where(Task.jira_key == jira_key))
                if existing and existing.archived_at:
                    raise WorkflowError(f"A task for {jira_key} exists but is archived. Unarchive or delete it first.", 409)
                if existing:
                    raise WorkflowError(f"A task for {jira_key} already exists. Open it from the task list.", 409)
                if req.project_id is not None:
                    self._assignable_project(session, req.project_id)

            try:
                info = await self.git.inspect_repository(req.repository_path)
            except GitError as exc:
                raise WorkflowError(str(exc)) from exc
            project_rel = self._working_dir(Path(info.root), req.project_relative_path)

            branch = f"{self.config.workspace.branch_prefix}{jira_key}"
            worktree_path = self.config.worktree_dir / jira_key
            try:
                # create_worktree fetches origin and resolves the base ref itself (never a local base
                # that is behind origin).
                created = await self.git.create_worktree(info.root, worktree_path, branch, req.base_branch)
                branch = created.branch
            except GitError as exc:
                raise WorkflowError(str(exc), 409) from exc

            zellij_session = f"{self.config.zellij.session_prefix}{jira_key}"
            workspace = TaskWorkspace(self.config.task_dir, jira_key)
            overrides = {role: o.model_dump(exclude_none=True) for role, o in req.agent_overrides.items() if o.model_dump(exclude_none=True)}
            workspace.create(
                {
                    "jiraKey": jira_key,
                    "repositoryPath": info.path,
                    "repositoryRoot": info.root,
                    "projectRelativePath": project_rel,
                    "baseBranch": req.base_branch,
                    "branchName": branch,
                    "worktreePath": str(worktree_path),
                    "zellijSession": zellij_session,
                    "agentOverrides": overrides,
                    "createdAt": _iso(_now()),
                },
                context,
            )

            # Synced before the DB session opens so no SQLite write lock is held during network
            # I/O. Keyframes are left to the Architect's first run (same reason, and ffmpeg is slow).
            # A local task's title is its prompt's first line, cut to fit the task list.
            jira_title = req.jira_title or (req.prompt.split("\n", 1)[0][:120].strip() if local else None)
            jira_service = None if local else build_jira_service(self.config.jira)
            jira_sync: TicketSync | None = None
            jira_sync_error = None
            if jira_service:
                try:
                    jira_sync = await jira_service.sync_ticket(jira_key, workspace.context_dir, at_task_creation=True)
                    jira_title = jira_title or jira_sync.title
                except Exception as exc:  # the worktree already exists; never leave it without a task row
                    log.warning("Jira sync failed for %s", jira_key, exc_info=True)
                    jira_sync_error = _describe_error(exc)

            with self.db.session() as session:
                # Checked again because the worktree already exists: a project deleted meanwhile
                # leaves the task without one rather than without a task row.
                project_id = req.project_id
                if project_id is not None and session.get(Project, project_id) is None:
                    project_id = None
                task = Task(
                    jira_key=jira_key,
                    project_id=project_id,
                    jira_title=jira_title,
                    repository_path=info.path,
                    repository_root=info.root,
                    project_relative_path=project_rel,
                    base_branch=req.base_branch,
                    branch_name=branch,
                    worktree_path=str(worktree_path),
                    zellij_session_name=zellij_session,
                    task_dir=str(workspace.root),
                    additional_context=context,
                    status=TaskStatus.ACTIVE,
                    # Shown on the task until the first agent run replaces it; the event below stays.
                    status_message=created.base_warning,
                    review_iteration=1,
                    max_review_iterations=self.config.workflow.max_review_iterations,
                    max_requirements_revisions=self.config.workflow.max_requirements_revisions,
                    agent_overrides=overrides,
                )
                session.add(task)
                session.flush()
                for spec_node in self.spec.nodes:
                    if spec_node.key == wf.ORCHESTRATOR and (req.implementation_mode or self.config.workflow.implementation_mode) == "direct":
                        continue
                    label, role = spec_node.label, spec_node.role
                    if spec_node.key == wf.ORCHESTRATOR and wf.planner_role(self.config, overrides) == Role.ARCHITECT:
                        label, role = wf.ARCHITECT_PLANNING_LABEL, Role.ARCHITECT
                    session.add(
                        WorkflowNode(
                            task_id=task.id,
                            key=spec_node.key,
                            kind=spec_node.kind,
                            label=label,
                            role=role,
                            state=NodeState.READY if spec_node.key == wf.ARCHITECT else NodeState.PENDING,
                            execution_mode=spec_node.execution_mode,
                            config_override={},
                        )
                    )
                self._log(session, task, "Task created", data={"repository": info.root, "baseBranch": req.base_branch})
                self._log(session, task, f"Worktree created: {worktree_path} on branch {branch}")
                if project_id != req.project_id:
                    self._log(session, task, "The chosen project was deleted while the task was being created, "
                                             "so the task has no project", level="warning")
                if created.base_warning:
                    self._log(session, task, created.base_warning, level="warning",
                              data={"baseBranch": req.base_branch})

                if jira_sync:
                    self._log_jira_sync(session, task, jira_sync)
                elif jira_sync_error:
                    self._log(session, task, f"Could not sync Jira ticket context: {jira_sync_error}", level="warning")

                try:
                    created = await self.zellij.ensure_session(zellij_session)
                    if created:
                        cwd = self._harness_cwd(task)
                        for name in ("Tests", "Shell"):
                            await self.zellij.new_tab(zellij_session, name, cwd)
                        self._log(session, task, f"Zellij session created: {zellij_session}")
                    else:
                        self._log(session, task, f"Zellij session already existed: {zellij_session}", level="warning")
                except (ZellijError, ProcessError, OSError) as exc:
                    # Task creation still succeeds; the session is retried on the first agent launch.
                    self._log(session, task, f"Zellij session could not be created yet: {exc}", level="error")
        return await self._publish(jira_key)

    # ------------------------------------------------------------------------------
    # node actions from the UI
    # ------------------------------------------------------------------------------
    async def _verify_checkpoint(self, task: Task, expected_sha: str | None) -> list[str]:
        """Check the worktree still matches the reviewed checkpoint. A stale `expected_sha` always
        blocks (the caller's own view of the task is out of date; no reason can fix that). Actual
        worktree drift -- new commits or uncommitted changes made since review -- comes back as
        overridable blockers instead, since a human can knowingly waive them with a reason, or
        discard the uncommitted portion outright via discard_worktree_drift."""
        if not task.current_sha or expected_sha != task.current_sha:
            raise WorkflowError("Checkpoint changed; refresh the task before approving", 409)
        try:
            head = await self.git.head_sha(task.worktree_path)
            paths = await self.git.dirty_paths(task.worktree_path)
        except GitError as exc:
            raise WorkflowError(str(exc), 409) from exc
        blockers = []
        if head != task.current_sha:
            blockers.append(
                f"Worktree changed after review: new commit(s) since the checkpoint "
                f"(reviewed {task.current_sha[:10]}, worktree is now at {head[:10]})")
        if paths:
            names = [p[3:] for p in paths[:5]]
            detail = ", ".join(names) + (f", +{len(paths) - 5} more" if len(paths) > 5 else "")
            blockers.append(f"Worktree changed after review: {len(paths)} file(s) touched since the checkpoint ({detail})")
        return blockers

    async def discard_worktree_drift(self, jira_key: str, expected_sha: str) -> dict[str, Any]:
        """Restore the worktree to the reviewed checkpoint when something (a background build,
        codegen, an IDE) touched files after review without adding real commits."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if not task.current_sha or expected_sha != task.current_sha:
                    raise WorkflowError("Checkpoint changed; refresh the task before discarding drift", 409)
                try:
                    if await self.git.head_sha(task.worktree_path) != task.current_sha:
                        raise WorkflowError(
                            "Worktree has new commits since the checkpoint; create a new checkpoint and review it", 409)
                    paths = await self.git.dirty_paths(task.worktree_path)
                except GitError as exc:
                    raise WorkflowError(str(exc), 409) from exc
                if not paths:
                    raise WorkflowError("Worktree already matches the reviewed checkpoint", 409)
                untracked = [p[3:] for p in paths if p.startswith("??")]
                if untracked:
                    raise WorkflowError(
                        "Untracked file(s) appeared since review; remove or commit them manually before retrying: "
                        + ", ".join(untracked[:5]), 409)
                try:
                    await self.git.reset_hard(task.worktree_path, task.current_sha)
                except GitError as exc:
                    raise WorkflowError(str(exc), 409) from exc
                self._log(session, task, "Human discarded worktree drift to match the reviewed checkpoint",
                          level="warning", data={"sha": task.current_sha, "paths": [p[3:] for p in paths]})
        return await self._publish(jira_key)

    def _final_blockers(self, session: Session, task: Task, documents: dict[str, str] | None = None) -> list[str]:
        blockers = []
        try:
            assurance.check_requirements(session, task, self.workspace(task), documents)
        except ValueError as exc:
            blockers.append(str(exc))
        runs = session.scalars(select(NodeRun).where(NodeRun.task_id == task.id).order_by(NodeRun.id)).all()
        latest = {run.node.key: run for run in runs}
        implementation = latest.get(wf.IMPLEMENTER)
        evidence = session.scalar(select(RunEvidence).where(RunEvidence.run_id == implementation.id)) if implementation else None
        verification = evidence.verification if evidence else None
        if not verification or verification.get("sha") != task.current_sha or not verification.get("passed"):
            blockers.append("Independent checkpoint verification is missing or failed")
        tests = (implementation.result or {}).get("tests") if implementation else None
        if not tests or tests.get("failed") is None:
            blockers.append("Test results are unverified")
        elif tests["failed"] > 0:
            blockers.append("Tests failed")
        elif not tests.get("executed"):
            blockers.append("No executed tests were reported")
        final = latest.get(wf.FINAL_ARCHITECT)
        assessment = (final.result or {}) if final else {}
        if assessment.get("requirementsVerified") is not True:
            blockers.append("Requirements are unverified")
        if not str(assessment.get("taskSummary", "")).strip() or not str(assessment.get("testsPerformed", "")).strip():
            blockers.append("Assessment evidence is incomplete")
        for field, label in (("overallRisk", "Overall"), ("securityRisk", "Security"),
                             ("regressionRisk", "Regression")):
            if str(assessment.get(field, "")).strip().upper().startswith("HIGH"):
                blockers.append(f"{label} risk is high")
        questions = session.scalar(select(AgentQuestion.id).where(
            AgentQuestion.task_id == task.id, AgentQuestion.answered.is_(False)).limit(1))
        discovery = latest.get(wf.ARCHITECT)
        if questions or (discovery and (discovery.result or {}).get("openQuestions")):
            blockers.append("Questions remain unanswered")
        if assessment.get("outstandingUncertainty"):
            blockers.append("Assessment contains outstanding uncertainty")
        for key in (wf.CODE_REVIEW, wf.SECURITY_REVIEW):
            review = latest.get(key)
            if not review or review.state != NodeState.COMPLETED or review.expected_sha != task.current_sha:
                blockers.append(f"{key}: current checkpoint has not been reviewed")
        if session.scalar(select(ReviewFinding.id).where(
            ReviewFinding.task_id == task.id, ReviewFinding.iteration == task.review_iteration,
            ReviewFinding.blocking.is_(True)).limit(1)):
            blockers.append("Blocking review findings remain")
        return blockers

    async def override_reviews(self, jira_key: str, reason: str, expected_sha: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.NEEDS_HUMAN_REVIEW or not reason.strip():
                    raise WorkflowError("Review override requires a stopped review loop and a reason", 409)
                blockers = await self._verify_checkpoint(task, expected_sha) + self._final_blockers(session, task)
                self._log(session, task, "Human overrode review loop blockers", level="warning",
                          data={"sha": task.current_sha, "reason": reason.strip(), "blockers": blockers})
                self._set_state(session, task, self._node(task, wf.IMPLEMENTER), NodeState.COMPLETED,
                                "Further fixes waived by human; findings retained")
                task.status = TaskStatus.ACTIVE
                task.status_message = "Review blockers overridden; final assessment still required"
                final = self._node(task, wf.FINAL_ARCHITECT)
                final.checkpoint_sha = task.current_sha
                final.iteration = task.review_iteration
                self._set_state(session, task, final, NodeState.READY)
        return await self._publish(jira_key)

    async def _stop_sessions(self, session: Session, task: Task, role: str | None = None,
                             *, node: WorkflowNode | None = None, graceful: bool = True) -> None:
        """Close the matching agents' tabs. With `graceful`, idle agents whose CLI writes its final
        usage only on a normal exit are asked to exit first, and that usage is collected."""
        agents = session.scalars(select(AgentSession).where(
            AgentSession.task_id == task.id, AgentSession.status == "running")).all()
        open_tabs: list[AgentSession] = []
        for agent in agents:
            if role and agent.role != role:
                continue
            if node is not None:
                run = session.get(NodeRun, agent.current_run_id) if agent.current_run_id else None
                if run is None or run.node_id not in self._session_node_ids(task, node):
                    continue
            try:
                if await self.zellij.session_exists(task.zellij_session_name):
                    panes = await self.zellij.list_panes(task.zellij_session_name)
                    if agent.zellij_tab_id is None or agent.zellij_tab_id < 0:
                        match = self._untracked_tab(session, task, agent, panes)
                        if match is None:
                            self._mark_stopped(agent)
                            continue
                        agent.zellij_tab_id = match.tab_id
                        agent.zellij_pane_id = match.pane_id
                    if any(p.tab_id == agent.zellij_tab_id for p in panes):
                        open_tabs.append(agent)
                        continue
            except Exception as exc:
                raise WorkflowError(f"Could not confirm agent stopped: {exc}. Retry cancellation.", 409) from exc
            self._mark_stopped(agent)
        if graceful and await self._exit_for_usage(session, task, open_tabs):
            try:
                live_tabs = {p.tab_id for p in await self.zellij.list_panes(task.zellij_session_name)}
            except Exception as exc:
                raise WorkflowError(f"Could not confirm agent stopped: {exc}. Retry cancellation.", 409) from exc
            open_tabs = [agent for agent in open_tabs if agent.zellij_tab_id in live_tabs]
        for agent in open_tabs:
            try:
                await self.zellij.close_tab(task.zellij_session_name, agent.zellij_tab_id)
            except Exception as exc:
                raise WorkflowError(f"Could not confirm agent stopped: {exc}. Retry cancellation.", 409) from exc
            self._mark_stopped(agent)

    def _mark_stopped(self, agent: AgentSession) -> None:
        agent.status = "stopped"
        agent.ended_at = _now()
        self.screen_tracker.cancel_nowait(agent.id)

    async def _exit_for_usage(self, session: Session, task: Task, agents: list[AgentSession]) -> bool:
        """Type the harness's exit command into each idle agent, then collect the usage it writes
        on exit, all within one shared deadline. Returns whether any agent was asked to exit.

        Only an idle screen is safe: typed into a dialog, the Enter would pick an option, and
        into a working agent, the text may be queued as its next prompt."""
        runs = []
        for agent in agents:
            config = self.config.harnesses.get(agent.harness)
            run = session.get(NodeRun, agent.current_run_id) if agent.current_run_id else None
            if not (config and config.usage_command) or not agent.zellij_pane_id or run is None:
                continue
            try:
                adapter = self.harnesses.get(agent.harness)
            except KeyError:
                continue
            if not adapter.exit_command:
                continue
            try:
                screen = await self.zellij.screen(task.zellij_session_name, agent.zellij_pane_id)
                if adapter.classify_screen(screen) != "idle":
                    continue
                await self.zellij.write_line(task.zellij_session_name, agent.zellij_pane_id, adapter.exit_command)
            except (ZellijError, ProcessError):
                continue  # the tab is closed as before
            runs.append(run)
        until = asyncio.get_running_loop().time() + EXIT_USAGE_WAIT_SECONDS
        for run in runs:
            await self._collect_usage(session, task, run, until=until)
        return bool(runs)

    @staticmethod
    def _pending_completion(run: NodeRun | None) -> dict[str, Any] | None:
        """A completion flowctl saved to disk but never got confirmed the backend received."""
        if run is None or not run.run_dir:
            return None
        path = Path(run.run_dir) / "pending-complete.json"
        if not path.exists():
            return None
        try:
            payload = TaskWorkspace.read_json(path)
        except (OSError, json.JSONDecodeError):
            return None
        if not isinstance(payload, dict) or payload.get("runId") != run.id:
            return None
        result = payload.get("result")
        return result if isinstance(result, dict) else None

    async def recover(self) -> None:
        """Reconcile persisted runs without launching agents or replaying work."""
        with self.db.session() as session:
            tasks = session.scalars(select(Task).options(selectinload(Task.nodes)).where(
                Task.status.not_in([TaskStatus.CANCELLED, *POST_READY_STATUSES]))).all()
            for task in tasks:
                for node in task.nodes:
                    if node.state not in ACTIVE_STATES and node.state != NodeState.INTERRUPTED:
                        continue
                    run = session.scalar(select(NodeRun).where(NodeRun.node_id == node.id).order_by(NodeRun.id.desc()))
                    if self._pending_completion(run) is not None:
                        # The agent finished and flowctl captured the result, but the backend never
                        # confirmed receipt before this restart. Don't apply it here: startup must not
                        # block on (or silently trigger) a checkpoint/verification run. Surface it and
                        # let a human hit Resume, same as any other recovery decision.
                        message = "Backend restarted. A finished result was saved to disk before the restart; Resume to submit it."
                    else:
                        message = "Backend restarted. Resume the existing agent or stop it and retry."
                        try:
                            live = await self._live_session(session, task, node.role, node=node)
                            if live is None:
                                message = "Agent is no longer present. Retry explicitly to launch a new run."
                            else:
                                self._ensure_screen_tracker(task, live)
                        except Exception as exc:
                            message = f"Could not inspect agent process: {exc}. Recovery requires a human action."
                    if run:
                        run.state = NodeState.INTERRUPTED
                    self._set_state(session, task, node, NodeState.INTERRUPTED, message)
                workers.recover_pending(self, session, task)

    async def recover_node(self, jira_key: str, node_key: str, retry: bool) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node = self._node(task, node_key)
                if node.state != NodeState.INTERRUPTED or task.status != TaskStatus.ACTIVE:
                    raise WorkflowError("Node is not awaiting recovery", 409)
                run = session.scalar(select(NodeRun).where(NodeRun.node_id == node.id).order_by(NodeRun.id.desc()))
                pending = self._pending_completion(run) if not retry else None
                if pending is not None:
                    run.state = NodeState.RUNNING
                    self._set_state(session, task, node, NodeState.RUNNING,
                        "Human resumed; submitting the completion result saved to disk before the restart")
                    await self._apply_completion(session, task, node, run, pending)
                    if run.run_dir:
                        Path(run.run_dir, "pending-complete.json").unlink(missing_ok=True)
                elif retry:
                    await self._stop_sessions(session, task, node.role, node=node)
                    if run:
                        self._finish_run(run, NodeState.FAILED, "Human retried after interruption")
                    await self._start_agent_run(session, task, node)
                else:
                    live = await self._live_session(session, task, node.role, node=node)
                    access = session.scalar(select(AgentAccess).where(AgentAccess.run_id == run.id)) if run else None
                    if not live or not access:
                        raise WorkflowError("No authenticated agent session survives; use Stop & retry", 409)
                    self._ensure_screen_tracker(task, live)
                    run.state = NodeState.RUNNING
                    self._set_state(session, task, node, NodeState.RUNNING, "Human resumed the surviving agent")
                    await self.zellij.write_line(task.zellij_session_name, live.zellij_pane_id,
                        f"AgentFlow recovered. Continue run {run.id}; use --run {run.id} on flowctl commands. "
                        "If completion was rejected while offline, resubmit the existing result; do not redo completed work.")
        return await self._publish(jira_key)

    async def run_node(self, jira_key: str, node_key: str, override: AgentOverride | None = None,
                       *, metered_approval=None, ready_only: bool = False) -> dict[str, Any]:
        """`ready_only` (chat) starts only a READY stage of an ACTIVE task: never a retry of a
        failed stage or a resumed review loop, which are decisions for the UI. Checked under the
        task lock, so a state change after the caller's own look cannot slip through."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node = self._node(task, node_key)
                if ready_only and (task.status != TaskStatus.ACTIVE or node.state != NodeState.READY):
                    raise WorkflowError(f"{node.label} is no longer ready to start", 409)
                if task.status in (*POST_READY_STATUSES, TaskStatus.CANCELLED):
                    raise WorkflowError(f"Task is {task.status}; nodes can no longer run", 409)
                if node.kind != NodeKind.AGENT:
                    raise WorkflowError(f"{node.label} is not an agent node", 409)
                if node.state not in (NodeState.READY, NodeState.FAILED):
                    raise WorkflowError(f"{node.label} is {node.state}; it can only run when READY (or FAILED, to retry)", 409)
                if override and override.model_dump(exclude_none=True):
                    if node.key.startswith("worker_"):
                        raise WorkflowError("Workers share the Implementer configuration", 409)
                    node.config_override = {**(node.config_override or {}), **override.model_dump(exclude_none=True)}
                if task.status == TaskStatus.NEEDS_HUMAN_REVIEW:
                    if node.key != wf.IMPLEMENTER:
                        raise WorkflowError("Resume the implementer or explicitly override the review blockers", 409)
                    task.review_iteration += 1
                    task.status = TaskStatus.ACTIVE
                    task.status_message = None
                    self._log(session, task, "Human resumed the stopped review loop", level="warning")
                await self._start_agent_run(session, task, node, metered_approval=metered_approval)
                if node.key == wf.ARCHITECT:
                    await self._auto_transition_jira(session, task, jira_key)
        return await self._publish(jira_key)

    async def _auto_transition_jira(self, session, task, jira_key: str) -> None:
        """Best-effort: move the ticket to the configured status when the human clicks Run on
        the Architect node. Never blocks or fails the run — called after the agent session has
        already started, and any Jira problem is logged as a warning, not raised."""
        if is_local_task(task.jira_key):
            return
        target = self.config.jira.auto_transition_status
        if not target:
            return
        jira_service = build_jira_service(self.config.jira)
        if not jira_service:
            return
        try:
            moved = await jira_service.transition_to(jira_key, target, self.config.jira.auto_transition_from_statuses)
            if moved:
                self._log(session, task, f"Jira ticket transitioned to {target}")
        except JiraError as exc:
            self._log(session, task, f"Could not update Jira status: {exc}", level="warning")

    async def _sync_jira_context(self, session: Session, task: Task) -> None:
        """Best-effort re-sync of the ticket's description, comments, attachments and video
        keyframes before an Architect run, so it never plans from a stale snapshot. Never blocks
        or fails the run. The caller has committed, and nothing touches the DB until the network
        and ffmpeg work is done, so no SQLite write lock is held meanwhile."""
        if is_local_task(task.jira_key):
            return
        jira_service = build_jira_service(self.config.jira)
        if not jira_service:
            return
        try:
            result = await jira_service.sync_ticket(task.jira_key, self.workspace(task).context_dir)
        except Exception as exc:  # untrusted payloads: nothing in here may stop the Architect starting
            log.warning("Jira sync failed for %s", task.jira_key, exc_info=True)
            self._log(session, task, f"Could not sync Jira ticket context: {_describe_error(exc)}", level="warning")
            return
        self._log_jira_sync(session, task, result)

    def _log_jira_sync(self, session: Session, task: Task, result: TicketSync) -> None:
        problems = result.skipped or result.frame_errors or result.comments_error
        self._log(session, task, f"Jira context synced: {result.summary()}", level="warning" if problems else "info",
                  data={"added": result.added, "removed": result.removed,
                        "skipped": [{"id": s.id, "file": s.filename, "reason": s.reason} for s in result.skipped],
                        "frames": result.frames, "frameErrors": result.frame_errors})

    async def approve(self, jira_key: str, node_key: str, reason: str | None = None,
                      expected_sha: str | None = None, expected_requirements_digest: str | None = None,
                      via: str | None = None) -> dict[str, Any]:
        """`via` names the channel the human approved through (e.g. "Slack") for the event log."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node = self._node(task, node_key)
                if task.status in (TaskStatus.CANCELLED, *POST_READY_STATUSES):
                    raise WorkflowError("Task no longer accepts approvals", 409)
                legacy_approval = (node.key == wf.REQUIREMENTS_GATE and node.state == NodeState.COMPLETED
                    and not session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id)))
                if node.state != NodeState.WAITING_FOR_APPROVAL and not legacy_approval:
                    raise WorkflowError(f"{node.label} is not waiting for approval", 409)
                if node.key == wf.REQUIREMENTS_GATE:
                    # The digest proves which documents the human was shown; every caller must send it.
                    if not expected_requirements_digest:
                        raise WorkflowError("Refresh and review the requirements before approving", 409)
                    if legacy_approval and any(n.state in ACTIVE_STATES for n in task.nodes):
                        raise WorkflowError("Stop active agents before approving a legacy requirements snapshot", 409)
                    if expected_requirements_digest != assurance.documents_digest(self.workspace(task).requirement_files()):
                        raise WorkflowError("Requirements changed since they were displayed; refresh and review again", 409)
                    try:
                        assurance.approve_requirements(session, task, self.workspace(task))
                    except ValueError as exc:
                        raise WorkflowError(str(exc), 409) from exc
                    self._set_state(session, task, node, NodeState.COMPLETED, "Requirements approved")
                    self._log(session, task, "Requirements approved" + (f" via {via}" if via else ""),
                              data={"via": via, "digest": expected_requirements_digest} if via else None)
                    # Existing tasks keep their original graph; new tasks plan worker subtasks.
                    if not legacy_approval:
                        next_key = wf.ORCHESTRATOR if any(n.key == wf.ORCHESTRATOR for n in task.nodes) else wf.IMPLEMENTER
                        implementer = self._node(task, next_key)
                        implementer.iteration = task.review_iteration
                        self._set_state(session, task, implementer, NodeState.READY)
                        await self._on_ready(session, task, implementer)
                elif node.key == wf.READY_FOR_PR:
                    drift_blockers = await self._verify_checkpoint(task, expected_sha)
                    try:
                        assurance.check_requirements(session, task, self.workspace(task))
                    except ValueError as exc:
                        raise WorkflowError(str(exc), 409) from exc
                    blockers = drift_blockers + self._final_blockers(session, task)
                    if blockers and not (reason and reason.strip()):
                        raise WorkflowError("Override reason required: " + "; ".join(blockers), 409,
                                            code=WorkflowError.WORKTREE_DRIFT if drift_blockers else None)
                    if blockers:
                        self._log(session, task, "Human overrode final blockers", level="warning",
                                  data={"sha": task.current_sha, "reason": reason.strip(), "blockers": blockers})
                    self._finish_ready_for_pr(session, task, node)
                else:
                    raise WorkflowError(f"{node.label} cannot be approved", 409)
        return await self._publish(jira_key)

    async def request_requirements_changes(self, jira_key: str, feedback: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status in (TaskStatus.CANCELLED, *POST_READY_STATUSES):
                    raise WorkflowError("Task no longer accepts approvals", 409)
                gate = self._node(task, wf.REQUIREMENTS_GATE)
                architect = self._node(task, wf.ARCHITECT)
                if gate.state != NodeState.WAITING_FOR_APPROVAL:
                    raise WorkflowError("Requirements gate is not waiting for approval", 409)
                if not feedback.strip():
                    raise WorkflowError("Feedback is required to send requirements back for revision", 400)
                if len(feedback) > MAX_FEEDBACK_LENGTH:
                    raise WorkflowError(f"Feedback must be at most {MAX_FEEDBACK_LENGTH} characters", 400)
                if architect.iteration + 1 > task.max_requirements_revisions:
                    raise WorkflowError(
                        f"Maximum requirement revisions reached ({task.max_requirements_revisions}); "
                        "approve as-is or edit the documents directly", 409)
                architect.iteration += 1
                feedback_text = feedback.strip()
                workspace = self.workspace(task)
                path = workspace.requirements_dir / f"feedback-{architect.iteration:02d}.md"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(feedback_text + "\n", encoding="utf-8")
                self._set_state(session, task, gate, NodeState.PENDING,
                                 f"Sent back for revision (round {architect.iteration})")
                self._set_state(session, task, architect, NodeState.READY,
                                 f"Revise requirements: {feedback_text[:200]}")
                self._log(session, task,
                          f"Requirements sent back for revision (round {architect.iteration}): {feedback_text}",
                          node_key=wf.REQUIREMENTS_GATE, level="warning")
                await self._on_ready(session, task, architect)
        return await self._publish(jira_key)

    async def set_node_override(self, jira_key: str, node_key: str, override: AgentOverride) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node = self._node(task, node_key)
                if node.kind != NodeKind.AGENT:
                    raise WorkflowError("Only agent nodes have harness settings", 409)
                if node.key.startswith("worker_"):
                    raise WorkflowError("Configure the parent Implementer; workers share its settings", 409)
                node.config_override = {k: v for k, v in override.model_dump().items() if v not in (None, "")}
                self._log(session, task, f"{node.label} configuration override set: {node.config_override}", node_key=node.key)
        return await self._publish(jira_key)

    async def cancel_task(self, jira_key: str) -> dict[str, Any]:
        verification = self._verification_tasks.get(jira_key.upper())
        if verification is not None:
            verification.cancel()
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                # Ready for PR / PR Created stay cancellable so an abandoned PR can be retired
                # (then archived or deleted); only a merged task is past cancelling.
                if task.status == TaskStatus.MERGED:
                    raise WorkflowError(f"Task already reached {task.status}", 409)
                await self._stop_sessions(session, task)
                task.status = TaskStatus.CANCELLED
                task.status_message = "Cancelled by user"
                for node in task.nodes:
                    if node.state in (NodeState.PENDING, NodeState.READY, *ACTIVE_STATES, NodeState.WAITING_FOR_APPROVAL):
                        self._set_state(session, task, node, NodeState.CANCELLED)
                for run in session.scalars(select(NodeRun).where(NodeRun.task_id == task.id)).all():
                    if run.state in ACTIVE_STATES:
                        self._finish_run(run, NodeState.CANCELLED)
                self._log(session, task, "Task cancelled; agent tabs stopped. Worktree, branch and artifacts preserved.", level="warning")
        return await self._publish(jira_key)

    async def archive_task(self, jira_key: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status not in FINISHED_STATUSES:
                    raise WorkflowError(f"Only a merged or cancelled task can be archived (currently {task.status})", 409)
                if task.archived_at is None:
                    task.archived_at = _now()
                    self._log(session, task, "Task archived")
        return await self._publish(jira_key)

    async def unarchive_task(self, jira_key: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.archived_at is not None:
                    task.archived_at = None
                    self._log(session, task, "Task unarchived")
        return await self._publish(jira_key)

    # ------------------------------------------------------------------------------
    # projects
    # ------------------------------------------------------------------------------
    def list_projects(self) -> list[dict[str, Any]]:
        """Every project, archived ones included (their tasks still show them), by name."""
        with self.db.session() as session:
            projects = session.scalars(select(Project).order_by(func.lower(Project.name))).all()
            return [project_dict(p) for p in projects]

    async def _publish_projects(self) -> None:
        await self.broadcaster.broadcast({"type": "projects", "projects": self.list_projects()})

    @staticmethod
    def _load_project(session: Session, project_id: int) -> Project:
        project = session.get(Project, project_id)
        if project is None:
            raise NotFound(f"Project {project_id} not found")
        return project

    def _assignable_project(self, session: Session, project_id: int) -> Project:
        project = self._load_project(session, project_id)
        if project.archived_at is not None:
            raise WorkflowError(f"Project {project.name} is archived. Unarchive it to add tasks to it.", 409)
        return project

    async def _apply_project_fields(self, session: Session, project: Project, req: ProjectRequest) -> None:
        """Copy the fields `req` sent onto `project`, checking the name and the default directories."""
        sent = req.model_fields_set
        if "name" in sent or project.name is None:
            if not req.name:
                raise WorkflowError("A project needs a name")
            clash = session.scalar(select(Project).where(func.lower(Project.name) == req.name.lower(),
                                                         Project.id != project.id))
            if clash is not None:
                raise WorkflowError(f"A project named {clash.name} already exists", 409)
            project.name = req.name
        if "color" in sent and req.color is not None:
            project.color = req.color
        if "description" in sent:
            project.description = req.description
        if "default_base_branch" in sent:
            project.default_base_branch = req.default_base_branch
        if "agent_overrides" in sent:
            project.agent_overrides = clean_overrides(req.agent_overrides or {})
        if {"default_repository_path", "default_project_relative_path"} & sent:
            repository = (req.default_repository_path if "default_repository_path" in sent
                          else project.default_repository_path)
            relative = (req.default_project_relative_path if "default_project_relative_path" in sent
                        else project.default_project_relative_path)
            if repository is None:
                if relative is not None:
                    raise WorkflowError("A default working directory needs a default repository")
                project.default_repository_path = project.default_project_relative_path = None
            else:
                try:
                    info = await self.git.inspect_repository(repository)
                except GitError as exc:
                    raise WorkflowError(str(exc)) from exc
                project.default_repository_path = info.root
                project.default_project_relative_path = self._working_dir(Path(info.root), relative)

    async def create_project(self, req: ProjectRequest) -> dict[str, Any]:
        async with self._projects_lock:
            with self.db.session() as session:
                existing = session.scalars(select(Project)).all()
                project = Project(color=req.color or next_color([p.color for p in existing]), agent_overrides={})
                await self._apply_project_fields(session, project, req)
                project.slug = unique_slug(project.name, {p.slug for p in existing})
                session.add(project)
                session.flush()
                result = project_dict(project)
            await self._publish_projects()
        return result

    async def update_project(self, project_id: int, req: ProjectRequest) -> dict[str, Any]:
        async with self._projects_lock:
            with self.db.session() as session:
                project = self._load_project(session, project_id)
                await self._apply_project_fields(session, project, req)
                session.flush()
                result = project_dict(project)
            await self._publish_projects()
        return result

    async def set_project_archived(self, project_id: int, archived: bool) -> dict[str, Any]:
        async with self._projects_lock:
            with self.db.session() as session:
                project = self._load_project(session, project_id)
                if archived != (project.archived_at is not None):
                    project.archived_at = _now() if archived else None
                session.flush()
                result = project_dict(project)
            await self._publish_projects()
        return result

    async def delete_project(self, project_id: int) -> dict[str, Any]:
        """Remove the project; its tasks stay, with no project."""
        async with self._projects_lock:
            with self.db.session() as session:
                project = self._load_project(session, project_id)
                tasks = session.scalars(select(Task).where(Task.project_id == project_id)).all()
                for task in tasks:
                    task.project_id = None
                    self._log(session, task, f"Removed from project {project.name}, which was deleted")
                keys = [task.jira_key for task in tasks]
                session.delete(project)
            await self._publish_projects()
        for key in keys:
            await self._publish(key)
        return {"deleted": project_id, "unassignedTasks": keys}

    async def set_task_project(self, jira_key: str, project_id: int | None) -> dict[str, Any]:
        async with self._lock(jira_key), self._projects_lock:
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.project_id != project_id:
                    before = session.get(Project, task.project_id) if task.project_id is not None else None
                    after = self._assignable_project(session, project_id) if project_id is not None else None
                    task.project_id = project_id
                    if after is not None:
                        self._log(session, task, f"Moved to project {after.name}")
                    elif before is not None:
                        self._log(session, task, f"Removed from project {before.name}")
        return await self._publish(jira_key)

    async def cleanup_worktree(self, jira_key: str, expected_sha: str, force: bool = False) -> dict[str, Any]:
        """`force` skips the check that the task branch is the reviewed checkpoint or in a merged
        pull request, for a task merged somewhere AgentFlow can't see (a local branch). The branch
        is kept."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.MERGED:
                    raise WorkflowError("Cleanup requires the task to be marked Merged", 409)
                if not expected_sha or expected_sha != task.current_sha:
                    raise WorkflowError("Final checkpoint changed; refresh before cleaning up", 409)
                target = Path(task.worktree_path)
                expected = self.config.worktree_dir / task.jira_key
                if target.resolve() != expected.resolve() or target.is_symlink():
                    raise WorkflowError("Worktree is outside the configured task location", 409)
                if target.resolve().parent != self.config.worktree_dir.resolve():
                    raise WorkflowError("Worktree must be directly inside the managed worktree directory", 409)
                try:
                    await self._check_cleanup_agents(session, task)
                except WorkflowError as exc:
                    raise self._cleanup_failed(session, task, exc, "stopping agent tabs", force) from exc
                # D4: commit the agent-session "stopped" status now, before the git step below
                # can fail. `_check_cleanup_agents` already closed those tabs for real (a side
                # effect that can't be undone); if a later GitError propagates out of this
                # `with self.db.session()` block it rolls back everything not yet committed,
                # which used to leave those rows "running" with pane ids that no longer exist.
                session.commit()
                try:
                    # D2: the task's zellij session can still hold panes cleanup never tracks
                    # (the "Tests"/"Shell" convenience tabs created alongside the task, cwd
                    # inside the worktree) even after every *agent* tab is confirmed closed.
                    # On Windows any of them keeps the directory locked and `git worktree
                    # remove` fails with "Permission denied". Close the whole session first.
                    await self.zellij.kill_session(task.zellij_session_name)
                except (ZellijError, ProcessError, OSError) as exc:
                    raise self._cleanup_failed(
                        session, task, WorkflowError(f"Could not close the task's Zellij session: {exc}", 409),
                        "closing the terminal session", force) from exc
                # A just-killed Windows process can hold its old cwd's directory handle open
                # a little longer than `kill_session` waited to confirm the session gone, so
                # `git worktree remove` can still hit "Permission denied" once. Retry once
                # after a short pause; a second attempt also benefits from git.cleanup_worktree's
                # own leftover-directory recovery (D3) if the first attempt partially succeeded
                # (deregistered the worktree, then failed to delete its directory). When the
                # directory itself couldn't be deleted, anything else still working inside it
                # (a terminal the human opened there) is killed before that retry.
                # Recorded before removal so the log names the unverified tip the human vouched for.
                forced_tip: str | None = None
                if force:
                    try:
                        forced_tip = await self.git.branch_sha(task.repository_root, task.branch_name)
                    except (GitError, ProcessError, OSError):
                        pass
                discarded: list[str] = []
                cleanup_exc: Exception | None = None
                for attempt in range(2):
                    try:
                        discarded = await self.git.cleanup_worktree(task.repository_root, task.worktree_path,
                                                        task.branch_name, expected_sha,
                                                        worktree_dir=self.config.worktree_dir,
                                                        merged_head_sha=task.merged_head_sha, force=force)
                        cleanup_exc = None
                        break
                    except BranchUnverified as exc:
                        cleanup_exc = exc
                        break  # Waiting can't make the branch reviewed or merged.
                    except (GitError, ProcessError, OSError) as exc:
                        cleanup_exc = exc
                        if attempt == 0:
                            if isinstance(exc, WorktreeBusy):
                                await self._stop_worktree_processes(session, task)
                            await asyncio.sleep(1.5)
                if cleanup_exc is not None:
                    code = WorkflowError.BRANCH_UNVERIFIED if isinstance(cleanup_exc, BranchUnverified) else None
                    message = str(cleanup_exc)
                    holders: list[WorktreeHolder] = []
                    if isinstance(cleanup_exc, WorktreeBusy):
                        # A directory that can't be deleted is usually held open by something
                        # AgentFlow won't close for the human (an editor with the folder open).
                        try:
                            holders = await find_holders(task.worktree_path)
                        except OSError as exc:
                            log.warning("[%s] Could not look for what holds the worktree: %s", task.jira_key, exc)
                    if holders:
                        names = ", ".join(f"{holder.name} ({holder.pid})" for holder in holders)
                        message += f" (still open in {names}: close {'it' if len(holders) == 1 else 'them'} and try again)"
                    raise self._cleanup_failed(
                        session, task, WorkflowError(message, 409, code=code), "removing the worktree", force,
                        data={"holders": [{"pid": holder.pid, "name": holder.name} for holder in holders]},
                    ) from cleanup_exc
                # Worker subtasks (workers.py) get their own scratch worktree per node; by
                # Merged, their checkpoints are already folded into the task branch, so these
                # are pure disposable clutter. Best-effort: the task's own worktree is already
                # gone by this point, so a worker removal failure shouldn't undo that success.
                worker_removed: list[str] = []
                worker_failed: list[str] = []
                for item in workers.work_items(session, task):
                    try:
                        await self.git.discard_worktree(task.repository_root, item.worktree_path)
                        worker_removed.append(item.worktree_path)
                    except (GitError, ProcessError, OSError) as exc:
                        worker_failed.append(f"{item.worktree_path}: {exc}")
                parts = ["Worktree cleaned up"]
                if discarded:
                    names = [p[3:] for p in discarded[:5]]
                    detail = ", ".join(names) + (f", +{len(discarded) - 5} more" if len(discarded) > 5 else "")
                    parts[0] += f"; discarded {len(discarded)} uncommitted file(s) ({detail})"
                parts[0] += "."
                parts.append("Ignored build/dependency output, if any, was cleared too" +
                              (" (not listed)." if discarded else "."))
                if worker_removed:
                    parts.append(f"Also removed {len(worker_removed)} worker worktree(s).")
                if worker_failed:
                    parts.append(f"Could not remove {len(worker_failed)} worker worktree(s): "
                                  + "; ".join(worker_failed[:3])
                                  + (f", +{len(worker_failed) - 3} more" if len(worker_failed) > 3 else "")
                                  + ".")
                parts.append("Branch and task artifacts preserved.")
                task.status_message = " ".join(parts)
                if force:
                    self._log(session, task, "Worktree cleanup forced past the reviewed/merged branch check",
                              level="warning", data={"sha": expected_sha, "branchSha": forced_tip,
                                                     "mergedHeadSha": task.merged_head_sha})
                self._log(session, task, task.status_message,
                          level="warning" if (discarded or worker_failed) else "info",
                          data={"sha": expected_sha, "discardedPaths": [p[3:] for p in discarded],
                                "workerWorktreesRemoved": worker_removed, "workerWorktreeFailures": worker_failed})
        return await self._publish(jira_key)

    async def finding_code(self, jira_key: str, finding_id: int) -> dict[str, Any]:
        """The code a review finding points at (`GET /api/tasks/{key}/findings/{id}/code`)."""
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            finding = session.scalar(
                select(ReviewFinding).where(ReviewFinding.id == finding_id, ReviewFinding.task_id == task.id)
            )
            if finding is None:
                raise NotFound(f"Finding {finding_id} not found")
        return await finding_code.finding_code(self.git, task, finding)

    async def worktree_cleanup_preview(self, jira_key: str) -> dict[str, Any]:
        """Read-only preview of what a cleanup would discard/close, for the confirm dialog."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.MERGED:
                    raise WorkflowError("Cleanup requires the task to be marked Merged", 409)
                target = Path(task.worktree_path)
                # Modified/untracked only: worth a human's attention before an irreversible
                # discard. Ignored files (build output, dependencies) can number in the tens
                # of thousands in a real repo, so they're deliberately not enumerated here -
                # cleanup discards them too, just silently (see git.cleanup_worktree).
                dirty: list[str] = []
                if _is_checkout(target):
                    try:
                        dirty = await self.git.dirty_paths(target, ignored=False)
                    except GitError as exc:
                        raise WorkflowError(str(exc), 409) from exc
                # Judge "open" the same way cleanup itself will (see _reconcile_agent_pane_status):
                # a live pane wins over DB status, which can be stale by the time a task is Merged.
                agents = session.scalars(select(AgentSession).where(AgentSession.task_id == task.id)).all()
                open_panes: list[str] = []
                # Cleanup kills the task's whole Zellij session (D2), not just tracked agent
                # tabs - e.g. the "Tests"/"Shell" convenience tabs created alongside the task
                # also have their cwd inside the worktree. Report the session-wide close and
                # any such other tabs so the confirm dialog can say so, not just the agent panes.
                session_will_be_closed = False
                other_open_tabs: list[str] = []
                try:
                    session_will_be_closed = await self.zellij.session_exists(task.zellij_session_name)
                    if session_will_be_closed:
                        live = await self.zellij.list_panes(task.zellij_session_name)
                        open_live = [p for p in live if not p.exited]
                        open_ids = {p.pane_id for p in open_live}
                        open_panes = [
                            a.zellij_tab_name for a in agents
                            if ((a.zellij_pane_id in open_ids) if a.zellij_pane_id else a.status == "running")
                        ]
                        tracked_ids = {a.zellij_pane_id for a in agents if a.zellij_pane_id}
                        other_open_tabs = sorted({p.tab_name for p in open_live if p.pane_id not in tracked_ids})
                    elif agents:
                        open_panes = [a.zellij_tab_name for a in agents if a.status == "running"]
                except (ZellijError, ProcessError, OSError):
                    session_will_be_closed = False
                    open_panes = [a.zellij_tab_name for a in agents if a.status == "running"]
                worker_worktrees = [item.worktree_path for item in workers.work_items(session, task)
                                    if Path(item.worktree_path).exists()]
        return {
            "dirtyPaths": [p[3:] for p in dirty],
            "openAgentPanes": open_panes,
            "workerWorktrees": worker_worktrees,
            "sessionWillBeClosed": session_will_be_closed,
            "otherOpenTabs": other_open_tabs,
        }

    # ------------------------------------------------------------------------------
    # deletion
    # ------------------------------------------------------------------------------
    def _deletable(self, session: Session, task: Task) -> None:
        if task.status not in FINISHED_STATUSES:
            raise WorkflowError(f"Only a merged or cancelled task can be deleted (currently {task.status})", 409)
        if self._unknown_cost_blocker(session, task):
            raise WorkflowError("Import the missing run costs before deleting: today's API spend for this task is "
                                "incomplete and the daily budget needs it", 409)

    def _unknown_cost_blocker(self, session: Session, task: Task) -> int:
        """Today's runs of this task whose cost is unknown, when a daily limit would miss them.
        Unknown cost only blocks launches under a daily limit, so without one it never blocks."""
        controls = self.config.workflow.cost_controls
        if controls.daily_budget_usd is None:
            return 0
        runs = session.scalars(select(NodeRun).where(NodeRun.task_id == task.id)).all()
        evidence = {e.run_id: e for e in session.scalars(
            select(RunEvidence).where(RunEvidence.run_id.in_([r.id for r in runs]))).all()}
        return unknown_cost_runs_today(runs, evidence, controls)

    def _task_folder(self, task: Task) -> Path:
        folder = Path(task.task_dir)
        if (folder.is_symlink() or folder.name != task.jira_key
                or folder.resolve().parent != self.config.task_dir.resolve()):
            raise WorkflowError("Task folder is outside the configured task directory", 409)
        return folder

    async def delete_preview(self, jira_key: str) -> dict[str, Any]:
        """Read-only: everything a delete could remove, for the confirm dialog."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status not in FINISHED_STATUSES:
                    raise WorkflowError(f"Only a merged or cancelled task can be deleted (currently {task.status})", 409)
                unknown = self._unknown_cost_blocker(session, task)
                items = [(item.worktree_path, item.branch_name) for item in workers.work_items(session, task)]
                root, branch, base = task.repository_root, task.branch_name, task.base_branch
                worktree, folder, zellij_session = Path(task.worktree_path), Path(task.task_dir), task.zellij_session_name
        dirty: list[str] = []
        worktree_error = None
        if _is_checkout(worktree):
            try:
                dirty = [p[3:] for p in await self.git.dirty_paths(worktree, ignored=False)]
            except GitError as exc:
                worktree_error = str(exc)
        try:
            branch_report = await self.git.branch_report(root, branch, base)
            worker_branches = [name for _, name in items if await self.git.branch_sha(root, name)]
        except (GitError, ProcessError, OSError) as exc:
            branch_report = {"name": branch, "exists": None, "sha": None, "error": str(exc)}
            worker_branches = []
        try:
            session_open = await self.zellij.session_exists(zellij_session)
        except (ZellijError, ProcessError, OSError):
            session_open = False
        return {
            "worktree": {"path": str(worktree), "exists": worktree.exists(), "dirtyPaths": dirty, "error": worktree_error},
            "branch": branch_report,
            "workerWorktrees": [path for path, _ in items if Path(path).exists()],
            "workerBranches": worker_branches,
            "taskFolder": {"path": str(folder), "exists": folder.exists()},
            "sessionWillBeClosed": session_open,
            "unknownCostRunsToday": unknown,
        }

    async def delete_task(self, jira_key: str, *, remove_worktree: bool, remove_branch: bool,
                          remove_task_folder: bool, expected_sha: str | None) -> dict[str, Any]:
        """Permanently delete a finished task. Disk steps run first and are idempotent, so a
        failure part-way leaves the task in place to retry; the database goes last, in one
        transaction; its runs' spend is kept (DeletedRunSpend) so the daily budget stays whole."""
        key = jira_key.upper()
        verification = self._verification_tasks.pop(key, None)
        if verification is not None:
            verification.cancel()
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                self._deletable(session, task)
                checked_out = [task.worktree_path, *(item.worktree_path for item in workers.work_items(session, task))]
                if remove_branch and not remove_worktree and any(Path(path).exists() for path in checked_out):
                    raise WorkflowError("The branches are checked out in worktrees; remove the worktrees to delete them", 409)
                await self._check_cleanup_agents(session, task)
                # Tabs just closed for real can't be reopened; keep their "stopped" rows even if a
                # disk step below fails (same reason as cleanup_worktree's D4).
                session.commit()
                folder = self._task_folder(task) if Path(task.task_dir).exists() else None
                root, branch, worktree = task.repository_root, task.branch_name, task.worktree_path
                zellij_session = task.zellij_session_name
                items = [(item.worktree_path, item.branch_name) for item in workers.work_items(session, task)]
                agent_ids = [agent.id for agent in task.sessions]
                expected_worktree = self.config.worktree_dir / task.jira_key

            if remove_worktree and Path(worktree).resolve() != expected_worktree.resolve():
                raise WorkflowError("Worktree is outside the configured task location", 409)
            if remove_worktree or remove_branch:
                try:
                    current = await self.git.branch_sha(root, branch)
                except (GitError, ProcessError, OSError) as exc:
                    raise WorkflowError(str(exc), 409) from exc
                if current != (expected_sha or None):
                    raise WorkflowError("The task branch changed since the preview; review it again before deleting", 409)

            try:
                # The session's tabs (Tests/Shell included) have their cwd in the worktree and would
                # lock it on Windows; the session belongs to the task either way.
                await self.zellij.kill_session(zellij_session)
            except (ZellijError, ProcessError, OSError) as exc:
                raise WorkflowError(f"Could not close the task's Zellij session: {exc}", 409) from exc
            for agent_id in agent_ids:
                self.screen_tracker.cancel_nowait(agent_id)

            discarded: list[str] = []
            deleted_branches: list[str] = []
            try:
                if remove_worktree:
                    for attempt in range(2):  # a just-killed Windows process can hold the directory briefly
                        try:
                            discarded = await self.git.remove_task_worktree(root, worktree, self.config.worktree_dir)
                            break
                        except (GitError, ProcessError, OSError):
                            if attempt:
                                raise
                            await asyncio.sleep(1.5)
                    for path, _ in items:
                        await self.git.discard_worktree(root, path)
                if remove_branch:
                    for name in [branch, *(name for _, name in items)]:
                        if await self.git.delete_branch(root, name):
                            deleted_branches.append(name)
            except (GitError, ProcessError, OSError) as exc:
                raise WorkflowError(str(exc), 409) from exc

            folder_note = None
            if folder is not None:
                try:
                    if remove_task_folder:
                        shutil.rmtree(folder)
                    else:
                        # A re-created task must start from an empty folder (TaskWorkspace.create
                        # reuses one, and run credentials are created exclusively), so move it aside.
                        kept = folder.with_name(f"{folder.name}.deleted-{_now():%Y%m%d%H%M%S}")
                        folder.rename(kept)
                        folder_note = str(kept)
                except OSError as exc:
                    raise WorkflowError(f"Could not {'remove' if remove_task_folder else 'move aside'} the task folder: {exc}", 409) from exc

            async with self._launch_lock:
                with self.db.session() as session:
                    task = self._load_task(session, jira_key)
                    self._purge_task(session, task)

        removed = {"worktree": remove_worktree, "discardedPaths": [p[3:] for p in discarded],
                   "branches": deleted_branches, "taskFolder": remove_task_folder and folder is not None,
                   "taskFolderMovedTo": folder_note}
        diagnostic_event("task.deleted", jiraKey=key, **removed)
        await self.broadcaster.broadcast({"type": "task.deleted", "jiraKey": key})
        return {"jiraKey": key, "deleted": True, "removed": removed}

    @staticmethod
    def _purge_task(session: Session, task: Task) -> None:
        """Delete the task row and everything under it, keeping its runs' spend."""
        runs = session.scalars(select(NodeRun).where(NodeRun.task_id == task.id)).all()
        run_ids = [run.id for run in runs]
        evidence = {e.run_id: e for e in session.scalars(select(RunEvidence).where(RunEvidence.run_id.in_(run_ids))).all()}
        for run in runs:
            mode, cost = run_charge(evidence.get(run.id))
            session.add(DeletedRunSpend(jira_key=task.jira_key, run_id=run.id, charged_at=charged_at(run),
                                        billing_mode=mode, cost_usd=cost))
        # These tables reference the task (or its runs, nodes and sessions) with no ORM cascade,
        # and foreign keys are enforced: clear them first, children before parents.
        session_ids = [agent.id for agent in task.sessions]
        session.execute(delete(ProcessLease).where(or_(ProcessLease.session_id.in_(session_ids),
                                                       ProcessLease.launch_run_id.in_(run_ids))))
        session.execute(delete(AgentAccess).where(or_(AgentAccess.task_id == task.id, AgentAccess.run_id.in_(run_ids))))
        session.execute(delete(RunEvidence).where(RunEvidence.run_id.in_(run_ids)))
        session.execute(delete(RequirementsApproval).where(RequirementsApproval.task_id == task.id))
        session.execute(delete(ImplementationWorkItem).where(ImplementationWorkItem.task_id == task.id))
        session.execute(delete(ChatThread).where(ChatThread.task_id == task.id))
        session.delete(task)  # nodes (and their runs), sessions, findings, events, artifacts, questions

    async def mark_pr_created(self, jira_key: str, pr_url: str) -> dict[str, Any]:
        try:
            pr_url = clean_pr_url(pr_url)
        except ValueError as exc:
            raise WorkflowError(str(exc), 400) from exc
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.READY_FOR_PR:
                    raise WorkflowError(f"Task must be Ready for PR to mark a PR created (currently {task.status})", 409)
                task.status = TaskStatus.PR_CREATED
                task.pr_url = pr_url
                task.pr_created_at = _now()
                task.pr_base_branch = None
                task.pr_sync_paused = False
                task.status_message = f"PR created: {pr_url}. Mark it Merged once it lands."
                self._log(session, task, "Human marked the pull request as created", data={"prUrl": pr_url})
        await self._transition_jira_for_pr(jira_key)
        return await self._publish(jira_key)

    async def _transition_jira_for_pr(self, jira_key: str) -> None:
        """Best-effort: move the ticket to jira.prCreatedTransitionStatus now the task is PR
        Created. Runs after that change is committed and the task lock released, so a slow or
        failing Jira never holds it up or undoes it; what happened is logged on the task."""
        target = self.config.jira.pr_created_transition_status
        if not target or is_local_task(jira_key):
            return
        jira_service = build_jira_service(self.config.jira)
        if not jira_service:
            return
        try:
            outcome = await jira_service.transition(
                jira_key, target, self.config.jira.pr_created_transition_from_statuses)
        except JiraError as exc:
            message, level = f"Could not move the Jira ticket to {target}: {exc}", "warning"
        else:
            if outcome.result == "moved":
                message, level = f"Jira ticket moved from {outcome.current} to {target}", "info"
            elif outcome.result == "unavailable":
                message, level = (f"Jira ticket left in {outcome.current}: its workflow has no transition to "
                                  f"{target}. Set jira.prCreatedTransitionStatus to your review status, or "
                                  "null to turn this off.", "warning")
            elif outcome.result == "not-allowed":
                message, level = (f"Jira ticket left in {outcome.current}: jira.prCreatedTransitionFromStatuses "
                                  "does not include it", "info")
            else:
                return
        async with self._lock(jira_key):
            with self.db.session() as session:
                try:
                    task = self._load_task(session, jira_key)
                except WorkflowError:
                    return  # deleted meanwhile
                self._log(session, task, message, level=level)

    async def revert_pr_created(self, jira_key: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.PR_CREATED:
                    raise WorkflowError(f"Task is not marked PR Created (currently {task.status})", 409)
                task.status = TaskStatus.READY_FOR_PR
                task.pr_url = None
                task.pr_created_at = None
                task.pr_base_branch = None
                task.status_message = (f"Ready for PR. Push branch {task.branch_name} and open the pull request."
                                       + self._pause_pr_sync(task))
                self._log(session, task, "Human reverted the PR-created status", level="warning")
        return await self._publish(jira_key)

    async def mark_pr_merged(self, jira_key: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.PR_CREATED:
                    raise WorkflowError(
                        f"Task must have a PR created before it can be marked Merged (currently {task.status})", 409)
                task.status = TaskStatus.MERGED
                task.merged_at = _now()
                task.pr_sync_paused = False
                task.status_message = "Merged. Clean up the worktree when you're ready."
                self._log(session, task, "Human marked the pull request as merged")
        return await self._publish(jira_key)

    async def revert_pr_merged(self, jira_key: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.status != TaskStatus.MERGED:
                    raise WorkflowError(f"Task is not marked Merged (currently {task.status})", 409)
                task.status = TaskStatus.PR_CREATED
                task.merged_at = None
                task.merged_head_sha = None
                task.status_message = (f"PR created: {task.pr_url}. Mark it Merged once it lands."
                                        if task.pr_url else "PR created. Mark it Merged once it lands.")
                task.status_message += self._pause_pr_sync(task)
                self._log(session, task, "Human reverted the Merged status", level="warning")
        return await self._publish(jira_key)

    def _pause_pr_sync(self, task: Task) -> str:
        """A human undo overrules GitHub: stop PR sync re-applying what was just undone.
        Returns the sentence to append to the status message."""
        if not self.config.github.pr_sync:
            return ""
        task.pr_sync_paused = True
        return " GitHub PR sync is paused for this task until you mark the PR yourself."

    async def apply_github_pr(self, jira_key: str, pr: PullRequest) -> dict[str, Any] | None:
        """Advance a task to match its pull request on GitHub (see workflow/pr_sync.py).

        Only ever moves forward (Ready for PR -> PR Created -> Merged), then records the merged
        head commit. Re-checks status under the lock since the human may have acted while gh
        was running. Returns None
        when nothing changed, so an unchanged PR costs no write and no broadcast."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                if task.pr_sync_paused:
                    return None
                before = (task.status, task.status_message, task.merged_head_sha, task.pr_base_branch)
                open_note = f"PR created: {pr.url}. It is marked Merged when GitHub shows it merged."
                closed_note = (f"PR closed on GitHub without merging: {pr.url}. Reopen it, or undo PR Created "
                               "and mark the replacement PR.")
                if task.status == TaskStatus.READY_FOR_PR and pr.state in ("OPEN", "MERGED"):
                    task.status = TaskStatus.PR_CREATED
                    task.pr_url = pr.url
                    task.pr_created_at = pr.created_at
                    task.status_message = open_note
                    self._log(session, task, "Detected the pull request on GitHub", data={"prUrl": pr.url})
                # The human may have recorded a different PR while gh was running; only act on that one.
                recorded = (task.pr_url or pr.url).rstrip("/").lower() == pr.url.rstrip("/").lower()
                if task.status == TaskStatus.PR_CREATED and recorded:
                    if pr.state == "MERGED":
                        task.status = TaskStatus.MERGED
                        task.merged_at = pr.merged_at or _now()
                        task.status_message = "Merged on GitHub. Clean up the worktree when you're ready."
                        self._log(session, task, "Detected the pull request merge on GitHub", data={"prUrl": pr.url})
                    elif pr.state == "CLOSED":
                        if task.status_message != closed_note:
                            task.status_message = closed_note
                            self._log(session, task, "The pull request was closed on GitHub without merging",
                                      level="warning", data={"prUrl": pr.url})
                    elif task.status_message == closed_note:
                        task.status_message = open_note  # reopened
                        self._log(session, task, "The pull request was reopened on GitHub", data={"prUrl": pr.url})
                # What GitHub merged lets cleanup accept commits added after final review (PR
                # feedback). Also backfilled for tasks already Merged, including by hand.
                if (task.status == TaskStatus.MERGED and recorded and pr.state == "MERGED"
                        and pr.head_sha and task.merged_head_sha != pr.head_sha):
                    task.merged_head_sha = pr.head_sha
                    if before[0] == TaskStatus.MERGED:
                        self._log(session, task, "Recorded the merged pull request's head commit from GitHub",
                                  data={"prUrl": pr.url, "sha": pr.head_sha})
                # Where the PR merges into: the human may have opened it against the wrong branch.
                if (task.status in (TaskStatus.PR_CREATED, TaskStatus.MERGED) and recorded
                        and pr.base_branch and task.pr_base_branch != pr.base_branch):
                    task.pr_base_branch = pr.base_branch
                    if pr.base_branch != task.base_branch:
                        self._log(session, task,
                                  f"The pull request targets {pr.base_branch}, but the task started from "
                                  f"{task.base_branch}", level="warning",
                                  data={"prUrl": pr.url, "prBaseBranch": pr.base_branch})
                if (task.status, task.status_message, task.merged_head_sha, task.pr_base_branch) == before:
                    return None
                # Not when the PR was found already merged: that ticket is past code review.
                opened = before[0] == TaskStatus.READY_FOR_PR and task.status == TaskStatus.PR_CREATED
        if opened:
            await self._transition_jira_for_pr(jira_key)
        return await self._publish(jira_key)

    async def open_pr(self, jira_key: str) -> dict[str, Any]:
        """Push the reviewed branch and return GitHub's new-PR form with the task's base branch
        already chosen as the target. No lock is held over the push, which can take a while."""
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            status, root, branch, base = task.status, task.repository_root, task.branch_name, task.base_branch
            reviewed, worktree = task.current_sha, task.worktree_path
        if status != TaskStatus.READY_FOR_PR:
            raise WorkflowError(f"Task must be Ready for PR to open a pull request (currently {status})", 409)
        try:
            remote = await self.git.remote_url(root)
            repo = repo_from_remote(remote, self.config.github.hosts) if remote else None
            if repo is None:
                raise WorkflowError(f"origin is not on a GitHub host AgentFlow knows (github.hosts). Push {branch} "
                                    f"and open the PR into {base} yourself.", 409)
            # Only what final review saw goes out; newer commits are the human's to push.
            tip = await self.git.branch_sha(root, branch)
        except (GitError, ProcessError) as exc:
            raise WorkflowError(str(exc), 409) from exc
        if tip is None or tip != reviewed:
            raise WorkflowError(f"{branch} is not at the reviewed checkpoint. Push it yourself if you mean to "
                                "include newer commits.", 409)
        try:
            await self.git.push_branch(worktree if Path(worktree).is_dir() else root, branch)
        except (GitError, ProcessError) as exc:
            raise WorkflowError(str(exc), 502) from exc
        url = compare_url(repo, base, branch)
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                self._log(session, task, f"Pushed {branch} to origin to open a pull request into {base}",
                          data={"compareUrl": url})
        return {"url": url, "task": await self._publish(jira_key)}

    async def retarget_pr(self, jira_key: str) -> dict[str, Any]:
        """Point the recorded PR at the branch the task started from (`gh pr edit --base`)."""
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            status, url, base = task.status, task.pr_url, task.base_branch
        if status != TaskStatus.PR_CREATED:
            raise WorkflowError(f"Task must have a PR created to retarget it (currently {status})", 409)
        settings = self.config.github
        if not url or not is_pr_url(url, settings.hosts):
            raise WorkflowError("The recorded PR link is not a pull request on a GitHub host AgentFlow knows "
                                "(github.hosts)", 409)
        try:
            await self.github(settings).retarget(url, base)
        except GitHubUnavailable as exc:
            raise WorkflowError(f"Could not retarget the PR: {exc}", 502) from exc
        except (GitHubError, ProcessError) as exc:
            # gh's stderr can carry credentials; it goes to the log, not the UI.
            log.warning("Retargeting %s failed: %s", url, exc)
            raise WorkflowError("Could not retarget the PR: gh pr edit failed. Check that you can edit it on GitHub.",
                                502) from exc
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                # The human may have undone PR Created while gh ran; record only against the same PR.
                if task.pr_url == url:
                    task.pr_base_branch = base
                    self._log(session, task, f"Human retargeted the pull request to {base}", data={"prUrl": url})
        return await self._publish(jira_key)

    def _cleanup_failed(self, session: Session, task: Task, error: WorkflowError, step: str, force: bool,
                        data: dict[str, Any] | None = None) -> WorkflowError:
        """Record why a cleanup stopped on the task itself: the status message (the task page
        shows it until a cleanup succeeds) and a warning event. Committed at once, since the
        exception propagating out of `with self.db.session()` would roll them back. Returns
        the error unchanged, for the caller to raise."""
        message = f"Worktree cleanup failed while {step}: {error}"
        diagnostic_event("worktree.cleanup.failed", path=task.worktree_path, step=step, force=force, code=error.code)
        task.status_message = message
        self._log(session, task, message, level="warning",
                  data={**(data or {}), "step": step, "force": force, "code": error.code})
        session.commit()
        return error

    async def _stop_worktree_processes(self, session: Session, task: Task) -> None:
        """Kill whatever still works inside the worktree after the branch passed cleanup's checks
        but the directory couldn't be deleted: e.g. a shell or agent the human started there in
        their own terminal or zellij session, which Windows won't let a directory be deleted
        out from under. Committed at once, since the kills can't be undone even if cleanup
        then fails for another reason."""
        try:
            stopped = await stop_processes_in(task.worktree_path)
        except OSError as exc:
            log.warning("[%s] Could not look for processes in the worktree: %s", task.jira_key, exc)
            return
        if not stopped:
            return
        names = ", ".join(f"{proc.name} ({proc.pid})" for proc in stopped)
        self._log(session, task, f"Closed {len(stopped)} process(es) still running in the worktree: {names}",
                  level="warning", data={"processes": [{"pid": proc.pid, "name": proc.name, "cwd": proc.cwd}
                                                       for proc in stopped]})
        session.commit()

    async def _check_cleanup_agents(self, session: Session, task: Task) -> None:
        """Refuse cleanup if a run is genuinely in flight; otherwise force-close any of
        this task's agent tabs still open (the same mechanism `cancel_task` uses)."""
        busy = {*ACTIVE_STATES, NodeState.INTERRUPTED}
        runs = session.scalars(select(NodeRun).where(NodeRun.task_id == task.id)).all()
        if any(node.state in busy for node in task.nodes) or any(run.state in busy for run in runs):
            raise WorkflowError("An agent run is still active or awaiting recovery", 409)
        await self._reconcile_agent_pane_status(session, task)
        await self._stop_sessions(session, task)

    async def _reconcile_agent_pane_status(self, session: Session, task: Task) -> None:
        """`_stop_sessions` only acts on agents whose DB status is "running", but a task
        reaching Merged can carry rows that drifted from Zellij reality (e.g. a crash
        recovery that never confirmed exit). Align status to any pane we can positively
        identify by pane id before delegating, so cleanup neither force-removes a
        worktree out from under a still-live process nor blocks on a genuinely-dead one."""
        agents = session.scalars(select(AgentSession).where(AgentSession.task_id == task.id)).all()
        if not agents:
            return
        try:
            live = (await self.zellij.list_panes(task.zellij_session_name)
                    if await self.zellij.session_exists(task.zellij_session_name) else [])
        except (ZellijError, ProcessError, OSError) as exc:
            raise WorkflowError(f"Cannot verify agents have stopped: {exc}", 409) from exc
        open_by_pane = {p.pane_id: p for p in live if not p.exited}
        for agent in agents:
            if not agent.zellij_pane_id:
                continue  # unknown identity: leave to _stop_sessions's tab-name fallback/refusal
            pane = open_by_pane.get(agent.zellij_pane_id)
            if pane is not None:
                agent.status = "running"
                agent.zellij_tab_id = pane.tab_id
            elif agent.status == "running":
                agent.status = "stopped"
                agent.ended_at = _now()
                self.screen_tracker.cancel_nowait(agent.id)

    async def open_zellij(self, jira_key: str, node_key: str | None = None) -> dict[str, Any]:
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            tab_name = None
            if node_key:
                node = self._node(task, node_key)
                if node.role:
                    tab_name = self._tab_name(node)
            zellij_session = task.zellij_session_name
        try:
            await self.zellij.ensure_session(zellij_session)
            if tab_name:
                await self.zellij.focus_tab(zellij_session, tab_name)
            await self.zellij.require_subscribe_support()
        except (ZellijError, ProcessError) as exc:
            raise WorkflowError(str(exc), 409) from exc
        return {
            "message": "Embedded read-only terminal view is ready",
            "session": zellij_session,
            "attachCommand": self.zellij.attach_command(zellij_session),
            "webUrl": self.zellij.web_url(zellij_session),
        }

    async def open_zellij_window(self, jira_key: str, node_key: str | None = None) -> dict[str, Any]:
        """Open an interactive terminal window on the task's session (sessions are headless on Windows)."""
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            tab_name = None
            if node_key:
                node = self._node(task, node_key)
                if node.role:
                    tab_name = self._tab_name(node)
            zellij_session = task.zellij_session_name
        try:
            state = await self.zellij.session_state(zellij_session)
            if state is None:
                raise WorkflowError("This task's Zellij session is not running; start an agent first", 409)
            if state == ZELLIJ_EXITED:
                tab_name = None  # attaching resurrects it; until then its tabs can't be focused
            await self.zellij.open_window(zellij_session, tab_name)
        except (ZellijError, ProcessError) as exc:
            raise WorkflowError(str(exc), 409) from exc
        return {"session": zellij_session, "tab": tab_name}

    async def reveal_path(self, jira_key: str, target: str) -> dict[str, Any]:
        """Open the task's worktree or agent CWD in the local OS file explorer."""
        if target not in ("worktree", "harnessCwd"):
            raise WorkflowError(f"Unknown reveal target: {target}", 400)
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            path = Path(task.worktree_path) if target == "worktree" else self._harness_cwd(task)
        if not path.is_dir():
            raise NotFound(f"Path no longer exists: {path}")
        if sys.platform == "win32":
            argv = ["explorer", str(path)]
        elif sys.platform == "darwin":
            argv = ["open", str(path)]
        else:
            argv = ["xdg-open", str(path)]
        try:
            await spawn_detached(argv)
        except OSError as exc:
            raise WorkflowError(f"Could not open file explorer: {exc}", 500) from exc
        return {"path": str(path)}

    # ------------------------------------------------------------------------------
    # agent signals (flowctl)
    # ------------------------------------------------------------------------------
    def _resolve_run(self, session: Session, task: Task, role: str, run_id: int | None) -> tuple[WorkflowNode, NodeRun]:
        if task.status in (TaskStatus.CANCELLED, *POST_READY_STATUSES):
            raise WorkflowError("Task no longer accepts agent signals", 409)
        if run_id is not None:
            run = session.get(NodeRun, run_id)
            if run is None or run.task_id != task.id:
                raise NotFound(f"Run {run_id} not found for task {task.jira_key}")
            node = self._node(task, run.node.key)
            if node.role != role:
                raise WorkflowError("Run does not belong to this role", 403)
            latest = session.scalar(select(NodeRun.id).where(NodeRun.node_id == node.id).order_by(NodeRun.id.desc()))
            if latest != run.id:
                raise WorkflowError("Stale run; signal rejected", 409)
            return node, run
        raise WorkflowError("An explicit run ID is required", 400)

    async def agent_question(
        self, jira_key: str, role: str, run_id: int | None, message: str, choices: list[str] | None = None,
        decision: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """`decision` is a validated tree (decisions.clean_decision), stored beside its top-level
        labels in `choices`."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node, run = self._resolve_run(session, task, role, run_id)
                if run.state not in ACTIVE_STATES:
                    raise WorkflowError("Run is no longer active", 409)
                if node.state not in ACTIVE_STATES:
                    raise WorkflowError(f"{node.label} is {node.state}; questions are only accepted while it is running", 409)
                session.add(AgentQuestion(
                    task_id=task.id, node_run_id=run.id, role=node.role or role, message=message, choices=choices or [],
                    decision=decision,
                ))
                self._set_state(session, task, node, NodeState.WAITING_FOR_USER, "Input required")
                run.state = NodeState.WAITING_FOR_USER
                self._log(session, task, f"{node.label} asked: {message}", node_key=node.key, level="warning")
                result = {"ok": True, "state": node.state, "runId": run.id}
        await self._publish(jira_key)
        return result

    async def answer_question(self, jira_key: str, question_uid: str, answer: str | None = None,
                              via: str | None = None, path: list[int] | None = None) -> dict[str, Any]:
        """Free text (`answer`) or a `path` of 0-based choice indexes; the line typed into the
        pane is built from the stored question (decisions.answer_line), so the same choice sent
        twice is one answer. `via` names the channel the human answered through (e.g. "Slack")
        for the event log."""
        if path is None:
            try:
                answer = clean_answer(answer or "")
            except ValueError as exc:
                raise WorkflowError(str(exc), 422) from exc
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                question = session.scalar(select(AgentQuestion).where(
                    AgentQuestion.task_id == task.id, AgentQuestion.uid == question_uid
                ))
                if question is None:
                    raise NotFound(f"Question {question_uid} not found")
                try:
                    answer = clean_answer(answer_line(question.choices or [], question.decision, path, answer))
                except ValueError as exc:
                    raise WorkflowError(str(exc), 422) from exc
                already_answered = question.answered
                if already_answered:
                    if question.answer != answer:
                        raise WorkflowError("This question has already been answered", 409)
                else:
                    if question.node_run_id is None:
                        raise WorkflowError("Question is not attached to an active agent run", 409)

                    node, run = self._resolve_run(session, task, question.role, question.node_run_id)
                    if run.state != NodeState.WAITING_FOR_USER or node.state != NodeState.WAITING_FOR_USER:
                        raise WorkflowError("The agent is no longer waiting for this answer", 409)

                    agent_session = session.scalar(select(AgentSession).where(
                        AgentSession.task_id == task.id,
                        AgentSession.role == question.role,
                        AgentSession.current_run_id == run.id,
                        AgentSession.status == "running",
                    ).order_by(AgentSession.id.desc()))
                    if agent_session is None or not agent_session.zellij_pane_id:
                        raise WorkflowError("The waiting agent pane is unavailable", 409)

                    try:
                        if not await self.zellij.session_exists(task.zellij_session_name):
                            raise WorkflowError("The Zellij session for this agent is unavailable", 409)
                        pane = await self.zellij.pane_state(task.zellij_session_name, agent_session.zellij_pane_id)
                        if pane is None or pane.exited:
                            raise WorkflowError("The waiting agent pane is no longer active", 409)
                        await self.zellij.write_line(task.zellij_session_name, agent_session.zellij_pane_id, answer)
                    except (ZellijError, ProcessError) as exc:
                        raise WorkflowError(f"Could not send the answer to the agent pane: {exc}", 409) from exc

                    question.answer = answer
                    question.answered = True
                    question.answered_at = _now()
                    still_open = self._open_question_count(session, run.id)
                    if still_open:
                        self._set_state(session, task, node, NodeState.WAITING_FOR_USER,
                                         f"Reply sent; waiting for {still_open} more question(s)")
                    else:
                        self._set_state(session, task, node, NodeState.RUNNING, "All replies sent; agent resumed")
                        run.state = NodeState.RUNNING
                    self._log(session, task, f"Human replied to question {question.uid}" + (f" via {via}" if via else ""),
                              node_key=node.key, data={"via": via} if via else None)
        if not already_answered:
            return await self._publish(jira_key)
        return self.snapshot(jira_key)

    async def terminal_target(self, jira_key: str, role: str) -> tuple[str, str]:
        """Resolve a read-only stream target from the task's active, tracked agent session."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                try:
                    node = None if role in {r.value for r in Role} else next((n for n in task.nodes if n.key == role), None)
                    if node is None:
                        role = Role(role).value
                        node = self._node_by_role(task, role)
                    role = node.role
                except ValueError as exc:
                    raise NotFound(f"Agent role {role} not found") from exc
                if node.state not in {*ACTIVE_STATES, NodeState.INTERRUPTED, NodeState.COMPLETED}:
                    raise WorkflowError("This agent does not have an active pane", 409)
                run = session.scalar(select(NodeRun).where(NodeRun.node_id == node.id).order_by(NodeRun.id.desc()))
                if run is None or run.state not in {*ACTIVE_STATES, NodeState.INTERRUPTED, NodeState.COMPLETED}:
                    raise WorkflowError("This agent does not have an active run", 409)
                agent_session = session.scalar(select(AgentSession).where(
                    AgentSession.task_id == task.id,
                    AgentSession.role == role,
                    AgentSession.current_run_id == run.id,
                    AgentSession.status == "running",
                ).order_by(AgentSession.id.desc()))
                if agent_session is None or not agent_session.zellij_pane_id:
                    raise WorkflowError("The active agent pane is unavailable", 409)
                zellij_session = task.zellij_session_name
                pane_id = agent_session.zellij_pane_id

        try:
            await self.zellij.require_subscribe_support()
            if not await self.zellij.session_exists(zellij_session):
                raise WorkflowError("The Zellij session for this agent is unavailable", 409)
            pane = await self.zellij.pane_state(zellij_session, pane_id)
            if pane is None or pane.exited:
                raise WorkflowError("The active agent pane is no longer available", 409)
        except (ZellijError, ProcessError) as exc:
            raise WorkflowError(str(exc), 409) from exc
        return zellij_session, pane_id

    async def agent_heartbeat(self, jira_key: str, role: str, run_id: int | None,
                              phase: str = "working") -> dict[str, Any]:
        """Coalesce an agent report without changing workflow state or recording an event."""
        if phase not in {"working", "investigating", "implementing", "testing", "reviewing"}:
            raise WorkflowError("Invalid heartbeat phase", 422)
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node, run = self._resolve_run(session, task, role, run_id)
                if run.state not in ACTIVE_STATES or node.state not in ACTIVE_STATES:
                    raise WorkflowError("Heartbeat requires an active run and node", 409)
                report = session.scalar(select(RunHeartbeat).where(RunHeartbeat.run_id == run.id))
                if report is None:
                    report = RunHeartbeat(run_id=run.id)
                    session.add(report)
                report.received_at = _now()
                report.phase = phase
                result = {"ok": True, "runId": run.id, "receivedAt": _iso(report.received_at)}
        await self._publish(jira_key)
        return result

    async def agent_running(self, jira_key: str, role: str, run_id: int | None) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node, run = self._resolve_run(session, task, role, run_id)
                if run.state not in ACTIVE_STATES:
                    raise WorkflowError("Run is no longer active", 409)
                if node.state not in ACTIVE_STATES:
                    raise WorkflowError(f"{node.label} is {node.state}; nothing to resume", 409)
                open_questions = self._open_question_count(session, run.id)
                if open_questions:
                    raise WorkflowError(
                        f"{open_questions} question(s) still awaiting a reply; wait for them in "
                        "this pane or in AgentFlow, then run flowctl running again", 409
                    )
                # A reply to the last open question already resumed this run (answer_question);
                # this call is then just the agent confirming what already happened.
                already_running = run.state == NodeState.RUNNING and node.state == NodeState.RUNNING
                if not already_running:
                    self._set_state(session, task, node, NodeState.RUNNING, "Running")
                    run.state = NodeState.RUNNING
                    self._log(session, task, f"{node.label} resumed (questions answered)", node_key=node.key)
                result = {"ok": True, "state": node.state, "runId": run.id}
        if not already_running:
            await self._publish(jira_key)
        return result

    async def agent_finding(self, jira_key: str, role: str, run_id: int | None, finding: Finding) -> dict[str, Any]:
        """Incremental finding submission (alternative to embedding findings in the result)."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node, run = self._resolve_run(session, task, role, run_id)
                if run.state not in ACTIVE_STATES or role not in (Role.CODE_REVIEWER, Role.SECURITY_REVIEWER):
                    raise WorkflowError("Findings require an active reviewer run", 409)
                if node.state not in ACTIVE_STATES:
                    raise WorkflowError(f"{node.label} is {node.state}; findings are only accepted while it is running", 409)
                pending_path = Path(run.run_dir or self.workspace(task).run_dir(ROLE_RUN_DIRS[Role(node.role)], run.run_number)) / "findings-pending.json"
                pending = TaskWorkspace.read_json(pending_path) if pending_path.exists() else []
                pending.append(finding.model_dump(by_alias=True, exclude_none=True))
                TaskWorkspace.write_json(pending_path, pending)
                self._log(session, task, f"{node.label} reported finding {finding.id or ''}: {finding.title}", node_key=node.key)
                result = {"ok": True, "count": len(pending)}
        await self._publish(jira_key)
        return result

    async def agent_fail(self, jira_key: str, role: str, run_id: int | None, message: str) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node, run = self._resolve_run(session, task, role, run_id)
                if run.state == NodeState.FAILED and run.error == message:
                    return {"ok": True, "state": node.state}
                if run.state not in ACTIVE_STATES or node.state not in ACTIVE_STATES:
                    raise WorkflowError("Run is no longer active", 409)
                run.state = NodeState.FAILED
                run.error = message
                run.completed_at = _now()
                await self._collect_usage(session, task, run)
                self._set_state(session, task, node, NodeState.FAILED, "Agent reported failure", error=message)
                result = {"ok": True, "state": node.state}
        await self._publish(jira_key)
        return result

    async def agent_exited(self, jira_key: str, role: str, run_id: int | None, exit_code: int) -> dict[str, Any]:
        """Launcher script reports the harness process ended."""
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                run = session.get(NodeRun, run_id) if run_id is not None else None
                if not run or run.task_id != task.id or run.node.role != role:
                    raise WorkflowError("Unknown launcher run", 409)
                node = self._node(task, run.node.key)
                run.exit_code = exit_code
                await self._collect_usage(session, task, run)
                agent_session = session.scalar(
                    select(AgentSession).join(ProcessLease, ProcessLease.session_id == AgentSession.id).where(
                        ProcessLease.launch_run_id == run.id, AgentSession.status == "running")
                )
                if agent_session is not None:
                    agent_session.status = "exited"
                    agent_session.ended_at = _now()
                    self.screen_tracker.cancel_nowait(agent_session.id)
                    # A persistent session may be serving a later run than the one it was launched for.
                    if agent_session.current_run_id and agent_session.current_run_id != run.id:
                        later = session.get(NodeRun, agent_session.current_run_id)
                        if later is not None:
                            run = later
                            node = self._node(task, later.node.key)
                if run.state in ACTIVE_STATES and node.state in ACTIVE_STATES:
                    run.state = NodeState.FAILED
                    run.completed_at = _now()
                    run.error = f"Harness exited with code {exit_code} before reporting completion"
                    self._set_state(session, task, node, NodeState.FAILED, f"Harness exited (code {exit_code})", error=run.error)
                else:
                    self._log(session, task, f"{node.label} harness process exited (code {exit_code})", node_key=node.key)
                result = {"ok": True, "state": node.state}
        await self._publish(jira_key)
        return result

    async def agent_complete(self, jira_key: str, role: str, run_id: int | None, result: dict[str, Any]) -> dict[str, Any]:
        async with self._lock(jira_key):
            with self.db.session() as session:
                task = self._load_task(session, jira_key)
                node, run = self._resolve_run(session, task, role, run_id)
                response = await self._apply_completion(session, task, node, run, result)
        await self._publish(jira_key)
        return response

    async def _apply_completion(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun,
                                 result: dict[str, Any]) -> dict[str, Any]:
        """Shared by the live `flowctl complete` call and a human replaying a persisted result
        after recovery (recover_node); must run inside an existing lock + session for the task."""
        if run.state == NodeState.COMPLETED and run.result == result:
            return {"ok": True, "state": node.state, "taskStatus": task.status}
        if run.state == NodeState.COMPLETED and node.key.startswith("worker_"):
            repeated = ImplementationResult.model_validate(result).model_dump(by_alias=True)
            repeated["checkpointSha"] = node.checkpoint_sha
            if repeated == run.result:
                return {"ok": True, "state": node.state, "taskStatus": task.status}
        if run.state not in ACTIVE_STATES:
            raise WorkflowError("Run is no longer active", 409)
        if node.state not in ACTIVE_STATES:
            raise WorkflowError(f"{node.label} is {node.state}; completion is only accepted while it is running", 409)
        # Refused before anything is recorded, so the still-running agent can correct and resubmit.
        rejection = self._review_loop_rejection(session, task, node, run, result)
        if rejection:
            raise WorkflowError(rejection, 400)
        if run.run_dir:
            run.result_path = str(TaskWorkspace.write_json(Path(run.run_dir) / "result.json", result))
            session.add(TaskArtifact(task_id=task.id, node_run_id=run.id, kind=f"{node.key}-result", path=run.result_path))
        run.result = result
        await self._collect_usage(session, task, run)
        try:
            if node.key == wf.ARCHITECT:
                await self._complete_discovery(session, task, node, run, result)
            elif node.key == wf.ORCHESTRATOR:
                await workers.accept_plan(self, session, task, node, run, result)
            elif node.key.startswith("worker_"):
                await workers.complete(self, session, task, node, run, result)
            elif node.key == wf.IMPLEMENTER:
                await self._complete_implementation(session, task, node, run, result)
            elif node.key in (wf.CODE_REVIEW, wf.SECURITY_REVIEW):
                await self._complete_review(session, task, node, run, result)
            elif node.key == wf.FINAL_ARCHITECT:
                await self._complete_final(session, task, node, run, result)
            else:
                raise WorkflowError(f"{node.label} does not accept completions", 409)
            error = None
        except (ValueError, GitError, ProcessError) as exc:
            run.state = NodeState.FAILED
            run.error = f"Result could not be accepted: {exc}"
            run.completed_at = _now()
            self._set_state(session, task, node, NodeState.FAILED, "Result validation failed", error=str(exc))
            # Commit the failure state rather than rolling it back with an exception.
            error = str(exc)
        response = {"ok": error is None, "state": node.state, "taskStatus": task.status}
        if error is not None:
            response["error"] = error
        return response

    # ------------------------------------------------------------------------------
    # completions per node
    # ------------------------------------------------------------------------------
    async def _collect_usage(self, session: Session, task: Task, run: NodeRun, *,
                             until: float | None = None) -> str:
        """Returns "collected", "pending", "failed" or "skipped". `until` (event-loop time) keeps
        polling a collector whose usage is not written yet; otherwise pending usage is collected
        again in the background."""
        config = self.config.harnesses.get(run.harness)
        if not config or not config.usage_command:
            return "skipped"
        evidence = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run.id))
        if evidence and evidence.usage:
            # A late exit callback must not replace accepted usage with totals from a
            # later run that has already resumed this provider conversation. Usage recorded
            # while the run's last turn was still open is replaced once the turn has ended.
            if evidence.usage.get("provenance") == "human-imported-provider-report":
                return "skipped"
            if run.state not in ACTIVE_STATES and not evidence.usage.get("partial"):
                return "skipped"
        agent = session.scalar(select(AgentSession).where(AgentSession.current_run_id == run.id))
        if agent and agent.harness_session_id and session.scalar(select(AgentSession.id).where(
                AgentSession.task_id == task.id, AgentSession.harness == agent.harness,
                AgentSession.harness_session_id == agent.harness_session_id, AgentSession.id > agent.id)):
            self._log(session, task, "Usage unavailable: this conversation has already advanced to another run", level="warning")
            return "skipped"
        # The collector may take up to 15s; don't sit on SQLite's writer lock (and the caller's
        # pending run/result writes) while waiting - see _complete_implementation's verification.
        session.commit()
        try:
            item = workers.work_item(session, run.node) if run.node.key.startswith("worker_") else None
            cwd = (Path(item.worktree_path) / (task.project_relative_path or "")) if item else self._harness_cwd(task)
            while True:
                try:
                    report = await collect_usage(config.usage_command, cwd, run.id,
                                                 agent.harness_session_id if agent else None)
                    break
                except UsagePending:
                    if until is None or asyncio.get_running_loop().time() >= until:
                        raise
                    await asyncio.sleep(0.5)
            report = self._run_usage(run, report)
        except UsagePending as exc:
            # The agent's own `flowctl complete` runs mid-turn, so the rest of that turn is
            # written after it: Copilot's credits appear only then, and Claude's transcript is
            # still growing. A background retry collects it once the turn has ended.
            partial = None
            if exc.report is not None:
                try:
                    partial = self._run_usage(run, exc.report)
                except (OSError, ValueError):
                    pass
            if partial is not None:
                self._record_usage(session, run, partial, partial=True)
            if run.id not in self._usage_retries:
                self._log(session, task, f"{run.harness} is still finishing this run's turn; "
                          + ("recorded its usage so far and " if partial else "")
                          + "collecting it again shortly")
                self._schedule_usage_retry(task.jira_key, run.id)
            return "pending"
        except (OSError, ValueError, asyncio.TimeoutError):
            self._log(session, task, "Provider usage unavailable; usage remains unknown", level="warning")
            return "failed"
        self._record_usage(session, run, report)
        return "collected"

    @staticmethod
    def _run_usage(run: NodeRun, report: UsageReport) -> UsageReport:
        if report.scope != "session":
            return report
        baseline_path = Path(run.run_dir) / "usage-baseline.json" if run.run_dir else None
        baseline = (json.loads(baseline_path.read_text(encoding="utf-8"))
                    if baseline_path and baseline_path.is_file() else None)
        return run_usage(report, baseline)

    @staticmethod
    def _record_usage(session: Session, run: NodeRun, report: UsageReport, *, partial: bool = False) -> None:
        evidence = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run.id))
        if evidence is None:
            evidence = RunEvidence(run_id=run.id)
            session.add(evidence)
        evidence.usage = {**report.record("configured-provider-collector"), **({"partial": True} if partial else {})}
        evidence.confirmed_model = report.model

    def _schedule_usage_retry(self, jira_key: str, run_id: int) -> None:
        retry = asyncio.ensure_future(self._retry_usage(jira_key, run_id))
        self._usage_retries[run_id] = retry
        retry.add_done_callback(lambda _t: self._usage_retries.pop(run_id, None))

    async def _retry_usage(self, jira_key: str, run_id: int) -> None:
        """Collect a run's pending usage again once its turn has had time to end."""
        try:
            for attempt, delay in enumerate(USAGE_RETRY_DELAYS, 1):
                await asyncio.sleep(delay)
                async with self._lock(jira_key):
                    with self.db.session() as session:
                        task = self._load_task(session, jira_key)
                        run = session.get(NodeRun, run_id)
                        if run is None:
                            return
                        status = await self._collect_usage(session, task, run)
                        if status == "pending" and attempt == len(USAGE_RETRY_DELAYS):
                            self._log(session, task, "Provider usage still incomplete; it is collected again "
                                      "when the agent session closes", level="warning")
                await self._publish(jira_key)
                if status != "pending":
                    return
        except WorkflowError:
            return  # the task was deleted meanwhile

    def _finish_run(self, run: NodeRun, state: NodeState, error: str | None = None) -> None:
        run.state = state
        run.error = error
        run.completed_at = _now()

    async def _complete_discovery(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun, raw: dict[str, Any]) -> None:
        result = DiscoveryResult.model_validate(raw)
        if result.status != "completed":
            self._finish_run(run, NodeState.FAILED, result.summary)
            self._set_state(session, task, node, NodeState.FAILED, f"Architect reported {result.status}", error=result.summary)
            return
        workspace = self.workspace(task)
        missing = [name for name in ("requirements.md", "acceptance-criteria.md", "implementation-plan.md")
                   if not workspace.read_text(f"requirements/{name}").strip()]
        if missing:
            self._finish_run(run, NodeState.FAILED, f"Missing deliverables: {', '.join(missing)}")
            self._set_state(session, task, node, NodeState.FAILED, "Deliverables missing", error=run.error)
            return
        if result.open_questions:
            workspace.write_json(workspace.requirements_dir / "questions.json", result.open_questions)
        self._finish_run(run, NodeState.COMPLETED)
        self._set_state(session, task, node, NodeState.COMPLETED, result.summary[:200] or "Discovery complete")
        gate = self._node(task, wf.REQUIREMENTS_GATE)
        self._set_state(session, task, gate, NodeState.WAITING_FOR_APPROVAL, "Review requirements and approve")

    async def _complete_implementation(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun, raw: dict[str, Any]) -> None:
        result = ImplementationResult.model_validate(raw)
        raw.clear()
        raw.update(result.model_dump(by_alias=True))
        if result.status != "completed":
            self._finish_run(run, NodeState.FAILED, result.summary)
            self._set_state(session, task, node, NodeState.FAILED, f"Implementer reported {result.status}", error=result.summary)
            return
        for item in workers.work_items(session, task):
            child = workers.node_for(task, item)
            if child.state != NodeState.COMPLETED or not child.checkpoint_sha:
                raise ValueError("All workers must complete before integration can finish")
            if not await self.git.is_ancestor(task.worktree_path, child.checkpoint_sha, "HEAD"):
                raise ValueError(f"Worker {item.plan_key} checkpoint has not been merged")
        try:
            sha, committed = await self.git.checkpoint(
                task.worktree_path,
                f"AgentFlow checkpoint: {task.jira_key} iteration {task.review_iteration}",
                commit=self.config.workflow.commit_checkpoints,
            )
        except GitError as exc:
            self._finish_run(run, NodeState.FAILED, str(exc))
            self._set_state(session, task, node, NodeState.FAILED, "Checkpoint failed", error=str(exc))
            return
        task.current_sha = sha
        evidence = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run.id))
        if evidence is None:
            evidence = RunEvidence(run_id=run.id)
            session.add(evidence)
        # Verification can run for minutes. Persist what is already decided (result, checkpoint
        # SHA, evidence row) and release SQLite's writer lock before waiting, so other tasks'
        # flowctl signals and UI actions aren't stalled behind this task's test run - the same
        # rule _start_agent_run applies before the launch lock. The task lock stays held.
        session.commit()
        verification = asyncio.create_task(assurance.verify_commands(
            self.config.workflow.verification_commands, self._harness_cwd(task), sha))
        self._verification_tasks[task.jira_key] = verification
        try:
            evidence.verification = await verification
        except asyncio.CancelledError:
            raise ValueError("Independent verification was cancelled") from None
        except (OSError, asyncio.TimeoutError) as exc:
            evidence.verification = {"sha": sha, "passed": False, "error": type(exc).__name__}
        finally:
            self._verification_tasks.pop(task.jira_key, None)
        if await self.git.head_sha(task.worktree_path) != sha or await self.git.is_dirty(task.worktree_path):
            raise ValueError("Verification changed the checkpoint; inspect changes and run completion again")
        node.checkpoint_sha = sha
        node.iteration = task.review_iteration
        raw["checkpointSha"] = sha
        run.result = dict(raw)
        if run.run_dir:
            TaskWorkspace.write_json(Path(run.run_dir) / "result.json", raw)
        self._finish_run(run, NodeState.COMPLETED)
        self._record_finding_responses(session, task, result)
        self._set_state(session, task, node, NodeState.COMPLETED, result.summary[:200] or "Implementation complete")
        self._log(session, task, f"Checkpoint SHA {sha[:10]}" + (" (committed)" if committed else " (HEAD, nothing to commit)"),
                  node_key=node.key, data={"sha": sha})
        for key in (wf.CODE_REVIEW, wf.SECURITY_REVIEW):
            review = self._node(task, key)
            review.iteration = task.review_iteration
            review.checkpoint_sha = sha
            review.message = None
            self._set_state(session, task, review, NodeState.READY)
        consolidate = self._node(task, wf.CONSOLIDATE)
        consolidate.iteration = task.review_iteration
        self._set_state(session, task, consolidate, NodeState.PENDING)
        for key in (wf.CODE_REVIEW, wf.SECURITY_REVIEW):
            await self._on_ready(session, task, self._node(task, key))

    async def _complete_review(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun, raw: dict[str, Any]) -> None:
        result = ReviewResult.model_validate(raw)
        expected = run.expected_sha or node.checkpoint_sha or task.current_sha or ""
        if result.status != "completed":
            self._finish_run(run, NodeState.FAILED, result.summary)
            self._set_state(session, task, node, NodeState.FAILED, f"Reviewer reported {result.status}", error=result.summary)
            return
        reported = (result.reviewed_sha or "").strip()
        if not expected or reported != expected:
            error = f"Review state mismatch: expected SHA {expected[:10]}, reviewer reported {reported[:10] or 'none'}. Rerun the review."
            self._finish_run(run, NodeState.FAILED, error)
            self._set_state(session, task, node, NodeState.FAILED, "Review state mismatch", error=error)
            return
        findings = self._reported_findings(run, result.findings)
        # The verdict is derived from blocking findings; a bare `changes_required` does not force another round.
        finding_type = "code-review" if node.key == wf.CODE_REVIEW else "security-review"
        prefix = "CODE" if node.key == wf.CODE_REVIEW else "SEC"
        _, prior_rows = self._previous_review(session, node, run.id)
        prior_by_id: dict[str, list[ReviewFinding]] = {}
        for prior_row in prior_rows or []:
            prior_by_id.setdefault(prior_row.finding_id, []).append(prior_row)
        # Ids are unique across the task, so an id always means the same issue; a re-raised
        # finding keeps its original id and counts how often it has come back.
        next_number = self._next_finding_number(session, task, finding_type, prefix)
        blocking = 0
        stored: list[dict[str, Any]] = []
        for finding in findings:
            prior = prior_by_id.get(finding.prior_id or "")
            if prior:
                fid, occurrence, prior_finding_id = finding.prior_id, max(r.occurrence for r in prior) + 1, prior[-1].id
            else:
                fid, occurrence, prior_finding_id = f"{prefix}-{next_number:03d}", 1, None
                next_number += 1
            is_blocking = finding.is_blocking()
            blocking += int(is_blocking)
            row = ReviewFinding(
                task_id=task.id, node_run_id=run.id, finding_id=fid, type=finding_type, iteration=task.review_iteration,
                reviewed_sha=expected, severity=finding.severity, blocking=is_blocking, file=finding.file, line=finding.line,
                end_line=finding.end_line if finding.line is not None and (finding.end_line or 0) > finding.line else None,
                title=finding.title or (finding.description or "")[:80] or fid, description=finding.description,
                recommendation=finding.recommendation, status="open", prior_finding_id=prior_finding_id, occurrence=occurrence,
            )
            session.add(row)
            stored.append({**finding.model_dump(by_alias=True, exclude_none=True), "id": fid, "type": finding_type,
                           "blocking": is_blocking, "occurrence": occurrence})
        # A not_fixed prior finding stays open: consolidation supersedes it with its re-raised successor.
        for ruling in result.prior_findings:
            for prior_row in prior_by_id.get(ruling.id, []):
                prior_row.resolution = ruling.evidence.strip()
                if ruling.status in ("fixed", "withdrawn"):
                    prior_row.status = "resolved" if ruling.status == "fixed" else "withdrawn"
                    prior_row.resolved_at = _now()
        verdict = "changes_required" if blocking else "approved"
        if run.run_dir:
            name = ("review" if node.key == wf.CODE_REVIEW else "security") + f"-{task.review_iteration:02d}.json"
            path = self.workspace(task).write_json(self.workspace(task).findings_dir / name, {
                "reviewedSha": expected, "verdict": verdict, "summary": result.summary, "findings": stored,
                "priorFindings": [ruling.model_dump(by_alias=True) for ruling in result.prior_findings],
            })
            session.add(TaskArtifact(task_id=task.id, node_run_id=run.id, kind=finding_type, path=str(path)))
        self._finish_run(run, NodeState.COMPLETED)
        self._set_state(session, task, node, NodeState.COMPLETED, f"{len(findings)} finding(s), {blocking} blocking — {verdict}")
        other_key = wf.SECURITY_REVIEW if node.key == wf.CODE_REVIEW else wf.CODE_REVIEW
        other = self._node(task, other_key)
        if other.state == NodeState.COMPLETED and other.iteration == task.review_iteration:
            consolidate = self._node(task, wf.CONSOLIDATE)
            self._set_state(session, task, consolidate, NodeState.READY)
            await self._on_ready(session, task, consolidate)

    async def _run_consolidate(self, session: Session, task: Task, node: WorkflowNode) -> None:
        self._set_state(session, task, node, NodeState.RUNNING)
        iteration = task.review_iteration
        findings = session.scalars(
            select(ReviewFinding).where(ReviewFinding.task_id == task.id, ReviewFinding.iteration == iteration)
        ).all()
        # Findings from the previous iteration are superseded by this review round.
        previous = session.scalars(
            select(ReviewFinding).where(ReviewFinding.task_id == task.id, ReviewFinding.iteration < iteration, ReviewFinding.status == "open")
        ).all()
        for finding in previous:
            finding.status = "superseded"
            finding.resolved_at = _now()
        blocking = [f for f in findings if f.blocking]
        workspace = self.workspace(task)
        package = {
            "iteration": iteration,
            "reviewedSha": task.current_sha,
            "blockingCount": len(blocking),
            "findings": [self._finding_dict(f) for f in findings],
        }
        path = workspace.write_json(workspace.findings_dir / f"consolidated-{iteration:02d}.json", package)
        session.add(TaskArtifact(task_id=task.id, kind="consolidated", path=str(path)))
        implementer = self._node(task, wf.IMPLEMENTER)
        if blocking:
            at_max = iteration >= task.max_review_iterations
            escalations = self._escalations(session, blocking)
            if at_max or escalations:
                summary = ", ".join(f.finding_id for f in blocking)
                reasons = [f"Maximum review iterations reached ({task.max_review_iterations})"] if at_max else []
                task.status = TaskStatus.NEEDS_HUMAN_REVIEW
                task.status_message = f"{'. '.join(reasons + escalations)}. Remaining blocking findings: {summary}"
                stop = "max iterations reached" if at_max else "escalated for a human decision"
                self._set_state(session, task, node, NodeState.COMPLETED, f"Iteration {iteration}: {len(blocking)} blocking, {stop}")
                self._log(session, task, task.status_message, level="error")
                implementer.iteration = iteration + 1
                self._set_state(session, task, implementer, NodeState.READY, "Human decision required before continuing")
                return
            task.review_iteration = iteration + 1
            implementer.iteration = task.review_iteration
            self._set_state(session, task, node, NodeState.COMPLETED, f"Iteration {iteration}: {len(blocking)} blocking finding(s) → Implementer fixes")
            self._set_state(session, task, implementer, NodeState.READY, f"Fix {len(findings)} finding(s) from iteration {iteration}")
            await self._on_ready(session, task, implementer)
            return
        for finding in findings:
            if finding.status == "open":
                finding.status = "accepted"
        self._set_state(session, task, node, NodeState.COMPLETED, f"Iteration {iteration}: no blocking findings — reviews passed")
        self._log(session, task, "Code Review passed and Security Review passed")
        final = self._node(task, wf.FINAL_ARCHITECT)
        final.checkpoint_sha = task.current_sha
        final.iteration = iteration
        self._set_state(session, task, final, NodeState.READY)
        await self._on_ready(session, task, final)

    # ------------------------------------------------------------------------------
    # review loop contract (see review_loop.py)
    # ------------------------------------------------------------------------------
    def _previous_review(self, session: Session, node: WorkflowNode, run_id: int) -> tuple[NodeRun | None, list[ReviewFinding] | None]:
        """The reviewer's own previous completed run and its findings (None when there is no
        follow-up). The follow-up prompt and the verdict check both use this, so they always agree
        on which findings need a verdict."""
        previous = session.scalar(select(NodeRun).where(
            NodeRun.node_id == node.id, NodeRun.state == NodeState.COMPLETED,
            NodeRun.id != run_id).order_by(NodeRun.id.desc()))
        if previous is None or not previous.expected_sha:
            return previous, None
        rows = session.scalars(select(ReviewFinding).where(ReviewFinding.node_run_id == previous.id)
                               .order_by(ReviewFinding.id)).all()
        return previous, list(rows)

    @staticmethod
    def _pending_findings(run: NodeRun) -> list[dict[str, Any]]:
        """Findings recorded incrementally with `flowctl finding` during this run."""
        pending_path = Path(run.run_dir) / "findings-pending.json" if run.run_dir else None
        return TaskWorkspace.read_json(pending_path) if pending_path and pending_path.exists() else []

    def _reported_findings(self, run: NodeRun, reported: list[Any]) -> list[Finding]:
        """The result's findings plus those recorded with `flowctl finding`, validated, with only
        identical duplicates removed (changed or conflicting evidence is retained). The contract
        check and storage both use this, so they always see the same findings. An item that fails
        validation is skipped here; validating the whole result reports it."""
        findings: list[Finding] = []
        for item in [*reported, *self._pending_findings(run)]:
            try:
                findings.append(item if isinstance(item, Finding) else Finding.model_validate(item))
            except ValueError:
                continue
        return list({f.model_dump_json(): f for f in findings}.values())

    def _fix_round_findings(self, task: Task) -> tuple[list[dict[str, Any]] | None, str | None]:
        """The consolidated findings (and their checkpoint) the implementer is fixing this round."""
        if task.review_iteration <= 1:
            return None, None
        workspace = self.workspace(task)
        consolidated = workspace.findings_dir / f"consolidated-{task.review_iteration - 1:02d}.json"
        if not consolidated.exists():
            return None, None
        package = TaskWorkspace.read_json(consolidated)
        return package.get("findings", []), package.get("reviewedSha")

    def _review_loop_rejection(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun,
                               raw: dict[str, Any]) -> str | None:
        if raw.get("status", "completed") != "completed":
            return None
        if node.key in (wf.CODE_REVIEW, wf.SECURITY_REVIEW):
            _, prior_rows = self._previous_review(session, node, run.id)
            prior_blocking = None if prior_rows is None else review_loop.blocking_ids(
                [{"id": r.finding_id, "blocking": r.blocking} for r in prior_rows])
            reported = raw.get("findings")
            reported = self._reported_findings(run, reported if isinstance(reported, list) else [])
            problems = review_loop.review_problems(raw, prior_blocking, reported)
            return review_loop.review_rejection(problems, prior_blocking) if problems else None
        if node.key == wf.IMPLEMENTER:
            given, _ = self._fix_round_findings(task)
            if given is None:
                return None
            problems = review_loop.response_problems(raw, given)
            return review_loop.response_rejection(problems, given) if problems else None
        return None

    def _record_finding_responses(self, session: Session, task: Task, result: ImplementationResult) -> None:
        given, _ = self._fix_round_findings(task)
        if not given or not result.finding_responses:
            return
        db_ids: dict[str, list[int]] = {}
        for finding in given:
            if finding.get("dbId") is not None:
                db_ids.setdefault(str(finding.get("id")), []).append(finding["dbId"])
        for response in result.finding_responses:
            for db_id in db_ids.get(response.id, []):
                row = session.get(ReviewFinding, db_id)
                if row is not None:
                    row.response = response.action
                    row.response_note = response.rationale.strip()

    @staticmethod
    def _next_finding_number(session: Session, task: Task, finding_type: str, prefix: str) -> int:
        used = session.scalars(select(ReviewFinding.finding_id).where(
            ReviewFinding.task_id == task.id, ReviewFinding.type == finding_type)).all()
        numbers = [int(match.group(1)) for fid in used if (match := re.fullmatch(rf"{prefix}-(\d+)", fid))]
        return max(numbers, default=0) + 1

    @staticmethod
    def _escalations(session: Session, blocking: list[ReviewFinding]) -> list[str]:
        """Blocking findings another round is unlikely to settle, so a human decides instead."""
        repeated = [f.finding_id for f in blocking if f.occurrence >= review_loop.REPEAT_ESCALATION]
        upheld = [f.finding_id for f in blocking if f.prior_finding_id is not None
                  and (prior := session.get(ReviewFinding, f.prior_finding_id)) is not None and prior.response == "disputed"]
        reasons = []
        if repeated:
            reasons.append(f"Raised {review_loop.REPEAT_ESCALATION} or more times without being fixed: {', '.join(repeated)}")
        if upheld:
            reasons.append(f"Disputed by the implementer and upheld by the reviewer: {', '.join(upheld)}")
        return reasons

    async def _complete_final(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun, raw: dict[str, Any]) -> None:
        assessment = FinalAssessment.model_validate(raw)
        if assessment.status != "completed":
            self._finish_run(run, NodeState.FAILED, assessment.task_summary)
            self._set_state(session, task, node, NodeState.FAILED, "Architect could not verify", error=assessment.task_summary)
            return
        workspace = self.workspace(task)
        data = assessment.model_dump(by_alias=True)
        data["finalSha"] = task.current_sha
        data["reviewIterations"] = task.review_iteration
        run.result = dict(data)
        path = workspace.write_json(workspace.final_dir / "assessment.json", data)
        session.add(TaskArtifact(task_id=task.id, node_run_id=run.id, kind="final-assessment", path=str(path)))
        if not (workspace.final_dir / "assessment.md").exists():
            (workspace.final_dir / "assessment.md").write_text(self._assessment_markdown(data), encoding="utf-8")
        self._finish_run(run, NodeState.COMPLETED)
        self._set_state(session, task, node, NodeState.COMPLETED, f"Assessment received. Risk {assessment.overall_risk}")
        self._log(session, task, f"Final assessment received. Risk: {assessment.overall_risk}")
        ready = self._node(task, wf.READY_FOR_PR)
        blockers = self._final_blockers(session, task)
        self._set_state(session, task, ready, NodeState.WAITING_FOR_APPROVAL,
                        "Blocked: " + "; ".join(blockers) if blockers else "Confirm the assessment to mark Ready for PR")

    def _finish_ready_for_pr(self, session: Session, task: Task, node: WorkflowNode) -> None:
        self._set_state(session, task, node, NodeState.COMPLETED, "Ready for PR")
        task.status = TaskStatus.READY_FOR_PR
        task.status_message = f"Ready for PR. Push branch {task.branch_name} and open the pull request."
        self._log(session, task, "Task is READY_FOR_PR")

    @staticmethod
    def _assessment_markdown(data: dict[str, Any]) -> str:
        lines = [f"# Final assessment", ""]
        for key, value in data.items():
            if key == "status":
                continue
            title = "".join(" " + c if c.isupper() else c for c in key).strip().capitalize()
            if isinstance(value, list):
                lines.append(f"## {title}")
                lines += [f"- {item}" for item in value] or ["- (none)"]
            else:
                lines.append(f"## {title}")
                lines.append(str(value if value not in (None, "") else "(none)"))
            lines.append("")
        return "\n".join(lines)

    # ------------------------------------------------------------------------------
    # execution modes
    # ------------------------------------------------------------------------------
    async def _on_ready(self, session: Session, task: Task, node: WorkflowNode) -> None:
        """A node just became READY. Automatic nodes start immediately; manual ones wait for the user."""
        if node.kind == NodeKind.SYSTEM and node.key == wf.CONSOLIDATE:
            await self._run_consolidate(session, task, node)
            return
        if node.kind == NodeKind.AGENT and node.execution_mode == ExecutionMode.AUTOMATIC:
            try:
                await self._start_agent_run(session, task, node)
            except WorkflowError as exc:
                run = session.scalar(select(NodeRun).where(NodeRun.node_id == node.id).order_by(NodeRun.id.desc()))
                if run is not None and run.state in ACTIVE_STATES:
                    self._finish_run(run, NodeState.FAILED, str(exc))
                self._set_state(session, task, node, NodeState.FAILED, "Automatic start failed", error=str(exc))

    # ------------------------------------------------------------------------------
    # launching agents
    # ------------------------------------------------------------------------------
    async def _start_agent_run(self, session: Session, task: Task, node: WorkflowNode,
                               *, metered_approval=None) -> NodeRun | None:
        # Persist the transition before waiting for the global launch lock. A caller
        # must not hold SQLite's writer lock while another launch needs to commit.
        session.commit()
        if node.key in (wf.ARCHITECT, wf.FINAL_ARCHITECT):
            # Outside _launch_lock: it is global, and a slow Jira fetch or ffmpeg run would
            # otherwise hold up every other task's launches.
            await self._sync_jira_context(session, task)
            session.commit()
        async with self._launch_lock:
            active = session.scalars(select(NodeRun).where(NodeRun.state.in_(list(ACTIVE_STATES) + [NodeState.INTERRUPTED]))).all()
            reason = None
            if len(active) >= self.config.workflow.max_concurrent_agents:
                reason = "Global agent capacity reached; start again when a slot is free"
            cfg = self.resolve_agent_config(task, node)
            billing = cfg["billingMode"]
            if billing != "subscription":
                reason = reason or launch_blocker(budget_status(session, task, self.config.workflow.cost_controls))
                if cfg["requiresMeteredApproval"]:
                    approved = (metered_approval and metered_approval.reason.strip()
                                and metered_approval.harness == cfg["harness"]
                                and metered_approval.model == cfg["model"])
                    if not approved:
                        reason = reason or "API launch needs approval: review the selected harness and model, and enter a reason"
            if reason:
                self._set_state(session, task, node, NodeState.READY, reason)
                self._log(session, task, reason, node_key=node.key)
                return None
            if metered_approval and billing != "subscription":
                self._log(session, task, "Human approved API launch", node_key=node.key,
                          data={"harness": cfg["harness"], "model": cfg["model"],
                                "reason": metered_approval.reason.strip()})
            result = await self._start_agent_run_unlocked(session, task, node)
            session.commit()
            return result

    async def _start_agent_run_unlocked(self, session: Session, task: Task, node: WorkflowNode) -> NodeRun:
        approval = None
        if node.key != wf.ARCHITECT:
            try:
                approval = assurance.check_requirements(session, task, self.workspace(task))
            except ValueError as exc:
                raise WorkflowError(str(exc), 409) from exc
        async with self._projects_lock:
            try:
                instruction_snapshot = instructions.active_snapshot(self.workspace(task).root)
                if instruction_snapshot is None:
                    instruction_snapshot = instructions.capture(
                        self.workspace(task).root, Path(task.worktree_path),
                        await self.git.head_sha(task.worktree_path))
            except (instructions.InstructionError, OSError, GitError) as exc:
                raise WorkflowError(str(exc), 409) from exc
        item = workers.work_item(session, node) if node.key.startswith("worker_") else None
        if item is not None:
            try:
                await workers.prepare(self, session, task, node, item)
                await self._stop_sessions(session, task, node.role, node=node)
            except (ValueError, GitError, ProcessError) as exc:
                raise WorkflowError(str(exc), 409) from exc
        role = Role(node.role)
        cfg = self.resolve_agent_config(task, node)
        harness_key = cfg["harness"]
        if not self.harnesses.is_usable(harness_key):
            caps = self.harnesses.capabilities.get(harness_key)
            reason = caps.message if caps and caps.message else "not enabled"
            raise WorkflowError(f"Harness '{harness_key}' is unavailable ({reason}). Choose another harness in the node configuration.", 409)
        adapter = self.harnesses.get(harness_key)

        try:
            await self.zellij.ensure_session(task.zellij_session_name)
        except (ZellijError, ProcessError) as exc:
            raise WorkflowError(str(exc), 409) from exc

        # Settle any earlier process before recording a run, so a refusal here leaves no
        # orphaned RUNNING run behind. A fresh process gets only this run's credential and
        # the requested settings. Resume by exact conversation identity below, never by a
        # shared cwd's latest session.
        if await self._live_session(session, task, role, node=node) is not None:
            await self._stop_sessions(session, task, role, node=node)

        previous_runs = session.scalars(select(NodeRun).where(NodeRun.node_id == node.id)).all()
        run_number = len(previous_runs) + 1
        # An implementer's HEAD at launch goes into the run's config.json, so a later retry can list
        # what an unfinished attempt committed. Best-effort: an unreadable worktree never blocks a launch.
        worktree = Path(item.worktree_path if item else task.worktree_path) if role == Role.IMPLEMENTER else None
        head_now = None
        if worktree is not None:
            try:
                head_now = await self.git.head_sha(worktree)
            except (GitError, ProcessError, OSError):
                head_now = None
        attempt = await self._previous_attempt(session, task, role, previous_runs, worktree, head_now,
                                               item.base_sha if item else None)
        workspace = self.workspace(task)
        # Run directories are per role so the architect's runs (discovery + final) live together.
        role_runs = session.scalars(select(NodeRun).join(WorkflowNode).where(NodeRun.task_id == task.id, WorkflowNode.role == node.role)).all()
        run_dir = workspace.run_dir(ROLE_RUN_DIRS[role], len(role_runs) + 1)

        run = NodeRun(
            task_id=task.id, node_id=node.id, run_number=run_number, iteration=task.review_iteration,
            state=NodeState.RUNNING, harness=harness_key, model=cfg["model"], reasoning=cfg["reasoning"],
            run_dir=str(run_dir), expected_sha=node.checkpoint_sha,
        )
        session.add(run)
        session.flush()
        session.add(RunEvidence(run_id=run.id, requirements_digest=approval.digest if approval else None,
                                billing_mode=self.config.harnesses[harness_key].billing_mode))
        token = secrets.token_urlsafe(32)
        session.add(AgentAccess(task_id=task.id, run_id=run.id, role=role, token_hash=digest(token)))
        credential_path = run_dir / "credential.json"
        TaskWorkspace.write_credential(credential_path, {"token": token, "runId": run.id})
        session.commit()

        prompt_text = self._build_prompt(session, task, node, run, run_dir)
        # Composed untouched: it quotes agent-written text, which must not get flowctl run ids inserted.
        attempt_text = "\n" + prompts.previous_attempt(attempt) if attempt is not None else ""
        # Which Jira sync this Architect is shown; recorded only once it has actually launched,
        # so the next prompt's "new since your last run" is measured from what it really saw.
        jira_generation = (jira_manifest_generation(workspace.context_dir)
                           if node.key in (wf.ARCHITECT, wf.FINAL_ARCHITECT) else None)
        prompt_text = instructions.compose(prompt_text, instruction_snapshot, role, run.id, appendix=attempt_text)
        prompt_path = run_dir / "prompt.md"
        try:
            if instruction_snapshot['files'][role]['content']:
                instructions.atomic_text(prompt_path, prompt_text)
            else:
                prompt_path.write_text(prompt_text, encoding="utf-8")
            instructions.record_run(run_dir, instruction_snapshot, role)
        except (instructions.InstructionError, OSError) as exc:
            self._finish_run(run, NodeState.FAILED, str(exc))
            self._set_state(session, task, node, NodeState.FAILED, 'Could not persist agent briefing', error=str(exc))
            session.commit()
            raise WorkflowError(f'Could not persist agent briefing: {exc}', 409) from exc
        run.prompt_path = str(prompt_path)
        TaskWorkspace.write_json(run_dir / "config.json", {"harness": harness_key, "model": cfg["model"], "reasoning": cfg["reasoning"],
                                                          "node": node.key, "role": role, "iteration": task.review_iteration, "expectedSha": node.checkpoint_sha,
                                                          "startSha": head_now})
        session.add(TaskArtifact(task_id=task.id, node_run_id=run.id, kind=f"{node.key}-prompt", path=str(prompt_path)))

        # The run id is typed too: a resumed conversation that won't (or can't) read the new
        # briefing would otherwise keep signalling with a run id remembered from its history.
        reference = (f"{adapter.prompt_reference(prompt_path)} This is AgentFlow run {run.id}: use --run {run.id} "
                     "on every flowctl command. The briefing and the task files it lists are outside the "
                     "worktree; reading them is expected.")
        cwd = (Path(item.worktree_path) / (task.project_relative_path or "")) if item else self._harness_cwd(task)
        tab_name = self._tab_name(node)

        launch = None
        previous_session = session.scalar(
            select(AgentSession).outerjoin(NodeRun, AgentSession.current_run_id == NodeRun.id).where(
                AgentSession.task_id == task.id, AgentSession.role == role, AgentSession.harness == harness_key,
                or_(NodeRun.node_id.in_(self._session_node_ids(task, node)),
                    AgentSession.current_run_id.is_(None) if item is None else False))
            .order_by(AgentSession.id.desc())
        )
        # A resumed launch that itself crashed (harness exited before completing) means the
        # underlying CLI session is broken, not just interrupted - retrying the same resume
        # would just repeat the crash forever. Fall through to a fresh launch instead.
        previous_run = session.get(NodeRun, previous_session.current_run_id) if previous_session and previous_session.current_run_id else None
        resume_is_broken = previous_run is not None and (previous_run.error or "").startswith("Harness exited with code")
        # Reviewers start every review round in a fresh conversation. The follow-up briefing carries
        # everything they need (their prior findings, the implementer's responses, the delta), and an
        # old conversation anchors them, either toward waving a fix through or toward repeating an
        # earlier objection. A retry within the same round may still resume.
        new_review_round = (role in (Role.CODE_REVIEWER, Role.SECURITY_REVIEWER)
                            and (previous_run is None or previous_run.iteration != task.review_iteration))
        if previous_session is not None and new_review_round:
            self._log(session, task, f"Starting {tab_name} in a fresh conversation for review round {task.review_iteration}",
                      node_key=node.key)
        fresh_instructions = instructions.needs_fresh(
            instruction_snapshot, previous_run.run_dir if previous_run else None)
        if previous_session is not None and not resume_is_broken and item is None and not new_review_round and not fresh_instructions:
            handle = SessionHandle(role=role, zellij_session=task.zellij_session_name, tab_name=tab_name,
                                   pane_id=previous_session.zellij_pane_id, cwd=cwd, harness_session_id=previous_session.harness_session_id)
            launch = adapter.build_resume(handle, model=cfg["model"], reasoning=cfg["reasoning"], initial_prompt=reference)
            if launch is not None:
                self._log(session, task, f"Original {tab_name} process is gone; resuming its conversation", node_key=node.key, level="warning")
        resumed = launch is not None
        if launch is None:
            if resume_is_broken:
                self._log(session, task, f"Previous {tab_name} resume attempt crashed; starting a fresh run instead", node_key=node.key, level="warning")
            launch = adapter.build_launch(model=cfg["model"], reasoning=cfg["reasoning"], initial_prompt=reference)
        baseline = await prepare_baseline(self.config.harnesses[harness_key].usage_command,
                                          cwd, run.id, launch.harness_session_id, resumed)
        TaskWorkspace.write_json(run_dir / "usage-baseline.json", baseline)
        adapter.prepare_workspace(cwd, repository_root=Path(task.repository_root))
        script = self._write_launcher(run_dir, launch, cwd, task, role, run.id,
                                      worktree_path=item.worktree_path if item else None)
        shell = self.zellij.shell_command()
        if sys.platform == "win32":
            command = [*shell, "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(script)]
        else:
            command = [*shell, str(script)]
        new_session = AgentSession(
            task_id=task.id, role=role, harness=harness_key, model=cfg["model"], reasoning=cfg["reasoning"],
            zellij_tab_name=tab_name, harness_session_id=launch.harness_session_id,
            status="running", current_run_id=run.id,
        )
        session.add(new_session)
        session.flush()
        session.add(ProcessLease(session_id=new_session.id, launch_run_id=run.id))
        # Persist intent for every role before opening an external terminal.
        self._set_state(session, task, node, NodeState.RUNNING, "Launching agent")
        session.commit()
        launch_started_at = _now()
        try:
            tab_id, pane_id = await self.zellij.new_tab(task.zellij_session_name, tab_name, cwd, command)
        except (ZellijError, ProcessError) as exc:
            self._finish_run(run, NodeState.FAILED, str(exc))
            self._set_state(session, task, node, NodeState.FAILED, "Agent launch failed", error=str(exc))
            session.commit()
            raise WorkflowError(f"Could not open zellij tab: {exc}", 409) from exc
        new_session.zellij_tab_id = tab_id
        new_session.zellij_pane_id = pane_id
        if jira_generation is not None:
            mark_jira_prompted(workspace.context_dir, jira_generation)
        self._ensure_screen_tracker(task, new_session)
        if new_session.harness_session_id is None:
            # Harnesses like Codex assign their own session id with no way to choose one
            # upfront; best-effort discover it now so a later run of this role can resume.
            new_session.harness_session_id = await adapter.capture_session_id(cwd, launch_started_at)
        self._log(session, task, f"{node.label} started: {harness_key} in zellij tab '{tab_name}' (cwd {cwd})", node_key=node.key,
                  data={"harness": harness_key, "requestedModel": cfg["model"]})
        node.iteration = task.review_iteration if node.key != wf.ARCHITECT else node.iteration
        self._set_state(session, task, node, NodeState.RUNNING, f"Run #{run_number} ({harness_key})")
        return run

    @staticmethod
    def _session_node_ids(task: Task, node: WorkflowNode) -> list[int]:
        return [n.id for n in task.nodes if n.role == Role.ARCHITECT] if node.role == Role.ARCHITECT else [node.id]

    @staticmethod
    def _tab_name(node: WorkflowNode) -> str:
        return f"Worker {node.key[7:]}" if node.key.startswith("worker_") else ROLE_LABELS[Role(node.role)]

    async def _live_session(self, session: Session, task: Task, role: str,
                            *, node: WorkflowNode | None = None) -> AgentSession | None:
        """The running AgentSession for a role, verified against zellij's pane list."""
        query = select(AgentSession).where(AgentSession.task_id == task.id,
                                          AgentSession.role == role, AgentSession.status == "running")
        if node is not None:
            query = query.join(NodeRun, AgentSession.current_run_id == NodeRun.id).where(
                NodeRun.node_id.in_(self._session_node_ids(task, node)))
        agent_session = session.scalar(query.order_by(AgentSession.id.desc()))
        if agent_session is None:
            return None
        pane = None
        try:
            if await self.zellij.session_exists(task.zellij_session_name):
                if agent_session.zellij_tab_id is None or agent_session.zellij_tab_id < 0:
                    # A launch whose new-tab call failed or timed out: the tab may still exist.
                    match = self._untracked_tab(session, task, agent_session,
                                                await self.zellij.list_panes(task.zellij_session_name))
                    if match is not None:
                        agent_session.zellij_tab_id = match.tab_id
                        agent_session.zellij_pane_id = match.pane_id
                elif not agent_session.zellij_pane_id:
                    agent_session.zellij_pane_id = await self.zellij.pane_for_tab(
                        task.zellij_session_name, agent_session.zellij_tab_id)
                    if not agent_session.zellij_pane_id:
                        raise ZellijError("its tab has no pane")
                if agent_session.zellij_pane_id:
                    pane = await self.zellij.pane_state(task.zellij_session_name, agent_session.zellij_pane_id)
        except (ZellijError, ProcessError) as exc:
            # Never mistake an unverifiable launch for a dead one.
            raise WorkflowError(f"Cannot establish the existing agent's pane identity ({exc}); stop it before retrying", 409) from exc
        if pane is None or pane.exited:
            agent_session.status = "exited"
            agent_session.ended_at = _now()
            self.screen_tracker.cancel_nowait(agent_session.id)
            return None
        return agent_session

    @staticmethod
    def _untracked_tab(session: Session, task: Task, agent: AgentSession, panes: list) -> Any | None:
        """The live pane in a tab named like `agent`'s that no other running session tracks.

        Used when a launch recorded its session but never learned the tab identity (the new-tab
        call failed or timed out). One match is that launch's tab; none proves nothing is running.
        """
        tracked = set(session.scalars(select(AgentSession.zellij_tab_id).where(
            AgentSession.task_id == task.id, AgentSession.status == "running", AgentSession.id != agent.id)))
        matches = [p for p in panes if p.tab_name == agent.zellij_tab_name and not p.exited and p.tab_id not in tracked]
        if len(matches) > 1:
            raise ZellijError(f"{agent.zellij_tab_name} tab identity is ambiguous; inspect Zellij before retrying")
        return matches[0] if matches else None

    def _write_launcher(self, run_dir: Path, launch: LaunchSpec, cwd: Path, task: Task, role: str, run_id: int,
                        *, worktree_path: str | None = None) -> Path:
        env = {
            **launch.env,
            "AGENTFLOW_API": self.api_base_url,
            "AGENTFLOW_TASK": task.jira_key,
            "AGENTFLOW_AGENT": role,
            "AGENTFLOW_RUN": str(run_id),
            "AGENTFLOW_CREDENTIAL_FILE": str(run_dir / "credential.json"),
            "AGENTFLOW_TASK_DIR": task.task_dir,
            "AGENTFLOW_WORKTREE": worktree_path or task.worktree_path,
            # A harness's own shell tool doesn't reliably inherit the PATH prepend below (confirmed:
            # Claude Code's Bash tool on Windows can start with a PATH that never saw this launcher's
            # $env:PATH change at all) - so flowctl must also be reachable by a plain env var lookup,
            # independent of PATH resolution in whatever shell the harness actually runs commands in.
            "AGENTFLOW_FLOWCTL": (self.bin_dir / "flowctl").as_posix(),
        }
        python = Path(sys.executable)
        if sys.platform == "win32":
            lines = ["# Generated by AgentFlow - launches one agent run inside its zellij tab"]
            allowed = ', '.join(_ps_quote(key) for key in sorted(SAFE_ENV_NAMES))
            lines.append("Get-ChildItem Env: | Where-Object { $_.Name.ToUpperInvariant() -notin @(" + allowed
                         + ") } | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }")
            for key, value in env.items():
                lines.append(f"$env:{key} = {_ps_quote(value)}")
            lines.append(f"$env:PATH = {_ps_quote(str(self.bin_dir))} + ';' + {_ps_quote(str(python.parent))} + ';' + $env:PATH")
            lines.append(f"Set-Location -LiteralPath {_ps_quote(str(cwd))}")
            lines.append(f"Write-Host 'AgentFlow: {task.jira_key} / {role} / run {run_id}' -ForegroundColor Cyan")
            argv = " ".join(_ps_quote(a) for a in launch.argv[1:])
            lines.append(f"& {_ps_quote(launch.argv[0])} {argv}".rstrip())
            lines.append("$code = $LASTEXITCODE")
            lines.append("if ($null -eq $code) { $code = 0 }")
            lines.append(f"& {_ps_quote(str(python))} -m agentflow.cli.flowctl exited --code $code | Out-Null")
            lines.append("exit $code")
            script = run_dir / "launch.ps1"
            script.write_text("\r\n".join(lines) + "\r\n", encoding="utf-8-sig")
        else:
            lines = ["#!/bin/sh", "# Generated by AgentFlow - launches one agent run inside its zellij tab"]
            for key, value in env.items():
                lines.append(f"export {key}={shlex.quote(value)}")
            lines.append(f"export PATH={shlex.quote(str(self.bin_dir))}:{shlex.quote(str(python.parent))}:$PATH")
            lines.append(f"cd {shlex.quote(str(cwd))} || exit 1")
            lines.append(f"echo 'AgentFlow: {task.jira_key} / {role} / run {run_id}'")
            names = sorted((SAFE_ENV_NAMES | set(env)) - {"PROGRAMFILES(X86)"})
            assignments = " ".join(f'{key}="${{{key}-}}"' for key in names)
            lines.append("env -i " + assignments + " " + " ".join(shlex.quote(a) for a in launch.argv))
            lines.append("code=$?")
            lines.append(f"{shlex.quote(str(python))} -m agentflow.cli.flowctl exited --code \"$code\" >/dev/null")
            lines.append('exit "$code"')
            script = run_dir / "launch.sh"
            script.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
            script.chmod(0o700)
        return script

    async def _previous_attempt(self, session: Session, task: Task, role: str, previous_runs: list[NodeRun],
                                worktree: Path | None, head: str | None, base_sha: str | None) -> dict[str, Any] | None:
        """The unfinished attempts at this stage in the current review round (the node's trailing FAILED
        runs) for a retry's briefing (prompts.previous_attempt); None when the latest earlier run did not
        fail. `worktree` is given for implementers only: their committed and uncommitted work is listed,
        measured from where the first unfinished attempt started (`base_sha` when it recorded nothing)."""
        unfinished = []
        for run in sorted(previous_runs, key=lambda r: r.id, reverse=True):
            if run.state != NodeState.FAILED or run.iteration != task.review_iteration:
                break
            unfinished.append(run)
        if not unfinished:
            return None
        latest, first = unfinished[0], unfinished[-1]
        numbers = {run.id: run.run_number for run in unfinished}
        launched = latest.exit_code is not None or session.scalar(select(AgentSession.id).where(
            AgentSession.current_run_id == latest.id, AgentSession.zellij_tab_id.is_not(None))) is not None
        heartbeat = session.scalar(select(RunHeartbeat).where(RunHeartbeat.run_id == latest.id))
        questions = session.scalars(select(AgentQuestion).where(
            AgentQuestion.node_run_id.in_(list(numbers)), AgentQuestion.answered.is_(True))
            .order_by(AgentQuestion.id)).all()
        attempt: dict[str, Any] = {
            "unfinishedAttempts": len(unfinished), "runNumber": latest.run_number,
            "harness": latest.harness, "model": latest.model, "startedAt": _attempt_time(latest.started_at),
            "endedAt": _attempt_time(latest.completed_at), "launched": launched,
            "error": latest.error, "exitCode": latest.exit_code,
            "heartbeat": {"phase": heartbeat.phase, "receivedAt": _attempt_time(heartbeat.received_at)} if heartbeat else None,
            "role": str(role),
            "answers": [{"runNumber": numbers[q.node_run_id], "question": q.message, "answer": q.answer or ""}
                        for q in questions[-10:]],
            "worktree": None,
        }
        if worktree is not None:
            # Each listing that git cannot produce is None and reported on its own; neither blocks the launch.
            start = _recorded_start_sha(first.run_dir) or base_sha
            committed: list[str] | None = []
            if head is not None and start and start != head:
                try:
                    committed = await self.git.changed_paths(str(worktree), start, head)
                except (GitError, ProcessError, OSError):
                    committed = None
            try:
                uncommitted: list[str] | None = await self.git.dirty_paths(worktree)
            except (GitError, ProcessError, OSError):
                uncommitted = None
            attempt["worktree"] = {"path": str(worktree), "startSha": start, "headSha": head,
                                   "committed": committed, "uncommitted": uncommitted}
        return attempt

    def _build_prompt(self, session: Session, task: Task, node: WorkflowNode, run: NodeRun, run_dir: Path) -> str:
        workspace = self.workspace(task)
        item = workers.work_item(session, node) if node.key.startswith("worker_") else None
        root = Path(item.worktree_path if item else task.worktree_path)
        ctx = prompts.PromptContext(
            jira_key=task.jira_key, worktree_root=root,
            project_dir=(root / task.project_relative_path) if task.project_relative_path else None,
            task_dir=workspace.root, run_dir=run_dir, run_id=run.id, role=node.role or "", base_branch=task.base_branch,
            branch_name=item.branch_name if item else task.branch_name, additional_context=task.additional_context or "", iteration=task.review_iteration,
            max_iterations=task.max_review_iterations, flowctl_path=(self.bin_dir / "flowctl").as_posix(),
        )
        requirements = workspace.requirement_files()
        if node.key == wf.ORCHESTRATOR:
            return worker_prompts.orchestrator(ctx)
        items = workers.work_items(session, task)
        if item:
            results = workers.completed_results(session, task, items)
            return worker_prompts.worker(ctx, item, [r for r in results if r["key"] in item.dependencies])
        if node.key == wf.ARCHITECT:
            if node.iteration > 0:
                feedback_path = workspace.requirements_dir / f"feedback-{node.iteration:02d}.md"
                feedback_text = feedback_path.read_text(encoding="utf-8") if feedback_path.exists() else ""
                return prompts.architect_revision(ctx, feedback_text, node.iteration)
            return prompts.architect_discovery(ctx)
        if node.key == wf.IMPLEMENTER:
            if items and task.review_iteration == 1:
                planner = self._node(task, wf.ORCHESTRATOR)
                plan_run = session.scalar(select(NodeRun).where(NodeRun.node_id == planner.id).order_by(NodeRun.id.desc()))
                summary = (plan_run.result or {}).get("summary", "") if plan_run else ""
                return worker_prompts.integration(ctx, workers.completed_results(session, task, items), summary)
            findings, previous_sha = self._fix_round_findings(task)
            return prompts.implementer(ctx, findings=findings, requirements=requirements, previous_sha=previous_sha)
        if node.key in (wf.CODE_REVIEW, wf.SECURITY_REVIEW):
            sha = node.checkpoint_sha or task.current_sha or ""
            impl_run = session.scalar(
                select(NodeRun).join(WorkflowNode).where(NodeRun.task_id == task.id, WorkflowNode.key == wf.IMPLEMENTER,
                                                         NodeRun.state == NodeState.COMPLETED).order_by(NodeRun.id.desc())
            )
            summary = (impl_run.result or {}).get("summary", "") if impl_run else ""
            # Only AgentFlow's own verification of exactly this checkpoint counts as evidence.
            evidence = session.scalar(select(RunEvidence).where(RunEvidence.run_id == impl_run.id)) if impl_run else None
            verification = evidence.verification if evidence and evidence.verification else None
            if verification and verification.get("sha") != sha:
                verification = None
            fn = prompts.code_review if node.key == wf.CODE_REVIEW else prompts.security_review
            previous, rows = self._previous_review(session, node, run.id)
            ctx.previous_review_sha = previous.expected_sha if previous else None
            previous_findings = None if rows is None else [self._finding_dict(f) for f in rows]
            return fn(ctx, sha=sha, implementation_summary=summary, requirements=requirements,
                      previous_findings=previous_findings, verification=verification)
        if node.key == wf.FINAL_ARCHITECT:
            return prompts.final_architect(ctx, final_sha=task.current_sha or "", history=self._history(session, task))
        raise WorkflowError(f"No prompt for node {node.key}")

    def _history(self, session: Session, task: Task) -> dict[str, Any]:
        runs = session.scalars(
            select(NodeRun).join(WorkflowNode).where(NodeRun.task_id == task.id, WorkflowNode.key.in_([wf.IMPLEMENTER, wf.CODE_REVIEW, wf.SECURITY_REVIEW]))
            .order_by(NodeRun.id)
        ).all()
        findings = session.scalars(select(ReviewFinding).where(ReviewFinding.task_id == task.id).order_by(ReviewFinding.id)).all()
        return {
            "iterations": task.review_iteration,
            "finalSha": task.current_sha,
            "runs": [
                {"node": r.node.key, "iteration": r.iteration, "state": r.state, "harness": r.harness,
                 "summary": (r.result or {}).get("summary"), "filesChanged": (r.result or {}).get("filesChanged"),
                 "tests": (r.result or {}).get("tests"), "knownIssues": (r.result or {}).get("knownIssues"),
                 "verdict": (r.result or {}).get("verdict"), "reviewedSha": (r.result or {}).get("reviewedSha")}
                for r in runs
            ],
            "findings": [self._history_finding(f) for f in findings],
        }

    # ------------------------------------------------------------------------------
    # snapshots for the UI
    # ------------------------------------------------------------------------------
    @staticmethod
    def _history_finding(f: ReviewFinding) -> dict[str, Any]:
        if f.status == "superseded":
            return {"id": f.finding_id, "iteration": f.iteration, "file": f.file, "line": f.line, "title": f.title, "status": f.status}
        if f.status in ("resolved", "withdrawn"):
            closed = {"id": f.finding_id, "iteration": f.iteration, "severity": f.severity, "blocking": f.blocking,
                      "file": f.file, "line": f.line, "title": f.title, "status": f.status, "resolution": f.resolution}
            if f.response:
                closed["implementerResponse"] = {"action": f.response, "rationale": f.response_note}
            return closed
        return {
            "id": f.finding_id, "type": f.type, "iteration": f.iteration, "severity": f.severity, "blocking": f.blocking,
            "file": f.file, "line": f.line, "title": f.title, "description": f.description,
            "recommendation": f.recommendation, "status": f.status,
        }

    @staticmethod
    def _finding_dict(f: ReviewFinding) -> dict[str, Any]:
        return {
            "id": f.finding_id, "dbId": f.id, "type": f.type, "iteration": f.iteration, "reviewedSha": f.reviewed_sha,
            "severity": f.severity, "blocking": f.blocking, "file": f.file, "line": f.line, "endLine": f.end_line,
            "title": f.title, "description": f.description, "recommendation": f.recommendation, "status": f.status,
            "occurrence": f.occurrence, "resolution": f.resolution, "response": f.response, "responseNote": f.response_note,
            "createdAt": _iso(f.created_at), "resolvedAt": _iso(f.resolved_at),
        }

    @staticmethod
    def _usage_summary(node_runs: list[NodeRun], evidence_by_run: dict[int, RunEvidence]) -> dict[str, Any] | None:
        if not node_runs:
            return None
        reports = [evidence_by_run[r.id].usage for r in node_runs
                   if r.id in evidence_by_run and evidence_by_run[r.id].usage is not None]
        costs = [r["costUsd"] for r in reports if r["costUsd"] is not None]
        modes = {r.id: evidence_by_run[r.id].billing_mode if r.id in evidence_by_run else "unknown"
                 for r in node_runs}
        metered = [r for r in node_runs if modes[r.id] == "metered"]
        api_costs = [evidence_by_run[r.id].usage["costUsd"] for r in metered
                     if r.id in evidence_by_run and evidence_by_run[r.id].usage
                     and evidence_by_run[r.id].usage.get("costUsd") is not None
                     and _cost_kind(evidence_by_run[r.id].usage) == "billed"]
        # Visibility only: what every run consumed, whatever the billing mode. Estimates are
        # list-price guesses and are never added to provider-billed amounts.
        billed = [r["costUsd"] for r in reports if r["costUsd"] is not None and _cost_kind(r) == "billed"]
        estimated = [r["costUsd"] for r in reports if r["costUsd"] is not None and _cost_kind(r) == "estimated"]
        # Cost-only reports (Copilot) carry no token counts; token totals cover the other runs.
        tokened = [r for r in reports if r.get("totalTokens") is not None]
        return {
            "runs": len(node_runs), "runsWithUsage": len(reports), "runsWithTokens": len(tokened),
            "runsWithCost": len(costs), "meteredRuns": len(metered),
            "meteredRunsWithCost": len(api_costs),
            "subscriptionRuns": sum(mode == "subscription" for mode in modes.values()),
            "unknownBillingRuns": sum(mode == "unknown" for mode in modes.values()),
            "apiCostUsd": sum(api_costs) if api_costs else (0 if not metered else None),
            "inputTokens": sum(r["inputTokens"] for r in tokened),
            "outputTokens": sum(r["outputTokens"] for r in tokened),
            "cachedInputTokens": sum(r["cachedInputTokens"] for r in tokened),
            "totalTokens": sum(r["totalTokens"] for r in tokened),
            "costUsd": sum(costs) if costs else None,
            "runsWithBilledCost": len(billed), "billedCostUsd": sum(billed) if billed else None,
            "runsWithEstimatedCost": len(estimated), "estimatedCostUsd": sum(estimated) if estimated else None,
        }

    async def list_tasks(self, archived: Literal["exclude", "include", "only"] = "exclude") -> list[dict[str, Any]]:
        query = select(Task).options(selectinload(Task.nodes)).order_by(Task.created_at.desc())
        if archived == "exclude":
            query = query.where(Task.archived_at.is_(None))
        elif archived == "only":
            query = query.where(Task.archived_at.is_not(None))
        with self.db.session() as session:
            tasks = session.scalars(query).all()
            runs = session.scalars(select(NodeRun)).all()
            questions = session.scalars(select(AgentQuestion).where(AgentQuestion.answered.is_(False))).all()
            evidence = {e.run_id: e for e in session.scalars(select(RunEvidence)).all()}
            work_items = session.scalars(select(ImplementationWorkItem)).all()

        missing = [t for t in tasks if not t.jira_title and not is_local_task(t.jira_key)]
        jira_service = build_jira_service(self.config.jira) if missing else None
        if jira_service:
            fetched = await asyncio.gather(
                *(self._fetch_jira_title(jira_service, t.jira_key) for t in missing),
                return_exceptions=True,
            )
            updates = {t.id: title for t, title in zip(missing, fetched) if isinstance(title, str) and title}
            if updates:
                with self.db.session() as session:
                    for task_id, title in updates.items():
                        db_task = session.get(Task, task_id)
                        if db_task:
                            db_task.jira_title = title
                for t in missing:
                    if t.id in updates:
                        t.jira_title = updates[t.id]

        return [{**self._task_summary(t),
                 "attention": attention_items(t, [r for r in runs if r.task_id == t.id],
                                              [q for q in questions if q.task_id == t.id]),
                 "totalUsage": self._usage_summary([r for r in runs if r.task_id == t.id], evidence),
                 **self._worktree_state(t, [i for i in work_items if i.task_id == t.id])}
                for t in tasks]

    @staticmethod
    async def _fetch_jira_title(jira_service, jira_key: str) -> str | None:
        try:
            issue = await jira_service.fetch_issue(jira_key)
        except JiraError:
            return None
        return (issue.get("fields", {}) or {}).get("summary") or None

    @staticmethod
    def _worktree_state(task: Task, items) -> dict[str, Any]:
        """Plain filesystem stats, not git calls - cheap, but only worth doing once the task is
        Merged (the only status where the UI offers to clean the worktree up)."""
        if task.status != TaskStatus.MERGED:
            return {"worktreeExists": None, "workerWorktreeCount": None}
        return {"worktreeExists": Path(task.worktree_path).exists(),
                "workerWorktreeCount": sum(1 for item in items if Path(item.worktree_path).exists())}

    @staticmethod
    def _question_dict(question: AgentQuestion, runs_by_id: dict[int, NodeRun], actionable: bool) -> dict[str, Any]:
        return {
            "uid": question.uid, "role": question.role, "message": question.message, "choices": question.choices,
            "decision": question.decision, "answered": question.answered, "answer": question.answer,
            "createdAt": _iso(question.created_at), "answeredAt": _iso(question.answered_at),
            "runId": question.node_run_id,
            "nodeKey": runs_by_id[question.node_run_id].node.key if question.node_run_id in runs_by_id else None,
            "actionable": actionable,
        }

    def open_questions(self) -> list[dict[str, Any]]:
        """Every question a human can answer now (the snapshot's `actionable`: unanswered, its run
        still waiting for the reply) across tasks that are not archived, oldest first, with the
        task and agent it belongs to: the Decisions inbox."""
        with self.db.session() as session:
            rows = session.execute(
                select(AgentQuestion, NodeRun, Task)
                .join(NodeRun, NodeRun.id == AgentQuestion.node_run_id)
                .join(Task, Task.id == AgentQuestion.task_id)
                .where(AgentQuestion.answered.is_(False), NodeRun.state == NodeState.WAITING_FOR_USER,
                       Task.archived_at.is_(None))
                .order_by(AgentQuestion.created_at, AgentQuestion.id)
            ).all()
            return [{**self._question_dict(question, {run.id: run}, True),
                     "jiraKey": task.jira_key, "jiraTitle": task.jira_title, "nodeLabel": run.node.label,
                     "harness": run.harness, "model": run.model}
                    for question, run, task in rows]

    def _task_summary(self, task: Task) -> dict[str, Any]:
        active = next((n for n in task.nodes if n.state in (NodeState.WAITING_FOR_USER, NodeState.WAITING_FOR_APPROVAL)), None)
        active = active or next((n for n in task.nodes if n.state in ACTIVE_STATES), None)
        ready = next((n for n in task.nodes if n.state == NodeState.READY), None)
        failed = next((n for n in task.nodes if n.state == NodeState.FAILED), None)
        phase = task.status
        if task.status == TaskStatus.ACTIVE:
            node = active or failed or ready
            phase = f"{node.label}: {node.state}" if node else "ACTIVE"
        return {
            "taskSource": "local" if is_local_task(task.jira_key) else "jira",
            "jiraKey": task.jira_key, "jiraTitle": task.jira_title, "status": task.status, "statusMessage": task.status_message, "phase": phase,
            "repositoryRoot": task.repository_root, "projectRelativePath": task.project_relative_path,
            "baseBranch": task.base_branch, "branchName": task.branch_name, "worktreePath": task.worktree_path,
            "zellijSession": task.zellij_session_name, "reviewIteration": task.review_iteration,
            "maxReviewIterations": task.max_review_iterations,
            "maxRequirementsRevisions": task.max_requirements_revisions, "currentSha": task.current_sha,
            "prUrl": task.pr_url, "prCreatedAt": _iso(task.pr_created_at), "mergedAt": _iso(task.merged_at),
            "prBaseBranch": task.pr_base_branch, "prSyncPaused": task.pr_sync_paused, "archivedAt": _iso(task.archived_at), "projectId": task.project_id,
            "createdAt": _iso(task.created_at), "updatedAt": _iso(task.updated_at),
        }

    @staticmethod
    def _session_display_status(agent_session: AgentSession, runs_by_id: dict[int, NodeRun]) -> str:
        """Persistent sessions (architect, implementer) stay alive between runs so their conversation
        can be reused later - the pane process is genuinely still 'running' even once its assigned
        run has finished. Report the state of that run (completed, failed, waiting_for_user, ...)
        instead, so the UI doesn't read an idle-but-connected session as actively working."""
        if agent_session.status != "running":
            return agent_session.status
        run = runs_by_id.get(agent_session.current_run_id) if agent_session.current_run_id is not None else None
        return run.state.lower() if run is not None else "idle"

    def jira_issue(self, jira_key: str) -> dict[str, Any] | None:
        """The task's last synced Jira ticket text (TaskWorkspace.jira_issue); not part of the
        snapshot, so only callers that may show it read the file."""
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            if is_local_task(task.jira_key):
                return None
            workspace = self.workspace(task)
        return workspace.jira_issue()

    def snapshot(self, jira_key: str) -> dict[str, Any]:
        with self.db.session() as session:
            task = self._load_task(session, jira_key)
            approval = session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id))
            evidence_rows = session.scalars(select(RunEvidence).join(NodeRun).where(NodeRun.task_id == task.id)).all()
            evidence_by_run = {row.run_id: row for row in evidence_rows}
            runs = session.scalars(select(NodeRun).where(NodeRun.task_id == task.id).order_by(NodeRun.id)).all()
            sessions = session.scalars(select(AgentSession).where(AgentSession.task_id == task.id).order_by(AgentSession.id)).all()
            findings = session.scalars(select(ReviewFinding).where(ReviewFinding.task_id == task.id).order_by(ReviewFinding.id)).all()
            events = session.scalars(select(WorkflowEvent).where(WorkflowEvent.task_id == task.id).order_by(WorkflowEvent.id.desc()).limit(300)).all()
            questions = session.scalars(select(AgentQuestion).where(AgentQuestion.task_id == task.id).order_by(AgentQuestion.id)).all()
            workspace = self.workspace(task)

            runs_by_node: dict[int, list[NodeRun]] = {}
            runs_by_id: dict[int, NodeRun] = {}
            for run in runs:
                runs_by_node.setdefault(run.node_id, []).append(run)
                runs_by_id[run.id] = run
            def _question_actionable(question: AgentQuestion) -> bool:
                if question.answered:
                    return False
                run = runs_by_id.get(question.node_run_id) if question.node_run_id is not None else None
                return run is not None and run.state == NodeState.WAITING_FOR_USER

            open_questions_by_role: dict[int, int] = {}
            for question in questions:
                if _question_actionable(question):
                    owner = runs_by_id[question.node_run_id].node_id
                    open_questions_by_role[owner] = open_questions_by_role.get(owner, 0) + 1

            heartbeats = {h.run_id: h for h in session.scalars(
                select(RunHeartbeat).where(RunHeartbeat.run_id.in_([r.id for r in runs]))).all()}
            nodes = []
            specs = {spec.key: spec for spec in self.spec.nodes}
            order = {key: index for index, key in enumerate(specs)}
            items = {item.node_id: item for item in workers.work_items(session, task)}
            for node in sorted(task.nodes, key=lambda n: order.get(n.key, len(order))):
                node_runs = runs_by_node.get(node.id, [])
                last = node_runs[-1] if node_runs else None
                effective = self.resolve_agent_config(task, node) if node.kind == NodeKind.AGENT else None
                if effective:
                    caps = self.harnesses.capabilities.get(effective["harness"])
                    effective["harnessDisplayName"] = caps.display_name if caps else effective["harness"]
                    effective["harnessAvailable"] = self.harnesses.is_usable(effective["harness"])
                node_findings = [f for f in findings if f.node_run_id in {r.id for r in node_runs}]
                # The Implementer coordinator node has no run of its own while workers are active
                # (it only goes READY once all of them complete), so its real state sits at PENDING
                # the whole time - display it as RUNNING whenever a worker is live, so the graph
                # doesn't read as idle. This is presentation only; node.state (below) still drives
                # button gating and the dispatch state machine.
                display_state = node.state
                if node.key == wf.IMPLEMENTER and node.state == NodeState.PENDING and items:
                    live_worker_states = {NodeState.RUNNING, NodeState.WAITING_FOR_USER, NodeState.WAITING_FOR_APPROVAL}
                    if any(n.state in live_worker_states for n in task.nodes if n.id in items):
                        display_state = NodeState.RUNNING
                nodes.append({
                    "key": node.key, "kind": node.kind, "label": node.label, "role": node.role, "state": node.state,
                    "displayState": display_state,
                    "executionMode": node.execution_mode, "iteration": node.iteration, "checkpointSha": node.checkpoint_sha,
                    "message": (f"{sum(n.state == NodeState.COMPLETED for n in task.nodes if n.id in items)}/{len(items)} workers complete. Select to view workers."
                                if node.key == wf.IMPLEMENTER and node.state == NodeState.PENDING and items else node.message),
                    "error": node.error, "configOverride": node.config_override or {},
                    "effectiveConfig": effective, "openQuestions": open_questions_by_role.get(node.id, 0) if node.state == NodeState.WAITING_FOR_USER else 0,
                    "findingsCount": len(node_findings), "blockingCount": sum(1 for f in node_findings if f.blocking),
                    "runCount": len(node_runs),
                    "usage": self._usage_summary(node_runs, evidence_by_run),
                    "lastRun": {
                        "id": last.id, "runNumber": last.run_number, "state": last.state, "harness": last.harness, "model": last.model,
                        "reasoning": last.reasoning, "runDir": last.run_dir, "promptPath": last.prompt_path, "resultPath": last.result_path,
                        "exitCode": last.exit_code, "error": last.error, "startedAt": _iso(last.started_at), "completedAt": _iso(last.completed_at),
                        "heartbeat": {
                            "observedAt": _iso(_now()),
                            "warningSince": _iso(max(last.started_at, node.updated_at)),
                            "receivedAt": _iso(heartbeats[last.id].received_at) if last.id in heartbeats else None,
                            "phase": heartbeats[last.id].phase if last.id in heartbeats else None,
                            "warningSeconds": self.config.workflow.heartbeat_warning_seconds,
                        },
                        "result": last.result,
                    } if last else None,
                    "description": specs[node.key].description if node.key in specs else items[node.id].brief,
                    "worker": {
                        "uid": items[node.id].uid, "key": items[node.id].plan_key,
                        "ownedPaths": items[node.id].owned_paths,
                        "acceptanceCriteria": items[node.id].acceptance_criteria,
                        "dependencies": items[node.id].dependencies,
                        "worktreePath": items[node.id].worktree_path,
                        "branchName": items[node.id].branch_name,
                    } if node.id in items else None,
                })

            assessment = None
            assessment_path = workspace.final_dir / "assessment.json"
            if assessment_path.exists():
                try:
                    assessment = TaskWorkspace.read_json(assessment_path)
                except ValueError:
                    assessment = None

            # Read and hash the requirement documents once per snapshot; the blockers check
            # compares the same digest against the approved one.
            requirements = workspace.requirement_files()
            return {
                **self._task_summary(task),
                "attention": attention_items(task, runs, questions),
                "budget": budget_status(session, task, self.config.workflow.cost_controls),
                **self._worktree_state(task, items.values()),
                "blockers": self._final_blockers(session, task, requirements),
                "additionalContext": task.additional_context,
                "taskDir": task.task_dir,
                "harnessCwd": str(self._harness_cwd(task)),
                "agentOverrides": task.agent_overrides or {},
                "zellij": {
                    "session": task.zellij_session_name,
                    "attachCommand": self.zellij.attach_command(task.zellij_session_name),
                    "webUrl": self.zellij.web_url(task.zellij_session_name),
                },
                "nodes": nodes,
                "edges": [{"source": s, "target": t} for s, t in self.spec.edges
                          if any(n.key == s for n in task.nodes) and any(n.key == t for n in task.nodes)]
                         + ([{"source": wf.REQUIREMENTS_GATE, "target": wf.IMPLEMENTER}]
                            if not any(n.key == wf.ORCHESTRATOR for n in task.nodes) else []),
                "sessions": [
                    {"id": s.id, "role": s.role, "harness": s.harness, "model": s.model, "reasoning": s.reasoning, "tabName": s.zellij_tab_name,
                     "tabId": s.zellij_tab_id, "paneId": s.zellij_pane_id, "harnessSessionId": s.harness_session_id, "status": s.status,
                     "activity": self._session_display_status(s, runs_by_id),
                     "screenState": s.screen_state if s.status == "running" else None,
                     "screenStateAt": _iso(s.screen_state_at),
                     "nodeKey": runs_by_id[s.current_run_id].node.key if s.current_run_id in runs_by_id else None,
                     "startedAt": _iso(s.started_at), "endedAt": _iso(s.ended_at)}
                    for s in sessions
                ],
                "findings": [self._finding_dict(f) for f in findings],
                "questions": [self._question_dict(q, runs_by_id, _question_actionable(q)) for q in questions],
                "events": [
                    {"id": e.id, "timestamp": _iso(e.timestamp), "level": e.level, "nodeKey": e.node_key, "message": e.message, "data": e.data}
                    for e in reversed(events)
                ],
                "requirements": requirements,
                "requirementsFeedback": workspace.requirements_feedback(),
                "approvedRequirements": approval.documents if approval else None,
                "requirementsDigest": approval.digest if approval else None,
                "requirementsDraftDigest": assurance.documents_digest(requirements),
                "executionEvidence": [{"runId": run.id, "nodeKey": run.node.key, "requestedModel": run.model,
                    "confirmedModel": evidence_by_run[run.id].confirmed_model if run.id in evidence_by_run else None,
                    "usage": evidence_by_run[run.id].usage if run.id in evidence_by_run else None,
                    "billingMode": evidence_by_run[run.id].billing_mode if run.id in evidence_by_run else "unknown",
                    "verification": evidence_by_run[run.id].verification if run.id in evidence_by_run else None,
                    "durationSeconds": ((_now() if run.completed_at is None else run.completed_at.replace(tzinfo=timezone.utc))
                        - run.started_at.replace(tzinfo=timezone.utc)).total_seconds(),
                    "iteration": run.iteration} for run in runs],
                "totalUsage": self._usage_summary(runs, evidence_by_run),
                "assessment": assessment,
                "assessmentMarkdown": workspace.read_text("final/assessment.md"),
            }


def _ps_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"
