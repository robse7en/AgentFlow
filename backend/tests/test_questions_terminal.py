import asyncio
import json
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import UUID

import pytest
from sqlalchemy import select, text

from agentflow.config import Config, ZellijConfig
from agentflow.database.database import Database
from agentflow.database.models import AgentQuestion, Task
from agentflow.services.process import spawn_detached, spawn_hidden_console
from agentflow.services.zellij import ZellijError, ZellijService
from agentflow.workflow.models import QuestionAnswerRequest, QuestionRequest
from agentflow.cli.flowctl import build_parser


def test_question_choices_and_single_line_reply_validation():
    question = QuestionRequest(
        role="architect",
        message="  Which rollout?  ",
        choices=[" Gradual ", "All at once"],
    )
    assert question.message == "Which rollout?"
    assert question.choices == ["Gradual", "All at once"]
    assert QuestionAnswerRequest(answer="  Gradual ").answer == "Gradual"

    with pytest.raises(ValueError):
        QuestionAnswerRequest(answer="one line\nanother line")
    with pytest.raises(ValueError):
        QuestionRequest(role="architect", message="Pick", choices=["Same", "same"])


def test_flowctl_question_accepts_repeatable_choice_options():
    args = build_parser().parse_args([
        "question", "--message", "Which rollout?", "--choice", "Gradual", "--choice", "All at once"
    ])
    assert args.choice == ["Gradual", "All at once"]


def test_existing_question_rows_receive_uids_and_response_fields(tmp_path):
    config = Config.model_validate({
        "app": {"dataDirectory": str(tmp_path), "databasePath": str(tmp_path / "legacy.db")},
    })
    database = Database(config.database_file)
    database.create_all()
    with database.session() as session:
        task = Task(
            jira_key="LEGACY-1",
            repository_path=str(tmp_path),
            repository_root=str(tmp_path),
            base_branch="main",
            branch_name="agent/LEGACY-1",
            worktree_path=str(tmp_path / "tree"),
            zellij_session_name="agentflow-LEGACY-1",
            task_dir=str(tmp_path / "tasks" / "LEGACY-1"),
            current_sha="a" * 40,
            max_review_iterations=1,
        )
        session.add(task)
        session.flush()
        task_id = task.id

    with database.engine.begin() as connection:
        connection.exec_driver_sql("DROP INDEX IF EXISTS ix_agent_questions_uid")
        connection.exec_driver_sql("ALTER TABLE agent_questions RENAME TO agent_questions_new")
        connection.exec_driver_sql(
            "CREATE TABLE agent_questions ("
            "id INTEGER PRIMARY KEY, task_id INTEGER NOT NULL, node_run_id INTEGER, "
            "role VARCHAR(64) NOT NULL, message TEXT NOT NULL, answered BOOLEAN NOT NULL DEFAULT 0, "
            "created_at DATETIME NOT NULL, answered_at DATETIME)"
        )
        connection.execute(
            text("INSERT INTO agent_questions (id, task_id, role, message, created_at) "
                 "VALUES (1, :task_id, 'architect', 'Legacy question', CURRENT_TIMESTAMP)"),
            {"task_id": task_id},
        )
        connection.exec_driver_sql("DROP TABLE agent_questions_new")

    database.create_all()
    try:
        with database.session() as session:
            question = session.scalar(select(AgentQuestion).where(AgentQuestion.id == 1))
            assert question is not None
            assert UUID(question.uid)
            assert question.choices == []
            assert question.answer is None
    finally:
        database.engine.dispose()


@pytest.mark.asyncio
async def test_embedded_terminal_requires_zellij_044_or_newer():
    service = ZellijService(ZellijConfig())
    service.version = AsyncMock(return_value="zellij 0.43.1")
    with pytest.raises(ZellijError, match="requires Zellij 0.44"):
        await service.require_subscribe_support()

    service.version.return_value = "zellij 0.44.0"
    await service.require_subscribe_support()


@pytest.mark.asyncio
async def test_ensure_session_reports_process_start_errors(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "linux")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value=None)
    monkeypatch.setattr(
        "agentflow.services.zellij.spawn_detached",
        AsyncMock(side_effect=PermissionError("access denied")),
    )

    with pytest.raises(ZellijError, match="Could not start Zellij session agentflow-TEST-1: access denied"):
        await service.ensure_session("agentflow-TEST-1")


@pytest.mark.asyncio
async def test_ensure_session_starts_a_missing_session(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "linux")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value=None)
    service.session_exists = AsyncMock(return_value=True)
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    spawn = AsyncMock(return_value=SimpleNamespace(returncode=0))
    monkeypatch.setattr("agentflow.services.zellij.spawn_detached", spawn)

    assert await service.ensure_session("agentflow-TEST-1") is True
    spawn.assert_awaited_once_with(
        ["zellij", "attach", "--create-background", "agentflow-TEST-1"],
        detach_from_console=False,
    )


@pytest.mark.asyncio
async def test_ensure_session_reports_early_zellij_exit(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "linux")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value=None)
    service.session_exists = AsyncMock(return_value=False)
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    monkeypatch.setattr(
        "agentflow.services.zellij.spawn_detached",
        AsyncMock(return_value=SimpleNamespace(returncode=1)),
    )

    with pytest.raises(ZellijError, match="exited with code 1 before creating session agentflow-TEST-1"):
        await service.ensure_session("agentflow-TEST-1")


def _panes_json(width: int) -> SimpleNamespace:
    """list-panes output whose widest pane (the tab bar) is `width` columns."""
    panes = [{"id": 1, "is_plugin": True, "pane_columns": width}, {"id": 0, "is_plugin": False, "pane_columns": width}]
    return SimpleNamespace(ok=True, stdout=json.dumps(panes), stderr="")


@pytest.mark.asyncio
async def test_ensure_session_starts_a_headless_server_and_sizing_client_on_windows(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value=None)
    service.session_exists = AsyncMock(return_value=True)
    service._zellij = AsyncMock(side_effect=[_panes_json(50), _panes_json(200)])
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    client = SimpleNamespace(returncode=None)
    hidden = AsyncMock(side_effect=[SimpleNamespace(returncode=0), client])
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)
    detached = AsyncMock()
    monkeypatch.setattr("agentflow.services.zellij.spawn_detached", detached)

    assert await service.ensure_session("agentflow-TEST-1") is True
    assert hidden.await_args_list[0].args == (["zellij", "attach", "--create-background", "agentflow-TEST-1"],)
    assert hidden.await_args_list[1].args == ('mode con: cols=200 lines=50 & "zellij" attach agentflow-TEST-1',)
    assert service._sizing_clients["agentflow-TEST-1"] is client
    detached.assert_not_awaited()  # no visible window any more


@pytest.mark.asyncio
async def test_new_windows_session_accepts_a_client_window_narrower_than_headless_cols(monkeypatch):
    """A disconnected or locked session caps console windows near 118 columns, whatever `mode con` asked for."""
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value=None)
    service.session_exists = AsyncMock(return_value=True)
    service._zellij = AsyncMock(side_effect=[_panes_json(50), _panes_json(118)])
    service.kill_session = AsyncMock()
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    client = SimpleNamespace(returncode=None)
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console",
                        AsyncMock(side_effect=[SimpleNamespace(returncode=0), client]))

    assert await service.ensure_session("agentflow-TEST-1") is True
    assert service._sizing_clients["agentflow-TEST-1"] is client
    service.kill_session.assert_not_awaited()


@pytest.mark.asyncio
async def test_new_windows_session_is_not_fooled_by_the_launchers_phantom_client(monkeypatch):
    """list-clients lists the launcher's phantom client throughout; only the viewport width counts."""
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value=None)
    service.session_exists = AsyncMock(return_value=True)
    service.client_count = AsyncMock(return_value=1)
    service._zellij = AsyncMock(return_value=_panes_json(50))
    service.kill_session = AsyncMock()
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    monkeypatch.setattr("agentflow.services.zellij.terminate_process_tree", AsyncMock())
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console",
                        AsyncMock(side_effect=[SimpleNamespace(returncode=0), SimpleNamespace(returncode=None)]))

    with pytest.raises(ZellijError, match="did not attach"):
        await service.ensure_session("agentflow-TEST-1")
    service.kill_session.assert_awaited_once_with("agentflow-TEST-1")  # retry starts clean


def test_headless_width_must_stay_above_zellijs_clientless_default():
    with pytest.raises(ValueError):
        ZellijConfig.model_validate({"headlessCols": 50})


@pytest.mark.asyncio
async def test_client_count_parses_list_clients_output():
    service = ZellijService(ZellijConfig())
    service._zellij = AsyncMock(return_value=SimpleNamespace(
        ok=True, stdout="CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1 terminal_2 bash\n2 terminal_3 zsh\n", stderr="",
    ))
    assert await service.client_count("agentflow-TEST-1") == 2

    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=True, stdout="CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n", stderr=""))
    assert await service.client_count("agentflow-TEST-1") == 0

    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=False, stdout="", stderr="no such session"))
    assert await service.client_count("agentflow-TEST-1") == 0


@pytest.mark.asyncio
async def test_ensure_client_attached_trusts_a_live_tracked_client_without_asking_zellij(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service._sizing_clients["agentflow-TEST-1"] = SimpleNamespace(returncode=None)
    service.client_count = AsyncMock(return_value=0)  # list-clients has missed working hidden clients
    hidden = AsyncMock()
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)

    await service.ensure_client_attached("agentflow-TEST-1")
    hidden.assert_not_awaited()
    service.client_count.assert_not_awaited()


@pytest.mark.asyncio
async def test_ensure_client_attached_reuses_an_untracked_client_after_a_restart(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.client_count = AsyncMock(return_value=1)
    hidden = AsyncMock()
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)

    await service.ensure_client_attached("agentflow-TEST-1")
    hidden.assert_not_awaited()


@pytest.mark.asyncio
async def test_ensure_client_attached_is_a_noop_on_non_windows(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "linux")
    service = ZellijService(ZellijConfig())
    service.client_count = AsyncMock(return_value=0)
    hidden = AsyncMock()
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)

    await service.ensure_client_attached("agentflow-TEST-1")
    hidden.assert_not_awaited()


@pytest.mark.asyncio
async def test_ensure_client_attached_starts_a_sized_hidden_client(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig.model_validate({"headlessCols": 180, "headlessRows": 45}))
    service.executable = lambda: r"C:\Program Files\Zellij\zellij.exe"
    service.client_count = AsyncMock(side_effect=[0, 0, 1])
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    client = SimpleNamespace(returncode=None)
    hidden = AsyncMock(return_value=client)
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)

    await service.ensure_client_attached("agentflow-TEST-1")
    hidden.assert_awaited_once_with(
        r'mode con: cols=180 lines=45 & "C:\Program Files\Zellij\zellij.exe" attach agentflow-TEST-1'
    )
    assert service._sizing_clients["agentflow-TEST-1"] is client


@pytest.mark.asyncio
async def test_ensure_client_attached_replaces_a_dead_tracked_client_even_if_one_is_listed(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service._sizing_clients["agentflow-TEST-1"] = SimpleNamespace(returncode=1)
    service.client_count = AsyncMock(return_value=1)
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    replacement = SimpleNamespace(returncode=None)
    hidden = AsyncMock(return_value=replacement)
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)

    await service.ensure_client_attached("agentflow-TEST-1")
    hidden.assert_awaited_once()
    assert service._sizing_clients["agentflow-TEST-1"] is replacement


@pytest.mark.asyncio
async def test_ensure_client_attached_raises_if_the_hidden_client_never_attaches(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.client_count = AsyncMock(return_value=0)
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    stuck = SimpleNamespace(returncode=None)
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", AsyncMock(return_value=stuck))
    terminate = AsyncMock()
    monkeypatch.setattr("agentflow.services.zellij.terminate_process_tree", terminate)

    with pytest.raises(ZellijError, match="did not attach"):
        await service.ensure_client_attached("agentflow-TEST-1")
    terminate.assert_awaited_once_with(stuck)


@pytest.mark.asyncio
async def test_ensure_client_attached_raises_if_the_hidden_client_exits(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.client_count = AsyncMock(return_value=0)
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console",
                        AsyncMock(return_value=SimpleNamespace(returncode=2)))

    with pytest.raises(ZellijError, match="exited with code 2"):
        await service.ensure_client_attached("agentflow-TEST-1")


@pytest.mark.asyncio
async def test_ensure_client_attached_refuses_session_names_cmd_would_reinterpret(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.client_count = AsyncMock(return_value=0)
    hidden = AsyncMock()
    monkeypatch.setattr("agentflow.services.zellij.spawn_hidden_console", hidden)

    with pytest.raises(ZellijError, match="Unsupported Zellij session name"):
        await service.ensure_client_attached("agentflow-X & calc")
    hidden.assert_not_awaited()


@pytest.mark.asyncio
async def test_open_window_focuses_the_tab_before_attaching_a_maximized_window(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "win32")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    calls = []
    service.focus_tab = AsyncMock(side_effect=lambda *args: calls.append(("focus", *args)))
    spawn = AsyncMock(side_effect=lambda argv, **kwargs: calls.append(("spawn", argv, kwargs)))
    monkeypatch.setattr("agentflow.services.zellij.spawn_detached", spawn)

    await service.open_window("agentflow-TEST-1", "Architect")
    assert calls == [
        ("focus", "agentflow-TEST-1", "Architect"),
        ("spawn", ["cmd", "/c", "start", "/max", "", "zellij", "attach", "agentflow-TEST-1"],
         {"breakaway_from_job": True}),
    ]


@pytest.mark.asyncio
async def test_open_window_explains_manual_attach_off_windows(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "linux")
    service = ZellijService(ZellijConfig())

    with pytest.raises(ZellijError, match="zellij attach agentflow-TEST-1"):
        await service.open_window("agentflow-TEST-1")


@pytest.mark.asyncio
async def test_kill_session_stops_the_tracked_hidden_client(monkeypatch):
    service = ZellijService(ZellijConfig())
    service.session_state = AsyncMock(side_effect=["running", None])
    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=True, stdout="", stderr=""))
    client = SimpleNamespace(returncode=None, wait=AsyncMock(return_value=0))
    service._sizing_clients["agentflow-TEST-1"] = client

    await service.kill_session("agentflow-TEST-1")
    client.wait.assert_awaited_once()
    assert "agentflow-TEST-1" not in service._sizing_clients


# Verbatim shape of `zellij list-sessions --no-formatting` (0.45.1, Windows).
_SESSIONS = SimpleNamespace(ok=True, stderr="", stdout=(
    "agentflow-AF-14687 [Created 6days 21h 34m 25s ago] (EXITED - attach to resurrect)\n"
    "agentflow-AF-14791 [Created 1day 23h 50m 34s ago] (EXITED - attach to resurrect)\n"
    "agentflow-AF-14419 [Created 12h 3m 2s ago] \n"
    "af-14687 [Created 1day 20h 34m 42s ago] (current)\n"))


@pytest.mark.asyncio
async def test_exited_sessions_are_not_treated_as_running():
    """`zellij action` against an exited session hangs until our timeout, so nothing may
    query one: cleanup used to time out after 30s on every attempt."""
    service = ZellijService(ZellijConfig())
    service._zellij = AsyncMock(return_value=_SESSIONS)
    assert await service.session_exists("agentflow-AF-14687") is False
    assert await service.session_state("agentflow-AF-14687") == "exited"
    assert await service.session_exists("agentflow-AF-14419") is True
    assert await service.session_exists("af-14687") is True
    assert await service.session_state("agentflow-NOPE-1") is None
    assert await service.list_sessions() == ["agentflow-AF-14419", "af-14687"]


@pytest.mark.asyncio
async def test_kill_session_deletes_an_exited_session_without_killing_it():
    service = ZellijService(ZellijConfig())
    service.session_state = AsyncMock(side_effect=["exited", None])
    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=True, stdout="", stderr=""))

    await service.kill_session("agentflow-TEST-1")
    assert [c.args for c in service._zellij.await_args_list] == [("delete-session", "agentflow-TEST-1", "--force")]


@pytest.mark.asyncio
async def test_kill_session_does_nothing_for_an_unknown_session():
    service = ZellijService(ZellijConfig())
    service.session_state = AsyncMock(return_value=None)
    service._zellij = AsyncMock()

    await service.kill_session("agentflow-TEST-1")
    service._zellij.assert_not_awaited()


@pytest.mark.asyncio
async def test_ensure_session_replaces_an_exited_session_with_a_fresh_one(monkeypatch):
    monkeypatch.setattr("agentflow.services.zellij.sys.platform", "linux")
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"
    service.session_state = AsyncMock(return_value="exited")
    service.session_exists = AsyncMock(return_value=True)
    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=True, stdout="", stderr=""))
    monkeypatch.setattr(asyncio, "sleep", AsyncMock())
    spawn = AsyncMock(return_value=SimpleNamespace(returncode=0))
    monkeypatch.setattr("agentflow.services.zellij.spawn_detached", spawn)

    assert await service.ensure_session("agentflow-TEST-1") is True
    service._zellij.assert_awaited_once_with("delete-session", "agentflow-TEST-1", "--force")
    spawn.assert_awaited_once()


@pytest.mark.asyncio
async def test_new_tab_ensures_a_client_is_attached_before_creating_the_pane(monkeypatch):
    service = ZellijService(ZellijConfig())
    service.ensure_client_attached = AsyncMock()
    service._zellij = AsyncMock(return_value=SimpleNamespace(ok=True, stdout="3\n", stderr=""))
    service.pane_for_tab = AsyncMock(return_value="terminal_9")

    tab_id, pane_id = await service.new_tab("agentflow-TEST-1", "architect", Path("."), ["echo", "hi"])

    service.ensure_client_attached.assert_awaited_once_with("agentflow-TEST-1")
    assert (tab_id, pane_id) == (3, "terminal_9")


@pytest.mark.asyncio
async def test_spawn_detached_can_break_away_from_the_hosting_job(monkeypatch):
    process = SimpleNamespace(returncode=None)
    create_process = AsyncMock(return_value=process)
    monkeypatch.setattr(asyncio, "create_subprocess_exec", create_process)
    monkeypatch.setattr("agentflow.services.process.sys.platform", "win32")

    result = await spawn_detached(["cmd", "/c", "start"], breakaway_from_job=True)

    assert result is process
    assert create_process.await_args.kwargs["creationflags"] == 0x00000200 | 0x00000008 | 0x01000000


@pytest.mark.asyncio
async def test_detached_process_can_keep_windows_console_association(monkeypatch):
    process = SimpleNamespace(returncode=None)
    create_process = AsyncMock(return_value=process)
    monkeypatch.setattr(asyncio, "create_subprocess_exec", create_process)
    monkeypatch.setattr("agentflow.services.process.sys.platform", "win32")

    result = await spawn_detached(["zellij", "attach"], detach_from_console=False)

    assert result is process
    assert create_process.await_args.kwargs["creationflags"] == 0x00000200


@pytest.mark.asyncio
@pytest.mark.skipif(sys.platform != "win32", reason="STARTUPINFO only exists on Windows")
async def test_spawn_hidden_console_hides_the_window_and_leaves_stdio_alone(monkeypatch):
    process = SimpleNamespace(returncode=None)
    create_exec = AsyncMock(return_value=process)
    create_shell = AsyncMock(return_value=process)
    monkeypatch.setattr(asyncio, "create_subprocess_exec", create_exec)
    monkeypatch.setattr(asyncio, "create_subprocess_shell", create_shell)

    assert await spawn_hidden_console(["zellij", "attach", "--create-background", "s"]) is process
    kwargs = create_exec.await_args.kwargs
    assert kwargs["creationflags"] == 0x00000010 | 0x00000200 | 0x01000000
    assert kwargs["startupinfo"].wShowWindow == 0
    # NUL std handles make zellij's first pane exit and take the server down with it.
    assert not {"stdin", "stdout", "stderr"} & kwargs.keys()

    await spawn_hidden_console('mode con: cols=200 lines=50 & "zellij" attach s')
    assert create_shell.await_args.args == ('mode con: cols=200 lines=50 & "zellij" attach s',)


@pytest.mark.asyncio
async def test_subscribe_streams_ndjson_and_targets_selected_session(monkeypatch):
    record = {"event": "pane_update", "pane_id": "terminal_7", "viewport": ["output"]}

    class FakeStdout:
        def __init__(self):
            self.lines = [(json.dumps(record) + "\n").encode(), b""]

        async def readline(self):
            return self.lines.pop(0)

    process = SimpleNamespace(stdout=FakeStdout(), returncode=0, wait=AsyncMock(return_value=0))
    create_process = AsyncMock(return_value=process)
    monkeypatch.setattr(asyncio, "create_subprocess_exec", create_process)
    service = ZellijService(ZellijConfig())
    service.executable = lambda: "zellij"

    result = [item async for item in service.subscribe("agentflow-TEST-1", "terminal_7")]

    assert result == [(json.dumps(record) + "\n").encode()]
    args = create_process.await_args.args
    assert args[:5] == ("zellij", "--session", "agentflow-TEST-1", "subscribe", "--pane-id")
    assert args[5:7] == ("terminal_7", "--scrollback")
