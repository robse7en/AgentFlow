"""WebSocket fan-out of workflow state changes, plus in-process listeners."""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any, Callable

from fastapi import WebSocket

log = logging.getLogger(__name__)

# How long closing a revoked socket may take before it is abandoned (it is already unregistered).
CLOSE_TIMEOUT_SECONDS = 5


class Broadcaster:
    def __init__(self) -> None:
        # Each socket with the check of whether the browser session that opened it is still current.
        self._clients: dict[WebSocket, Callable[[], bool]] = {}
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        self._lock = asyncio.Lock()

    def subscribe(self, listener: Callable[[dict[str, Any]], None]) -> Callable[[], None]:
        """Register an in-process listener (e.g. chat notifications); returns an unsubscribe.

        Listeners run synchronously inside every broadcast, so they must only hand the
        message off (queue it) - never block, and never raise into the workflow."""
        self._listeners.append(listener)

        def unsubscribe() -> None:
            if listener in self._listeners:
                self._listeners.remove(listener)
        return unsubscribe

    async def connect(self, websocket: WebSocket, authorized: Callable[[], bool] = lambda: True) -> bool:
        """Accept and register a socket; False (and closed) if its session was replaced meanwhile.

        `authorized` is checked again after accepting: pairing may replace the session while the
        handshake completes, and `revoke` only closes sockets that are already registered."""
        await websocket.accept()
        async with self._lock:
            if authorized():
                self._clients[websocket] = authorized
                return True
        await self._close(websocket)
        return False

    async def disconnect(self, websocket: WebSocket) -> None:
        async with self._lock:
            self._clients.pop(websocket, None)

    async def revoke(self) -> None:
        """Close the sockets whose browser session is no longer current (pairing replaced it)."""
        async with self._lock:
            revoked = [client for client, authorized in self._clients.items() if not authorized()]
            for client in revoked:
                del self._clients[client]
        for client in revoked:
            await self._close(client)

    @staticmethod
    async def _close(websocket: WebSocket) -> None:
        # Bounded: an unresponsive browser must not hold up the pairing that revoked it.
        try:
            await asyncio.wait_for(websocket.close(code=1008), CLOSE_TIMEOUT_SECONDS)
        except Exception:  # noqa: BLE001 - already closed, gone or not answering
            pass

    async def broadcast(self, message: dict[str, Any]) -> None:
        for listener in list(self._listeners):
            try:
                listener(message)
            except Exception:  # noqa: BLE001 - a listener must never break a workflow action
                log.exception("broadcast listener failed")
        payload = json.dumps(message, default=str)
        async with self._lock:
            clients = list(self._clients)
        dead: list[WebSocket] = []
        for client in clients:
            # Checked before every send, not only at the snapshot: an earlier send may have
            # waited while pairing replaced this client's session or it disconnected.
            authorized = self._clients.get(client)
            if authorized is None or not authorized():
                continue
            try:
                await client.send_text(payload)
            except Exception:  # noqa: BLE001 - client went away
                dead.append(client)
        if dead:
            async with self._lock:
                for client in dead:
                    self._clients.pop(client, None)

    @property
    def client_count(self) -> int:
        return len(self._clients)
