"""Jira search proxy for the New Task form. Never returns the configured API token."""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, Query

from ..services.jira_client import JiraError, build_service
from .deps import AppState, get_state

router = APIRouter(prefix="/api/jira", tags=["jira"])


@router.get("/search")
async def search(query: str = Query(min_length=1, max_length=200), state: AppState = Depends(get_state)):
    service = build_service(state.config.jira)
    if not service:
        raise HTTPException(400, "Jira integration is not configured (see jira.* in config.json)")
    try:
        results = await service.search_issues(query)
    except JiraError as exc:
        raise HTTPException(502, str(exc)) from exc
    return [{"key": r.key, "summary": r.summary, "iconUrl": r.icon_url, "section": r.section} for r in results]
