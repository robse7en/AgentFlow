"""Configuration and system routes: /api/config and its edits, reload, harnesses, directory browsing
and the diagnostic log."""

from __future__ import annotations

import json
import os
import sys

import httpx
import pytest

import golden


@pytest.fixture(scope="module")
def backend(launch):
    with launch(STUB_MANUAL="1") as backend:
        yield backend


@pytest.fixture(scope="module")
def human(backend):
    return backend.human()


def on_disk(backend) -> dict:
    return json.loads(backend.config_file.read_text(encoding="utf-8"))


def test_config_describes_the_running_backend(backend, human):
    config = human.ok("GET", "/api/config")
    assert config["configPath"] == str(backend.config_file)
    assert config["databasePath"] == backend.config["app"]["databasePath"]
    assert config["platform"] == sys.platform
    assert config["workflow"]["maxReviewIterations"] == 3
    assert config["jiraEnabled"] is False
    assert [h["key"] for h in config["harnesses"]] == ["stub"]
    stub = config["harnesses"][0]
    assert stub["available"] is True and stub["type"] == "generic-cli" and stub["displayName"] == "Stub Agent"
    assert config["agents"]["orchestrator"]["harness"] == "stub", "an unset orchestrator runs as the architect"
    golden.check_shape("system-config", config)  # not config.json: .gitignore keeps that name out


def test_harness_routes_list_and_redetect(human):
    listed = human.ok("GET", "/api/harnesses")
    assert listed == human.ok("GET", "/api/config")["harnesses"]
    assert human.ok("POST", "/api/harnesses/detect") == listed


def test_default_repository_is_trimmed_persisted_and_cleared(backend, human):
    before = on_disk(backend)
    result = human.ok("PUT", "/api/config/default-repository", json={"path": "  D:/Repos/Thing  "})
    assert result["defaultRepositoryDirectory"] == "D:/Repos/Thing"
    expected = {**before, "workspace": {**before["workspace"], "defaultRepositoryDirectory": "D:/Repos/Thing"}}
    # json.dumps(raw, indent=2) + "\n", written in text mode (CRLF on Windows).
    text = json.dumps(expected, indent=2) + "\n"
    assert backend.config_file.read_bytes() == text.replace("\n", os.linesep).encode("utf-8")
    cleared = human.ok("PUT", "/api/config/default-repository", json={"path": None})
    assert cleared["defaultRepositoryDirectory"] is None
    assert on_disk(backend)["workspace"]["defaultRepositoryDirectory"] is None


def test_exchange_rate_must_be_positive(backend, human):
    assert human.put("/api/config/exchange-rate", json={"usdToNzdRate": 0}).status_code == 422
    result = human.ok("PUT", "/api/config/exchange-rate", json={"usdToNzdRate": 1.75})
    assert result["workflow"]["usdToNzdRate"] == 1.75
    assert on_disk(backend)["workflow"]["usdToNzdRate"] == 1.75


def test_cost_controls_and_billing_modes_persist(backend, human):
    payload = {"controls": {"dailyBudgetUsd": 20, "copilotRoles": ["implementer"]}, "billing": {"stub": "subscription"}}
    result = human.ok("PUT", "/api/config/cost-controls", json=payload)
    assert result["workflow"]["costControls"]["dailyBudgetUsd"] == 20
    assert result["harnesses"][0]["billingMode"] == "subscription"
    saved = on_disk(backend)
    assert saved["workflow"]["costControls"] == result["workflow"]["costControls"]
    assert saved["harnesses"]["stub"]["billingMode"] == "subscription"
    assert saved["harnesses"]["stub"]["command"] == backend.config["harnesses"]["stub"]["command"]
    unknown = human.put("/api/config/cost-controls", json={"controls": {}, "billing": {"nope": "metered"}})
    assert unknown.status_code == 422
    invalid = human.put("/api/config/cost-controls", json={"controls": {}, "billing": {"stub": "free"}})
    assert invalid.status_code == 422


def test_reload_refuses_restart_only_changes_and_invalid_files(backend, human):
    original = backend.config_file.read_bytes()
    try:
        moved = on_disk(backend)
        moved["server"]["port"] = backend.port + 1
        backend.config_file.write_text(json.dumps(moved), encoding="utf-8")
        refused = human.post("/api/config/reload")
        assert refused.status_code == 409
        assert refused.json()["detail"] == "Restart AgentFlow to change database, data directory, host or port"
        backend.config_file.write_text("{not json", encoding="utf-8")
        assert human.post("/api/config/reload").status_code == 400
    finally:
        backend.config_file.write_bytes(original)
    assert human.post("/api/config/reload").status_code == 200


def test_directory_browsing(human, tmp_path):
    for name in ("b-dir", "A-dir", ".hidden", "node_modules"):
        (tmp_path / name).mkdir()
    (tmp_path / "file.txt").write_text("x", encoding="utf-8")
    (tmp_path / "A-dir" / ".git").mkdir()
    top = human.ok("GET", "/api/fs/list")
    assert top["path"] is None and top["roots"] and top["entries"]
    listing = human.ok("GET", "/api/fs/list", params={"path": str(tmp_path)})
    assert [e["name"] for e in listing["entries"]] == ["A-dir", "b-dir"]
    assert listing["isGitRepo"] is False
    assert human.ok("GET", "/api/fs/list", params={"path": str(tmp_path / "A-dir")})["isGitRepo"] is True
    golden.check_shape("fs-list", listing)
    # A root-constrained listing never leaves its root.
    rooted = human.ok("GET", "/api/fs/list", params={"root": str(tmp_path), "path": "../.."})
    assert rooted["path"] == rooted["root"] and rooted["relative"] == "" and rooted["parent"] is None
    inside = human.ok("GET", "/api/fs/list", params={"root": str(tmp_path), "path": "A-dir"})
    assert inside["relative"] == "A-dir" and inside["parent"] == inside["root"]
    golden.check_shape("fs-list-rooted", inside)
    assert human.get("/api/fs/list", params={"path": str(tmp_path / "missing")}).status_code == 400
    assert human.get("/api/fs/list", params={"root": str(tmp_path / "missing")}).status_code == 400


def test_diagnostics_need_a_human_session(backend, human):
    response = human.get("/api/diagnostics")
    assert response.status_code == 200 and response.headers["x-request-id"]
    assert int(response.headers["content-length"]) == len(response.content)
    events = [json.loads(line)["event"] for line in response.text.splitlines() if line.strip()]
    assert "application.starting" in events and "request.completed" in events
    agent = httpx.get(f"{backend.url}/api/diagnostics", headers={"Authorization": "Bearer secret"})
    assert agent.status_code == 403 and agent.json() == {"detail": "Agents cannot perform human actions"}
    anonymous = httpx.get(f"{backend.url}/api/diagnostics")
    assert anonymous.status_code == 401
    assert anonymous.json() == {"detail": "Pair this browser using the code in the backend terminal"}


def test_every_response_carries_a_request_id(backend):
    first = httpx.get(f"{backend.url}/api/health").headers["x-request-id"]
    second = httpx.get(f"{backend.url}/api/health").headers["x-request-id"]
    assert first and second and first != second
