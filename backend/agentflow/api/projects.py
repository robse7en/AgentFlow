"""Projects: named groups of tasks, with defaults for the New Task form."""

from __future__ import annotations

from fastapi import APIRouter, Depends

from ..workflow.engine import WorkflowError
from ..workflow.models import ProjectRequest
from .deps import AppState, get_state, http_error

router = APIRouter(prefix="/api/projects", tags=["projects"])


@router.get("")
def list_projects(state: AppState = Depends(get_state)):
    return state.engine.list_projects()


@router.post("", status_code=201)
async def create_project(req: ProjectRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.create_project(req)
    except WorkflowError as exc:
        raise http_error(exc)


@router.patch("/{project_id}")
async def update_project(project_id: int, req: ProjectRequest, state: AppState = Depends(get_state)):
    try:
        return await state.engine.update_project(project_id, req)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{project_id}/archive")
async def archive_project(project_id: int, state: AppState = Depends(get_state)):
    try:
        return await state.engine.set_project_archived(project_id, True)
    except WorkflowError as exc:
        raise http_error(exc)


@router.post("/{project_id}/unarchive")
async def unarchive_project(project_id: int, state: AppState = Depends(get_state)):
    try:
        return await state.engine.set_project_archived(project_id, False)
    except WorkflowError as exc:
        raise http_error(exc)


@router.delete("/{project_id}")
async def delete_project(project_id: int, state: AppState = Depends(get_state)):
    try:
        return await state.engine.delete_project(project_id)
    except WorkflowError as exc:
        raise http_error(exc)
