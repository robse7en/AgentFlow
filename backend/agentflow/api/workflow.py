"""Agent-facing endpoints used by flowctl."""

from __future__ import annotations

from fastapi import APIRouter, Depends

from ..workflow.engine import WorkflowError
from ..workflow.models import (HeartbeatRequest, CompleteRequest, ExitedRequest, FailRequest, FindingRequest,
                               LessonRequest, QuestionRequest, RunningRequest)
from .deps import AppState, get_state, http_error

router = APIRouter(prefix="/api/tasks/{jira_key}/agent", tags=["agent"])


@router.post("/question")
async def question(jira_key: str, req: QuestionRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_question(jira_key, req.role, req.run_id, req.message, req.choices,
                                                 req.decision)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/heartbeat")
async def heartbeat(jira_key: str, req: HeartbeatRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_heartbeat(jira_key, req.role, req.run_id, req.phase)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/lesson")
async def lesson(jira_key: str, req: LessonRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_lesson(jira_key, req.role, req.run_id, req.message)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/running")
async def running(jira_key: str, req: RunningRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_running(jira_key, req.role, req.run_id)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/complete")
async def complete(jira_key: str, req: CompleteRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_complete(jira_key, req.role, req.run_id, req.result)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/finding")
async def finding(jira_key: str, req: FindingRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_finding(jira_key, req.role, req.run_id, req.finding)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/fail")
async def fail(jira_key: str, req: FailRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_fail(jira_key, req.role, req.run_id, req.message)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/exited")
async def exited(jira_key: str, req: ExitedRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.agent_exited(jira_key, req.role, req.run_id, req.exit_code)
    except WorkflowError as exc:
        raise http_error(exc)
