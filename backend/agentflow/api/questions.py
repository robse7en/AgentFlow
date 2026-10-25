"""Open agent questions across tasks, for the Decisions inbox."""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException

from .deps import AppState, get_state

router = APIRouter(prefix="/api/questions", tags=["questions"])


@router.get("")
def list_questions(open: bool = True, state: AppState = Depends(get_state)):
    if not open:
        raise HTTPException(422, "Only open questions are listed: use open=true")
    return state.engine.open_questions()
