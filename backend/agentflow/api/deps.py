"""Shared application state handed to routers."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from pathlib import Path

from fastapi import HTTPException, Request

from ..config import Config
from ..database.database import Database
from ..harnesses.registry import HarnessRegistry
from ..services.events import Broadcaster
from ..services.git_workspace import GitService
from ..services.zellij import ZellijService
from ..workflow.engine import WorkflowEngine, WorkflowError


@dataclass
class AppState:
    config: Config
    config_file: Path
    db: Database
    git: GitService
    zellij: ZellijService
    harnesses: HarnessRegistry
    broadcaster: Broadcaster
    engine: WorkflowEngine
    system: dict
    # Serializes every read-modify-write of config_file (and reload) in this process. Taken
    # before engine._launch_lock, never while holding it. Reload swaps fields into this object,
    # so the lock outlives reloads; a state build_state returns for a reload is never used whole.
    config_lock: asyncio.Lock = field(default_factory=asyncio.Lock)


def get_state(request: Request) -> AppState:
    return request.app.state.agentflow


class WorkflowHTTPException(HTTPException):
    """An HTTPException that also carries WorkflowError.code; main.py serialises it as
    {"detail": ..., "code": ...} so the UI can react to specific errors without text matching."""

    def __init__(self, status_code: int, detail: str, code: str | None) -> None:
        super().__init__(status_code=status_code, detail=detail)
        self.code = code


def http_error(exc: WorkflowError) -> HTTPException:
    return WorkflowHTTPException(exc.status_code, str(exc), exc.code)
