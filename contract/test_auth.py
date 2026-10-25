"""Human access: health, browser pairing, the session cookie, origin and host checks, and /api/ws."""

from __future__ import annotations

import json

import httpx
import pytest
from websockets.exceptions import ConnectionClosed, InvalidStatus
from websockets.sync.client import connect

from support import new_key


@pytest.fixture(scope="module")
def backend(launch):
    with launch(STUB_MANUAL="1") as backend:
        yield backend


@pytest.fixture(scope="module")
def human(backend):
    return backend.human()


def test_health_needs_no_session(backend):
    response = httpx.get(f"{backend.url}/api/health")
    assert response.status_code == 200
    body = response.json()
    assert body["ok"] is True and isinstance(body["version"], str)


def test_ui_routes_need_a_paired_session(backend, human):
    assert httpx.get(f"{backend.url}/api/tasks").status_code == 401
    assert httpx.get(f"{backend.url}/api/tasks", cookies={"agentflow_human": "forged"}).status_code == 401


def test_pairing_rejects_a_wrong_code(backend, human):
    response = httpx.post(f"{backend.url}/api/auth/pair", json={"code": "wrong"}, headers={"Origin": backend.url})
    assert response.status_code == 401


def test_pairing_code_is_single_use(backend, human):
    used = backend.pairing_code()  # the human fixture already paired with it
    response = httpx.post(f"{backend.url}/api/auth/pair", json={"code": used}, headers={"Origin": backend.url})
    assert response.status_code == 401


def test_pairing_needs_the_local_ui_origin(backend):
    for headers in ({}, {"Origin": "http://evil.example"}):
        response = httpx.post(f"{backend.url}/api/auth/pair", json={"code": "x"}, headers=headers)
        assert response.status_code == 403, headers


def test_pairing_rejects_a_malformed_body(backend):
    response = httpx.post(f"{backend.url}/api/auth/pair", json={"nope": 1}, headers={"Origin": backend.url})
    assert response.status_code == 422


def test_paired_session_reads_without_origin_but_mutates_only_from_the_ui(backend, human):
    cookies = human.client.cookies
    assert httpx.get(f"{backend.url}/api/tasks", cookies=cookies).status_code == 200
    assert httpx.get(f"{backend.url}/api/auth/session", cookies=cookies).json() == {"ok": True}
    for headers in ({}, {"Origin": "http://evil.example"}):
        response = httpx.post(f"{backend.url}/api/config/reload", cookies=cookies, headers=headers)
        assert response.status_code == 403, headers
    assert human.post("/api/config/reload").status_code == 200


def test_vite_dev_server_origin_is_allowed(backend, human):
    response = httpx.post(f"{backend.url}/api/config/reload", cookies=human.client.cookies,
                          headers={"Origin": "http://localhost:5173"})
    assert response.status_code == 200


def test_unknown_host_header_is_rejected(backend):
    assert httpx.get(f"{backend.url}/api/health", headers={"Host": "evil.example"}).status_code == 400


def _ws_url(backend) -> str:
    return backend.url.replace("http://", "ws://") + "/api/ws"


def test_websocket_requires_a_session(backend):
    with pytest.raises(InvalidStatus):
        with connect(_ws_url(backend), origin=backend.url, open_timeout=10) as ws:
            ws.recv(timeout=5)


def test_websocket_pushes_task_snapshots(backend, human, repo):
    cookie = "; ".join(f"{k}={v}" for k, v in human.client.cookies.items())
    with connect(_ws_url(backend), origin=backend.url, additional_headers={"Cookie": cookie}, open_timeout=10) as ws:
        key = new_key("WS")
        human.create_task(key, repo)
        while True:
            message = json.loads(ws.recv(timeout=30))
            if message.get("type") == "task" and message["task"]["jiraKey"] == key:
                break
        assert message["task"]["nodes"], "a pushed snapshot carries the full task"
        ws.send("ping")  # keep-alive frames from the UI are accepted and ignored


def test_session_survives_a_backend_restart(launch):
    with launch(STUB_MANUAL="1") as backend:
        cookies = backend.human().client.cookies
        backend.stop()
        backend.start()
        assert httpx.get(f"{backend.url}/api/tasks", cookies=cookies).status_code == 200


def test_a_replaced_session_stops_receiving_events(launch, repo):
    """AF-AUDIT-004: pairing a second browser replaces the session, so the first browser's open
    event stream closes and receives nothing published after it. Pairing codes are single-use, so
    this needs a restart: the first session survives it, then the new code pairs the second."""
    with launch(STUB_MANUAL="1") as backend:
        first = backend.human()
        backend.stop()
        backend.start()
        cookie = lambda human: "; ".join(f"{k}={v}" for k, v in human.client.cookies.items())
        with connect(_ws_url(backend), origin=backend.url, additional_headers={"Cookie": cookie(first)},
                     open_timeout=10) as old:
            second = backend.human()
            with connect(_ws_url(backend), origin=backend.url, additional_headers={"Cookie": cookie(second)},
                         open_timeout=10) as new:
                key = new_key("RV")
                second.create_task(key, repo)
                while True:
                    message = json.loads(new.recv(timeout=30))
                    if message.get("type") == "task" and message["task"]["jiraKey"] == key:
                        break
                with pytest.raises(ConnectionClosed):
                    while True:
                        message = json.loads(old.recv(timeout=10))
                        assert message.get("type") != "task", "a replaced session received a task event"
        assert httpx.get(f"{backend.url}/api/tasks", cookies=first.client.cookies).status_code == 401
