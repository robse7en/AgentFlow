"""Background classification of each active agent's live terminal screen.

One long-lived `zellij subscribe` per active `AgentSession`, feeding each rendered
frame through the harness adapter's `classify_screen`. State is written to
`AgentSession.screen_state` - and the task republished - only on a transition, so an
animating spinner (the common case) causes neither a DB write nor a broadcast.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Awaitable, Callable

from ..database.database import Database
from ..database.models import AgentSession, utcnow
from ..harnesses.base import HarnessAdapter
from ..services.zellij import ZellijService

log = logging.getLogger(__name__)


class ScreenTracker:
    def __init__(self, zellij: ZellijService, db: Database, publish: Callable[[str], Awaitable[None]]) -> None:
        self.zellij = zellij
        self.db = db
        self._publish = publish
        self._tasks: dict[int, asyncio.Task] = {}

    def start(self, jira_key: str, session_id: int, zellij_session: str, pane_id: str, adapter: HarnessAdapter) -> None:
        """Idempotent: a session already being tracked is left alone."""
        if session_id in self._tasks:
            return
        task = asyncio.ensure_future(self._run(jira_key, session_id, zellij_session, pane_id, adapter))
        self._tasks[session_id] = task
        task.add_done_callback(lambda _t, sid=session_id: self._tasks.pop(sid, None))

    def cancel_nowait(self, session_id: int) -> None:
        """Cancel a tracker without waiting for its `subscribe` subprocess to actually
        exit. `subscribe`'s cleanup can take up to a couple of seconds (terminate, wait,
        fall back to kill), which is unaffordable at a call site already holding
        `self._lock(jira_key)` or an open `db.session()` - use this there instead of
        `stop`. The task's own done-callback still removes it once cleanup finishes."""
        task = self._tasks.get(session_id)
        if task is not None:
            task.cancel()

    async def stop(self, session_id: int) -> None:
        task = self._tasks.pop(session_id, None)
        if task is None:
            return
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        except Exception:
            log.exception("Screen tracker for agent session %s ended with an error", session_id)

    async def stop_all(self) -> None:
        for session_id in list(self._tasks):
            await self.stop(session_id)

    async def _run(self, jira_key: str, session_id: int, zellij_session: str, pane_id: str, adapter: HarnessAdapter) -> None:
        last_state: str | None = None
        try:
            async for line in self.zellij.subscribe(zellij_session, pane_id):
                viewport = self.zellij.parse_pane_update(line)
                if viewport is None:
                    continue
                state = adapter.classify_screen(viewport)
                if state == last_state:
                    continue
                recorded = self._record(session_id, state)
                if recorded is None:
                    # The row wasn't visible to this read yet (e.g. a launch still mid-
                    # transaction elsewhere) - transient; retry next frame instead of
                    # caching this classification as already-seen.
                    continue
                last_state = state
                if recorded:
                    await self._publish(jira_key)
        except asyncio.CancelledError:
            raise
        except Exception:
            log.exception("Screen tracker for agent session %s crashed", session_id)

    def _record(self, session_id: int, state: str) -> bool | None:
        """True: state changed and was persisted (caller should broadcast). False: the
        DB already agrees, or the session is no longer "running" (`_stop_sessions` or
        `_live_session` closed it out from under this still-draining stream) - no-op
        either way. None: no such row (yet) - transient, see `_run`."""
        with self.db.session() as db_session:
            agent = db_session.get(AgentSession, session_id)
            if agent is None:
                return None
            if agent.status != "running" or agent.screen_state == state:
                return False
            agent.screen_state = state
            agent.screen_state_at = utcnow()
        return True
