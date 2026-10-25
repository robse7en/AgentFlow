import io
import json
import sys
from argparse import Namespace

import pytest

from agentflow.cli import flowctl


def _args(**overrides):
    defaults = dict(task="DE-1", agent="architect", run=1, api=None, file=None, json=None,
                     summary=None, sha=None, verdict=None)
    defaults.update(overrides)
    return Namespace(**defaults)


def test_complete_reads_json_from_stdin_when_file_is_dash(monkeypatch):
    """The usual path for a short result: piped in with a heredoc, never written to disk."""
    monkeypatch.setattr("sys.stdin", io.StringIO('{"status": "completed", "summary": "done"}'))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)
    posted = {}

    def fake_post(args, path, payload, *, on_response=None):
        posted.update(payload)
        return {"state": "COMPLETED", "taskStatus": "ACTIVE"}

    monkeypatch.setattr(flowctl, "_post", fake_post)

    flowctl.cmd_complete(_args(file="-"))

    assert posted["result"] == {"status": "completed", "summary": "done"}


def test_complete_exits_cleanly_on_invalid_json_from_stdin(monkeypatch):
    monkeypatch.setattr("sys.stdin", io.StringIO("not json"))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)

    with pytest.raises(SystemExit, match="not valid JSON"):
        flowctl.cmd_complete(_args(file="-"))


def test_complete_does_not_inject_status_for_orchestrator_work_plans(monkeypatch):
    """WorkPlan forbids extra fields and has no status field at all; injecting one made
    every orchestrator completion fail backend validation with extra_forbidden."""
    monkeypatch.setattr("sys.stdin", io.StringIO('{"summary": "done", "subtasks": []}'))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)
    posted = {}

    def fake_post(args, path, payload, *, on_response=None):
        posted.update(payload)
        return {"state": "COMPLETED", "taskStatus": "ACTIVE"}

    monkeypatch.setattr(flowctl, "_post", fake_post)

    flowctl.cmd_complete(_args(agent="orchestrator", file="-"))

    assert posted["result"] == {"summary": "done", "subtasks": []}


def test_complete_file_path_still_works_alongside_the_new_dash_convention(tmp_path, monkeypatch):
    result_path = tmp_path / "result.json"
    result_path.write_text('{"status": "completed"}', encoding="utf-8")
    monkeypatch.setattr(flowctl, "_require", lambda args: None)
    posted = {}

    def fake_post(args, path, payload, *, on_response=None):
        posted.update(payload)
        return {"state": "COMPLETED", "taskStatus": "ACTIVE"}

    monkeypatch.setattr(flowctl, "_post", fake_post)

    flowctl.cmd_complete(_args(file=str(result_path)))

    assert posted["result"] == {"status": "completed"}


def test_complete_keeps_pending_file_when_backend_unreachable(tmp_path, monkeypatch):
    """A transport failure never reaches on_response, so the durability file must survive it -
    that's the file AgentFlow replays on its next restart instead of losing the finished run."""
    (tmp_path / "credential.json").write_text("{}", encoding="utf-8")
    monkeypatch.setenv("AGENTFLOW_CREDENTIAL_FILE", str(tmp_path / "credential.json"))
    monkeypatch.setattr("sys.stdin", io.StringIO('{"status": "completed", "summary": "done"}'))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)

    def fake_post(args, path, payload, *, on_response=None):
        sys.exit("flowctl: cannot reach AgentFlow at http://127.0.0.1:8765")

    monkeypatch.setattr(flowctl, "_post", fake_post)

    with pytest.raises(SystemExit, match="cannot reach"):
        flowctl.cmd_complete(_args(file="-"))

    saved = json.loads((tmp_path / "pending-complete.json").read_text(encoding="utf-8"))
    assert saved == {"role": "architect", "runId": 1, "result": {"status": "completed", "summary": "done"}}


def test_complete_clears_pending_file_once_backend_responds_even_with_a_rejection(tmp_path, monkeypatch):
    """A rejection (e.g. stale run) still means the backend decided; nothing is left to replay."""
    (tmp_path / "credential.json").write_text("{}", encoding="utf-8")
    monkeypatch.setenv("AGENTFLOW_CREDENTIAL_FILE", str(tmp_path / "credential.json"))
    monkeypatch.setattr("sys.stdin", io.StringIO('{"status": "completed", "summary": "done"}'))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)

    def fake_post(args, path, payload, *, on_response=None):
        if on_response:
            on_response()
        sys.exit("flowctl: complete rejected (409): Stale run; signal rejected")

    monkeypatch.setattr(flowctl, "_post", fake_post)

    with pytest.raises(SystemExit, match="rejected"):
        flowctl.cmd_complete(_args(file="-"))

    assert not (tmp_path / "pending-complete.json").exists()


def test_complete_clears_pending_file_on_success(tmp_path, monkeypatch):
    (tmp_path / "credential.json").write_text("{}", encoding="utf-8")
    monkeypatch.setenv("AGENTFLOW_CREDENTIAL_FILE", str(tmp_path / "credential.json"))
    monkeypatch.setattr("sys.stdin", io.StringIO('{"status": "completed", "summary": "done"}'))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)

    def fake_post(args, path, payload, *, on_response=None):
        if on_response:
            on_response()
        return {"state": "COMPLETED", "taskStatus": "ACTIVE"}

    monkeypatch.setattr(flowctl, "_post", fake_post)

    flowctl.cmd_complete(_args(file="-"))

    assert not (tmp_path / "pending-complete.json").exists()


def test_complete_skips_durability_write_without_a_credential_file(monkeypatch):
    monkeypatch.delenv("AGENTFLOW_CREDENTIAL_FILE", raising=False)
    monkeypatch.setattr("sys.stdin", io.StringIO('{"status": "completed", "summary": "done"}'))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)

    def fake_post(args, path, payload, *, on_response=None):
        assert on_response is None
        return {"state": "COMPLETED", "taskStatus": "ACTIVE"}

    monkeypatch.setattr(flowctl, "_post", fake_post)

    flowctl.cmd_complete(_args(file="-"))


def _accepting_post(args, path, payload, *, on_response=None):
    if on_response:
        on_response()
    return {"state": "COMPLETED", "taskStatus": "ACTIVE"}


def _run_dir_with_submission(tmp_path, monkeypatch):
    (tmp_path / "credential.json").write_text("{}", encoding="utf-8")
    monkeypatch.setenv("AGENTFLOW_CREDENTIAL_FILE", str(tmp_path / "credential.json"))
    monkeypatch.setattr(flowctl, "_require", lambda args: None)
    submission = tmp_path / "submission.json"
    submission.write_text('{"summary": "plan", "subtasks": []}', encoding="utf-8")
    return submission


def test_complete_deletes_the_run_dir_submission_file_once_accepted(tmp_path, monkeypatch):
    """A result too long for a heredoc goes through submission.json; once AgentFlow has it,
    nothing is left for the file to do."""
    submission = _run_dir_with_submission(tmp_path, monkeypatch)
    posted = {}

    def fake_post(args, path, payload, *, on_response=None):
        posted.update(payload)
        return _accepting_post(args, path, payload, on_response=on_response)

    monkeypatch.setattr(flowctl, "_post", fake_post)

    flowctl.cmd_complete(_args(agent="orchestrator", file=str(submission)))

    assert posted["result"] == {"summary": "plan", "subtasks": []}
    assert not submission.exists()


def test_complete_keeps_the_submission_file_when_the_result_is_rejected(tmp_path, monkeypatch):
    """The agent fixes the file and runs the same command again."""
    submission = _run_dir_with_submission(tmp_path, monkeypatch)
    monkeypatch.setattr(flowctl, "_post", lambda *a, **k: {"error": "subtask api has no owned paths",
                                                             "state": "RUNNING", "taskStatus": "ACTIVE"})

    with pytest.raises(SystemExit, match="REJECTED"):
        flowctl.cmd_complete(_args(agent="orchestrator", file=str(submission)))

    assert submission.exists()


def test_complete_never_deletes_any_other_file_it_was_given(tmp_path, monkeypatch):
    _run_dir_with_submission(tmp_path, monkeypatch)
    other = tmp_path / "package.json"
    other.write_text('{"name": "app"}', encoding="utf-8")
    monkeypatch.setattr(flowctl, "_post", _accepting_post)

    flowctl.cmd_complete(_args(file=str(other)))

    assert other.exists()
