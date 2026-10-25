"""Projects through the UI API: create, edit, archive, delete, and the tasks assigned to them."""

from __future__ import annotations

import json
from pathlib import Path

import httpx
import pytest
from websockets.sync.client import connect

import golden
from support import new_key


@pytest.fixture(scope="module")
def backend(launch):
    with launch(STUB_MANUAL="1") as backend:
        yield backend


@pytest.fixture(scope="module")
def human(backend):
    return backend.human()


def create(human, **fields):
    return human.ok("POST", "/api/projects", json={"name": new_key("Project"), **fields})


def test_create_edit_and_list(human, repo):
    name = new_key("Billing")
    created = human.ok("POST", "/api/projects", json={
        "name": f"  {name}  ", "description": "Invoice pages", "color": "teal",
        "defaultRepositoryPath": str(repo), "defaultProjectRelativePath": "src\\", "defaultBaseBranch": "main",
        "agentOverrides": {"implementer": {"harness": "codex", "model": None}, "code_reviewer": {}}})
    assert created["name"] == name and created["slug"] == name.lower()  # BILLING-1A2B3C -> billing-1a2b3c
    assert created["color"] == "teal" and created["description"] == "Invoice pages"
    assert Path(created["defaultRepositoryPath"]) == repo
    assert created["defaultProjectRelativePath"] == "src" and created["defaultBaseBranch"] == "main"
    assert created["agentOverrides"] == {"implementer": {"harness": "codex"}}
    assert created["archivedAt"] is None
    golden.check_shape("project", created)

    edited = human.ok("PATCH", f"/api/projects/{created['id']}", json={"name": f"{name} renamed", "description": None})
    assert edited["slug"] == created["slug"], "the slug outlives a rename"
    assert edited["description"] is None and edited["color"] == "teal"
    assert edited["defaultProjectRelativePath"] == "src", "fields not sent are kept"
    assert edited["id"] in [p["id"] for p in human.ok("GET", "/api/projects")]


def test_refusals(human):
    project = create(human)
    assert human.post("/api/projects", json={"name": project["name"].upper()}).status_code == 409
    assert human.post("/api/projects", json={"name": "   "}).status_code == 400
    assert human.post("/api/projects", json={"name": new_key("P"), "color": "magenta"}).status_code == 422
    assert human.post("/api/projects", json={"name": new_key("P"), "defaultProjectRelativePath": "src"}).status_code == 400
    assert human.client.patch("/api/projects/999999", json={"name": "x"}).status_code == 404
    assert human.client.delete("/api/projects/999999").status_code == 404


def test_none_is_reserved_for_the_no_project_filter(human):
    # The module's own backend: no other test makes a project named None.
    assert human.ok("POST", "/api/projects", json={"name": "None"})["slug"] == "none-2"
    assert human.ok("POST", "/api/projects", json={"name": "日本"})["slug"] == "project"


def test_tasks_join_leave_and_outlive_a_project(human, repo):
    project = create(human)
    key = new_key("PJ")
    snap = human.create_task(key, repo, projectId=project["id"])
    assert snap["projectId"] == project["id"]
    listed = [t for t in human.ok("GET", "/api/tasks") if t["jiraKey"] == key][0]
    assert listed["projectId"] == project["id"]

    assert human.put(f"/api/tasks/{key}/project", json={"projectId": None}).json()["projectId"] is None
    assert human.put(f"/api/tasks/{key}/project", json={"projectId": 999999}).status_code == 404
    human.ok("PUT", f"/api/tasks/{key}/project", json={"projectId": project["id"]})

    assert human.ok("POST", f"/api/projects/{project['id']}/archive")["archivedAt"]
    other = new_key("PJ")
    refused = human.post("/api/tasks", json={"jiraKey": other, "repositoryPath": str(repo), "baseBranch": "main",
                                             "projectId": project["id"]})
    assert refused.status_code == 409
    assert other not in [t["jiraKey"] for t in human.ok("GET", "/api/tasks")]
    assert human.ok("POST", f"/api/projects/{project['id']}/unarchive")["archivedAt"] is None

    deleted = human.ok("DELETE", f"/api/projects/{project['id']}")
    assert deleted == {"deleted": project["id"], "unassignedTasks": [key]}
    assert human.task(key)["projectId"] is None
    assert project["id"] not in [p["id"] for p in human.ok("GET", "/api/projects")]


def test_project_changes_are_pushed(backend, human):
    cookie = "; ".join(f"{k}={v}" for k, v in human.client.cookies.items())
    url = backend.url.replace("http://", "ws://") + "/api/ws"
    with connect(url, origin=backend.url, additional_headers={"Cookie": cookie}, open_timeout=10) as ws:
        project = create(human)
        while True:
            message = json.loads(ws.recv(timeout=30))
            if message.get("type") == "projects" and project["id"] in [p["id"] for p in message["projects"]]:
                break
        assert [p for p in message["projects"] if p["id"] == project["id"]][0] == project


def test_agents_cannot_manage_projects(backend):
    agent = {"Authorization": "Bearer not-a-human"}
    assert httpx.get(f"{backend.url}/api/projects", headers=agent).status_code == 403
    assert httpx.put(f"{backend.url}/api/tasks/ANY-1/project", json={"projectId": None}, headers=agent).status_code == 403
