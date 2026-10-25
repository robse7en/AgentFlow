"""Decision-tree agent questions: validation, the answer line, storage and the Decisions inbox."""

import argparse
import dataclasses
import io
import json
from datetime import timedelta
from types import SimpleNamespace
from unittest.mock import patch

import pytest
from pydantic import ValidationError
from sqlalchemy import inspect, select, text

from agentflow.cli import flowctl
from agentflow.config import Config
from agentflow.database.database import Database
from agentflow.database.models import AgentQuestion, Task
from agentflow.services.chat import slack
from agentflow.services.chat.base import AnswerPrompt, AnswerRequest
from agentflow.workflow import default_workflow as wf
from agentflow.workflow.decisions import ANSWER_SEPARATOR, answer_line, source_allowed
from agentflow.workflow.engine import WorkflowError
from agentflow.workflow.models import QuestionAnswerRequest, QuestionRequest
from agentflow.workflow.states import NodeState

from test_chat import OPERATOR, FakeAdapter, eventually, started
from test_chat_answers import dispatch, needs_slack, slack_stubs
from test_workflow_safety import add_run, add_session, engine, task_state  # noqa: F401 - fixture

TREE = {
    "message": "312 inactive accounts still own open invoices. What should the cleanup do?",
    "context": " 4,108 accounts inactive · billing reads invoice.owner_id ",
    "risk": "high",
    "choices": [
        {"label": "Soft-delete, keep invoices", "recommended": True, "followUp": {
            "message": "When are they purged for good?",
            "choices": [
                {"label": "After their invoices close", "recommended": True, "detail": "A nightly job purges"},
                {"label": "Never", "warning": "Table grows ~4k rows a year",
                 "note": "Legal asked for a purge date.", "source": "docs/privacy.md"},
            ]}},
        {"label": "Skip accounts with open invoices", "detail": "", "recommended": False},
        {"label": "Hard-delete everything", "warning": "Breaks history on 312 invoices",
         "note": "Billing reads owner_id.", "source": "https://example.com/billing#owner"},
    ],
}


def tree(**changes):
    data = json.loads(json.dumps(TREE))
    data.update(changes)
    return data


def asked(data):
    return QuestionRequest(role="architect", **data)


def rejected(data) -> str:
    with pytest.raises(ValidationError) as raised:
        asked(data)
    return str(raised.value)


def choice(index, **changes):
    data = tree()
    data["choices"][index].update(changes)
    return data


def follow_up_option(index, **changes):
    data = tree()
    data["choices"][0]["followUp"]["choices"][index].update(changes)
    return data


# ------------------------------------------------------------------------------ validation

def test_a_tree_is_stored_as_validated_with_its_top_level_labels_as_choices():
    question = asked(tree())
    assert question.choices == ["Soft-delete, keep invoices", "Skip accounts with open invoices", "Hard-delete everything"]
    assert question.decision == {
        "context": "4,108 accounts inactive · billing reads invoice.owner_id",
        "risk": "high",
        "choices": [
            {"label": "Soft-delete, keep invoices", "recommended": True, "followUp": {
                "message": "When are they purged for good?",
                "choices": [
                    {"label": "After their invoices close", "recommended": True, "detail": "A nightly job purges"},
                    {"label": "Never", "warning": "Table grows ~4k rows a year",
                     "note": "Legal asked for a purge date.", "source": "docs/privacy.md"},
                ]}},
            # A blank detail and `recommended: false` are simply absent.
            {"label": "Skip accounts with open invoices"},
            {"label": "Hard-delete everything", "warning": "Breaks history on 312 invoices",
             "note": "Billing reads owner_id.", "source": "https://example.com/billing#owner"},
        ],
    }


def test_flat_questions_keep_their_shape_and_messages():
    question = asked({"message": " Which rollout? ", "choices": [" Gradual ", "All at once"]})
    assert (question.message, question.choices, question.decision) == ("Which rollout?", ["Gradual", "All at once"], None)
    assert asked({"message": "Free text only"}).decision is None
    assert "Question choices must be unique" in rejected({"message": "Pick", "choices": ["Same", "same"]})
    assert "Each choice must contain 1 to 250 characters" in rejected({"message": "Pick", "choices": ["x" * 251]})


def test_context_or_risk_alone_make_a_tree_without_choices():
    question = asked({"message": "Anything else?", "risk": "low"})
    assert question.choices == [] and question.decision == {"context": None, "risk": "low", "choices": []}


@pytest.mark.parametrize("data, problem", [
    (tree(choices=["Plain", {"label": "Object"}]), "choices: use objects for every choice"),
    (tree(risk="severe"), "risk: must be low, medium or high"),
    (tree(context="line\nbreak"), "context: must be a single line"),
    (tree(context="x" * 501), "context: must be at most 500 characters"),
    (tree(choices=[{"label": str(n)} for n in range(9)]), "List should have at most 8 items"),
    (choice(1, label=" "), "choices[1].label: cannot be empty"),
    (choice(1, label="x" * 251), "choices[1].label: must be at most 250 characters"),
    (choice(1, label=f"A{ANSWER_SEPARATOR}B"), "choices[1].label: cannot contain \u2192"),
    (choice(1, label="A\u2192B"), "choices[1].label: cannot contain \u2192"),
    (choice(1, label="Hard-delete EVERYTHING"), "choices: labels must be unique"),
    (choice(1, recommended=True), "choices: at most one choice can be recommended"),
    (choice(1, recommended="yes"), "choices[1].recommended: must be true or false"),
    (choice(1, detail="x" * 201), "choices[1].detail: must be at most 200 characters"),
    (choice(1, warning="x" * 121), "choices[1].warning: must be at most 120 characters"),
    (choice(1, note="x" * 301), "choices[1].note: must be at most 300 characters"),
    (choice(1, source="docs/a.md"), "choices[1].source: needs a note"),
    (choice(2, source="x" * 201), "choices[2].source: must be at most 200 characters"),
    (choice(1, warn="typo"), "choices[1]: unknown field 'warn'"),
    (choice(1, followUp={"message": "Then?", "choices": [{"label": "Only one"}]}),
     "choices[1].followUp.choices: must have 2 to 8 choices"),
    (choice(1, followUp={"message": "Then?", "choices": [{"label": str(n)} for n in range(9)]}),
     "choices[1].followUp.choices: must have 2 to 8 choices"),
    (choice(1, followUp={"message": " ", "choices": [{"label": "A"}, {"label": "B"}]}),
     "choices[1].followUp.message: cannot be empty"),
    (choice(1, followUp={"message": "x" * 501, "choices": [{"label": "A"}, {"label": "B"}]}),
     "choices[1].followUp.message: must be at most 500 characters"),
    (choice(1, followUp={"message": "Then?", "choices": [{"label": "A"}, {"label": "B"}], "risk": "low"}),
     "choices[1].followUp: unknown field 'risk'"),
    (choice(1, followUp="Then?"), "choices[1].followUp: must be an object"),
    (follow_up_option(1, followUp={"message": "Deeper?", "choices": [{"label": "A"}, {"label": "B"}]}),
     "choices[0].followUp.choices[1].followUp: follow-up options cannot ask another follow-up"),
    (follow_up_option(1, recommended=True), "choices[0].followUp.choices: at most one choice can be recommended"),
    (follow_up_option(1, label="after their invoices close"), "choices[0].followUp.choices: labels must be unique"),
    (follow_up_option(1, note=None), "choices[0].followUp.choices[1].source: needs a note"),
])
def test_every_rule_rejects_with_the_field_at_fault(data, problem):
    assert problem in rejected(data)


@pytest.mark.parametrize("source, allowed", [
    ("docs/privacy.md", True), ("src\\billing\\models.py", True), ("README.md", True),
    ("https://example.com/policy", True),
    ("../secrets.txt", False), ("docs/../../etc/passwd", False), ("docs\\..\\x", False),
    ("/etc/passwd", False), ("\\\\server\\share", False), ("C:/Windows/win.ini", False), ("c:x", False),
    ("http://example.com", False), ("file:///etc/passwd", False), ("https://", False),
    ("https://example.com/a b", False),
])
def test_a_source_is_a_relative_path_or_an_https_url(source, allowed):
    assert source_allowed(source) is allowed
    data = choice(1, note="Evidence.", source=source)
    if allowed:
        assert asked(data).decision["choices"][1]["source"] == source
    else:
        assert "choices[1].source: must be a repository-relative path" in rejected(data)


# ------------------------------------------------------------------------------ the answer line

def test_answer_lines_for_paths_and_free_text():
    question = asked(tree())
    labels, decision = question.choices, question.decision
    assert answer_line(labels, decision, [2], None) == "3: Hard-delete everything"
    assert answer_line(labels, decision, [0, 0], None) == \
        "1.1: Soft-delete, keep invoices \u2192 After their invoices close"
    assert answer_line(labels, decision, None, "Ask legal first") == "other: Ask legal first"
    # Flat questions: the choice text, or the free text, unchanged.
    assert answer_line(["Gradual", "All at once"], None, [1], None) == "All at once"
    assert answer_line(["Gradual"], None, None, "Something else") == "Something else"


@pytest.mark.parametrize("path, problem", [
    ([0], "That choice has a follow-up question; pick one of its options"),
    ([3], "That choice does not exist"), ([0, 2], "That choice does not exist"),
    ([1, 0], "That choice does not exist"), ([0, 0, 0], "That choice does not exist"), ([], "That choice does not exist"),
])
def test_invalid_paths_are_refused(path, problem):
    question = asked(tree())
    with pytest.raises(ValueError, match=problem):
        answer_line(question.choices, question.decision, path, None)
    with pytest.raises(ValueError, match="That choice does not exist"):
        answer_line(["Gradual"], None, [0, 0], None)


def test_the_longest_path_line_fits_in_one_answer():
    labels = [f"{n}" + "x" * 249 for n in range(8)]
    data = {"message": "Long?", "choices": [{"label": label, "followUp": {
        "message": "Then?", "choices": [{"label": label} for label in labels]}} for label in labels]}
    question = asked(data)
    line = answer_line(question.choices, question.decision, [7, 7], None)
    assert line.startswith("8.8: ") and len(line) == 5 + 250 + len(ANSWER_SEPARATOR) + 250
    assert len(line) <= 4000 and line.isprintable()


def test_answer_requests_take_free_text_or_a_path():
    assert QuestionAnswerRequest(path=[0, 1]).path == [0, 1]
    assert QuestionAnswerRequest(answer=" ok ").answer == "ok"
    for bad in ({}, {"answer": "x", "path": [0]}, {"path": []}, {"path": [0, 1, 2]}, {"path": [-1]},
                {"path": ["1"]}, {"path": [True]}):
        with pytest.raises(ValidationError):
            QuestionAnswerRequest(**bad)


# ------------------------------------------------------------------------------ the engine

async def ask(engine, run_id, data):
    question = asked(data)
    await engine.agent_question("TEST-1", "implementer", run_id, question.message, question.choices, question.decision)
    return engine.snapshot("TEST-1")["questions"][-1]


@pytest.fixture
def waiting(engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    return run_id


async def test_a_path_answer_types_the_built_line_and_is_idempotent(engine, waiting):
    question = await ask(engine, waiting, tree())
    assert question["decision"]["risk"] == "high" and question["choices"][0] == "Soft-delete, keep invoices"

    for path in ([0], [5]):  # a branch with a follow-up, a missing choice: nothing is typed
        with pytest.raises(WorkflowError) as raised:
            await engine.answer_question("TEST-1", question["uid"], path=path)
        assert raised.value.status_code == 422
    engine.zellij.write_line.assert_not_awaited()

    snapshot = await engine.answer_question("TEST-1", question["uid"], path=[0, 1])
    line = f"1.2: Soft-delete, keep invoices{ANSWER_SEPARATOR}Never"
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", line)
    assert snapshot["questions"][0]["answer"] == line
    assert task_state(engine, wf.IMPLEMENTER) == NodeState.RUNNING

    # The same path again (say Slack after the browser) is the same answer; another one is not.
    await engine.answer_question("TEST-1", question["uid"], path=[0, 1])
    engine.zellij.write_line.assert_awaited_once()
    with pytest.raises(WorkflowError, match="already been answered"):
        await engine.answer_question("TEST-1", question["uid"], path=[2])
    with pytest.raises(WorkflowError, match="already been answered"):
        await engine.answer_question("TEST-1", question["uid"], "Never")


async def test_free_text_on_a_tree_gets_the_other_prefix_within_the_answer_limit(engine, waiting):
    question = await ask(engine, waiting, tree())
    with pytest.raises(WorkflowError, match="at most 4000 characters"):
        await engine.answer_question("TEST-1", question["uid"], "x" * 3994)
    await engine.answer_question("TEST-1", question["uid"], "  " + "x" * 3993 + "  ")
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "other: " + "x" * 3993)


async def test_flat_questions_accept_a_path_and_answer_with_the_choice_text(engine, waiting):
    question = await ask(engine, waiting, {"message": "Which rollout?", "choices": ["Gradual", "All at once"]})
    assert question["decision"] is None
    await engine.answer_question("TEST-1", question["uid"], path=[1])
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "All at once")
    # Typing the same text is the same answer.
    await engine.answer_question("TEST-1", question["uid"], "All at once")
    engine.zellij.write_line.assert_awaited_once()


async def test_the_node_keeps_waiting_until_the_last_of_several_answers(engine, waiting):
    first = await ask(engine, waiting, tree())
    second = await ask(engine, waiting, {"message": "Where should it run?", "risk": "low", "choices": [
        {"label": "Nightly worker"}, {"label": "New job", "followUp": {"message": "How often?", "choices": [
            {"label": "Weekly", "recommended": True}, {"label": "Daily"}]}}]})

    snapshot = await engine.answer_question("TEST-1", first["uid"], path=[2])
    node = next(n for n in snapshot["nodes"] if n["key"] == wf.IMPLEMENTER)
    assert node["state"] == NodeState.WAITING_FOR_USER and node["openQuestions"] == 1
    assert [q["actionable"] for q in snapshot["questions"]] == [False, True]

    snapshot = await engine.answer_question("TEST-1", second["uid"], path=[1, 0])
    node = next(n for n in snapshot["nodes"] if n["key"] == wf.IMPLEMENTER)
    assert node["state"] == NodeState.RUNNING and node["openQuestions"] == 0
    assert snapshot["questions"][1]["answer"] == f"2.1: New job{ANSWER_SEPARATOR}Weekly"


def add_task(engine, key, archived=False):
    with engine.db.session() as session:
        source = engine._load_task(session, "TEST-1")
        task = Task(jira_key=key, jira_title=f"{key} title", repository_path=source.repository_path,
                    repository_root=source.repository_root, base_branch="main", branch_name=f"agent/{key}",
                    worktree_path=source.worktree_path, zellij_session_name=f"agentflow-{key}",
                    task_dir=source.task_dir, current_sha=source.current_sha, max_review_iterations=1,
                    archived_at=source.created_at if archived else None)
        session.add(task)
        session.flush()
        for node in source.nodes:
            session.add(type(node)(task_id=task.id, key=node.key, kind=node.kind, label=node.label, role=node.role,
                                   state=node.state, execution_mode=node.execution_mode))
        return task.id


async def test_the_inbox_lists_open_questions_across_tasks_oldest_first(engine, waiting):
    from agentflow.database.models import NodeRun, WorkflowNode

    newer = await ask(engine, waiting, {"message": "Newer, on TEST-1"})
    other_id = add_task(engine, "TEST-2")
    archived_id = add_task(engine, "TEST-3", archived=True)
    with engine.db.session() as session:
        rows = {}
        for task_id in (other_id, archived_id):
            node = session.scalar(select(WorkflowNode).where(WorkflowNode.task_id == task_id,
                                                             WorkflowNode.key == wf.ARCHITECT))
            node.state = NodeState.WAITING_FOR_USER
            run = NodeRun(task_id=task_id, node_id=node.id, run_number=1, state=NodeState.WAITING_FOR_USER,
                          expected_sha="a" * 40, iteration=0, harness="claude", model="opus")
            session.add(run)
            session.flush()
            rows[task_id] = run.id
        earlier = session.scalar(select(AgentQuestion).where(AgentQuestion.uid == newer["uid"])).created_at
        session.add(AgentQuestion(task_id=other_id, node_run_id=rows[other_id], role="architect",
                                  message="Older, on TEST-2", choices=[], created_at=earlier - timedelta(minutes=5),
                                  decision={"context": None, "risk": "medium", "choices": []}))
        session.add(AgentQuestion(task_id=archived_id, node_run_id=rows[archived_id], role="architect",
                                  message="On an archived task", choices=[], created_at=earlier))
        # Unanswered, but its run no longer waits for it: not actionable, so not listed.
        session.add(AgentQuestion(task_id=other_id, node_run_id=None, role="architect", message="Orphan", choices=[]))

    listed = engine.open_questions()
    assert [(q["jiraKey"], q["message"]) for q in listed] == [("TEST-2", "Older, on TEST-2"), ("TEST-1", "Newer, on TEST-1")]
    assert listed[0] | {"uid": None, "createdAt": None, "runId": None} == {
        "uid": None, "role": "architect", "message": "Older, on TEST-2", "choices": [],
        "decision": {"context": None, "risk": "medium", "choices": []}, "answered": False, "answer": None,
        "createdAt": None, "answeredAt": None, "runId": None, "nodeKey": wf.ARCHITECT, "actionable": True,
        "jiraKey": "TEST-2", "jiraTitle": "TEST-2 title", "nodeLabel": "Architect", "harness": "claude", "model": "opus",
    }

    await engine.answer_question("TEST-1", newer["uid"], "Done")
    assert [q["jiraKey"] for q in engine.open_questions()] == ["TEST-2"]


# ------------------------------------------------------------------------------ storage

def test_an_existing_database_gains_the_decision_column_and_old_rows_stay_flat(tmp_path):
    config = Config.model_validate({"app": {"dataDirectory": str(tmp_path), "databasePath": str(tmp_path / "old.db")}})
    database = Database(config.database_file)
    database.create_all()
    with database.session() as session:
        task = Task(jira_key="OLD-1", repository_path=str(tmp_path), repository_root=str(tmp_path), base_branch="main",
                    branch_name="agent/OLD-1", worktree_path=str(tmp_path / "tree"), zellij_session_name="agentflow-OLD-1",
                    task_dir=str(tmp_path / "tasks"), current_sha="a" * 40, max_review_iterations=1)
        session.add(task)
        session.flush()
        task_id = task.id
    with database.engine.begin() as connection:
        connection.exec_driver_sql("ALTER TABLE agent_questions DROP COLUMN decision")
        connection.execute(text(
            "INSERT INTO agent_questions (uid, task_id, role, message, choices, answered, created_at) "
            "VALUES ('00000000-0000-4000-8000-000000000001', :task, 'architect', 'Old', '[\"A\"]', 0, CURRENT_TIMESTAMP)"),
            {"task": task_id})
    database.create_all()
    try:
        assert [c["name"] for c in inspect(database.engine).get_columns("agent_questions")][-1] == "decision"
        with database.session() as session:
            question = session.scalar(select(AgentQuestion))
            assert (question.message, question.choices, question.decision) == ("Old", ["A"], None)
    finally:
        database.engine.dispose()


# ------------------------------------------------------------------------------ flowctl

def test_flowctl_question_takes_a_message_or_a_file_but_not_both():
    parser = flowctl.build_parser()
    assert parser.parse_args(["question", "--file", "-"]).file == "-"
    for argv in (["question"], ["question", "--message", "x", "--file", "-"]):
        with pytest.raises(SystemExit) as raised:
            parser.parse_args(argv)
        assert raised.value.code == 2


def test_flowctl_question_file_reads_utf8_stdin_and_drops_identity_fields(monkeypatch):
    sent = {}
    monkeypatch.setattr(flowctl, "_require", lambda args: None)
    monkeypatch.setattr(flowctl, "_post", lambda args, path, payload: sent.update(path=path, payload=payload)
                        or {"state": "WAITING_FOR_USER"})
    body = json.dumps({**TREE, "role": "reviewer", "runId": 99}, ensure_ascii=False).encode("utf-8")
    monkeypatch.setattr(flowctl.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(b"\xef\xbb\xbf" + body)))
    flowctl.cmd_question(argparse.Namespace(file="-", choice=[], message=None))
    assert sent["path"] == "question"
    assert sent["payload"] == TREE  # "·" survived; role and run come from the environment only

    with pytest.raises(SystemExit, match="--choice cannot be combined with --file"):
        flowctl.cmd_question(argparse.Namespace(file="-", choice=["A"], message=None))
    monkeypatch.setattr(flowctl.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(b"[1, 2]")))
    with pytest.raises(SystemExit, match="must be a JSON object"):
        flowctl.cmd_question(argparse.Namespace(file="-", choice=[], message=None))


# ------------------------------------------------------------------------------ Slack, two steps

async def slack_tree(engine, **overrides):
    """A running chat service that has DMed one decision tree."""
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    adapter = FakeAdapter()
    service = await started(engine, adapter, **overrides)
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    await ask(engine, run_id, tree())
    await eventually(lambda: len(adapter.sent) == 2)
    return service, adapter, engine.snapshot("TEST-1")["questions"][0]["uid"]


def slack_request(uid, **fields):
    return AnswerRequest(**{"user_id": OPERATOR, "channel": f"D-{OPERATOR}", "jira_key": "TEST-1",
                            "question_uid": uid, **fields})


async def test_slack_branch_shows_its_follow_up_and_a_leaf_sends_the_path(engine):
    service, adapter, uid = await slack_tree(engine)
    prompt = adapter.prompts[1]
    assert prompt.decision["risk"] == "high" and prompt.branch is None
    buttons = slack.answer_blocks(prompt)[-1]["elements"]
    assert [b["value"].rsplit("|", 1)[1] for b in buttons] == ["0", "1", "2", "reply"]
    assert buttons[0]["style"] == "primary" and buttons[0]["text"]["text"].endswith(" ›")

    # A branch with a follow-up is not an answer yet: the notice shows its options instead.
    shown = await service._on_answer(slack_request(uid, choice_index=0))
    assert isinstance(shown, AnswerPrompt) and shown.branch == 0 and shown.decision == prompt.decision
    assert await service._on_answer(slack_request(uid, back=True)) == AnswerPrompt(
        "TEST-1", uid, prompt.choices, prompt.decision)
    assert await service._on_answer(slack_request(uid, path=(0, 5))) == "That choice is no longer offered."
    assert await service._on_answer(slack_request(uid, path=(9, 0))) == "That choice is no longer offered."
    engine.zellij.write_line.assert_not_awaited()

    assert await service._on_answer(slack_request(uid, path=(0, 0))) is None
    line = f"1.1: Soft-delete, keep invoices{ANSWER_SEPARATOR}After their invoices close"
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", line)
    await eventually(lambda: adapter.updates)
    assert adapter.updates[-1][1] == "ts2" and adapter.updates[-1][2].startswith(f"*Answered:* {slack.escape(line)}")
    # The notice is closed: its controls now say so instead of acting.
    assert await service._on_answer(slack_request(uid, choice_index=0)) == "That question is no longer open."
    await service.stop()


async def test_slack_leaf_branch_and_free_text_on_a_tree(engine):
    service, adapter, uid = await slack_tree(engine)
    assert await service._on_answer(slack_request(uid, text="Ask legal first")) is None
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "other: Ask legal first")

    question = await ask(engine, engine.snapshot("TEST-1")["questions"][0]["runId"], tree())
    engine.zellij.write_line.reset_mock()
    assert await service._on_answer(slack_request(question["uid"], choice_index=2)) is None
    engine.zellij.write_line.assert_awaited_once_with("agentflow-TEST-1", "terminal_7", "3: Hard-delete everything")
    await service.stop()


async def test_slack_tree_answered_in_the_browser_resolves_the_notice(engine):
    service, adapter, uid = await slack_tree(engine)
    await engine.answer_question("TEST-1", uid, path=[2])
    await eventually(lambda: adapter.updates)
    assert adapter.updates[-1][2].startswith("*Answered:* 3: Hard-delete everything")
    assert await service._on_answer(slack_request(uid, path=(0, 0))) == "That question is no longer open."
    assert await service._on_answer(slack_request(uid, choice_index=2)) == "That question is no longer open."
    engine.zellij.write_line.assert_awaited_once()
    await service.stop()


async def test_slack_tree_without_question_text_offers_no_controls(engine):
    service, adapter, uid = await slack_tree(engine, includeQuestionText=False)
    assert adapter.prompts[1] is None and "Soft-delete" not in adapter.sent[1][1]
    assert await service._on_answer(slack_request(uid, choice_index=0)) == \
        "Answering from Slack is turned off; answer in AgentFlow."
    await service.stop()


@needs_slack
async def test_slack_clicks_carry_the_path_and_a_follow_up_edits_the_notice_in_place():
    from slack_sdk.web.async_client import AsyncWebClient
    from slack_sdk.web.async_slack_response import AsyncSlackResponse

    question = asked(tree())
    prompt = AnswerPrompt("TEST-1", "uid-1", tuple(question.choices), question.decision)
    calls, patches = slack_stubs()
    updates = []

    async def chat_update(self, **kwargs):
        updates.append(kwargs)
        return AsyncSlackResponse(client=self, http_verb="POST", api_url="x", req_args={}, data={"ok": True},
                                  headers={}, status_code=200)
    patches.append(patch.object(AsyncWebClient, "chat_update", chat_update))
    answers = []

    async def on_answer(request):
        answers.append(request)
        if request.back:
            return prompt
        return dataclasses.replace(prompt, branch=0) if request.choice_index == 0 else None
    adapter = slack.SlackAdapter("xoxb-test", "xapp-test")
    adapter._on_answer = on_answer
    notice = slack.notice_blocks("Implementer asked a question", prompt)

    def click(action_id, value, blocks):
        return {"type": "block_actions", "team": {"id": "T1"}, "user": {"id": OPERATOR}, "api_app_id": "A1",
                "channel": {"id": "D1"}, "trigger_id": "t1",
                "message": {"ts": "2.0", "thread_ts": "1.0", "text": "Implementer asked a question", "blocks": blocks},
                "actions": [{"action_id": action_id, "block_id": "answer", "type": "button",
                             "value": value, "action_ts": "3.0"}]}

    for p in patches:
        p.start()
    try:
        await dispatch(adapter, click("agentflow_answer_choice_0", "TEST-1|uid-1|0", notice))
        await eventually(lambda: updates)
        follow_up = updates[-1]["blocks"]
        assert updates[-1]["ts"] == "2.0" and follow_up[0] == notice[0]  # same message, same question text
        assert follow_up[1]["block_id"] == "followup" and "When are they purged for good?" in follow_up[1]["text"]["text"]
        values = [b["value"] for b in follow_up[-1]["elements"]]
        assert values == ["TEST-1|uid-1|0.0", "TEST-1|uid-1|0.1", "TEST-1|uid-1|back", "TEST-1|uid-1|reply"]

        await dispatch(adapter, click("agentflow_answer_back", "TEST-1|uid-1|back", follow_up))
        await eventually(lambda: len(updates) == 2)
        assert updates[-1]["blocks"] == notice  # the "You picked" section is gone again

        await dispatch(adapter, click("agentflow_answer_follow_1", "TEST-1|uid-1|0.1", follow_up))
        await eventually(lambda: len(answers) == 3)
    finally:
        for p in patches:
            p.stop()
    assert [(a.choice_index, a.path, a.back) for a in answers] == [(0, None, False), (None, None, True), (None, (0, 1), False)]
    assert len(updates) == 2 and calls["chat_postMessage"] == []  # a delivered answer posts nothing
