"""Event sockets belong to the browser session that opened them (AF-AUDIT-004): pairing a new
browser closes the replaced session's sockets, and nothing published afterwards reaches them."""

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi.testclient import TestClient
from starlette.websockets import WebSocketDisconnect

import agentflow.main as main
from agentflow.api.security import COOKIE, HumanAccess, digest
from agentflow.main import create_app
from agentflow.services.events import Broadcaster
from test_workflow_safety import engine  # noqa: F401 - fixture

ORIGIN = {"Origin": "http://127.0.0.1"}
# Absolute: the test client would otherwise send Host: testserver, which TrustedHostMiddleware refuses.
WS = "ws://127.0.0.1/api/ws"


@pytest.fixture
def make_app(engine, monkeypatch):  # noqa: F811
    async def build_state(config, config_file, broadcaster, existing_db=None):
        engine.broadcaster = broadcaster  # as the real build_state wires it: engine events reach /api/ws
        return SimpleNamespace(db=engine.db, engine=engine)

    monkeypatch.setattr(main, "build_state", build_state)
    monkeypatch.setattr(engine, "recover", AsyncMock())
    return lambda: create_app(engine.config, config_file=engine.config.database_file.parent / "config.json")


def test_pairing_after_a_restart_closes_the_replaced_sessions_socket(make_app, engine):  # noqa: F811
    first = make_app()
    with TestClient(first, base_url="http://127.0.0.1") as browser_a:
        browser_a.post("/api/auth/pair", json={"code": first.state.human.pairing_code}, headers=ORIGIN).raise_for_status()
        token_a = browser_a.cookies[COOKIE]

    # Restart: session A survives in the data directory, and a new single-use code is printed.
    restarted = make_app()
    with TestClient(restarted, base_url="http://127.0.0.1") as client:
        client.cookies.set(COOKIE, token_a)
        with client.websocket_connect(WS, headers=ORIGIN) as socket_a:
            assert engine.broadcaster.client_count == 1
            client.cookies.clear()
            client.post("/api/auth/pair", json={"code": restarted.state.human.pairing_code},
                        headers=ORIGIN).raise_for_status()
            token_b = client.cookies[COOKIE]
            with client.websocket_connect(WS, headers=ORIGIN) as socket_b:
                # A task event published after the pairing (it broadcasts the task snapshot).
                assert client.put("/api/tasks/TEST-1/budget", json={"amount": 5}, headers=ORIGIN).status_code == 200
                event = socket_b.receive_json()
                assert event["type"] == "task" and event["task"]["jiraKey"] == "TEST-1"
                # A's next message is the close, not the event: it was closed during the pairing.
                with pytest.raises(WebSocketDisconnect) as closed:
                    socket_a.receive_text()
                assert closed.value.code == 1008
                assert engine.broadcaster.client_count == 1
                assert token_b not in json.dumps(event) and digest(token_b) not in json.dumps(event)

        # A's HTTP requests and new sockets are refused; B keeps working.
        assert client.get("/api/tasks").status_code == 200
        client.cookies.set(COOKIE, token_a)
        assert client.get("/api/tasks").status_code == 401
        with pytest.raises(WebSocketDisconnect):
            with client.websocket_connect(WS, headers=ORIGIN):
                pass
    log = restarted.state.diagnostic_path.read_text(encoding="utf-8")
    for secret in (token_a, token_b, digest(token_a), digest(token_b)):
        assert secret not in log


def test_a_rejected_pairing_code_leaves_the_current_socket_open(make_app, engine):  # noqa: F811
    app = make_app()
    with TestClient(app, base_url="http://127.0.0.1") as client:
        code = app.state.human.pairing_code
        client.post("/api/auth/pair", json={"code": code}, headers=ORIGIN).raise_for_status()
        with client.websocket_connect(WS, headers=ORIGIN) as socket:
            assert client.post("/api/auth/pair", json={"code": code}, headers=ORIGIN).status_code == 401
            assert client.put("/api/tasks/TEST-1/budget", json={"amount": 5}, headers=ORIGIN).status_code == 200
            assert socket.receive_json()["type"] == "task"


class FakeSocket:
    def __init__(self, accept_gate: asyncio.Event | None = None, send_gate: asyncio.Event | None = None):
        self.accept_gate, self.send_gate = accept_gate, send_gate
        self.sending = asyncio.Event()
        self.sent: list[str] = []
        self.closed: int | None = None

    async def accept(self):
        if self.accept_gate:
            await self.accept_gate.wait()

    async def send_text(self, text):
        self.sending.set()
        if self.send_gate:
            await self.send_gate.wait()
        self.sent.append(text)

    async def close(self, code=1000):
        self.closed = code


def session_check(human: HumanAccess):
    """What the /api/ws handshake hands the broadcaster for a socket it authorizes now."""
    generation = human.generation
    return lambda: human.generation == generation


@pytest.mark.asyncio
async def test_a_handshake_racing_pairing_cannot_register_the_replaced_session(tmp_path):
    human = HumanAccess(tmp_path / "browser-session")
    broadcaster = Broadcaster()
    gate = asyncio.Event()
    socket = FakeSocket(accept_gate=gate)
    # The cookie was valid when checked, but pairing replaces the session before the accept completes.
    connecting = asyncio.create_task(broadcaster.connect(socket, session_check(human)))
    await asyncio.sleep(0)
    human.pair(human.pairing_code)
    await broadcaster.revoke()  # nothing registered yet to close
    gate.set()
    assert await connecting is False
    assert socket.closed == 1008 and broadcaster.client_count == 0
    await broadcaster.broadcast({"type": "task"})
    assert socket.sent == []


@pytest.mark.asyncio
async def test_a_broadcast_in_progress_skips_sockets_revoked_meanwhile(tmp_path):
    human = HumanAccess(tmp_path / "browser-session")
    broadcaster = Broadcaster()
    gate = asyncio.Event()
    slow, waiting = FakeSocket(send_gate=gate), FakeSocket()
    assert await broadcaster.connect(slow, session_check(human))
    assert await broadcaster.connect(waiting, session_check(human))
    sending = asyncio.create_task(broadcaster.broadcast({"type": "task", "jiraKey": "TEST-1"}))
    await slow.sending.wait()  # the broadcast has its client list and is blocked on the first send
    human.pair(human.pairing_code)
    await broadcaster.revoke()
    assert slow.closed == waiting.closed == 1008 and broadcaster.client_count == 0
    gate.set()
    await sending
    assert waiting.sent == []  # queued behind the slow send, then revoked: never delivered
    await broadcaster.broadcast({"type": "task", "jiraKey": "TEST-1"})
    assert len(slow.sent) == 1 and waiting.sent == []  # only the send already under way completed


@pytest.mark.asyncio
async def test_only_sockets_of_the_replaced_session_are_closed(tmp_path):
    human = HumanAccess(tmp_path / "browser-session")
    broadcaster = Broadcaster()
    old = FakeSocket()
    assert await broadcaster.connect(old, session_check(human))
    human.pair(human.pairing_code)
    await broadcaster.revoke()
    new = FakeSocket()
    assert await broadcaster.connect(new, session_check(human))
    await broadcaster.revoke()  # e.g. a rejected pairing attempt: the current session stays
    await broadcaster.broadcast({"type": "task"})
    assert old.closed == 1008 and old.sent == []
    assert new.closed is None and len(new.sent) == 1
