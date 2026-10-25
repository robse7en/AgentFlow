"""Harness detection endpoints."""

from __future__ import annotations

from fastapi import APIRouter, Depends

from .config import system_info
from .deps import AppState, get_state

router = APIRouter(prefix="/api/harnesses", tags=["harnesses"])


@router.get("")
def list_harnesses(state: AppState = Depends(get_state)):
    return system_info(state)["harnesses"]


@router.post("/detect")
async def detect(state: AppState = Depends(get_state)):
    await state.harnesses.detect_all()
    return system_info(state)["harnesses"]
