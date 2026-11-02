"""Task creation, listing and workflow node actions."""

from __future__ import annotations

from uuid import UUID
from typing import Literal

from fastapi import APIRouter, Depends
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, ConfigDict, Field, field_validator

from ..workflow.engine import WorkflowError
from ..workflow.models import (MAX_FEEDBACK_LENGTH, MAX_PR_URL_LENGTH, AgentOverride, CreateTaskRequest,
                               QuestionAnswerRequest, RunNodeRequest, TaskProjectRequest, clean_pr_url)
from .deps import AppState, get_state, http_error
from sqlalchemy import select
from ..database.models import NodeRun, RunEvidence
from fastapi import HTTPException
from ..workflow.usage import UsageReport

router = APIRouter(prefix="/api/tasks", tags=["tasks"])


class ApprovalRequest(BaseModel):
    expected_requirements_digest: str | None = Field(None, alias="expectedRequirementsDigest", min_length=64, max_length=64)
    reason: str | None = Field(None, max_length=4000)
    expected_sha: str | None = Field(None, alias="expectedSha")


class RecoveryRequest(BaseModel):
    retry: bool = False


class TaskBudgetRequest(BaseModel):
    amount: float | None = Field(None, gt=0, allow_inf_nan=False)


@router.put("/{jira_key}/budget")
async def set_task_budget(jira_key: str, payload: TaskBudgetRequest, state: AppState = Depends(get_state)):
    try:
        async with state.engine._lock(jira_key), state.engine._launch_lock:
            with state.db.session() as session:
                task = state.engine._load_task(session, jira_key)
                task.api_budget_usd = payload.amount
                state.engine._log(session, task, "Human updated ticket API budget", data={"amountUsd": payload.amount})
    except WorkflowError as exc:
        raise http_error(exc)
    return await state.engine._publish(jira_key)


class RequirementsFeedbackRequest(BaseModel):
    feedback: str = Field(..., min_length=1, max_length=MAX_FEEDBACK_LENGTH)


class UsageImportReport(UsageReport):
    billing_mode: Literal["metered", "subscription", "unknown"] | None = Field(None, alias="billingMode")


@router.put("/{jira_key}/runs/{run_id}/usage")
async def import_usage(jira_key: str, run_id: int, payload: UsageImportReport, state: AppState = Depends(get_state)):
    """Human-only import of provider usage. Unknown usage is never represented as zero."""
    if payload.scope != "run":
        raise HTTPException(422, "Import usage for this run only, not cumulative session totals")
    async with state.engine._lock(jira_key):
        with state.db.session() as session:
            task = state.engine._load_task(session, jira_key)
            run = session.get(NodeRun, run_id)
            if not run or run.task_id != task.id:
                raise HTTPException(404, "Run not found")
            evidence = session.scalar(select(RunEvidence).where(RunEvidence.run_id == run.id))
            if evidence is None:
                evidence = RunEvidence(run_id=run.id)
                session.add(evidence)
            evidence.usage = payload.record("human-imported-provider-report")
            evidence.confirmed_model = payload.model
            if payload.billing_mode is not None:
                evidence.billing_mode = payload.billing_mode
            state.engine._log(session, task, "Human imported provider usage", data={"runId": run.id, "source": payload.source})
    return await state.engine._publish(jira_key)


class WorktreeCleanupRequest(BaseModel):
    expected_sha: str = Field(alias="expectedSha", min_length=40, max_length=64)
    # Skip the reviewed-or-merged branch check (the human says it was merged elsewhere).
    force: bool = False


class ExpectedShaRequest(BaseModel):
    expected_sha: str = Field(alias="expectedSha", min_length=40, max_length=64)


class MarkPrCreatedRequest(BaseModel):
    pr_url: str = Field(alias="prUrl", min_length=8, max_length=MAX_PR_URL_LENGTH)

    @field_validator("pr_url")
    @classmethod
    def _looks_like_url(cls, value: str) -> str:
        return clean_pr_url(value)


@router.get("/{jira_key}/worktree/cleanup-preview")
async def worktree_cleanup_preview(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.worktree_cleanup_preview(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.get("/{jira_key}/findings/{finding_id}/code")
async def finding_code(jira_key: str, finding_id: int, state: AppState = Depends(get_state)):
    """The code a review finding points at."""
    try:
        return await state.engine.finding_code(jira_key, finding_id)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/worktree/cleanup")
async def cleanup_worktree(jira_key: str, req: WorktreeCleanupRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.cleanup_worktree(jira_key, req.expected_sha, force=req.force)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/worktree/discard-drift")
async def discard_worktree_drift(jira_key: str, req: ExpectedShaRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.discard_worktree_drift(jira_key, req.expected_sha)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/pr/created")
async def mark_pr_created(jira_key: str, req: MarkPrCreatedRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.mark_pr_created(jira_key, req.pr_url)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/pr/created/revert")
async def revert_pr_created(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.revert_pr_created(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/pr/merged")
async def mark_pr_merged(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.mark_pr_merged(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/pr/merged/revert")
async def revert_pr_merged(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.revert_pr_merged(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/pr/open")
async def open_pr(jira_key: str, state: AppState = Depends(get_state)):
    """Push the branch; answers {url, task}, `url` being GitHub's new-PR form already targeting
    the task's base branch."""
    try:
        return await state.engine.open_pr(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/pr/retarget")
async def retarget_pr(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.retarget_pr(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/review-override")
async def override_reviews(jira_key: str, req: ApprovalRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.override_reviews(jira_key, req.reason or "", req.expected_sha or "")
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/nodes/{node_key}/recover")
async def recover_node(jira_key: str, node_key: str, req: RecoveryRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.recover_node(jira_key, node_key, req.retry)
    except WorkflowError as exc:
        raise http_error(exc)


class ValidateRepositoryRequest(BaseModel):
    path: str
    base_branch: str | None = Field(None, alias="baseBranch")
    project_relative_path: str | None = Field(None, alias="projectRelativePath")
    model_config = ConfigDict(populate_by_name=True)


@router.get("")
async def list_tasks(archived: Literal["exclude", "include", "only"] = "exclude", state: AppState = Depends(get_state)):
    return await state.engine.list_tasks(archived)


@router.post("", status_code=201)
async def create_task(req: CreateTaskRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.create_task(req)
    except WorkflowError as exc:
        raise http_error(exc)


class CostEstimateRequest(BaseModel):
    implementation_mode: Literal["direct", "parallel"] | None = Field(None, alias="implementationMode")
    agent_overrides: dict[str, AgentOverride] = Field(default_factory=dict, alias="agentOverrides")
    model_config = ConfigDict(populate_by_name=True)


@router.post("/cost-estimate")
def cost_estimate(req: CostEstimateRequest, state: AppState = Depends(get_state)):
    """What a new task would cost on a route, from past runs (display only)."""
    return state.engine.cost_estimate(req.implementation_mode, req.agent_overrides)


@router.post("/validate-repository")
async def validate_repository(req: ValidateRepositoryRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.validate_repository(req.path, req.base_branch, req.project_relative_path)
    except Exception as exc:  # noqa: BLE001 - GitError etc. become an actionable message
        return {"ok": False, "checks": [{"ok": False, "message": str(exc)}], "branches": []}


@router.get("/{jira_key}")
def get_task(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return state.engine.snapshot(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/cancel")
async def cancel_task(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.cancel_task(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/archive")
async def archive_task(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.archive_task(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/unarchive")
async def unarchive_task(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.unarchive_task(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.put("/{jira_key}/project")
async def set_task_project(jira_key: str, req: TaskProjectRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.set_task_project(jira_key, req.project_id)
    except WorkflowError as exc:
        raise http_error(exc)


class DeleteTaskRequest(BaseModel):
    remove_worktree: bool = Field(alias="removeWorktree")
    remove_branch: bool = Field(alias="removeBranch")
    remove_task_folder: bool = Field(alias="removeTaskFolder")
    # The branch tip the preview showed; required to remove the worktree or branch.
    expected_sha: str | None = Field(None, alias="expectedSha", min_length=40, max_length=64)


@router.get("/{jira_key}/delete-preview")
async def delete_preview(jira_key: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.delete_preview(jira_key)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/delete")
async def delete_task(jira_key: str, req: DeleteTaskRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.delete_task(jira_key, remove_worktree=req.remove_worktree,
                                              remove_branch=req.remove_branch,
                                              remove_task_folder=req.remove_task_folder,
                                              expected_sha=req.expected_sha)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/questions/{question_uid}/answer")
async def answer_question(
    jira_key: str,
    question_uid: UUID,
    req: QuestionAnswerRequest,
    state: AppState = Depends(get_state),
):
    try:
        return await state.engine.answer_question(jira_key, str(question_uid), req.answer, path=req.path)
    except WorkflowError as exc:
        raise http_error(exc)


@router.get("/{jira_key}/terminal/{role}")
async def stream_terminal(jira_key: str, role: str, state: AppState = Depends(get_state)):
    try:
        zellij_session, pane_id = await state.engine.terminal_target(jira_key, role)
    except WorkflowError as exc:
        raise http_error(exc)
    return StreamingResponse(
        state.zellij.subscribe(zellij_session, pane_id),
        media_type="application/x-ndjson",
        headers={"Cache-Control": "no-store", "X-Accel-Buffering": "no"},
    )


@router.post("/{jira_key}/nodes/{node_key}/run")
async def run_node(jira_key: str, node_key: str, req: RunNodeRequest | None = None, state: AppState = Depends(get_state)):
    try:
        return await state.engine.run_node(jira_key, node_key, req.override if req else None,
                                          metered_approval=req.metered_approval if req else None)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/nodes/{node_key}/approve")
async def approve_node(jira_key: str, node_key: str, req: ApprovalRequest | None = None, state: AppState = Depends(get_state)):
    try:
        return await state.engine.approve(jira_key, node_key, req.reason if req else None,
            req.expected_sha if req else None, req.expected_requirements_digest if req else None)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/nodes/{node_key}/request-changes")
async def request_changes(jira_key: str, node_key: str, req: RequirementsFeedbackRequest, state: AppState = Depends(get_state)):
    if node_key != "requirements_gate":
        raise HTTPException(404, "Requesting changes is only supported for the requirements gate")
    try:
        return await state.engine.request_requirements_changes(jira_key, req.feedback)
    except WorkflowError as exc:
        raise http_error(exc)


@router.put("/{jira_key}/nodes/{node_key}/config")
async def set_node_config(jira_key: str, node_key: str, override: AgentOverride, state: AppState = Depends(get_state)):
    try:
        return await state.engine.set_node_override(jira_key, node_key, override)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/zellij/open")
async def open_zellij(jira_key: str, node: str | None = None, state: AppState = Depends(get_state)):
    try:
        return await state.engine.open_zellij(jira_key, node)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/zellij/window")
async def open_zellij_window(jira_key: str, node: str | None = None, state: AppState = Depends(get_state)):
    try:
        return await state.engine.open_zellij_window(jira_key, node)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{jira_key}/reveal/{target}")
async def reveal_path(jira_key: str, target: str, state: AppState = Depends(get_state)):
    try:
        return await state.engine.reveal_path(jira_key, target)
    except WorkflowError as exc:
        raise http_error(exc)
