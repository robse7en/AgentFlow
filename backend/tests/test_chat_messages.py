import asyncio
from types import SimpleNamespace
from unittest.mock import patch

import pytest

from agentflow.services.chat import slack as slack_format
from agentflow.services.chat.base import AnswerOffer, AnswerRequest, Button, MessageRequest, Reply
from agentflow.services.jira_client import _issue_markdown
from agentflow.workflow.task_details import task_details
from test_chat import OPERATOR, FakeAdapter, ask, eventually, started
from test_chat_answers import dispatch, needs_slack, slack_stubs
from test_workflow_safety import engine  # noqa: F401 - fixture

DM = f"D-{OPERATOR}"


def typed(text, thread=None, user=OPERATOR, channel=DM):
    return MessageRequest(user, channel, thread, text)


async def asked(engine, **overrides):
    """A running service whose TEST-1 thread (ts1) holds one open question."""
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **overrides)
    await ask(engine)
    await eventually(lambda: len(adapter.sent) == 2)
    return service, engine.snapshot("TEST-1")["questions"][0]["uid"]


@pytest.mark.asyncio
async def test_typed_messages_use_the_same_commands_as_the_slash_command(engine):
    service = await started(engine, FakeAdapter())
    reply = await service._on_message(typed("list"))
    assert isinstance(reply, Reply) and "TEST-1" in reply.text and reply.offer is None
    assert (await service._on_message(typed("Status test-1"))).text.startswith("*TEST-1*")
    assert "`digest`" in (await service._on_message(typed("what's up?"))).text  # help
    await service.stop()


@pytest.mark.asyncio
async def test_messages_from_anyone_else_or_anywhere_else_get_no_reply(engine):
    service = await started(engine, FakeAdapter())
    assert await service._on_message(typed("list", user="U0INTRUDER")) is None
    assert await service._on_message(typed("list", channel="C0PUBLIC")) is None
    await service.stop()


@pytest.mark.asyncio
async def test_a_reply_in_a_task_thread_is_about_that_task(engine):
    service, _ = await asked(engine)
    assert (await service._on_message(typed("status", thread="ts1"))).text.startswith("*TEST-1*")
    assert "TEST-1" in (await service._on_message(typed("status", thread="ts-unknown"))).text  # the list
    assert (await service._on_message(typed("status", thread="ts1", channel="C0PUBLIC"))) is None
    await service.stop()


@pytest.mark.asyncio
async def test_typed_text_in_a_thread_is_offered_as_an_answer_never_sent(engine):
    service, uid = await asked(engine)
    reply = await service._on_message(typed("  Gradual, over a week ", thread="ts1"))
    assert reply.offer == AnswerOffer("TEST-1", uid, "Gradual, over a week")
    engine.zellij.write_line.assert_not_awaited()

    assert (await service._on_message(typed("one\ntwo", thread="ts1"))).text == \
        "Not offered as an answer: Answer must be a single line."
    assert (await service._on_message(typed("Gradual", thread=None))).offer is None  # not in a thread

    # An answer that starts with a command word runs the command and is still offered;
    # a bare command word is only a command.
    mixed = await service._on_message(typed("status quo is fine", thread="ts1"))
    assert mixed.text.startswith("No AgentFlow task QUO.") and mixed.offer.text == "status quo is fine"
    assert (await service._on_message(typed("status", thread="ts1"))).offer is None

    # Using the offer's control is an ordinary answer, with all of its checks.
    assert await service._on_answer(AnswerRequest(OPERATOR, DM, "TEST-1", uid, text=reply.offer.text)) is None
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "Gradual, over a week")
    after = await service._on_message(typed("Gradual", thread="ts1"))
    assert after.offer is None  # nothing is open any more
    await service.stop()


@pytest.mark.asyncio
async def test_no_offer_when_answering_is_off(engine):
    service, _ = await asked(engine, allowActions=[])
    reply = await service._on_message(typed("Gradual", thread="ts1"))
    assert reply.offer is None and "`list`" in reply.text
    await service.stop()


@pytest.mark.asyncio
async def test_details_are_off_by_default(engine):
    service = await started(engine, FakeAdapter())
    for command in ("details TEST-1", "activity TEST-1"):
        assert (await service._on_command(OPERATOR, command)).text.startswith("Task details are off.")
    await service.stop()


@pytest.mark.asyncio
async def test_details_post_only_the_allowed_sections(engine):
    service, _ = await asked(engine, details=["requirements", "activity"])
    refused = (await service._on_command(OPERATOR, "details TEST-1 findings")).text
    assert refused == "Not enabled in slack.details: findings."

    everything = (await service._on_message(typed("details", thread="ts1"))).text
    assert "*Requirements* (approved)" in everything and "> Approved scope" in everything
    assert "*Recent activity*" in everything and "⚠ Implementer asked: Which rollout?" in everything
    assert "*Open findings*" not in everything and "*Cost*" not in everything and "*Errors*" not in everything

    activity = (await service._on_command(OPERATOR, "activity TEST-1 1")).text
    assert "*Recent activity*" in activity and "*Requirements*" not in activity
    assert activity.count("\n• ") == 1
    assert (await service._on_command(OPERATOR, "details")).text == "Which task? Use `details KEY`, or ask in the task's thread."
    assert (await service._on_command(OPERATOR, "details NOPE-9")).text == "No AgentFlow task NOPE-9."
    await service.stop()


@pytest.mark.asyncio
async def test_requirements_changed_after_approval_are_not_called_approved(engine):
    with engine.db.session() as session:
        workspace = engine.workspace(engine._load_task(session, "TEST-1"))
    (workspace.requirements_dir / "requirements.md").write_text("Rewritten after approval")
    service = await started(engine, FakeAdapter(), details=["requirements"])
    text = (await service._on_command(OPERATOR, "details TEST-1")).text
    assert "*Requirements* (changed since approval)" in text and "> Rewritten after approval" in text
    await service.stop()


def test_disallowed_sections_are_never_read():
    """What is not allowed never enters the output, so nothing built on it can leak it."""
    snapshot = {"jiraKey": "K-1", "status": "ACTIVE", "statusMessage": "SECRET status",
                "findings": [{"status": "open", "title": "SECRET finding", "severity": "high", "blocking": True}],
                "nodes": [{"key": "a", "label": "A", "error": "SECRET error", "usage": {"runs": 1}}],
                "totalUsage": {"runs": 1, "billedCostUsd": 1.5}, "events": [{"message": "SECRET event"}],
                "requirements": {"requirements": "SECRET requirement"}, "prUrl": "https://x/SECRET-pr",
                "jiraIssue": {"markdown": "## Description\n\nSECRET ticket"}}
    details = task_details(snapshot, ["cost"])
    assert list(details) == ["cost"] and "SECRET" not in repr(details)
    assert list(task_details(snapshot, ["cost", "errors"], ["errors"])) == ["errors"]


def jira_issue_markdown(description, comments):
    """jira-issue.md as services/jira_client._issue_markdown writes it."""
    parts = ["# K-1: Title", "## Description", description, f"## Comments ({len(comments)})"]
    for number, body in enumerate(comments, 1):
        parts += [f"### Comment {number} - Author {number}, 2026-09-0{number} 10:00", body]
    return "\n\n".join(parts) + "\n"


def test_pr_and_ticket_details_are_trimmed_and_escaped():
    description = "\n".join(f"step {i} <!channel>" for i in range(40))
    comments = [f"old comment {i}" for i in range(1, 3)] + ["Ping <!here>\n## Comments (9)", "done", "x" * 900]
    snapshot = {"jiraKey": "K-1", "phase": "READY_FOR_PR", "prUrl": "https://github.com/o/r/pull/7",
                "prCreatedAt": "2026-09-01T10:00:00+00:00", "mergedAt": None, "prSyncPaused": True,
                "worktreePath": "/secret/worktree", "repositoryRoot": "/secret/repo",
                "jiraIssue": {"markdown": jira_issue_markdown(description, comments), "syncedAt": "2026-09-02T08:30:00+00:00"}}
    details = task_details(snapshot, ["pr", "ticket"])
    assert details["pr"] == {"url": "https://github.com/o/r/pull/7", "createdAt": "2026-09-01T10:00:00+00:00",
                             "mergedAt": None, "syncPaused": True}
    assert "/secret" not in repr(details)
    ticket = details["ticket"]
    assert ticket["description"].startswith("step 0") and "step 14" in ticket["description"]
    assert "step 15" not in ticket["description"] and ticket["description"].endswith("…")
    assert ticket["comments"]["count"] == 5
    assert [c["by"] for c in ticket["comments"]["latest"]] == ["Author 3, 2026-09-03 10:00", "Author 4, 2026-09-04 10:00",
                                                               "Author 5, 2026-09-05 10:00"]
    assert len(ticket["comments"]["latest"][2]["excerpt"]) <= 301  # trimmed, plus the ellipsis

    text = slack_format.render_details(snapshot, details, "http://ui/tasks/K-1")
    assert "*Pull request*\nhttps://github.com/o/r/pull/7\nCreated 2026-09-01\nGitHub PR sync is paused" in text
    assert "*Jira ticket* (synced 2026-09-02 08:30 UTC)" in text and "_Latest comments_ (3 of 5)" in text
    assert "<!channel>" not in text and "<!here>" not in text and "&lt;!here&gt;" in text
    assert "old comment" not in text


def test_pr_and_ticket_details_say_when_there_is_nothing_yet():
    empty = {"jiraKey": "K-1", "phase": "ACTIVE"}
    text = slack_format.render_details(empty, task_details(empty, ["pr", "ticket"]), "http://ui/tasks/K-1")
    assert "*Pull request*\nNone recorded yet." in text and "*Jira ticket*\nNot synced from Jira yet." in text

    unfetched = "# K-1: T\n\n## Description\n\n\n\n## Comments\n\nNot fetched (HTTP 403) - read them through your Jira MCP.\n"
    snapshot = {**empty, "jiraIssue": {"markdown": unfetched, "syncedAt": None}}
    text = slack_format.render_details(snapshot, task_details(snapshot, ["ticket"]), "http://ui/tasks/K-1")
    assert "> (no description)\nComments were not synced." in text and "MCP" not in text
    no_comments = {**empty, "jiraIssue": {"markdown": jira_issue_markdown("Fix it", []), "syncedAt": None}}
    assert "No comments." in slack_format.render_details(no_comments, task_details(no_comments, ["ticket"]), "u")


def test_ticket_details_read_what_the_jira_sync_writes():
    """Parsed from the real writer, so a format change there breaks this test, not Slack silently."""
    fields, rendered = {"summary": "Title"}, {"description": "<p>First line</p><p>Second line</p>"}
    comments = [{"id": str(i), "author": {"displayName": f"Dev {i}"}, "created": "2026-09-01T10:00:00.000+0000",
                 "renderedBody": f"<p>Comment {i}</p>"} for i in range(1, 5)]
    ticket = task_details({"jiraIssue": {"markdown": _issue_markdown("K-1", fields, rendered, comments)}}, ["ticket"])["ticket"]
    assert ticket["description"] == "First line\nSecond line"
    assert ticket["comments"]["count"] == 4
    assert [(c["by"], c["excerpt"]) for c in ticket["comments"]["latest"]] == [
        (f"Dev {i}, 2026-09-01 10:00", f"Comment {i}") for i in (2, 3, 4)]
    off = task_details({"jiraIssue": {"markdown": _issue_markdown("K-1", fields, rendered, None)}}, ["ticket"])["ticket"]
    assert off["description"] == "First line\nSecond line" and off["comments"] is None
    failed = _issue_markdown("K-1", fields, rendered, None, comments_error="HTTP 403")
    assert task_details({"jiraIssue": {"markdown": failed}}, ["ticket"])["ticket"]["comments"] is None
    # Tasks synced before comments were fetched: the description follows the title directly.
    legacy = "# K-1: Title\n\nRelease Instructions\n\nFollow the normal release cycle.\n"
    ticket = task_details({"jiraIssue": {"markdown": legacy}}, ["ticket"])["ticket"]
    assert ticket["description"] == "Release Instructions\nFollow the normal release cycle." and ticket["comments"] is None


@pytest.mark.asyncio
async def test_the_synced_ticket_is_read_only_when_ticket_details_are_asked_for(engine):
    with engine.db.session() as session:
        workspace = engine.workspace(engine._load_task(session, "TEST-1"))
    workspace.context_dir.mkdir(parents=True, exist_ok=True)
    (workspace.context_dir / "jira-issue.md").write_text(jira_issue_markdown("Make login faster", ["Ship it"]), encoding="utf-8")
    service = await started(engine, FakeAdapter(), details=["pr", "cost"])
    with patch.object(engine, "jira_issue", wraps=engine.jira_issue) as read:
        text = (await service._on_command(OPERATOR, "details TEST-1")).text
        assert "*Pull request*\nNone recorded yet." in text and "Make login faster" not in text
        assert "Not enabled in slack.details: ticket." in (await service._on_command(OPERATOR, "details TEST-1 ticket")).text
        read.assert_not_called()
    await service.stop()

    service = await started(engine, FakeAdapter(), details=["pr", "ticket"])
    with patch.object(engine, "jira_issue", wraps=engine.jira_issue) as read:
        text = (await service._on_command(OPERATOR, "details TEST-1 pr")).text
        assert "*Pull request*" in text and "*Jira ticket*" not in text
        read.assert_not_called()  # allowed, but not asked for
        text = (await service._on_command(OPERATOR, "details TEST-1")).text
        assert "> Make login faster" in text and "> Ship it" in text
        read.assert_called_once_with("TEST-1")
    await service.stop()


def test_details_render_untrusted_text_escaped_and_trimmed():
    snapshot = {"jiraKey": "K-1", "phase": "Code Review: FAILED",
                "findings": [{"status": "open", "title": "Ping <!channel>", "severity": "high", "blocking": True,
                              "file": "a`b.py", "line": 3}] +
                            [{"status": "open", "title": f"minor {i}", "severity": "low"} for i in range(20)] +
                            [{"status": "resolved", "title": "gone"}],
                "nodes": [{"label": "Code Review", "error": "\n".join(f"line {i} <http://x|y>" for i in range(50))}],
                "totalUsage": {"runs": 2, "billedCostUsd": 1.25, "estimatedCostUsd": 3.5, "totalTokens": 12000},
                "budget": {"task": {"knownUsd": 1.25, "limitUsd": 10, "unknownRuns": 0}}}
    text = slack_format.render_details(snapshot, task_details(snapshot, ["findings", "errors", "cost"]), "http://ui/tasks/K-1")
    assert "<!channel>" not in text and "&lt;!channel&gt;" in text and "<http://x|y>" not in text
    assert "• [high, blocking] Ping &lt;!channel&gt; · `a'b.py:3`" in text
    assert "…and 6 more" in text and "gone" not in text  # 21 open, 15 shown; resolved hidden
    assert "line 5" in text and "line 6" not in text  # errors are cut to a few lines
    assert "Total: 2 runs · billed $1.25 · estimated $3.50 · 12,000 tokens" in text
    assert "Ticket API spend: $1.25 of $10.00" in text and text.endswith("<http://ui/tasks/K-1|Open in AgentFlow>")


@pytest.mark.asyncio
async def test_digest_lists_what_waits_and_shows_spend_only_when_cost_is_allowed(engine):
    service, _ = await asked(engine)
    digest = await service._on_message(typed("digest"))
    assert digest.text.startswith("*AgentFlow digest*: 1 active task, 1 waiting on you")
    assert "needs you for 0m: Implementer: Answer questions (1 question)" in digest.text
    assert "API spend" not in digest.text
    await service.stop()

    service = await started(engine, FakeAdapter(), details=["cost"])
    assert "Today's API spend: $0.00" in (await service._on_command(OPERATOR, "digest")).text
    await service.stop()


def test_typed_slack_text_reaches_agentflow_as_written():
    assert slack_format.unslack("a &lt; b &amp;&amp; c &gt; d") == "a < b && c > d"
    assert slack_format.unslack("see <https://x.io/a?b=1&amp;c=2|x.io> and <https://y.io>") == "see x.io and https://y.io"
    assert slack_format.unslack("<@U123> <!here> <#C1|general>") == "@U123 here general"
    assert slack_format.unslack("&amp;lt;") == "&lt;"  # unescaped once, not twice


def test_offer_buttons_carry_the_text_and_refuse_what_does_not_fit():
    offer = AnswerOffer("TEST-1", "u" * 36, "x | y")
    blocks = slack_format.offer_blocks(offer)
    button = blocks[-1]["elements"][0]
    assert button["action_id"] == slack_format.TYPED_ACTION
    assert slack_format.parse_offer(button["value"]) == ("TEST-1", "u" * 36, "x | y")
    too_long = slack_format.offer_blocks(AnswerOffer("TEST-1", "u" * 36, "z" * 1990))
    assert [block["type"] for block in too_long] == ["section"]
    assert too_long[0]["text"]["text"] == slack_format.OFFER_TOO_LONG


def test_a_reply_renders_its_text_then_its_buttons_then_the_offer():
    reply = Reply("No AgentFlow task QUO.", AnswerOffer("TEST-1", "u", "cancel it"),
                  (Button("cancel", "Cancel TEST-1", "TEST-1", "Sure?"),))
    blocks = slack_format.reply_blocks(reply)
    assert [block["type"] for block in blocks] == ["section", "actions", "section", "actions"]
    assert blocks[0]["text"]["text"] == "No AgentFlow task QUO."
    assert blocks[1]["elements"][0]["style"] == "danger" and blocks[3]["elements"][0]["value"] == "TEST-1|u|cancel it"
    assert blocks[2]["text"]["text"] == f"{slack_format.OFFER_PROMPT}\n> cancel it"
    assert slack_format.reply_blocks(Reply("short")) is None


@needs_slack
@pytest.mark.asyncio
async def test_slack_dm_messages_reach_the_service_and_bots_are_ignored():
    calls, patches = slack_stubs()
    seen = []

    async def on_message(request):
        seen.append(request)
        return Reply("status reply")
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_message = on_message

    def event(**fields):
        return {"type": "event_callback", "team_id": "T1", "api_app_id": "A1", "event_id": f"Ev{len(fields)}",
                "event_time": 1, "event": {"type": "message", "channel": "D1", "user": OPERATOR, "ts": "5.0",
                                            "channel_type": "im", **fields}}
    for p in patches:
        p.start()
    try:
        # Listeners run after the ack, so the ignored events go first and the valid one last;
        # once its reply is posted, the earlier ones have been handled too.
        await dispatch(adapter, event(text="from a bot", bot_id="B9"))
        await dispatch(adapter, event(text="edited", subtype="message_changed"))
        await dispatch(adapter, event(text="in a channel", channel_type="channel"))
        await dispatch(adapter, event(text="status &lt;x&gt;", thread_ts="1.0"))
        await eventually(lambda: calls["chat_postMessage"])
        await asyncio.sleep(0.1)
    finally:
        for p in patches:
            p.stop()
    assert seen == [MessageRequest(OPERATOR, "D1", "1.0", "status <x>")]
    posted = calls["chat_postMessage"]
    assert len(posted) == 1 and posted[0]["thread_ts"] == "1.0" and posted[0]["text"] == "status reply"
    # 👀 on the typed message while it is worked on, swapped for ✅ once the reply is posted.
    eyes = {"channel": "D1", "timestamp": "5.0", "name": "eyes"}
    done = {"channel": "D1", "timestamp": "5.0", "name": "white_check_mark"}
    assert calls["reactions"] == [("add", eyes), ("remove", eyes), ("add", done)]


@needs_slack
@pytest.mark.asyncio
async def test_slack_replies_without_the_reaction_when_slack_refuses_it():
    from slack_sdk.errors import SlackApiError
    from slack_sdk.web.async_client import AsyncWebClient

    calls, patches = slack_stubs()

    async def missing_scope(self, **_):
        raise SlackApiError("missing_scope", {"ok": False, "error": "missing_scope"})

    async def on_message(request):
        return Reply("status reply")
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_message = on_message
    message = {"type": "event_callback", "team_id": "T1", "api_app_id": "A1", "event_id": "Ev1", "event_time": 1,
               "event": {"type": "message", "channel": "D1", "user": OPERATOR, "ts": "5.0", "channel_type": "im",
                         "text": "status"}}
    for p in [*patches, patch.object(AsyncWebClient, "reactions_add", missing_scope)]:
        p.start()
    try:
        await dispatch(adapter, message)
        await eventually(lambda: calls["chat_postMessage"])
        await asyncio.sleep(0.1)
    finally:
        patch.stopall()
    assert calls["chat_postMessage"][0]["text"] == "status reply"
    assert calls["reactions"] == []  # nothing to take off


@needs_slack
@pytest.mark.asyncio
async def test_slack_unanswered_message_loses_its_reaction_without_a_check_mark():
    calls, patches = slack_stubs()

    async def on_message(request):
        return None  # not the operator's DM
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_message = on_message
    message = {"type": "event_callback", "team_id": "T1", "api_app_id": "A1", "event_id": "Ev1", "event_time": 1,
               "event": {"type": "message", "channel": "D1", "user": "U0INTRUDER", "ts": "5.0", "channel_type": "im",
                         "text": "status"}}
    for p in patches:
        p.start()
    try:
        await dispatch(adapter, message)
        await eventually(lambda: len(calls["reactions"]) == 2)
        await asyncio.sleep(0.1)
    finally:
        patch.stopall()
    assert [(verb, kwargs["name"]) for verb, kwargs in calls["reactions"]] == [("add", "eyes"), ("remove", "eyes")]
    assert calls["chat_postMessage"] == []


@needs_slack
@pytest.mark.asyncio
async def test_slack_send_as_answer_delivers_the_text_and_drops_the_button():
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    calls, patches = slack_stubs()
    updates, answers = [], []

    async def chat_update(self, **kwargs):
        updates.append(kwargs)
        return AsyncSlackResponse(client=self, http_verb="POST", api_url="x", req_args={}, data={"ok": True},
                                  headers={}, status_code=200)

    async def on_answer(request):
        answers.append(request)
        return None
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_answer = on_answer
    click = {"type": "block_actions", "team": {"id": "T1"}, "user": {"id": OPERATOR}, "api_app_id": "A1",
             "channel": {"id": "D1"}, "trigger_id": "t1", "message": {"ts": "6.0", "thread_ts": "1.0", "blocks": []},
             "actions": [{"action_id": slack_format.TYPED_ACTION, "block_id": "offer", "type": "button",
                          "value": "TEST-1|uid-1|a | b <c>", "action_ts": "7.0"}]}
    for p in [*patches, patch.object(AsyncWebClient, "chat_update", chat_update)]:
        p.start()
    try:
        assert (await dispatch(adapter, click)).status == 200
        await eventually(lambda: updates)
    finally:
        patch.stopall()
    assert answers == [AnswerRequest(OPERATOR, "D1", "TEST-1", "uid-1", text="a | b <c>")]
    assert updates[0]["ts"] == "6.0" and updates[0]["text"] == "*Sent as your answer.*\n> a | b &lt;c&gt;"
    assert calls["chat_postMessage"] == []
