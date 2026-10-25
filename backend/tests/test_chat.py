import asyncio
import importlib.util
import logging
import sys
from pathlib import Path
from unittest.mock import patch

import pytest
from sqlalchemy import select

from agentflow.config import Config, SlackConfig
from agentflow.database.models import ChatThread
from agentflow.services.chat import start_chat
from agentflow.services.chat.base import AnswerRequest, ChatAdapter, PermanentChatError, Reply
from agentflow.services.chat.service import ChatService
from agentflow.services.chat import slack as slack_format
from agentflow.services.events import Broadcaster
from agentflow.services.process import clean_env
from agentflow.workflow import default_workflow as wf
from agentflow.workflow.notifications import notices
from test_workflow_safety import add_run, add_session, engine  # noqa: F401 - fixture

OPERATOR = "U0OPERATOR"


class FakeAdapter(ChatAdapter):
    """Records what would be posted; renders with the real Slack formatting."""

    platform = "slack"
    display_name = "Slack"

    def __init__(self, fail_sends=0, fail_connects=0, reject_notices=0):
        self.sent, self.updates, self.prompts, self.buttons = [], [], [], []
        self.fail_sends, self.fail_connects, self.reject_notices = fail_sends, fail_connects, reject_notices
        self.handlers = None
        self.connected = False
        self.gate = None  # an asyncio.Event that holds connect() open, like a slow Slack
        self.fail_updates = False

    async def connect(self, handlers):
        if self.gate is not None:
            await self.gate.wait()
        if self.fail_connects:
            self.fail_connects -= 1
            raise ConnectionError("slack unreachable")
        self.handlers, self.connected = handlers, True

    async def disconnect(self):
        self.connected = False

    async def open_direct(self, user_id):
        return f"D-{user_id}"

    async def send(self, channel, text, thread_id=None, prompt=None, buttons=()):
        if self.fail_sends:
            self.fail_sends -= 1
            raise ConnectionError("slack unreachable")
        if thread_id and self.reject_notices:
            self.reject_notices -= 1
            raise PermanentChatError("invalid_blocks")
        self.sent.append((channel, text, thread_id))
        self.prompts.append(prompt)
        self.buttons.append(buttons)
        return f"ts{len(self.sent)}"

    async def update(self, channel, message_id, text):
        if self.fail_updates:
            raise PermanentChatError("message_not_found")
        self.updates.append((channel, message_id, text))

    def render_title(self, snapshot, url):
        return slack_format.render_title(snapshot, url)

    def render_notice(self, notice, answerable=False):
        return slack_format.render_notice(notice, answerable)

    def render_resolved(self, question, include_text):
        return slack_format.render_resolved(question, include_text)

    def render_status(self, snapshot, url):
        return slack_format.render_status(snapshot, url)

    def render_list(self, tasks):
        return slack_format.render_list(tasks)

    def render_details(self, snapshot, details, url):
        return slack_format.render_details(snapshot, details, url)

    def render_digest(self, tasks, spend):
        return slack_format.render_digest(tasks, spend)

    def render_created(self, snapshot, notes=()):
        return slack_format.render_created(snapshot, notes)

    def render_creation_failed(self, jira_key, detail):
        return slack_format.render_creation_failed(jira_key, detail)

    def render_note(self, text):
        return slack_format.escape(text)

    def render_requirements(self, jira_key, documents):
        return slack_format.render_requirements(jira_key, documents)

    def render_ai_answer(self, text):
        return slack_format.render_ai_answer(text)

    def render_help(self):
        return slack_format.render_help()


def settings(**overrides):
    return SlackConfig.model_validate({"enabled": True, "userId": OPERATOR, **overrides})


async def eventually(predicate, timeout=3.0):
    deadline = asyncio.get_running_loop().time() + timeout
    while not predicate():
        if asyncio.get_running_loop().time() > deadline:
            raise AssertionError("condition not reached")
        await asyncio.sleep(0.01)


async def quiet():
    """Let the worker drain whatever is queued."""
    await asyncio.sleep(0.1)


async def started(engine, adapter, **overrides):
    service = ChatService(engine, adapter, settings(**overrides), retry_delay=0.01)
    service.start()
    await eventually(lambda: service.channel is not None)
    await quiet()
    return service


def set_node(engine, key, state, error=None):
    with engine.db.session() as session:
        node = engine._node(engine._load_task(session, "TEST-1"), key)
        node.state, node.error = state, error


async def ask(engine, message="Which rollout?", choices=("Gradual", "All at once")):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    await engine.agent_question("TEST-1", "implementer", run_id, message, list(choices))


@pytest.mark.asyncio
async def test_question_is_announced_once_in_a_dm_thread(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    assert adapter.sent == []  # nothing needs the operator yet

    await ask(engine)
    await eventually(lambda: len(adapter.sent) == 2)
    (channel, title, parent), (_, notice, thread) = adapter.sent
    assert channel == f"D-{OPERATOR}" and parent is None and thread == "ts1"
    assert "*TEST-1*" in title and "http://127.0.0.1:5173/tasks/TEST-1" in title
    assert "Implementer asked a question" in notice
    assert "> Which rollout?" in notice and "Answer below, or in AgentFlow." in notice
    prompt = adapter.prompts[1]
    assert prompt.jira_key == "TEST-1" and prompt.choices == ("Gradual", "All at once")

    await engine._publish("TEST-1")
    await quiet()
    assert len(adapter.sent) == 2
    await service.stop()


@pytest.mark.asyncio
async def test_restart_repeats_nothing_and_catches_up_in_the_same_thread(engine):
    first = FakeAdapter()
    service = await started(engine, first)
    await ask(engine)
    await eventually(lambda: len(first.sent) == 2)
    await service.stop()

    # Changed while chat was down: no publish reaches the service.
    set_node(engine, wf.ARCHITECT, "FAILED", error="stderr: token=SECRET")
    second = FakeAdapter()
    service = await started(engine, second)
    await eventually(lambda: len(second.sent) == 1)
    channel, text, thread = second.sent[0]
    assert thread == "ts1" and text == "Architect failed"
    await service.stop()


@pytest.mark.asyncio
async def test_a_condition_that_clears_and_returns_is_announced_again(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    set_node(engine, wf.ARCHITECT, "FAILED")
    await engine._publish("TEST-1")
    await eventually(lambda: len(adapter.sent) == 2)
    set_node(engine, wf.ARCHITECT, "RUNNING")
    await engine._publish("TEST-1")
    await quiet()
    set_node(engine, wf.ARCHITECT, "FAILED")
    await engine._publish("TEST-1")
    await eventually(lambda: len(adapter.sent) == 3)
    assert [text for _, text, _ in adapter.sent[1:]] == ["Architect failed", "Architect failed"]
    await service.stop()


@pytest.mark.asyncio
async def test_a_title_that_cannot_be_edited_does_not_hold_up_notices(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    await ask(engine)
    await eventually(lambda: len(adapter.sent) == 2)
    adapter.fail_updates = True  # e.g. retention deleted the thread's first message
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").jira_title = "Renamed in Jira"
    set_node(engine, wf.ARCHITECT, "FAILED")
    await engine._publish("TEST-1")
    await eventually(lambda: len(adapter.sent) == 3)
    assert adapter.sent[2][1] == "Architect failed"
    await engine._publish("TEST-1")
    await quiet()
    assert len(adapter.sent) == 3  # the failed title edit is not retried either
    await service.stop()


@pytest.mark.asyncio
async def test_open_questions_are_announced_when_chat_first_starts(engine):
    await ask(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    await eventually(lambda: len(adapter.sent) == 2)
    await service.stop()


@pytest.mark.asyncio
async def test_a_task_that_fails_while_chat_is_connecting_is_still_announced(engine):
    """Whether a task is history is decided from its status at startup, not when Slack answers."""
    adapter = FakeAdapter()
    adapter.gate = asyncio.Event()
    service = ChatService(engine, adapter, settings(), retry_delay=0.01)
    service.start()
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = "FAILED"
    await engine._publish("TEST-1")
    adapter.gate.set()
    await eventually(lambda: [text for _, text, _ in adapter.sent[1:]] == ["Task failed."])
    await service.stop()


@pytest.mark.asyncio
async def test_first_start_records_finished_history_silently(engine):
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = "READY_FOR_PR"
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    assert adapter.sent == []
    with engine.db.session() as session:
        row = session.scalar(select(ChatThread))
        assert row.notified == {"status:READY_FOR_PR": None} and row.thread_id is None
    await service.stop()


@pytest.mark.asyncio
async def test_chat_failures_never_break_the_workflow_and_are_retried(engine):
    adapter = FakeAdapter(fail_sends=3, fail_connects=1)
    service = await started(engine, adapter)
    await ask(engine)  # publishes; must not raise although every send fails at first
    await eventually(lambda: len(adapter.sent) == 2)
    await quiet()
    assert [thread for _, _, thread in adapter.sent] == [None, "ts1"]  # one thread, no duplicates
    await service.stop()


def test_a_failing_listener_does_not_stop_the_broadcast():
    broadcaster, seen = Broadcaster(), []
    broadcaster.subscribe(lambda message: 1 / 0)
    unsubscribe = broadcaster.subscribe(seen.append)
    asyncio.run(broadcaster.broadcast({"type": "task"}))
    unsubscribe()
    asyncio.run(broadcaster.broadcast({"type": "task"}))
    assert seen == [{"type": "task"}]


@pytest.mark.asyncio
async def test_findings_status_messages_and_errors_are_never_posted(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        task.status = "NEEDS_HUMAN_REVIEW"
        task.status_message = "Remaining blocking findings: SECRET-FINDING in auth.py"
    set_node(engine, wf.CODE_REVIEW, "FAILED", error="SECRET stderr")
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    await eventually(lambda: len(adapter.sent) == 3)
    posted = "\n".join(text for _, text, _ in adapter.sent)
    assert "Needs human review" in posted and "Code Review failed" in posted
    assert "SECRET" not in posted
    await service.stop()


@pytest.mark.asyncio
async def test_question_text_can_be_withheld(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter, includeQuestionText=False)
    await ask(engine, message="Internal detail?")
    await eventually(lambda: len(adapter.sent) == 2)
    assert adapter.sent[1][1] == "Implementer asked a question\nAnswer it in AgentFlow."
    await service.stop()


@pytest.mark.asyncio
async def test_untrusted_agent_text_cannot_mention_or_link(engine):
    adapter = FakeAdapter()
    service = await started(engine, adapter, allowActions=[])  # choices rendered as text
    await ask(engine, message="Ping <!channel> via <http://evil.example|here> & done", choices=["<@U1>", "a`b"])
    await eventually(lambda: len(adapter.sent) == 2)
    notice = adapter.sent[1][1]
    assert "<!channel>" not in notice and "<http" not in notice and "<@U1>" not in notice
    assert "&lt;!channel&gt;" in notice and "&amp; done" in notice and "`a'b`" in notice
    await service.stop()


def test_notify_setting_filters_conditions():
    snapshot = {"jiraKey": "K-1", "status": "READY_FOR_PR", "questions": [],
                "attention": [{"nodeKey": "x", "label": "X", "state": "READY", "questions": 0}]}
    assert list(notices(snapshot, ["READY_FOR_PR"], True)) == ["status:READY_FOR_PR"]
    assert list(notices(snapshot, ["READY"], True)) == ["node:x:READY"]


@pytest.mark.asyncio
async def test_commands_answer_only_the_operator(engine, caplog):
    service = ChatService(engine, FakeAdapter(), settings())
    assert (await service._on_command("U0SOMEONEELSE", "list")).text == "You are not authorized to use AgentFlow."
    assert "unauthorized user U0SOMEONEELSE" in caplog.text  # tells a mismatched slack.userId apart
    assert "TEST-1" in (await service._on_command(OPERATOR, "list")).text
    assert "TEST-1" in (await service._on_command(OPERATOR, "status")).text
    status = (await service._on_command(OPERATOR, "status test-1")).text
    assert status.startswith("*TEST-1*") and "/tasks/TEST-1|Open in AgentFlow>" in status
    assert (await service._on_command(OPERATOR, "status <nope>")).text == "No AgentFlow task NOPE."
    help_text = (await service._on_command(OPERATOR, "")).text
    assert "`list`" in help_text and "`digest`" in help_text and "`/agentflow`" in help_text


def test_list_hides_finished_tasks_and_caps_length():
    tasks = [{"jiraKey": f"K-{i}", "status": "ACTIVE", "phase": "Architect: RUNNING", "attention": []} for i in range(30)]
    tasks.append({"jiraKey": "DONE-1", "status": "MERGED", "phase": "MERGED", "attention": []})
    rendered = slack_format.render_list(tasks)
    assert "DONE-1" not in rendered and rendered.endswith("…and 5 more")
    assert slack_format.render_list(tasks[-1:]) == "No active AgentFlow tasks."


def test_start_chat_explains_why_it_is_off(engine, monkeypatch, caplog):
    monkeypatch.delenv("AGENTFLOW_SLACK_BOT_TOKEN", raising=False)
    monkeypatch.delenv("AGENTFLOW_SLACK_APP_TOKEN", raising=False)
    caplog.set_level(logging.INFO, logger="agentflow.services.chat")
    assert start_chat(Config(), engine, Path("C:/af/config.json")) is None  # disabled by default
    assert "Slack is off" in caplog.text and str(Path("C:/af/config.json")) in caplog.text
    config = Config.model_validate({"slack": {"enabled": True}})
    assert start_chat(config, engine) is None
    assert "xoxb-" in caplog.text and "xapp-" in caplog.text and "slack.userId" in caplog.text


def test_start_chat_without_slack_libraries_leaves_the_backend_running(engine, monkeypatch, caplog):
    monkeypatch.setenv("AGENTFLOW_SLACK_BOT_TOKEN", "xoxb-test")
    monkeypatch.setenv("AGENTFLOW_SLACK_APP_TOKEN", "xapp-test")
    for name in ("slack_bolt", "slack_bolt.async_app", "slack_sdk", "slack_sdk.web.async_client",
                 "slack_sdk.http_retry.builtin_async_handlers"):
        monkeypatch.setitem(sys.modules, name, None)
    config = Config.model_validate({"slack": {"enabled": True, "userId": OPERATOR}})
    assert start_chat(config, engine) is None
    assert 'pip install -e "backend[slack]"' in caplog.text


def test_tokens_prefer_environment_and_never_reach_agents(monkeypatch):
    monkeypatch.setenv("AGENTFLOW_SLACK_BOT_TOKEN", "xoxb-env")
    monkeypatch.setenv("AGENTFLOW_SLACK_APP_TOKEN", "xapp-env")
    config = SlackConfig.model_validate({"enabled": True, "botToken": "xoxb-file", "userId": OPERATOR})
    assert config.resolved_bot_token() == "xoxb-env" and config.problems() == []
    assert not any(name.startswith("AGENTFLOW_SLACK") for name in clean_env())


def test_user_id_must_be_a_slack_member_id():
    with pytest.raises(ValueError):
        SlackConfig.model_validate({"userId": "@rob"})


@pytest.mark.skipif(importlib.util.find_spec("slack_bolt") is None, reason="slack extra not installed")
@pytest.mark.asyncio
async def test_slack_slash_command_is_acked_and_answered_privately():
    """Drives Bolt's real dispatcher offline; Slack's HTTP endpoints are stubbed."""
    from slack_bolt.context.respond import async_respond
    from slack_bolt.request.async_request import AsyncBoltRequest
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    async def auth_test(self, **_):
        return AsyncSlackResponse(client=self, http_verb="POST", api_url="auth.test", req_args={}, status_code=200,
                                  headers={}, data={"ok": True, "team_id": "T1", "user_id": "UBOT", "bot_id": "B1",
                                                    "url": "https://t.slack.com/", "team": "T", "user": "bot"})
    replies, commands = [], []

    async def respond(self, text="", **kwargs):
        replies.append((text, kwargs.get("response_type")))

    async def on_command(user_id, text):
        commands.append((user_id, text))
        return Reply("reply")

    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_command = on_command
    body = {"team_id": "T1", "channel_id": "D1", "user_id": OPERATOR, "command": "/agentflow", "text": "list",
            "response_url": "https://hooks.slack.com/commands/1", "trigger_id": "1", "api_app_id": "A1"}
    with patch.object(AsyncWebClient, "auth_test", auth_test), patch.object(async_respond.AsyncRespond, "__call__", respond):
        response = await adapter._app.async_dispatch(AsyncBoltRequest(body=body, mode="socket_mode"))
        await eventually(lambda: replies)
    assert response.status == 200
    assert commands == [(OPERATOR, "list")] and replies == [("reply", "ephemeral")]


@pytest.mark.skipif(importlib.util.find_spec("slack_bolt") is None, reason="slack extra not installed")
@pytest.mark.asyncio
async def test_slack_listens_only_on_its_configured_command():
    """Every app in a workspace shares slash command names, so each person's app has its own."""
    from slack_bolt.request.async_request import AsyncBoltRequest
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    async def auth_test(self, **_):
        return AsyncSlackResponse(client=self, http_verb="POST", api_url="auth.test", req_args={}, status_code=200,
                                  headers={}, data={"ok": True, "team_id": "T1", "user_id": "UBOT", "bot_id": "B1",
                                                    "url": "https://t.slack.com/", "team": "T", "user": "bot"})
    commands = []

    async def on_command(user_id, text):
        commands.append(text)
        return Reply("reply")

    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test", "/agentflow-rob")
    adapter._on_command = on_command
    statuses = {}
    with patch.object(AsyncWebClient, "auth_test", auth_test):
        for command in ("/agentflow", "/agentflow-rob"):
            body = {"team_id": "T1", "channel_id": "D1", "user_id": OPERATOR, "command": command, "text": command,
                    "response_url": "https://hooks.slack.com/commands/1", "trigger_id": "1", "api_app_id": "A1"}
            statuses[command] = (await adapter._app.async_dispatch(AsyncBoltRequest(body=body, mode="socket_mode"))).status
        await eventually(lambda: commands)
    assert statuses == {"/agentflow": 404, "/agentflow-rob": 200} and commands == ["/agentflow-rob"]
    assert "after `/agentflow-rob`" in adapter.render_help()
    assert SlackConfig.model_validate({"command": "/agentflow-rob"}).command == "/agentflow-rob"
    for bad in ("agentflow", "/AgentFlow", "/agent flow", "/" + "a" * 32):
        with pytest.raises(ValueError):
            SlackConfig.model_validate({"command": bad})


@pytest.mark.skipif(importlib.util.find_spec("slack_bolt") is None, reason="slack extra not installed")
@pytest.mark.asyncio
async def test_slack_without_a_command_listens_for_none():
    """An app without a slash command takes commands in its DM only, and its help says so."""
    from slack_bolt.request.async_request import AsyncBoltRequest
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    async def auth_test(self, **_):
        return AsyncSlackResponse(client=self, http_verb="POST", api_url="auth.test", req_args={}, status_code=200,
                                  headers={}, data={"ok": True, "team_id": "T1", "user_id": "UBOT", "bot_id": "B1",
                                                    "url": "https://t.slack.com/", "team": "T", "user": "bot"})

    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test", None)
    body = {"team_id": "T1", "channel_id": "D1", "user_id": OPERATOR, "command": "/agentflow", "text": "list",
            "response_url": "https://hooks.slack.com/commands/1", "trigger_id": "1", "api_app_id": "A1"}
    with patch.object(AsyncWebClient, "auth_test", auth_test):
        response = await adapter._app.async_dispatch(AsyncBoltRequest(body=body, mode="socket_mode"))
    assert response.status == 404
    assert "Type these in your DM with AgentFlow." in adapter.render_help() and "after `" not in adapter.render_help()


@pytest.mark.skipif(importlib.util.find_spec("slack_bolt") is None, reason="slack extra not installed")
def test_backend_starts_and_pairs_while_slack_is_unreachable(tmp_path, monkeypatch, capsys):
    from fastapi.testclient import TestClient
    from slack_sdk.web.async_client import AsyncWebClient
    from agentflow.main import create_app

    async def unreachable(self, **_):
        raise ConnectionError("slack.com unreachable")
    monkeypatch.setattr(AsyncWebClient, "auth_test", unreachable)
    monkeypatch.setenv("AGENTFLOW_SLACK_BOT_TOKEN", "xoxb-test")
    monkeypatch.setenv("AGENTFLOW_SLACK_APP_TOKEN", "xapp-test")
    config = Config.model_validate({
        "app": {"dataDirectory": str(tmp_path), "databasePath": str(tmp_path / "t.db")},
        "workspace": {"taskDirectory": str(tmp_path / "tasks"), "worktreeDirectory": str(tmp_path / "trees")},
        "slack": {"enabled": True, "userId": OPERATOR},
    })
    with TestClient(create_app(config, tmp_path / "config.json"), base_url="http://127.0.0.1") as client:
        assert client.get("/api/health").json()["ok"] is True
    assert "pairing code" in capsys.readouterr().out
