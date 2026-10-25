"""Local application permissions, not an OS sandbox against same-user processes."""

from __future__ import annotations

import hashlib
import secrets
from pathlib import Path
from urllib.parse import urlsplit

from fastapi import HTTPException, Request
from sqlalchemy import select

from ..database.models import AgentAccess, Task
from ..workflow.states import POST_READY_STATUSES, TaskStatus

COOKIE = "agentflow_human"


def digest(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


class HumanAccess:
    """One browser session that survives restarts. Only the token's hash is written to disk,
    so reading the file does not grant access; pairing again replaces the session."""

    def __init__(self, session_file: Path) -> None:
        self.pairing_code = secrets.token_urlsafe(24)
        self.session_file = session_file
        # Counts session replacements in this process. Event sockets remember the generation
        # that authorized them (never the token or its hash) and are closed when it changes.
        self.generation = 0
        try:
            self.session_hash = session_file.read_text(encoding="utf-8").strip()
        except OSError:
            self.session_hash = ""

    def authenticated(self, cookies: dict) -> bool:
        token = cookies.get(COOKIE, "")
        return bool(token and self.session_hash) and secrets.compare_digest(digest(token), self.session_hash)

    def pair(self, code: str) -> str:
        if not self.pairing_code or not secrets.compare_digest(code, self.pairing_code):
            raise HTTPException(401, "Invalid or already used pairing code. Restart to pair again.")
        self.pairing_code = ""
        token = secrets.token_urlsafe(32)
        self.session_hash = digest(token)
        self.generation += 1
        self.session_file.parent.mkdir(parents=True, exist_ok=True)
        self.session_file.write_text(self.session_hash, encoding="utf-8")
        self.session_file.chmod(0o600)
        return token


def allowed_origin(origin: str | None, host: str) -> bool:
    if not origin:
        return False
    parsed = urlsplit(origin)
    return origin in ("http://localhost:5173", "http://127.0.0.1:5173") or (
        parsed.scheme == "http" and parsed.netloc == host
        and parsed.hostname in ("localhost", "127.0.0.1", "::1")
    )


async def authorize_agent(request: Request, jira_key: str) -> None:
    """A bearer token grants callbacks only for its exact task, role and run."""
    state = request.app.state.agentflow
    token = request.headers.get("authorization", "").removeprefix("Bearer ")
    try:
        body = await request.json() if request.method == "POST" else None
    except ValueError as exc:
        raise HTTPException(422, "Expected a JSON object") from exc
    if request.method == "POST" and not isinstance(body, dict):
        raise HTTPException(422, "Expected a JSON object")
    # Launching may deliver the prompt before its transaction commits. Wait for that
    # transaction before looking up the credential, then let the engine validate freshness.
    async with state.engine._lock(jira_key):
        with state.db.session() as session:
            access = session.scalar(select(AgentAccess).where(AgentAccess.token_hash == digest(token)))
            task = session.scalar(select(Task).where(Task.jira_key == jira_key.upper()))
            if not access or not task or access.task_id != task.id:
                raise HTTPException(403, "Agent credential does not authorize this task")
            if task.status in (TaskStatus.CANCELLED, *POST_READY_STATUSES):
                raise HTTPException(409, "Task no longer accepts agent requests")
            if body is not None and (body.get("role") != access.role or body.get("runId") != access.run_id):
                raise HTTPException(403, "Agent credential does not authorize this role or run")
