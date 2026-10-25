"""Human-only instruction routes. Existing middleware enforces operator access."""
from typing import Literal
from fastapi import APIRouter, Depends
from .deps import AppState, get_state, http_error
from ..services.git_workspace import GitError
from ..workflow import instruction_service as service, instructions as guidance
from ..workflow.engine import WorkflowError
from ..workflow.instruction_service import ApplyUpdate, SourceSave

router = APIRouter(prefix='/api', tags=['instructions'])


async def operator_result(operation):
    try:
        return await operation
    except (WorkflowError, guidance.InstructionError, OSError, GitError) as exc:
        raise http_error(exc if isinstance(exc, WorkflowError) else WorkflowError(str(exc), 409))


@router.get('/projects/{project_id}/instructions')
async def project_instructions(project_id: int, state: AppState = Depends(get_state)):
    return await operator_result(service.project_instructions(state.engine, project_id))


@router.put('/projects/{project_id}/instructions/{role}')
async def save_project_source(project_id: int, role: str, req: SourceSave,
                              state: AppState = Depends(get_state)):
    return await operator_result(service.save_project_source(state.engine, project_id, role, req))


@router.get('/tasks/{jira_key}/instructions')
async def task_instructions(jira_key: str, nodeKey: str = 'architect', runId: int | None = None,
                            preview: Literal['snapshot', 'source'] = 'snapshot',
                            state: AppState = Depends(get_state)):
    return await operator_result(service.task_instructions(state.engine, jira_key, nodeKey, runId, preview))


@router.put('/tasks/{jira_key}/instructions/{role}')
async def save_task_source(jira_key: str, role: str, req: SourceSave,
                           state: AppState = Depends(get_state)):
    return await operator_result(service.save_task_source(state.engine, jira_key, role, req))


@router.post('/tasks/{jira_key}/instructions/apply')
async def apply_update(jira_key: str, req: ApplyUpdate, state: AppState = Depends(get_state)):
    return await operator_result(service.apply_update(state.engine, jira_key, req))
