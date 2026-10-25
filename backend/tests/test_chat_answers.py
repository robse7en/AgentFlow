import importlib.util
import json
from types import SimpleNamespace
from unittest.mock import patch

import pytest
from sqlalchemy import select

from agentflow.database.models import WorkflowEvent
from agentflow.services.chat import slack as slack_format
from agentflow.services.chat.base import AnswerPrompt, AnswerRequest
from agentflow.services.chat.service import ChatService
from agentflow.services.zellij import ZellijError
from agentflow.workflow import default_workflow as wf
from test_chat import OPERATOR, FakeAdapter, ask, eventually, quiet, set_node, settings, started
from test_workflow_safety import engine  # noqa: F401 - fixture

DM = f"D-{OPERATOR}"


async def asked(engine, **overrides):
    """A running service that has DMed one question with two choices."""
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **overrides)
    await ask(engine)
    await eventually(lambda: len(adapter.sent) == 2)
    return service, adapter, engine.snapshot("TEST-1")["questions"][0]["uid"]


def request(uid, **fields):
    return AnswerRequest(**{"user_id": OPERATOR, "channel": DM, "jira_key": "TEST-1", "question_uid": uid, **fields})


@pytest.mark.asyncio
async def test_a_choice_is_answered_by_index_and_the_notice_loses_its_buttons(engine):
    service, adapter, uid = await asked(engine)
    assert await service._on_answer(request(uid, choice_index=1)) is None
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "All at once")
    await eventually(lambda: adapter.updates)
    channel, message_id, text = adapter.updates[-1]
    assert (channel, message_id) == (DM, "ts2")  # the question notice, not the thread title
    assert text.startswith("*Answered:* All at once") and "> Which rollout?" in text
    with engine.db.session() as session:
        event = session.scalar(select(WorkflowEvent).where(WorkflowEvent.message.like("Human replied%")))
        assert event.message.endswith("via Slack") and event.data == {"via": "Slack"}
    await service.stop()


@pytest.mark.asyncio
async def test_free_text_answers_use_the_engines_validation(engine):
    service, adapter, uid = await asked(engine)
    assert await service._on_answer(request(uid, text="one\ntwo")) == "Not sent: Answer must be a single line."
    engine.zellij.write_line.assert_not_awaited()
    assert await service._on_answer(request(uid, text="  Gradual, but over a week  ")) is None
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "Gradual, but over a week")
    await service.stop()


@pytest.mark.asyncio
async def test_answers_are_accepted_only_from_the_operator_in_their_dm(engine):
    service, adapter, uid = await asked(engine)
    refused = {
        "You are not authorized to answer AgentFlow questions.": request(uid, user_id="U0INTRUDER", choice_index=0),
        "Answer from your AgentFlow direct messages.": request(uid, channel="C0PUBLIC", choice_index=0),
        "That choice is no longer offered.": request(uid, choice_index=2),
        "That question is no longer open.": request("not-a-question", choice_index=0),
        "That task no longer exists.": AnswerRequest(OPERATOR, DM, "NOPE-1", uid, choice_index=0),
    }
    for reason, attempt in refused.items():
        assert await service._on_answer(attempt) == reason
    engine.zellij.write_line.assert_not_awaited()

    assert await service._on_answer(request(uid, choice_index=0)) is None
    assert await service._on_answer(request(uid, choice_index=1)) == "Not sent: This question has already been answered."
    await service.stop()


@pytest.mark.asyncio
async def test_answering_can_be_switched_off(engine):
    service, adapter, uid = await asked(engine, allowActions=[])
    assert adapter.prompts[1] is None  # no buttons
    assert "`Gradual` / `All at once`" in adapter.sent[1][1] and "Answer it in AgentFlow." in adapter.sent[1][1]
    assert await service._on_answer(request(uid, choice_index=0)) == "Answering from Slack is turned off; answer in AgentFlow."
    engine.zellij.write_line.assert_not_awaited()
    await service.stop()


@pytest.mark.asyncio
async def test_no_buttons_when_question_text_is_withheld(engine):
    service, adapter, uid = await asked(engine, includeQuestionText=False)
    assert adapter.prompts[1] is None
    assert await service._on_answer(request(uid, choice_index=0)) == "Answering from Slack is turned off; answer in AgentFlow."
    await service.stop()


@pytest.mark.asyncio
async def test_delivery_failures_are_reported_without_command_output(engine):
    service, adapter, uid = await asked(engine)
    engine.zellij.write_line.side_effect = ZellijError("write-chars failed: C:\\Users\\secret\\token=abc")
    reply = await service._on_answer(request(uid, choice_index=0))
    assert reply == "Not sent: AgentFlow could not deliver the answer. Check AgentFlow."
    await service.stop()


@pytest.mark.asyncio
async def test_notices_answered_elsewhere_or_abandoned_are_updated(engine):
    service, adapter, uid = await asked(engine)
    await engine.answer_question("TEST-1", uid, "Gradual")  # from the browser
    await eventually(lambda: adapter.updates)
    assert adapter.updates[-1][2].startswith("*Answered:* Gradual")

    run_id = engine.snapshot("TEST-1")["questions"][0]["runId"]
    await engine.agent_question("TEST-1", "implementer", run_id, "Second?", [])  # same agent run asks again
    await eventually(lambda: len(adapter.sent) == 3)
    set_node(engine, wf.IMPLEMENTER, "FAILED")  # the agent stopped waiting
    with engine.db.session() as session:
        from agentflow.database.models import NodeRun
        for run in session.scalars(select(NodeRun)).all():
            run.state = "FAILED"
    await engine._publish("TEST-1")
    await eventually(lambda: any(t.startswith("*No longer waiting") for _, _, t in adapter.updates))
    await service.stop()


@pytest.mark.asyncio
async def test_rejected_content_is_recorded_not_retried(engine):
    adapter = FakeAdapter(reject_notices=1)
    service = await started(engine, adapter)
    await ask(engine)
    await eventually(lambda: len(adapter.sent) == 1)  # thread title only; the notice was rejected
    await quiet()
    await engine._publish("TEST-1")
    await quiet()
    assert len(adapter.sent) == 1 and adapter.reject_notices == 0
    await service.stop()


def test_block_kit_limits_hold_for_the_largest_question():
    choices = tuple(f"{index}" + "&<>" * 83 for index in range(8))  # 250 chars, expands on escaping
    notice = SimpleNamespace(key="question:u", summary="Implementer asked a question",
                             question="&" * 4000, choices=choices, question_uid="u")
    text = slack_format.render_notice(notice, answerable=True)
    blocks = slack_format.notice_blocks(text, AnswerPrompt("TEST-123", "0" * 36, choices))
    assert len(blocks) <= 50
    sections = [b for b in blocks if b["type"] == "section"]
    assert all(len(b["text"]["text"]) <= 3000 for b in sections)
    assert "".join(b["text"]["text"] for b in sections).count("&amp;") == 4000  # nothing cut mid-escape
    buttons = blocks[-1]["elements"]
    assert len(buttons) == 9 and len({b["action_id"] for b in buttons}) == 9
    assert all(len(b["text"]["text"]) <= 75 and len(b["value"]) <= 2000 for b in buttons)
    assert all(slack_format.ANSWER_CONTROL.match(b["action_id"]) for b in buttons)
    modal = slack_format.answer_modal(sections, {"key": "TEST-123", "uid": "0" * 36, "channel": "D1", "thread": "1.2"})
    assert len(modal["title"]["text"]) <= 24 and len(modal["private_metadata"]) <= 3000


needs_slack = pytest.mark.skipif(importlib.util.find_spec("slack_bolt") is None, reason="slack extra not installed")


def slack_stubs():
    """Class-level stubs for every Slack HTTP call Bolt makes, recording what would be sent."""
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    calls = {"views_open": [], "chat_postMessage": [], "reactions": []}

    def ok(client, data):
        return AsyncSlackResponse(client=client, http_verb="POST", api_url="x", req_args={}, data={"ok": True, **data},
                                  headers={}, status_code=200)

    async def auth_test(self, **_):
        return ok(self, {"team_id": "T1", "user_id": "UBOT", "bot_id": "B1", "url": "https://t.slack.com/",
                         "team": "T", "user": "bot"})

    async def views_open(self, **kwargs):
        calls["views_open"].append(kwargs)
        return ok(self, {})

    async def chat_post(self, **kwargs):
        calls["chat_postMessage"].append(kwargs)
        return ok(self, {"ts": "9.9"})

    async def reactions_add(self, **kwargs):
        calls["reactions"].append(("add", kwargs))
        return ok(self, {})

    async def reactions_remove(self, **kwargs):
        calls["reactions"].append(("remove", kwargs))
        return ok(self, {})
    patches = [patch.object(AsyncWebClient, "auth_test", auth_test), patch.object(AsyncWebClient, "views_open", views_open),
               patch.object(AsyncWebClient, "chat_postMessage", chat_post),
               patch.object(AsyncWebClient, "reactions_add", reactions_add),
               patch.object(AsyncWebClient, "reactions_remove", reactions_remove)]
    return calls, patches


async def dispatch(adapter, body):
    from slack_bolt.request.async_request import AsyncBoltRequest
    return await adapter._app.async_dispatch(AsyncBoltRequest(body=body, mode="socket_mode"))


@needs_slack
@pytest.mark.asyncio
async def test_slack_button_click_reaches_the_service_and_reports_refusals():
    calls, patches = slack_stubs()
    answers = []

    async def on_answer(req):
        answers.append(req)
        return "Not sent: This question has already been answered."
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_answer = on_answer
    body = {"type": "block_actions", "team": {"id": "T1"}, "user": {"id": OPERATOR}, "api_app_id": "A1",
            "channel": {"id": "D1"}, "trigger_id": "t1",
            "message": {"ts": "2.0", "thread_ts": "1.0", "blocks": []},
            "actions": [{"action_id": "agentflow_answer_choice_1", "block_id": "answer", "type": "button",
                         "value": "TEST-1|uid-1|1", "action_ts": "3.0"}]}
    for p in patches:
        p.start()
    try:
        assert (await dispatch(adapter, body)).status == 200
        await eventually(lambda: calls["chat_postMessage"])
    finally:
        for p in patches:
            p.stop()
    assert answers == [AnswerRequest(OPERATOR, "D1", "TEST-1", "uid-1", choice_index=1)]
    posted = calls["chat_postMessage"][0]
    assert posted["channel"] == "D1" and posted["thread_ts"] == "1.0" and "already been answered" in posted["text"]


@needs_slack
@pytest.mark.asyncio
async def test_slack_reply_opens_a_modal_and_submission_reaches_the_service():
    calls, patches = slack_stubs()
    answers = []

    async def on_answer(req):
        answers.append(req)
        return None
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_answer = on_answer
    question = {"type": "section", "text": {"type": "mrkdwn", "text": "Implementer asked a question"}}
    click = {"type": "block_actions", "team": {"id": "T1"}, "user": {"id": OPERATOR}, "api_app_id": "A1",
             "channel": {"id": "D1"}, "trigger_id": "t1",
             "message": {"ts": "2.0", "thread_ts": "1.0", "blocks": [question, {"type": "actions", "elements": []}]},
             "actions": [{"action_id": "agentflow_answer_reply", "block_id": "answer", "type": "button",
                          "value": "TEST-1|uid-1|reply", "action_ts": "3.0"}]}
    for p in patches:
        p.start()
    try:
        await dispatch(adapter, click)
        await eventually(lambda: calls["views_open"])
        view = calls["views_open"][0]["view"]
        assert calls["views_open"][0]["trigger_id"] == "t1" and view["blocks"][0] == question
        assert answers == []  # nothing is answered by opening the modal
        submission = {"type": "view_submission", "team": {"id": "T1"}, "user": {"id": OPERATOR}, "api_app_id": "A1",
                      "view": {**view, "id": "V1", "state": {"values": {"answer": {"value": {
                          "type": "plain_text_input", "value": "Ship it gradually"}}}}}}
        assert (await dispatch(adapter, submission)).status == 200
        await eventually(lambda: answers)
    finally:
        for p in patches:
            p.stop()
    assert json.loads(view["private_metadata"]) == {"key": "TEST-1", "uid": "uid-1", "channel": "D1", "thread": "1.0"}
    assert answers == [AnswerRequest(OPERATOR, "D1", "TEST-1", "uid-1", text="Ship it gradually")]
    assert calls["chat_postMessage"] == []  # success is shown by editing the notice, not a new message
