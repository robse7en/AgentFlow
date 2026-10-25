import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import pytest
from sqlalchemy import select

from agentflow.database.models import ChatThread, Task, WorkflowNode
from agentflow.services.chat import slack as slack_format
from agentflow.services.chat.base import ActionRequest, AnswerOffer, Button, OptionsRequest, TaskDraft, TaskForm
from agentflow.services.git_workspace import GitError
from agentflow.workflow import default_workflow as wf
from agentflow.workflow.engine import WorkflowError
from test_chat import OPERATOR, FakeAdapter, eventually, quiet, set_node, started
from test_chat_answers import dispatch, needs_slack, slack_stubs
from test_chat_messages import asked, typed
from test_workflow_safety import engine  # noqa: F401 - fixture

DM = f"D-{OPERATOR}"
CREATE = {"allowActions": ["create"]}
CREATE_AND_RUN = {"allowActions": ["create", "run"]}


def draft(**fields):
    return TaskDraft(**{"user_id": OPERATOR, "jira_key": "NEW-1", "repository_path": "D:/Repos/app",
                        "base_branch": "main", **fields})


def fake_create_task(engine):
    """Stands in for engine.create_task (git worktree, Jira, zellij): records the task and publishes."""
    async def create(request):
        with engine.db.session() as session:
            task = Task(jira_key=request.jira_key, repository_path=request.repository_path,
                        repository_root=request.repository_path, base_branch=request.base_branch,
                        branch_name=f"agent/{request.jira_key}", worktree_path="unused",
                        zellij_session_name=f"agentflow-{request.jira_key}",
                        task_dir=str(engine.config.task_dir / request.jira_key))
            session.add(task)
            session.flush()
            for node in engine.spec.nodes:
                if node.key == wf.ORCHESTRATOR:
                    continue
                session.add(WorkflowNode(task_id=task.id, key=node.key, kind=node.kind, label=node.label, role=node.role,
                                         execution_mode="manual", state="READY" if node.key == wf.ARCHITECT else "PENDING"))
            engine.workspace(task).create({}, None)
        return await engine._publish(request.jira_key)
    return AsyncMock(side_effect=create)


def run(value="TEST-1|architect", user=OPERATOR, channel=DM):
    return ActionRequest(user, channel, "run", value)


@pytest.mark.asyncio
async def test_creating_tasks_is_off_by_default(engine):
    engine.create_task = AsyncMock()
    service = await started(engine, FakeAdapter())
    off = (await service._on_message(typed("new"))).text
    assert off.startswith("Creating tasks from Slack is turned off.")
    assert await service._on_new_task(OPERATOR, None) == off
    assert await service._on_submit_task(draft()) == {"ticket": off}
    engine.create_task.assert_not_awaited()
    await service.stop()


@pytest.mark.asyncio
async def test_new_offers_a_button_that_opens_a_prefilled_form(engine, tmp_path):
    service = await started(engine, FakeAdapter(), **CREATE)
    reply = await service._on_message(typed("new abc-12"))
    assert reply.buttons == (Button("new_task", "Create ABC-12", "ABC-12"),)
    assert (await service._on_message(typed("new"))).buttons == (Button("new_task", "Create a task", ""),)

    form = await service._on_new_task(OPERATOR, "abc-12")
    assert form == TaskForm("ABC-12", str(tmp_path), False, "direct")  # the fixture task's repository
    engine.config.workspace.default_repository_directory = "D:/Default"
    assert (await service._on_new_task(OPERATOR, "not a key")).repository == "D:/Default"
    assert (await service._on_new_task(OPERATOR, "not a key")).jira_key is None
    assert await service._on_new_task("U0INTRUDER", None) == "You are not authorized to use AgentFlow."
    await service.stop()


@pytest.mark.asyncio
async def test_form_fields_offer_repositories_branches_and_what_was_typed(engine, tmp_path):
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(branches=["develop", "main", "release/main"]))
    service = await started(engine, FakeAdapter(), **CREATE)

    def ask(field, query, repository=None, user=OPERATOR):
        return service._on_options(OptionsRequest(user, field, query, repository))

    assert await ask("repository", "") == [(str(tmp_path), str(tmp_path))]
    assert (await ask("repository", "C:\\New\\repo"))[0] == ("C:\\New\\repo", "C:\\New\\repo (as typed)")
    assert await ask("repository", "", user="U0INTRUDER") == []

    assert await ask("branch", "ma", "D:/Repos/app") == [
        ("main", "main"), ("release/main", "release/main"), ("ma", "ma (as typed)")]
    assert await ask("branch", "main", "D:/Repos/app") == [("main", "main"), ("release/main", "release/main")]
    engine.git.inspect_repository.assert_awaited_once()  # cached between keystrokes
    assert await ask("branch", "x") == []  # no repository chosen yet

    engine.git.inspect_repository = AsyncMock(side_effect=GitError("not a git repository"))
    assert await ask("branch", "feature/y", "D:/elsewhere") == [("feature/y", "feature/y (as typed)")]

    assert await ask("ticket", "abc-1") == [("ABC-1", "ABC-1 (as typed)")]  # Jira is not configured
    assert await ask("ticket", "login bug") == []
    await service.stop()


@pytest.mark.asyncio
async def test_the_project_field_offers_the_repository_folders(engine):
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root="D:/Repos/app"))
    engine.git.directories = AsyncMock(return_value=[
        ".github", ".github/workflows", "backend", "backend/api", "frontend", "frontend/node_modules/x", "web/api"])
    service = await started(engine, FakeAdapter(), **CREATE)

    def ask(query, repository="D:/Repos/app"):
        return service._on_options(OptionsRequest(OPERATOR, "project", query, repository))

    assert await ask("") == [(".", "Whole repository"), ("backend", "backend"), ("frontend", "frontend"),
                             ("backend/api", "backend/api"), ("web/api", "web/api")]
    assert await ask("api") == [("backend/api", "backend/api"), ("web/api", "web/api"), ("api", "api (as typed)")]
    assert await ask("web\\api\\") == [("web/api", "web/api")]
    engine.git.directories.assert_awaited_once_with("D:/Repos/app")  # cached between keystrokes
    assert await ask("x", repository=None) == []  # no repository chosen yet

    engine.git.inspect_repository = AsyncMock(side_effect=GitError("not a git repository"))
    assert await ask("src", "D:/elsewhere") == [("src", "src (as typed)")]
    await service.stop()


@pytest.mark.asyncio
async def test_the_form_is_checked_quickly_and_creation_runs_afterwards(engine):
    engine.create_task = AsyncMock()
    service = await started(engine, FakeAdapter(), **CREATE)
    assert "ticket" in await service._on_submit_task(draft(jira_key="ab c"))
    assert await service._on_submit_task(draft(repository_path=" ", base_branch="")) == {
        "repository": "Choose a repository.", "branch": "Choose a base branch."}
    assert await service._on_submit_task(draft(jira_key="test-1")) == {"ticket": "A task for TEST-1 already exists."}
    assert await service._on_submit_task(draft(user_id="U0INTRUDER")) == {"ticket": "You are not authorized to use AgentFlow."}
    engine.create_task.assert_not_awaited()

    assert await service._on_submit_task(draft(jira_key="new-1", project_relative_path=" api ",
                                               implementation_mode="parallel", additional_context="  ")) == {}
    await eventually(lambda: engine.create_task.await_count == 1)
    request = engine.create_task.await_args.args[0]
    assert (request.jira_key, request.repository_path, request.base_branch) == ("NEW-1", "D:/Repos/app", "main")
    assert (request.project_relative_path, request.implementation_mode, request.additional_context) == ("api", "parallel", None)
    await service.stop()


@pytest.mark.asyncio
async def test_a_created_task_opens_its_thread_with_a_run_button(engine):
    engine.create_task = fake_create_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **CREATE_AND_RUN)
    assert await service._on_submit_task(draft()) == {}
    await eventually(lambda: len(adapter.sent) == 2)
    (channel, title, parent), (_, intro, thread) = adapter.sent
    assert channel == DM and parent is None and thread == "ts1" and title.startswith("*NEW-1*")
    assert intro == "Created from Slack: branch `agent/NEW-1` from `main`."
    assert adapter.buttons[1] == (Button("run", "Run Architect", "NEW-1|architect", "Start Architect for NEW-1?"),)
    with engine.db.session() as session:
        row = session.scalar(select(ChatThread).join(Task).where(Task.jira_key == "NEW-1"))
        assert (row.channel, row.thread_id) == (DM, "ts1")
    await engine._publish("NEW-1")
    await quiet()
    assert len(adapter.sent) == 2  # the intro is posted once
    # Replies in the new thread are about the new task.
    assert (await service._on_message(typed("status", thread="ts1"))).text.startswith("*NEW-1*")
    await service.stop()


@pytest.mark.asyncio
async def test_a_stage_that_cannot_start_from_slack_is_explained_not_offered(engine):
    engine.create_task = fake_create_task(engine)
    engine.config.workflow.cost_controls.require_metered_approval = True
    adapter = FakeAdapter()
    service = await started(engine, adapter, **CREATE_AND_RUN)
    await service._on_submit_task(draft())
    await eventually(lambda: len(adapter.sent) == 2)
    assert adapter.sent[1][1].endswith("\nArchitect needs API launch approval; start it in AgentFlow.")
    assert adapter.buttons[1] == ()
    await service.stop()


@pytest.mark.asyncio
async def test_new_in_a_question_thread_is_only_a_command_when_bare(engine):
    service, uid = await asked(engine, allowActions=["answer", "create"])
    reply = await service._on_message(typed("new approach, please", thread="ts1"))
    assert reply.offer == AnswerOffer("TEST-1", uid, "new approach, please") and reply.buttons == ()
    assert (await service._on_message(typed("new", thread="ts1"))).buttons == (Button("new_task", "Create a task", ""),)
    assert (await service._on_message(typed("new abc-2", thread="ts1"))).buttons[0].value == "ABC-2"
    assert "`list`" in (await service._on_message(typed("new approach, please"))).text  # outside a thread: help
    await service.stop()


@pytest.mark.asyncio
async def test_plain_words_ask_for_the_create_button(engine):
    service = await started(engine, FakeAdapter(), **CREATE)
    for text in ("create a new task", "Create a task", "new task", "make me a new task.", "can you create a task?",
                 "please start a new task", "add task"):
        assert (await service._on_message(typed(text))).buttons == (Button("new_task", "Create a task", ""),), text
    for text in ("new task for abc-12", "create a task ABC-12, please"):
        assert (await service._on_message(typed(text))).buttons == (Button("new_task", "Create ABC-12", "ABC-12"),), text
    for text in ("create a new task for the login bug", "why can't I create a task", "task"):
        assert (await service._on_message(typed(text))).buttons == (), text
    await service.stop()


@pytest.mark.asyncio
async def test_a_create_phrase_in_a_question_thread_is_also_offered_as_the_answer(engine):
    service, uid = await asked(engine, allowActions=["answer", "create"])
    reply = await service._on_message(typed("create a new task", thread="ts1"))
    assert reply.buttons == (Button("new_task", "Create a task", ""),)
    assert reply.offer == AnswerOffer("TEST-1", uid, "create a new task")
    await service.stop()


@pytest.mark.asyncio
async def test_a_create_phrase_says_when_creating_is_off(engine):
    service = await started(engine, FakeAdapter())
    reply = await service._on_message(typed("create a new task"))
    assert reply.text.startswith("Creating tasks from Slack is turned off.") and reply.buttons == ()
    await service.stop()


@pytest.mark.asyncio
async def test_no_run_button_unless_running_is_allowed(engine):
    engine.create_task = fake_create_task(engine)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **CREATE)
    await service._on_submit_task(draft())
    await eventually(lambda: len(adapter.sent) == 2)
    assert adapter.buttons[1] == ()
    await service.stop()


@pytest.mark.asyncio
async def test_a_failed_creation_is_a_direct_message_without_git_output(engine):
    engine.create_task = AsyncMock(side_effect=WorkflowError("fatal: C:\\Users\\me\\secret <b>", 409))
    adapter = FakeAdapter()
    service = await started(engine, adapter, **CREATE)
    await service._on_submit_task(draft())
    await eventually(lambda: adapter.sent)
    assert adapter.sent == [(DM, "*Could not create NEW-1.*\nThe reason is in the AgentFlow backend log.", None)]
    await service.stop()

    adapter = FakeAdapter()
    service = await started(engine, adapter, details=["errors"], **CREATE)
    await service._on_submit_task(draft())
    await eventually(lambda: adapter.sent)
    assert adapter.sent[0][1] == "*Could not create NEW-1.*\n> fatal: C:\\Users\\me\\secret &lt;b&gt;"
    await service.stop()


@pytest.mark.asyncio
async def test_a_ready_stage_starts_from_a_button(engine):
    async def start(key, node_key, **_):
        set_node(engine, node_key, "RUNNING")
    engine.run_node = AsyncMock(side_effect=start)
    set_node(engine, wf.ARCHITECT, "READY")
    service = await started(engine, FakeAdapter(), allowActions=["run"])
    assert await service._on_action(run()) is None
    engine.run_node.assert_awaited_once_with("TEST-1", "architect", ready_only=True)
    assert await service._on_action(run()) == "Architect is RUNNING; only a READY stage can be started from Slack."
    await service.stop()


@pytest.mark.asyncio
async def test_starting_a_stage_is_refused_when_the_ui_must_decide(engine):
    engine.run_node = AsyncMock()
    set_node(engine, wf.ARCHITECT, "READY")
    service = await started(engine, FakeAdapter(), allowActions=["run"])
    assert await service._on_action(run(user="U0INTRUDER")) == "You are not authorized to use AgentFlow."
    assert await service._on_action(run(channel="C0PUBLIC")) == "Use AgentFlow's buttons in your direct messages."
    assert await service._on_action(run("NOPE-1|architect")) == "That task no longer exists."
    assert await service._on_action(run("TEST-1|nope")) == "That stage no longer exists."

    set_node(engine, wf.IMPLEMENTER, "FAILED")  # a retry is the UI's call
    assert await service._on_action(run("TEST-1|implementer")) == \
        "Implementer is FAILED; only a READY stage can be started from Slack."

    engine.config.workflow.cost_controls.require_metered_approval = True
    assert await service._on_action(run()) == "Architect needs API launch approval; start it in AgentFlow."
    engine.config.workflow.cost_controls.require_metered_approval = False

    engine.config.workflow.cost_controls.daily_budget_usd = 0.5
    assert (await service._on_action(run())).startswith("Not started: Daily API budget reached")
    engine.config.workflow.cost_controls.daily_budget_usd = None

    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = "NEEDS_HUMAN_REVIEW"  # resuming the loop is the UI's call
    assert await service._on_action(run()) == "TEST-1 is NEEDS_HUMAN_REVIEW; start stages in AgentFlow."
    engine.run_node.assert_not_awaited()
    await service.stop()

    service = await started(engine, FakeAdapter())  # "run" not allowed
    assert await service._on_action(run()) == "Starting stages from Slack is turned off; start it in AgentFlow."
    await service.stop()


@pytest.mark.asyncio
async def test_a_launch_the_engine_declines_is_reported(engine):
    async def decline(key, node_key, **_):
        with engine.db.session() as session:
            node = engine._node(engine._load_task(session, "TEST-1"), node_key)
            node.message = "Global agent capacity reached; start again when a slot is free"
    set_node(engine, wf.ARCHITECT, "READY")
    engine.run_node = AsyncMock(side_effect=decline)
    service = await started(engine, FakeAdapter(), allowActions=["run"])
    assert await service._on_action(run()) == \
        "Not started: Global agent capacity reached; start again when a slot is free."
    engine.run_node = AsyncMock(side_effect=WorkflowError("zellij: C:\\secret output", 409))
    assert await service._on_action(run()) == "Not started: AgentFlow could not do it. Check AgentFlow."
    await service.stop()


def test_task_form_fits_slack_limits():
    long_path = "D:/" + "deep/" * 40 + "repo"
    modal = slack_format.task_modal(TaskForm("ABC-1", long_path, True, "parallel"))
    assert len(modal["title"]["text"]) <= 24 and modal["callback_id"] == slack_format.TASK_FORM_CALLBACK
    blocks = {block["block_id"]: block for block in modal["blocks"]}
    assert blocks["ticket"]["element"]["initial_option"]["value"] == "ABC-1"
    assert "initial_option" not in blocks["repository"]["element"]  # too long for an option value
    assert blocks["branch"]["element"]["initial_option"]["value"] == "main"
    assert blocks["project"]["element"]["action_id"] == "agentflow_task_project"
    assert blocks["mode"]["element"]["initial_option"]["value"] == "parallel"
    assert blocks["project"]["optional"] and blocks["context"]["optional"]
    text_form = slack_format.task_modal(TaskForm("ABC-1", None, False, "direct"))
    assert text_form["blocks"][0]["element"] == {"type": "plain_text_input", "action_id": "value", "multiline": False,
                                                 "max_length": 64, "initial_value": "ABC-1"}

    options = slack_format.options_payload([("x" * 151, "too long"), ("D:/a", "D:/" + "b" * 100)])
    assert len(options) == 1 and len(options[0]["text"]["text"]) == 75 and options[0]["text"]["text"].startswith("…")


def test_buttons_carry_confirmations_and_distinct_ids():
    block = slack_format.buttons_block((Button("run", "Run Architect", "K-1|architect", "Start Architect for K-1?"),
                                        Button("run", "Run Code Review", "K-1|code_review")))
    first, second = block["elements"]
    assert first["action_id"] != second["action_id"]
    assert first["confirm"]["text"]["text"] == "Start Architect for K-1?" and "confirm" not in second


def test_a_button_without_a_value_leaves_it_out():
    # Slack rejects "value": "" (invalid_blocks), which a key-less create-task button would send.
    keyed, keyless = slack_format.buttons_block((Button("new_task", "Create ABC-1", "ABC-1"),
                                                 Button("new_task", "Create a task", "")))["elements"]
    assert keyed["value"] == "ABC-1" and "value" not in keyless


def test_submitted_form_values_are_read_from_every_input_kind():
    values = {"ticket": {"agentflow_task_ticket": {"type": "external_select", "selected_option": {"value": "ABC-1"}}},
              "repository": {"agentflow_task_repository": {"type": "external_select", "selected_option": {"value": "D:/r"}}},
              "branch": {"agentflow_task_branch": {"type": "external_select", "selected_option": None}},
              "project": {"agentflow_task_project": {"type": "external_select", "selected_option": None}},
              "mode": {"value": {"type": "static_select", "selected_option": {"value": "direct"}}},
              "context": {"value": {"type": "plain_text_input", "value": "Be careful"}}}
    assert slack_format.task_draft(OPERATOR, values) == TaskDraft(OPERATOR, "ABC-1", "D:/r", "", None, "direct", "Be careful")


def slack_payload(kind, **fields):
    return {"type": kind, "team": {"id": "T1"}, "user": {"id": OPERATOR}, "api_app_id": "A1", **fields}


@needs_slack
@pytest.mark.asyncio
async def test_slack_new_opens_the_form_and_searches_and_submissions_reach_the_service():
    calls, patches = slack_stubs()
    seen = {"new": [], "options": [], "submit": []}

    async def new_task(user_id, key):
        seen["new"].append((user_id, key))
        return TaskForm(key, "D:/r", True, "direct")

    async def options(request):
        seen["options"].append(request)
        return [("main", "main")]

    async def submit(draft_):
        seen["submit"].append(draft_)
        return {"branch": "Choose a base branch."}
    adapter = slack_format.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_new_task, adapter._on_options, adapter._on_submit_task = new_task, options, submit
    command = {"team_id": "T1", "channel_id": "D1", "user_id": OPERATOR, "command": "/agentflow", "text": "new ABC-1",
               "response_url": "https://hooks.slack.com/commands/1", "trigger_id": "t9", "api_app_id": "A1"}
    for p in patches:
        p.start()
    try:
        await dispatch(adapter, command)
        await eventually(lambda: calls["views_open"])
        view = calls["views_open"][0]["view"]
        assert calls["views_open"][0]["trigger_id"] == "t9" and view["callback_id"] == slack_format.TASK_FORM_CALLBACK
        state = {"values": {"repository": {"agentflow_task_repository": {
            "type": "external_select", "selected_option": {"value": "D:/r", "text": {"type": "plain_text", "text": "D:/r"}}}}}}
        suggestion = slack_payload("block_suggestion", action_id="agentflow_task_branch", block_id="branch", value="ma",
                                   view={**view, "id": "V1", "state": state})
        response = await dispatch(adapter, suggestion)
        submission = slack_payload("view_submission", view={**view, "id": "V1", "state": state})
        rejected = await dispatch(adapter, submission)
    finally:
        for p in patches:
            p.stop()
    assert seen["new"] == [(OPERATOR, "ABC-1")]
    assert seen["options"] == [OptionsRequest(OPERATOR, "branch", "ma", "D:/r")]
    assert json.loads(response.body) == {"options": [{"text": {"type": "plain_text", "text": "main"}, "value": "main"}]}
    assert seen["submit"] == [TaskDraft(OPERATOR, "", "D:/r", "", None, None, None)]
    assert json.loads(rejected.body) == {"response_action": "errors", "errors": {"branch": "Choose a base branch."}}


@needs_slack
@pytest.mark.asyncio
async def test_slack_run_button_starts_the_stage_and_replaces_the_buttons():
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
    intro = {"type": "section", "text": {"type": "mrkdwn", "text": "Created from Slack: branch `agent/K-1`."}}
    click = slack_payload("block_actions", channel={"id": "D1"}, trigger_id="t1",
                          message={"ts": "6.0", "thread_ts": "1.0", "blocks": [intro, {"type": "actions", "elements": []}]},
                          actions=[{"action_id": f"{slack_format.RUN_ACTION}_0", "block_id": "buttons", "type": "button",
                                    "value": "K-1|architect", "text": {"type": "plain_text", "text": "Run Architect"},
                                    "action_ts": "7.0"}])
    for p in [*patches, patch.object(AsyncWebClient, "chat_update", chat_update)]:
        p.start()
    try:
        await dispatch(adapter, click)
        await eventually(lambda: updates)
    finally:
        patch.stopall()
    assert actions == [ActionRequest(OPERATOR, "D1", "run", "K-1|architect")]
    assert updates[0]["text"] == "Created from Slack: branch `agent/K-1`.\n*Architect started.*"
