"""Projects: named groups of tasks with New Task form defaults (docs: the Projects plan artifact)."""
import sqlite3
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from pydantic import ValidationError
from sqlalchemy import select

from agentflow.database.database import Database
from agentflow.database.models import Project, Task, WorkflowEvent
from agentflow.services.git_workspace import CreatedWorktree, GitError
from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.models import CreateTaskRequest, ProjectRequest
from agentflow.workflow.projects import PROJECT_COLORS, slug_base, unique_slug
from test_task_archive_delete import paired
from test_workflow_safety import client, engine  # noqa: F401 - fixtures


def project(**fields):
    return ProjectRequest.model_validate(fields)


def task_project(engine, key="TEST-1"):
    with engine.db.session() as session:
        return engine._load_task(session, key).project_id


def events(engine):
    with engine.db.session() as session:
        return [e.message for e in session.scalars(select(WorkflowEvent).order_by(WorkflowEvent.id))]


@pytest.fixture
def repo(engine, tmp_path):
    """A repository root with two app directories; the git inspection is stubbed to it."""
    root = tmp_path / "mono"
    (root / "Apps" / "Billing").mkdir(parents=True)
    (root / "Apps" / "Accounts").mkdir(parents=True)
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root=str(root), path=str(root)))
    return root


# naming --------------------------------------------------------------------------------------

@pytest.mark.parametrize("name, slug", [
    ("Billing App", "billing-app"),
    ("  Accounts: Statements & PDFs!  ", "accounts-statements-pdfs"),
    ("日本", "project"),
    ("x" * 100, "x" * 48),
])
def test_slug_is_lowercase_ascii_words(name, slug):
    assert slug_base(name) == slug


def test_slug_skips_taken_and_reserved_ones():
    assert unique_slug("None", set()) == "none-2"
    assert unique_slug("Billing", {"billing", "billing-2"}) == "billing-3"
    assert unique_slug("y" * 60, {"y" * 48}) == "y" * 46 + "-2"


def test_an_unknown_colour_is_refused():
    with pytest.raises(ValidationError, match="Colour must be one of"):
        project(name="Billing", color="magenta")


# create, edit, archive, delete -----------------------------------------------------------------

@pytest.mark.asyncio
async def test_create_picks_a_slug_and_the_least_used_colour(engine):
    first = await engine.create_project(project(name="  Billing App  "))
    second = await engine.create_project(project(name="Admin App", description="Back office"))
    assert (first["name"], first["slug"], first["color"]) == ("Billing App", "billing-app", PROJECT_COLORS[0])
    assert second["color"] == PROJECT_COLORS[1]
    assert second["description"] == "Back office"
    assert first["agentOverrides"] == {} and first["archivedAt"] is None and first["createdAt"]
    assert [p["name"] for p in engine.list_projects()] == ["Admin App", "Billing App"]


@pytest.mark.asyncio
async def test_a_project_needs_a_unique_name_ignoring_case(engine):
    with pytest.raises(WorkflowError, match="needs a name"):
        await engine.create_project(project(name="   "))
    created = await engine.create_project(project(name="Billing"))
    with pytest.raises(WorkflowError, match="already exists") as refused:
        await engine.create_project(project(name="BILLING"))
    assert refused.value.status_code == 409
    renamed = await engine.update_project(created["id"], project(name="billing"))
    assert renamed["name"] == "billing"
    with pytest.raises(WorkflowError, match="needs a name"):
        await engine.update_project(created["id"], project(name=None))


@pytest.mark.asyncio
async def test_renaming_keeps_the_slug(engine):
    created = await engine.create_project(project(name="Billing App"))
    renamed = await engine.update_project(created["id"], project(name="Billing and Invoices"))
    assert renamed["slug"] == "billing-app"
    assert (await engine.create_project(project(name="Billing App")))["slug"] == "billing-app-2"


@pytest.mark.asyncio
async def test_an_edit_changes_only_the_fields_it_sends(engine):
    created = await engine.create_project(project(name="Billing", description="Invoice pages", color="teal",
                                                  defaultBaseBranch="develop"))
    unchanged = await engine.update_project(created["id"], project())
    assert (unchanged["description"], unchanged["color"], unchanged["defaultBaseBranch"]) == ("Invoice pages", "teal", "develop")
    cleared = await engine.update_project(created["id"], project(description=None, defaultBaseBranch="  "))
    assert cleared["description"] is None and cleared["defaultBaseBranch"] is None
    assert cleared["color"] == "teal"


@pytest.mark.asyncio
async def test_default_repository_and_working_directory_are_checked(engine, repo):
    created = await engine.create_project(project(name="Billing", defaultRepositoryPath=str(repo),
                                                  defaultProjectRelativePath="Apps\\Billing\\"))
    assert created["defaultRepositoryPath"] == str(repo)
    assert created["defaultProjectRelativePath"] == "Apps/Billing"
    # Changing the directory alone is checked against the repository already stored.
    moved = await engine.update_project(created["id"], project(defaultProjectRelativePath="Apps/Accounts"))
    assert moved["defaultProjectRelativePath"] == "Apps/Accounts"
    root = await engine.update_project(created["id"], project(defaultProjectRelativePath="."))
    assert root["defaultProjectRelativePath"] is None

    with pytest.raises(WorkflowError, match="does not exist"):
        await engine.update_project(created["id"], project(defaultProjectRelativePath="Apps/Nope"))
    with pytest.raises(WorkflowError, match="inside the repository"):
        await engine.update_project(created["id"], project(defaultProjectRelativePath=".."))
    with pytest.raises(WorkflowError, match="needs a default repository"):
        await engine.create_project(project(name="Admin", defaultProjectRelativePath="Apps/Billing"))

    cleared = await engine.update_project(created["id"], project(defaultRepositoryPath=None))
    assert cleared["defaultRepositoryPath"] is None and cleared["defaultProjectRelativePath"] is None

    engine.git.inspect_repository = AsyncMock(side_effect=GitError("Not a git repository: D:/nowhere"))
    with pytest.raises(WorkflowError, match="Not a git repository"):
        await engine.update_project(created["id"], project(defaultRepositoryPath="D:/nowhere"))


@pytest.mark.asyncio
async def test_agent_overrides_drop_empty_roles(engine):
    created = await engine.create_project(project(name="Billing", agentOverrides={
        "implementer": {"harness": "codex", "model": None}, "code_reviewer": {}}))
    assert created["agentOverrides"] == {"implementer": {"harness": "codex"}}
    cleared = await engine.update_project(created["id"], project(agentOverrides=None))
    assert cleared["agentOverrides"] == {}


@pytest.mark.asyncio
async def test_project_changes_are_broadcast(engine):
    engine.broadcaster.broadcast = AsyncMock()
    created = await engine.create_project(project(name="Billing"))
    await engine.set_project_archived(created["id"], True)
    messages = [call.args[0] for call in engine.broadcaster.broadcast.await_args_list]
    assert [m["type"] for m in messages] == ["projects", "projects"]
    assert messages[-1]["projects"][0]["archivedAt"]


@pytest.mark.asyncio
async def test_unknown_project_is_404(engine):
    for action in (engine.update_project(9, project(name="x")), engine.set_project_archived(9, True),
                   engine.delete_project(9), engine.set_task_project("TEST-1", 9)):
        with pytest.raises(WorkflowError, match="Project 9 not found") as missing:
            await action
        assert missing.value.status_code == 404


# assigning tasks -------------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_a_task_moves_between_projects_and_out(engine):
    billing = await engine.create_project(project(name="Billing"))
    admin = await engine.create_project(project(name="Admin"))
    snapshot = await engine.set_task_project("TEST-1", billing["id"])
    assert snapshot["projectId"] == billing["id"]
    assert [t["projectId"] for t in await engine.list_tasks()] == [billing["id"]]
    await engine.set_task_project("TEST-1", admin["id"])
    assert (await engine.set_task_project("TEST-1", None))["projectId"] is None
    assert [m for m in events(engine) if "project" in m] == [
        "Moved to project Billing", "Moved to project Admin", "Removed from project Admin"]


@pytest.mark.asyncio
async def test_an_archived_project_takes_no_new_tasks_but_keeps_its_own(engine):
    billing = await engine.create_project(project(name="Billing"))
    await engine.set_task_project("TEST-1", billing["id"])
    archived = await engine.set_project_archived(billing["id"], True)
    assert archived["archivedAt"]
    # Already there: nothing to refuse.
    assert (await engine.set_task_project("TEST-1", billing["id"]))["projectId"] == billing["id"]
    await engine.set_task_project("TEST-1", None)
    with pytest.raises(WorkflowError, match="archived") as refused:
        await engine.set_task_project("TEST-1", billing["id"])
    assert refused.value.status_code == 409
    assert (await engine.set_project_archived(billing["id"], False))["archivedAt"] is None
    assert (await engine.set_task_project("TEST-1", billing["id"]))["projectId"] == billing["id"]


@pytest.mark.asyncio
async def test_deleting_a_project_keeps_its_tasks_and_publishes_them(engine):
    billing = await engine.create_project(project(name="Billing"))
    await engine.set_task_project("TEST-1", billing["id"])
    engine.broadcaster.broadcast = AsyncMock()
    result = await engine.delete_project(billing["id"])
    assert result == {"deleted": billing["id"], "unassignedTasks": ["TEST-1"]}
    assert task_project(engine) is None
    assert engine.list_projects() == []
    messages = [call.args[0] for call in engine.broadcaster.broadcast.await_args_list]
    assert messages[0] == {"type": "projects", "projects": []}
    assert messages[1]["type"] == "task" and messages[1]["task"]["projectId"] is None
    assert events(engine)[-1] == "Removed from project Billing, which was deleted"


# creating a task in a project ------------------------------------------------------------------

def creating(engine, repo):
    engine.git.create_worktree = AsyncMock(return_value=CreatedWorktree("agent/LOCAL"))
    engine.zellij.ensure_session = AsyncMock(return_value=False)
    return lambda project_id: CreateTaskRequest(taskSource="local", prompt="Fix the billing page",
                                                repositoryPath=str(repo), baseBranch="main", projectId=project_id)


@pytest.mark.asyncio
async def test_a_new_task_can_start_in_a_project(engine, repo):
    request = creating(engine, repo)
    billing = await engine.create_project(project(name="Billing"))
    snapshot = await engine.create_task(request(billing["id"]))
    assert snapshot["projectId"] == billing["id"]


@pytest.mark.asyncio
async def test_a_new_task_is_refused_an_unknown_or_archived_project_before_any_worktree(engine, repo):
    request = creating(engine, repo)
    with pytest.raises(WorkflowError, match="not found"):
        await engine.create_task(request(42))
    archived = await engine.create_project(project(name="Old"))
    await engine.set_project_archived(archived["id"], True)
    with pytest.raises(WorkflowError, match="archived"):
        await engine.create_task(request(archived["id"]))
    engine.git.create_worktree.assert_not_awaited()


@pytest.mark.asyncio
async def test_a_project_deleted_during_creation_leaves_the_task_without_one(engine, repo):
    request = creating(engine, repo)
    billing = await engine.create_project(project(name="Billing"))

    async def delete_meanwhile(*args):
        await engine.delete_project(billing["id"])
        return CreatedWorktree("agent/LOCAL")
    engine.git.create_worktree = AsyncMock(side_effect=delete_meanwhile)
    snapshot = await engine.create_task(request(billing["id"]))
    assert snapshot["projectId"] is None
    assert "The chosen project was deleted while the task was being created, so the task has no project" in events(engine)


# HTTP --------------------------------------------------------------------------------------------

def test_projects_over_http(client, engine):
    headers = paired(client)
    created = client.post("/api/projects", json={"name": "Billing", "color": "teal"}, headers=headers)
    assert created.status_code == 201
    project_id = created.json()["id"]
    assert client.post("/api/projects", json={"name": "Admin", "color": "magenta"}, headers=headers).status_code == 422
    assert client.post("/api/projects", json={"name": "billing"}, headers=headers).status_code == 409
    assert client.patch(f"/api/projects/{project_id}", json={"description": "Invoice pages"},
                        headers=headers).json()["description"] == "Invoice pages"
    assert client.put("/api/tasks/TEST-1/project", json={"projectId": project_id},
                      headers=headers).json()["projectId"] == project_id
    assert client.put("/api/tasks/TEST-1/project", json={}, headers=headers).status_code == 422
    assert client.get("/api/tasks", headers=headers).json()[0]["projectId"] == project_id
    assert client.post(f"/api/projects/{project_id}/archive", headers=headers).json()["archivedAt"]
    assert client.post(f"/api/projects/{project_id}/unarchive", headers=headers).json()["archivedAt"] is None
    assert [p["slug"] for p in client.get("/api/projects", headers=headers).json()] == ["billing"]
    assert client.delete(f"/api/projects/{project_id}", headers=headers).json()["unassignedTasks"] == ["TEST-1"]
    assert client.delete(f"/api/projects/{project_id}", headers=headers).status_code == 404


def test_projects_are_human_actions(client):
    agent = {"Authorization": "Bearer test-token"}
    assert client.get("/api/projects", headers=agent).status_code == 403
    assert client.post("/api/projects", json={"name": "x"}, headers=agent).status_code == 403
    assert client.put("/api/tasks/TEST-1/project", json={"projectId": None}, headers=agent).status_code == 403
    assert client.get("/api/projects").status_code == 401
    headers = paired(client)
    assert client.post("/api/projects", json={"name": "x"},
                       headers={"Origin": "http://evil.invalid"}).status_code == 403
    assert client.get("/api/projects", headers=headers).status_code == 200


# upgrading an existing database ----------------------------------------------------------------

# tasks as created before projects existed (app/crates/core/src/db/schema.rs before this change).
OLD_TASKS = """CREATE TABLE tasks (
	id INTEGER NOT NULL, jira_key VARCHAR(64) NOT NULL, api_budget_usd FLOAT, jira_title TEXT,
	repository_path TEXT NOT NULL, repository_root TEXT NOT NULL, project_relative_path TEXT,
	base_branch VARCHAR(255) NOT NULL, branch_name VARCHAR(255) NOT NULL, worktree_path TEXT NOT NULL,
	zellij_session_name VARCHAR(255) NOT NULL, task_dir TEXT NOT NULL, additional_context TEXT,
	status VARCHAR(32) NOT NULL, status_message TEXT, review_iteration INTEGER NOT NULL,
	max_review_iterations INTEGER NOT NULL, max_requirements_revisions INTEGER NOT NULL, current_sha VARCHAR(64),
	pr_url TEXT, pr_created_at DATETIME, merged_at DATETIME, pr_sync_paused BOOLEAN DEFAULT '0' NOT NULL,
	merged_head_sha VARCHAR(64), archived_at DATETIME, agent_overrides JSON NOT NULL,
	created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL, PRIMARY KEY (id));
CREATE UNIQUE INDEX ix_tasks_jira_key ON tasks (jira_key);
INSERT INTO tasks VALUES (1, 'OLD-1', NULL, NULL, '/r', '/r', NULL, 'main', 'agent/OLD-1', '/w', 'agentflow-OLD-1',
	'/t', NULL, 'MERGED', NULL, 1, 5, 3, NULL, NULL, NULL, NULL, 0, NULL, NULL, '{}',
	'2025-01-01 00:00:00.000000', '2025-01-01 00:00:00.000000');"""


def test_upgrade_adds_the_project_column_and_its_index(tmp_path):
    path = tmp_path / "old.db"
    connection = sqlite3.connect(path)
    connection.executescript(OLD_TASKS)
    connection.close()
    db = Database(path)
    db.create_all()
    db.create_all()
    with db.session() as session:
        assert session.scalar(select(Task)).project_id is None
        session.add(Project(slug="billing", name="Billing", color="blue", agent_overrides={}))
        session.flush()
        session.scalar(select(Task)).project_id = session.scalar(select(Project)).id
    db.engine.dispose()
    connection = sqlite3.connect(path)
    assert [r[2] for r in connection.execute("PRAGMA index_info('ix_tasks_project_id')")] == ["project_id"]
    assert [(r[2], r[3], r[4]) for r in connection.execute("PRAGMA foreign_key_list('tasks')")] == [
        ("projects", "project_id", "id")]
    connection.close()
