import json
from unittest.mock import AsyncMock, patch

import pytest

from agentflow.services.chat import slack as slack_format
from agentflow.services.chat.base import ActionRequest, Button, TextForm, TextSubmission
from agentflow.workflow import default_workflow as wf
from agentflow.workflow.engine import WorkflowError
from test_chat import OPERATOR, FakeAdapter, eventually, set_node, started
from test_chat_answers import dispatch, needs_slack, slack_stubs
from test_chat_create import slack_payload
from test_chat_messages import asked, typed
from test_workflow_safety import engine  # noqa: F401 - fixture

DM = f"D-{OPERATOR}"
EVERYTHING = {"allowActions": ["answer", "run", "requestChanges", "cancel", "markPr"]}
CANCEL_CONFIRM = "Cancel TEST-1? Its agents are stopped. The worktree, branch and files are kept."


def set_status(engine, status):
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = status


async def command(service, text, thread=None):
    return await service._on_message(typed(text, thread=thread))


# The engine enforces what chat relies on -------------------------------------------------
@pytest.mark.asyncio
async def test_ready_only_runs_refuse_retries_and_resumes_under_the_lock(engine):
    set_node(engine, wf.IMPLEMENTER, "FAILED")
    with pytest.raises(WorkflowError, match="Implementer is no longer ready to start"):
        await engine.run_node("TEST-1", wf.IMPLEMENTER, ready_only=True)
    set_node(engine, wf.ARCHITECT, "READY")
    set_status(engine, "NEEDS_HUMAN_REVIEW")
    with pytest.raises(WorkflowError, match="Architect is no longer ready to start"):
        await engine.run_node("TEST-1", wf.ARCHITECT, ready_only=True)
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        assert task.status == "NEEDS_HUMAN_REVIEW" and task.review_iteration == 1  # nothing resumed


@pytest.mark.asyncio
async def test_the_engine_checks_pr_links_and_feedback_itself(engine):
    set_status(engine, "READY_FOR_PR")
    for bad in ("javascript:alert(1)", "https://x.io/pr/1\nInjected", "https://" + "x" * 2000):
        with pytest.raises(WorkflowError, match="PR URL must"):
            await engine.mark_pr_created("TEST-1", bad)
    await engine.mark_pr_created("TEST-1", "  https://git.example/pr/1  ")
    assert engine.snapshot("TEST-1")["prUrl"] == "https://git.example/pr/1"

    set_status(engine, "ACTIVE")
    set_node(engine, wf.REQUIREMENTS_GATE, "WAITING_FOR_APPROVAL")
    with pytest.raises(WorkflowError, match="Feedback must be at most 4000 characters"):
        await engine.request_requirements_changes("TEST-1", "x" * 4001)


# Typed commands only post buttons -------------------------------------------------------
@pytest.mark.asyncio
async def test_acting_commands_are_off_by_default(engine):
    service = await started(engine, FakeAdapter())
    assert (await command(service, "cancel TEST-1")).text == \
        'Cancelling tasks from Slack is turned off. Add "cancel" to slack.allowActions in config.json.'
    assert (await command(service, "run TEST-1")).text.startswith("Starting stages from Slack is turned off.")
    assert (await command(service, "changes TEST-1")).text.startswith("Requesting changes from Slack is turned off.")
    assert (await command(service, "pr TEST-1")).text.startswith("Marking pull requests from Slack is turned off.")
    await service.stop()


@pytest.mark.asyncio
async def test_cancel_typed_or_from_a_thread_is_a_confirmed_button(engine):
    engine.cancel_task = AsyncMock()
    service, _ = await asked(engine, **EVERYTHING)
    expected = (Button("cancel", "Cancel TEST-1", "TEST-1", CANCEL_CONFIRM),)
    assert (await command(service, "cancel test-1")).buttons == expected
    bare = await command(service, "cancel", thread="ts1")  # "Proceed or cancel?" may be answered so
    assert bare.buttons == expected and bare.offer.text == "cancel"
    # An answer that happens to start with "cancel" keeps its offer next to the button.
    mixed = await command(service, "cancel the rollout", thread="ts1")
    assert mixed.buttons == expected and mixed.offer.text == "cancel the rollout"
    assert (await command(service, "cancel")).text == "Which task? Use `cancel KEY`, or ask in the task's thread."
    set_status(engine, "MERGED")
    assert (await command(service, "cancel TEST-1")).text == "TEST-1 is MERGED; it can no longer be cancelled."
    engine.cancel_task.assert_not_awaited()
    await service.stop()


@pytest.mark.asyncio
async def test_slash_command_buttons_are_posted_in_the_dm(engine):
    """Buttons act only in the operator's DM, and a private slash reply cannot be edited afterwards."""
    adapter = FakeAdapter()
    service = await started(engine, adapter, **EVERYTHING)
    reply = await service._on_command(OPERATOR, "cancel TEST-1")
    assert reply.text == "The button is in your direct messages with AgentFlow." and reply.buttons == ()
    assert adapter.sent[-1] == (DM, "Cancel TEST-1?", None)
    assert adapter.buttons[-1] == (Button("cancel", "Cancel TEST-1", "TEST-1", CANCEL_CONFIRM),)
    assert (await service._on_command(OPERATOR, "run TEST-1")).text == "Nothing in TEST-1 is ready to start."
    await service.stop()


@pytest.mark.asyncio
async def test_run_typed_offers_what_can_start_and_explains_the_rest(engine):
    service = await started(engine, FakeAdapter(), **EVERYTHING)
    assert (await command(service, "run TEST-1")).text == "Nothing in TEST-1 is ready to start."
    set_node(engine, wf.ARCHITECT, "READY")
    reply = await command(service, "run TEST-1")
    assert reply.buttons == (Button("run", "Run Architect", "TEST-1|architect", "Start Architect for TEST-1?"),)
    assert (await command(service, "run TEST-1 code_review")).text == \
        "Code Review is PENDING; only a READY stage can be started from Slack."
    assert (await command(service, "run TEST-1 <nope>")).text == "TEST-1 has no stage &lt;nope&gt;."
    engine.config.workflow.cost_controls.require_metered_approval = True
    assert (await command(service, "run TEST-1")).text == "Architect needs API launch approval; start it in AgentFlow."
    await service.stop()


@pytest.mark.asyncio
async def test_run_in_a_thread_can_name_just_the_stage(engine):
    service, _ = await asked(engine, **EVERYTHING)
    set_node(engine, wf.ARCHITECT, "READY")
    assert (await command(service, "run architect", thread="ts1")).buttons[0].value == "TEST-1|architect"
    await service.stop()


@pytest.mark.asyncio
async def test_pr_typed_follows_the_task_status(engine):
    service = await started(engine, FakeAdapter(), **EVERYTHING)
    assert (await command(service, "pr TEST-1")).text == \
        "TEST-1 is ACTIVE; pull requests are tracked once it is Ready for PR."
    set_status(engine, "READY_FOR_PR")
    assert (await command(service, "pr TEST-1")).buttons == (Button("mark_pr", "Mark PR created…", "TEST-1"),)
    set_status(engine, "PR_CREATED")
    assert (await command(service, "pr TEST-1")).buttons == (
        Button("mark_merged", "Mark merged", "TEST-1", "Mark TEST-1's pull request as merged?"),)
    await service.stop()


# Buttons act, with the engine's refusals ------------------------------------------------
@pytest.mark.asyncio
async def test_cancel_and_merge_buttons_act_through_the_engine(engine):
    service = await started(engine, FakeAdapter(), **EVERYTHING)
    assert await service._on_action(ActionRequest(OPERATOR, DM, "mark_merged", "TEST-1")) == \
        "Not done: Task must have a PR created before it can be marked Merged (currently ACTIVE)."
    assert await service._on_action(ActionRequest("U0INTRUDER", DM, "cancel", "TEST-1")) == \
        "You are not authorized to use AgentFlow."
    assert await service._on_action(ActionRequest(OPERATOR, DM, "cancel", "TEST-1")) is None
    assert engine.snapshot("TEST-1")["status"] == "CANCELLED"
    await service.stop()

    service = await started(engine, FakeAdapter())
    assert (await service._on_action(ActionRequest(OPERATOR, DM, "cancel", "TEST-1"))).startswith(
        "Cancelling tasks from Slack is turned off.")
    await service.stop()


@pytest.mark.asyncio
async def test_a_stage_that_changed_since_the_check_is_refused_by_the_engine(engine):
    set_node(engine, wf.ARCHITECT, "READY")
    engine.run_node = AsyncMock(side_effect=WorkflowError("Architect is no longer ready to start", 409))
    service = await started(engine, FakeAdapter(), **EVERYTHING)
    assert await service._on_action(ActionRequest(OPERATOR, DM, "run", "TEST-1|architect")) == \
        "Not started: Architect is no longer ready to start."
    await service.stop()


# One-field forms ------------------------------------------------------------------------
@pytest.mark.asyncio
async def test_a_pr_link_is_checked_on_the_form_and_recorded_afterwards(engine):
    service = await started(engine, FakeAdapter(), **EVERYTHING)
    assert await service._on_text_form(OPERATOR, "mark_pr", "TEST-1") == \
        "TEST-1 is ACTIVE; a PR can be recorded only when it is Ready for PR."
    set_status(engine, "READY_FOR_PR")
    form = await service._on_text_form(OPERATOR, "mark_pr", "test-1")
    assert isinstance(form, TextForm) and form.value == "TEST-1" and not form.multiline

    def submit(text, user=OPERATOR):
        return service._on_submit_text(TextSubmission(user, "mark_pr", "TEST-1", text))
    assert await submit("ftp://x") == {"text": "PR URL must start with http:// or https://"}
    assert await submit("https://x", user="U0INTRUDER") == {"text": "You are not authorized to use AgentFlow."}
    assert await submit(" https://git.example/pr/7 ") == {}
    await eventually(lambda: service.adapter.sent)
    assert engine.snapshot("TEST-1")["prUrl"] == "https://git.example/pr/7"
    assert service.adapter.sent[-1] == (DM, "Pull request recorded.", None)  # no thread yet: a DM
    await service.stop()


@pytest.mark.asyncio
async def test_requested_changes_go_to_the_architect_with_a_run_button(engine):
    service, _ = await asked(engine, **EVERYTHING)
    adapter = service.adapter
    assert await service._on_text_form(OPERATOR, "request_changes", "TEST-1") == \
        "TEST-1's requirements are not waiting for approval."
    set_node(engine, wf.REQUIREMENTS_GATE, "WAITING_FOR_APPROVAL")
    form = await service._on_text_form(OPERATOR, "request_changes", "TEST-1")
    assert form.multiline and form.max_length == 4000

    def submit(text):
        return service._on_submit_text(TextSubmission(OPERATOR, "request_changes", "TEST-1", text))
    assert await submit("   ") == {"text": "Feedback cannot be empty."}
    sent = len(adapter.sent)
    assert await submit("Cover the <admin> role too") == {}
    await eventually(lambda: any(text == "Requirements sent back to the Architect." for _, text, _ in adapter.sent[sent:]))
    index = next(i for i, (_, text, _) in enumerate(adapter.sent) if text == "Requirements sent back to the Architect.")
    assert adapter.sent[index][2] == "ts1"  # in the task's thread
    assert adapter.buttons[index] == (Button("run", "Run Architect", "TEST-1|architect", "Start Architect for TEST-1?"),)
    assert engine.snapshot("TEST-1")["requirementsFeedback"][-1]["feedback"] == "Cover the <admin> role too"
    await service.stop()


# Notices invite the matching action ------------------------------------------------------
@pytest.mark.asyncio
async def test_notices_carry_the_action_they_invite(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter, notify=["READY", "WAITING_FOR_APPROVAL", "READY_FOR_PR"], **EVERYTHING)
    set_node(engine, wf.ARCHITECT, "READY")
    await engine._publish("TEST-1")
    await eventually(lambda: len(adapter.sent) == 2)
    assert adapter.buttons[1] == (Button("run", "Run Architect", "TEST-1|architect", "Start Architect for TEST-1?"),)

    set_node(engine, wf.ARCHITECT, "COMPLETED")
    set_node(engine, wf.REQUIREMENTS_GATE, "WAITING_FOR_APPROVAL")
    await engine._publish("TEST-1")
    await eventually(lambda: len(adapter.sent) == 3)
    assert adapter.buttons[2] == (Button("request_changes", "Request changes…", "TEST-1"),)

    set_node(engine, wf.REQUIREMENTS_GATE, "COMPLETED")
    set_status(engine, "READY_FOR_PR")
    await engine._publish("TEST-1")
    await eventually(lambda: len(adapter.sent) == 4)
    assert adapter.buttons[3] == (Button("mark_pr", "Mark PR created…", "TEST-1"),)
    await service.stop()


# Slack wiring ---------------------------------------------------------------------------
def test_text_form_fits_slack_limits():
    modal = slack_format.text_modal(TextForm("mark_pr", "TEST-1", "Mark PR created", "Record it.", "Pull request URL",
                                             False, 2000))
    assert len(modal["title"]["text"]) <= 24 and modal["callback_id"] == slack_format.TEXT_FORM_CALLBACK
    assert json.loads(modal["private_metadata"]) == {"kind": "mark_pr", "value": "TEST-1"}
    assert modal["blocks"][0]["text"]["type"] == "plain_text"  # the intro is not parsed as markup


@needs_slack
@pytest.mark.asyncio
async def test_slack_form_button_opens_the_form_and_the_submission_reaches_the_service():
    calls, patches = slack_stubs()
    submitted = []

    async def text_form(user_id, kind, value):
        return TextForm(kind, value, "Request changes", "Intro", "What should change?", True, 4000)

    async def submit_text(submission):
        submitted.append(submission)
        return {"text": "Feedback cannot be empty."}
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_text_form, adapter._on_submit_text = text_form, submit_text
    click = slack_payload("block_actions", channel={"id": "D1"}, trigger_id="t3",
                          message={"ts": "6.0", "thread_ts": "1.0", "blocks": []},
                          actions=[{"action_id": "agentflow_request_changes_0", "block_id": "buttons", "type": "button",
                                    "value": "TEST-1", "action_ts": "7.0"}])
    for p in patches:
        p.start()
    try:
        await dispatch(adapter, click)
        await eventually(lambda: calls["views_open"])
        view = calls["views_open"][0]["view"]
        state = {"values": {"text": {"value": {"type": "plain_text_input", "value": "  "}}}}
        response = await dispatch(adapter, slack_payload("view_submission", view={**view, "id": "V1", "state": state}))
    finally:
        for p in patches:
            p.stop()
    assert calls["views_open"][0]["trigger_id"] == "t3" and view["callback_id"] == slack_format.TEXT_FORM_CALLBACK
    assert submitted == [TextSubmission(OPERATOR, "request_changes", "TEST-1", "  ")]
    assert json.loads(response.body) == {"response_action": "errors", "errors": {"text": "Feedback cannot be empty."}}


@needs_slack
@pytest.mark.asyncio
async def test_slack_cancel_button_acts_and_says_so():
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
    question = {"type": "section", "text": {"type": "mrkdwn", "text": "Cancel TEST-1?"}}
    click = slack_payload("block_actions", channel={"id": "D1"}, trigger_id="t1",
                          message={"ts": "6.0", "blocks": [question, {"type": "actions", "elements": []}]},
                          actions=[{"action_id": "agentflow_cancel_0", "block_id": "buttons", "type": "button",
                                    "value": "TEST-1", "text": {"type": "plain_text", "text": "Cancel TEST-1"},
                                    "action_ts": "7.0"}])
    for p in [*patches, patch.object(AsyncWebClient, "chat_update", chat_update)]:
        p.start()
    try:
        await dispatch(adapter, click)
        await eventually(lambda: updates)
    finally:
        patch.stopall()
    assert actions == [ActionRequest(OPERATOR, "D1", "cancel", "TEST-1")]
    assert updates[0]["text"] == "Cancel TEST-1?\n*Task cancelled.*"
