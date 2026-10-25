from types import SimpleNamespace
from unittest.mock import patch

import pytest
from sqlalchemy import select

from agentflow.database.models import WorkflowEvent
from agentflow.services.chat import slack as slack_format
from agentflow.services.chat.base import ActionRequest, Button
from agentflow.workflow import default_workflow as wf
from test_chat import OPERATOR, FakeAdapter, eventually, started
from test_chat_answers import dispatch, needs_slack, slack_stubs
from test_chat_create import fake_create_task, slack_payload
from test_chat_messages import typed
from test_workflow_safety import engine  # noqa: F401 - fixture

DM = f"D-{OPERATOR}"
APPROVE = {"allowActions": ["approveRequirements", "requestChanges", "run"]}
DOCUMENTS = {"requirements.md": "Must <!channel> log in & out.", "acceptance-criteria.md": "Tests pass.",
             "implementation-plan.md": "Edit auth.py."}


async def gated_task(engine, documents=DOCUMENTS):
    """NEW-1: requirements written by the Architect, the gate waiting for approval, none approved yet."""
    await fake_create_task(engine)(SimpleNamespace(jira_key="NEW-1", repository_path="D:/r", base_branch="main"))
    with engine.db.session() as session:
        task = engine._load_task(session, "NEW-1")
        workspace = engine.workspace(task)
        for name, text in documents.items():
            (workspace.requirements_dir / name).write_text(text, encoding="utf-8")
        engine._node(task, wf.ARCHITECT).state = "COMPLETED"
        engine._node(task, wf.REQUIREMENTS_GATE).state = "WAITING_FOR_APPROVAL"
    return workspace


def action(kind, value):
    return ActionRequest(OPERATOR, DM, kind, value)


def approve_button(adapter):
    return next(button for buttons in adapter.buttons for button in buttons if button.kind == "approve_requirements")


@pytest.mark.asyncio
async def test_approving_from_slack_is_off_by_default(engine):
    await gated_task(engine)
    service = await started(engine, FakeAdapter())
    assert (await service._on_message(typed("approve NEW-1"))).text.startswith(
        "Approving requirements from Slack is turned off.")
    assert (await service._on_action(action("review_requirements", "NEW-1"))).startswith(
        "Approving requirements from Slack is turned off.")
    assert (await service._on_action(action("approve_requirements", "NEW-1|" + "0" * 64))).startswith(
        "Approving requirements from Slack is turned off.")
    await service.stop()


@pytest.mark.asyncio
async def test_review_posts_every_document_in_full_then_a_bound_approve_button(engine):
    await gated_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    assert (await service._on_message(typed("approve new-1"))).buttons == (
        Button("review_requirements", "Review requirements", "NEW-1"),)
    assert await service._on_action(action("review_requirements", "NEW-1")) is None

    posted = "\n".join(text for _, text, _ in adapter.sent)
    assert "*Requirements*\nMust &lt;!channel&gt; log in &amp; out." in posted  # escaped, not shortened
    assert "*Acceptance criteria*\nTests pass." in posted and "*Implementation plan*\nEdit auth.py." in posted
    assert "Assumptions" not in posted  # empty documents are not shown
    assert adapter.sent[-1][1] == "Approve these requirements for NEW-1?"
    digest = engine.snapshot("NEW-1")["requirementsDraftDigest"]
    assert adapter.buttons[-1] == (
        Button("approve_requirements", "Approve requirements", f"NEW-1|{digest}",
               "Approve the requirements posted above for NEW-1? The next stage can then start."),
        Button("request_changes", "Request changes…", "NEW-1"))
    await service.stop()


@pytest.mark.asyncio
async def test_the_approve_button_approves_exactly_what_was_posted(engine):
    await gated_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    await service._on_action(action("review_requirements", "NEW-1"))
    button = approve_button(adapter)
    assert await service._on_action(action(button.kind, button.value)) is None

    snapshot = engine.snapshot("NEW-1")
    nodes = {node["key"]: node["state"] for node in snapshot["nodes"]}
    assert nodes[wf.REQUIREMENTS_GATE] == "COMPLETED" and nodes[wf.IMPLEMENTER] == "READY"
    assert snapshot["approvedRequirements"]["requirements"] == DOCUMENTS["requirements.md"]
    with engine.db.session() as session:
        event = session.scalar(select(WorkflowEvent).where(WorkflowEvent.message.like("Requirements approved%")))
        assert event.message == "Requirements approved via Slack" and event.data["via"] == "Slack"
    # The next stage is offered, since READY notices (which would carry it) are off by default.
    assert adapter.sent[-1][1] == "Requirements approved. Start the next stage:"
    assert adapter.buttons[-1][0].value == "NEW-1|implementer"

    assert await service._on_action(action(button.kind, button.value)) == \
        "Not approved: Requirements Approval is not waiting for approval."
    await service.stop()


@pytest.mark.asyncio
async def test_requirements_changed_after_they_were_posted_are_not_approved(engine):
    workspace = await gated_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    await service._on_action(action("review_requirements", "NEW-1"))
    (workspace.requirements_dir / "implementation-plan.md").write_text("Edit auth.py and drop the tests.")
    button = approve_button(adapter)
    assert await service._on_action(action(button.kind, button.value)) == \
        "Not approved: Requirements changed since they were displayed; refresh and review again."
    assert await service._on_action(action(button.kind, "NEW-1|not-a-digest")) == \
        "That approval button is not valid; use Review requirements again."
    assert engine.snapshot("NEW-1")["approvedRequirements"] is None
    await service.stop()


@pytest.mark.asyncio
async def test_nothing_to_approve_unless_every_document_was_posted(engine):
    await gated_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    sends = adapter.send

    async def flaky(channel, text, thread_id=None, prompt=None, buttons=()):  # the second document fails
        if text.startswith("*Acceptance criteria*"):
            raise ConnectionError("slack unreachable")
        return await sends(channel, text, thread_id, prompt, buttons)
    adapter.send = flaky
    assert await service._on_action(action("review_requirements", "NEW-1")) == \
        "Could not post the requirements in full, so there is nothing to approve. Try again, or use AgentFlow."
    assert not any(b.kind == "approve_requirements" for buttons in adapter.buttons for b in buttons)
    await service.stop()


@pytest.mark.asyncio
async def test_long_or_unready_requirements_are_left_to_agentflow(engine):
    await gated_task(engine, {**DOCUMENTS, "requirements.md": "x\n" * 31000})
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    assert await service._on_action(action("review_requirements", "NEW-1")) == \
        "NEW-1's requirements are too long to review in Slack; review and approve them in AgentFlow."
    assert await service._on_action(action("review_requirements", "TEST-1")) == \
        "TEST-1's requirements are not waiting for approval."
    # Only the gate's own notice (from the startup catch-up); no documents, no approve button.
    assert not any("Requirements for" in text for _, text, _ in adapter.sent)
    assert not any(b.kind == "approve_requirements" for buttons in adapter.buttons for b in buttons)
    await service.stop()


@pytest.mark.asyncio
async def test_every_document_the_digest_covers_is_shown(engine):
    """A document added to the workspace later, with no title here, is still posted before approval."""
    await gated_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    real_snapshot = engine.snapshot

    def with_extra_document(key):
        snapshot = real_snapshot(key)
        snapshot["requirements"] = {**snapshot["requirements"], "rollbackPlan": "Revert the migration."}
        return snapshot
    with patch.object(engine, "snapshot", with_extra_document):
        assert await service._on_action(action("review_requirements", "NEW-1")) is None
    assert any(text == "*rollbackPlan*\nRevert the migration." for _, text, _ in adapter.sent)
    await service.stop()


@pytest.mark.asyncio
async def test_no_approve_button_when_there_is_nothing_to_show(engine):
    await gated_task(engine, {})
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    assert await service._on_action(action("review_requirements", "NEW-1")) == "NEW-1 has no requirements to review yet."
    assert not any(b.kind == "approve_requirements" for buttons in adapter.buttons for b in buttons)
    await service.stop()


@pytest.mark.asyncio
async def test_the_gate_notice_offers_review_and_changes(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter, **APPROVE)
    await gated_task(engine)
    await engine._publish("NEW-1")
    await eventually(lambda: len(adapter.sent) == 2)
    assert adapter.buttons[1] == (Button("review_requirements", "Review requirements", "NEW-1"),
                                  Button("request_changes", "Request changes…", "NEW-1"))
    await service.stop()


def test_long_documents_are_split_across_messages_never_cut():
    documents = [("Requirements", "&" * 12000), ("Risks", "\n".join(f"risk {i}" for i in range(4000)))]
    messages = slack_format.render_requirements("K-1", documents)
    assert len(messages) > 3
    for message in messages:
        blocks = slack_format._sections(message)
        assert len(blocks) <= 50 and all(len(block["text"]["text"]) <= 3000 for block in blocks)
        assert len(message) <= 40000
    joined = "".join(messages)
    assert joined.count("&amp;") == 12000 and "risk 0" in joined and "risk 3999" in joined


@needs_slack
@pytest.mark.asyncio
async def test_slack_review_and_approve_buttons_reach_the_service():
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    calls, patches = slack_stubs()
    updates, actions = [], []

    async def chat_update(self, **kwargs):
        updates.append(kwargs)
        return AsyncSlackResponse(client=self, http_verb="POST", api_url="x", req_args={}, data={"ok": True},
                                  headers={}, status_code=200)

    async def on_action(request):
        actions.append(request)
        return None
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_action = on_action
    prompt = {"type": "section", "text": {"type": "mrkdwn", "text": "Approve these requirements for K-1?"}}

    def click(action_id, value, text):
        return slack_payload("block_actions", channel={"id": "D1"}, trigger_id="t1",
                             message={"ts": "6.0", "thread_ts": "1.0", "blocks": [prompt, {"type": "actions", "elements": []}]},
                             actions=[{"action_id": action_id, "block_id": "buttons", "type": "button", "value": value,
                                       "text": {"type": "plain_text", "text": text}, "action_ts": "7.0"}])
    for p in [*patches, patch.object(AsyncWebClient, "chat_update", chat_update)]:
        p.start()
    try:
        await dispatch(adapter, click("agentflow_review_requirements_0", "K-1", "Review requirements"))
        await eventually(lambda: actions)
        await dispatch(adapter, click("agentflow_approve_requirements_0", "K-1|" + "a" * 64, "Approve requirements"))
        await eventually(lambda: updates)
    finally:
        patch.stopall()
    assert actions == [ActionRequest(OPERATOR, "D1", "review_requirements", "K-1"),
                       ActionRequest(OPERATOR, "D1", "approve_requirements", "K-1|" + "a" * 64)]
    assert len(updates) == 1  # Review keeps its button; Approve's message says it is done
    assert updates[0]["text"] == "Approve these requirements for K-1?\n*Requirements approved.*"
