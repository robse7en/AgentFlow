import pytest

from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.states import TaskStatus
from test_workflow_safety import SHA, client, engine


@pytest.fixture
def ready(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.status = TaskStatus.READY_FOR_PR
        task.current_sha = SHA
    return engine


@pytest.mark.asyncio
async def test_pr_lifecycle_advances_and_records_url(ready):
    snap = await ready.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/1")
    assert snap["status"] == "PR_CREATED"
    assert snap["prUrl"] == "https://github.com/acme/repo/pull/1"
    assert snap["prCreatedAt"] is not None

    snap = await ready.mark_pr_merged("TEST-1")
    assert snap["status"] == "MERGED"
    assert snap["mergedAt"] is not None
    assert snap["prUrl"] == "https://github.com/acme/repo/pull/1"


@pytest.mark.asyncio
async def test_pr_lifecycle_rejects_out_of_order_transitions(ready):
    with pytest.raises(WorkflowError, match="must have a PR created"):
        await ready.mark_pr_merged("TEST-1")
    with pytest.raises(WorkflowError, match="not marked PR Created"):
        await ready.revert_pr_created("TEST-1")
    with pytest.raises(WorkflowError, match="not marked Merged"):
        await ready.revert_pr_merged("TEST-1")

    await ready.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/1")
    with pytest.raises(WorkflowError, match="must be Ready for PR"):
        await ready.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/2")


@pytest.mark.asyncio
async def test_pr_lifecycle_is_reversible(ready):
    await ready.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/1")
    await ready.mark_pr_merged("TEST-1")

    snap = await ready.revert_pr_merged("TEST-1")
    assert snap["status"] == "PR_CREATED"
    assert snap["mergedAt"] is None
    assert snap["prUrl"] == "https://github.com/acme/repo/pull/1"

    snap = await ready.revert_pr_created("TEST-1")
    assert snap["status"] == "READY_FOR_PR"
    assert snap["prUrl"] is None
    assert snap["prCreatedAt"] is None


@pytest.mark.asyncio
async def test_no_further_nodes_approvals_or_agent_signals_once_pr_created(ready):
    await ready.mark_pr_created("TEST-1", "https://github.com/acme/repo/pull/1")
    with pytest.raises(WorkflowError, match="no longer accepts approvals"):
        await ready.approve("TEST-1", "requirements_gate")
    with ready.db.session() as session:
        task = ready._load_task(session, "TEST-1")
        node = ready._node(task, "implementer")
        node.state = "FAILED"
    with pytest.raises(WorkflowError, match="nodes can no longer run"):
        await ready.run_node("TEST-1", "implementer")


def test_mark_pr_created_endpoint_validates_url_and_auth(client, ready):
    url = "/api/tasks/TEST-1/pr/created"
    payload = {"prUrl": "https://github.com/acme/repo/pull/1"}
    assert client.post(url, json=payload).status_code == 401
    origin = {"Origin": "http://127.0.0.1"}
    code = client.app.state.human.pairing_code
    assert client.post("/api/auth/pair", json={"code": code}, headers=origin).status_code == 200
    assert client.post(url, json={"prUrl": "not-a-url"}, headers=origin).status_code == 422
    result = client.post(url, json=payload, headers=origin)
    assert result.status_code == 200
    assert result.json()["status"] == "PR_CREATED"

    merge_result = client.post("/api/tasks/TEST-1/pr/merged", headers=origin)
    assert merge_result.status_code == 200
    assert merge_result.json()["status"] == "MERGED"

    revert_result = client.post("/api/tasks/TEST-1/pr/merged/revert", headers=origin)
    assert revert_result.status_code == 200
    assert revert_result.json()["status"] == "PR_CREATED"
