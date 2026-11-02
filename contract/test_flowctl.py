"""flowctl's command-line contract: arguments, messages, exit codes and the files it manages.

Every test runs once per flowctl implementation: the Python module always, and the Rust binary
when it is built (app/target/*/flowctl.exe or $CONTRACT_RUST_FLOWCTL). Agents and their prompts
depend on these exact messages, so the Rust port has to reproduce them.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from conftest import rust_flowctl
from support import (ARCHITECT_RESULT, ROOT, agent_run, free_port, git, new_key, node, python_executable, reach_reviews,
                     write_requirements)

AGENT_VARS = ("AGENTFLOW_API", "AGENTFLOW_TASK", "AGENTFLOW_AGENT", "AGENTFLOW_RUN", "AGENTFLOW_CREDENTIAL_FILE")


@pytest.fixture(params=["python", "rust"])
def flowctl(request):
    if request.param == "python":
        command = [python_executable(), "-m", "agentflow.cli.flowctl"]
    else:
        binary = rust_flowctl()
        if binary is None:
            pytest.skip("Rust flowctl not built (cargo build in app/)")
        command = [str(binary)]

    def run(*args: str, env: dict[str, str] | None = None, stdin: str | None = None) -> subprocess.CompletedProcess:
        base = {k: v for k, v in os.environ.items() if k not in AGENT_VARS}
        base["PYTHONPATH"] = str(ROOT / "backend")
        # Piped Python output on Windows is in the console code page (the "—" in status lines is
        # not UTF-8 there); the Rust binary always writes UTF-8.
        base["PYTHONIOENCODING"] = "utf-8"
        return subprocess.run([*command, *args], env={**base, **(env or {})}, input=stdin, capture_output=True,
                              text=True, encoding="utf-8", timeout=120)

    run.name = request.param
    return run


# ---------------------------------------------------------------------------- without a backend

@pytest.fixture
def offline_run(tmp_path):
    """Agent variables for run 5 pointing at a port nothing listens on."""
    credential = tmp_path / "credential.json"
    credential.write_text(json.dumps({"runId": 5, "token": "t"}), encoding="utf-8")
    return {"AGENTFLOW_API": f"http://127.0.0.1:{free_port()}", "AGENTFLOW_TASK": "CT-1", "AGENTFLOW_AGENT": "architect",
            "AGENTFLOW_RUN": "5", "AGENTFLOW_CREDENTIAL_FILE": str(credential)}


def test_usage_errors_exit_2(flowctl):
    assert flowctl().returncode == 2
    assert flowctl("question").returncode == 2  # --message is required
    assert flowctl("complete", "--verdict", "maybe").returncode == 2
    assert flowctl("finding", "--title", "t", "--severity", "huge").returncode == 2


def test_task_and_agent_are_required(flowctl):
    result = flowctl("running")
    assert (result.returncode, result.stderr.strip()) == (1, "flowctl: missing --task (or $AGENTFLOW_TASK)")
    result = flowctl("running", "--task", "CT-1")
    assert (result.returncode, result.stderr.strip()) == (1, "flowctl: missing --agent (or $AGENTFLOW_AGENT)")


def test_a_scoped_credential_and_run_id_are_required(flowctl, offline_run):
    result = flowctl("running", "--task", "CT-1", "--agent", "architect")
    assert result.returncode == 1
    assert result.stderr.strip() == "flowctl: scoped credential file and explicit run ID required; restart older agent runs"
    result = flowctl("running", "--run", "6", env=offline_run)
    assert (result.returncode, result.stderr.strip()) == (1, "flowctl: no credential for this run; use --run from the current prompt")


def test_unreachable_backend_keeps_the_completion_for_replay(flowctl, offline_run):
    result = flowctl("complete", "--json", '{"summary": "done"}', env=offline_run)
    assert result.returncode == 1
    assert result.stderr.startswith(f"flowctl: cannot reach AgentFlow at {offline_run['AGENTFLOW_API']}: ")
    pending = Path(offline_run["AGENTFLOW_CREDENTIAL_FILE"]).parent / "pending-complete.json"
    assert json.loads(pending.read_text(encoding="utf-8")) == {
        "role": "architect", "runId": 5, "result": {"summary": "done", "status": "completed"}}
    # Same bytes as Python's json.dumps(indent=2), so either implementation's file replays.
    assert pending.read_text(encoding="utf-8") == (
        '{\n  "role": "architect",\n  "runId": 5,\n  "result": {\n    "summary": "done",\n    "status": "completed"\n  }\n}')


def test_orchestrator_results_get_no_status(flowctl, offline_run):
    env = {**offline_run, "AGENTFLOW_AGENT": "orchestrator"}
    flowctl("complete", "--json", '{"summary": "plan", "subtasks": []}', env=env)
    pending = Path(offline_run["AGENTFLOW_CREDENTIAL_FILE"]).parent / "pending-complete.json"
    assert json.loads(pending.read_text(encoding="utf-8"))["result"] == {"summary": "plan", "subtasks": []}


def test_summary_flags_build_the_result(flowctl, offline_run):
    flowctl("complete", "--summary", "s", "--sha", "abc", "--verdict", "approved", env=offline_run)
    pending = Path(offline_run["AGENTFLOW_CREDENTIAL_FILE"]).parent / "pending-complete.json"
    assert json.loads(pending.read_text(encoding="utf-8"))["result"] == {
        "status": "completed", "summary": "s", "reviewedSha": "abc", "verdict": "approved"}


def test_bad_results_are_refused_before_sending(flowctl, offline_run, tmp_path):
    result = flowctl("complete", "--file", "-", env=offline_run, stdin="not json")
    assert result.returncode == 1 and result.stderr.startswith("flowctl: stdin is not valid JSON")
    missing = tmp_path / "missing.json"
    result = flowctl("complete", "--file", str(missing), env=offline_run)
    assert (result.returncode, result.stderr.strip()) == (1, f"flowctl: result file not found: {missing}")
    result = flowctl("complete", "--file", "-", env=offline_run, stdin="[1, 2]")
    assert (result.returncode, result.stderr.strip()) == (1, "flowctl: result must be a JSON object")


def test_result_file_may_start_with_a_bom(flowctl, offline_run, tmp_path):
    file = tmp_path / "result.json"
    file.write_bytes(b"\xef\xbb\xbf" + b'{"summary": "bom"}')
    flowctl("complete", "--file", str(file), env=offline_run)
    pending = Path(offline_run["AGENTFLOW_CREDENTIAL_FILE"]).parent / "pending-complete.json"
    assert json.loads(pending.read_text(encoding="utf-8"))["result"]["summary"] == "bom"


def test_exited_never_fails_when_the_backend_is_down(flowctl, offline_run):
    result = flowctl("exited", "--code", "3", env=offline_run)
    assert result.returncode == 0 and result.stderr.startswith("flowctl: cannot reach AgentFlow")


# ---------------------------------------------------------------------------- against a backend

@pytest.fixture(scope="module")
def backend(launch):
    # Manual-mode agents idle until the backend stops, so each test's run keeps an agent slot.
    with launch(workflow={"maxConcurrentAgents": 32}, STUB_MANUAL="1") as backend:
        yield backend


@pytest.fixture(scope="module")
def human(backend):
    return backend.human()


@pytest.fixture
def architect(backend, human, repo):
    key = new_key("FC")
    human.create_task(key, repo)
    human.run(key, "architect")
    return agent_run(backend, human.wait_node(key, "architect", "RUNNING"), "architect")


def test_question_running_complete(flowctl, architect, human):
    env = architect.env()
    result = flowctl("question", "-m", "Which tenants?", "--choice", "All", "--choice", "Some", env=env)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == ("Question recorded. Node is now WAITING_FOR_USER. "
                                     "Wait for the human to reply in AgentFlow, then run: flowctl running")
    [question] = human.task(architect.key)["questions"]
    assert question["choices"] == ["All", "Some"]
    human.answer(architect.key, question["uid"], "All")

    result = flowctl("running", env=env)
    assert (result.returncode, result.stdout.strip()) == (0, "Marked running (RUNNING)."), result.stderr

    status = flowctl("status", env=env)
    assert status.returncode == 0, status.stderr
    assert status.stdout.splitlines()[0] == f"Task {architect.key}: ACTIVE (iteration 1/3)"

    # A long result goes through submission.json in the run directory, removed once accepted.
    write_requirements(Path(human.task(architect.key)["taskDir"]))
    submission = architect.run_dir / "submission.json"
    submission.write_text(json.dumps(ARCHITECT_RESULT), encoding="utf-8")
    result = flowctl("complete", "--file", str(submission), env=env)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "Completion accepted. Node state: COMPLETED; task status: ACTIVE."
    assert not submission.exists()
    assert not (architect.run_dir / "pending-complete.json").exists()


def test_question_file_asks_a_decision_tree(flowctl, architect, human, tmp_path):
    env = architect.env()
    tree = {"message": "Which cleanup?", "context": "4,108 accounts · 312 invoices", "risk": "medium",
            "role": "reviewer", "runId": 1,  # ignored: the run's own identity is sent
            "choices": [{"label": "Soft-delete · keep", "recommended": True, "followUp": {
                "message": "Purge when?", "choices": [{"label": "Later"}, {"label": "Never"}]}},
                {"label": "Hard-delete"}]}
    assert flowctl("question", "--message", "x", "--file", "-", env=env).returncode == 2
    refused = flowctl("question", "--file", "-", "--choice", "A", env=env, stdin=json.dumps(tree))
    assert refused.returncode == 1 and "--choice cannot be combined with --file" in refused.stderr
    bad = flowctl("question", "--file", "-", env=env, stdin=json.dumps({**tree, "risk": "severe"}))
    assert bad.returncode == 1 and "question rejected (422)" in bad.stderr and "risk: must be low, medium or high" in bad.stderr
    assert flowctl("question", "--file", "-", env=env, stdin="[1]").returncode == 1

    result = flowctl("question", "--file", "-", env=env, stdin=json.dumps(tree, ensure_ascii=False))
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == ("Question recorded. Node is now WAITING_FOR_USER. "
                                     "Wait for the human to reply in AgentFlow, then run: flowctl running")
    path = tmp_path / "question.json"
    path.write_text(json.dumps({"message": "From a file", "choices": [{"label": "Yes"}, {"label": "No"}]}), encoding="utf-8-sig")
    assert flowctl("question", "--file", str(path), env=env).returncode == 0
    first, second = human.task(architect.key)["questions"]
    assert first["choices"] == ["Soft-delete · keep", "Hard-delete"] and first["role"] == "architect"
    assert first["decision"]["context"] == "4,108 accounts · 312 invoices"
    assert first["decision"]["choices"][0]["followUp"]["choices"] == [{"label": "Later"}, {"label": "Never"}]
    assert second["decision"] == {"context": None, "risk": None, "choices": [{"label": "Yes"}, {"label": "No"}]}
    missing = flowctl("question", "--file", str(tmp_path / "missing.json"), env=env)
    assert missing.returncode == 1 and "question file not found" in missing.stderr


def test_heartbeats_report_without_resuming_or_completing(flowctl, architect, human):
    env = architect.env()
    before = human.task(architect.key)
    for phase in ('investigating', 'testing'):
        result = flowctl('heartbeat', '--phase', phase, env=env)
        assert (result.returncode, result.stdout.strip()) == (0, 'Agent heartbeat recorded.'), result.stderr
    snap = human.task(architect.key)
    report = node(snap, 'architect')['lastRun']['heartbeat']
    assert report['phase'] == 'testing' and report['receivedAt']
    assert snap['events'] == before['events']
    assert architect.post('heartbeat', {'phase': 'working'}, token='wrong-token').status_code == 403
    assert architect.post('heartbeat', {'phase': 'working', 'role': 'implementer'}).status_code == 403
    assert architect.post('heartbeat', {'phase': 'working', 'runId': architect.run_id + 999}).status_code == 403
    assert architect.post('heartbeat', {'phase': 'invalid'}).status_code == 422
    assert architect.post('question', {'message': 'Continue?'}).status_code == 200
    result = flowctl('heartbeat', env=env)
    assert result.returncode == 0, result.stderr
    assert node(human.task(architect.key), 'architect')['state'] == 'WAITING_FOR_USER'
    assert architect.post('fail', {'message': 'End heartbeat test'}).status_code == 200
    assert architect.post('heartbeat', {}).status_code == 409


def test_rejected_completion_keeps_the_submission(flowctl, architect, human):
    write_requirements(Path(human.task(architect.key)["taskDir"]))
    submission = architect.run_dir / "submission.json"
    submission.write_text(json.dumps({**ARCHITECT_RESULT, "summary": ["not", "text"]}), encoding="utf-8")
    result = flowctl("complete", "--file", str(submission), env=architect.env())
    assert result.returncode == 1
    assert result.stderr.startswith("flowctl: completion REJECTED — "), result.stderr
    assert result.stderr.rstrip().endswith("Node state: FAILED; task status: ACTIVE.")
    assert submission.exists()
    assert not (architect.run_dir / "pending-complete.json").exists()


def test_discovery_without_its_documents_fails_the_node_but_reports_accepted(flowctl, architect):
    """Current behaviour, pinned so the port matches it: missing deliverables fail the node without
    an `error` in the response, so flowctl says "accepted". Arguably it should be a rejection the
    agent can fix; change both implementations together if that changes."""
    result = flowctl("complete", "--json", json.dumps(ARCHITECT_RESULT), env=architect.env())
    assert (result.returncode, result.stdout.strip()) == (0, "Completion accepted. Node state: FAILED; task status: ACTIVE.")


def test_fail_reports_the_node_state(flowctl, architect):
    result = flowctl("fail", "-m", "Cannot continue", env=architect.env())
    assert (result.returncode, result.stdout.strip()) == (0, "Failure recorded. Node state: FAILED."), result.stderr


@pytest.fixture
def reviewers(backend, human, repo):
    return reach_reviews(backend, human, repo, "FR")


def test_findings_recorded_incrementally_are_used_by_a_bare_completion(flowctl, reviewers, human):
    code, _ = reviewers
    env = code.env()
    result = flowctl("finding", "--id", "CODE-001", "--severity", "high", "--blocking", "--title", "Null check missing",
                     "--file", "src/app.txt", "--line", "1", "--end-line", "2", "--description", "Crashes on empty input.",
                     "--recommendation", "Guard it.", env=env)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == ("Finding recorded (1 pending for this run). Include them all in your final "
                                     "result or complete without findings to use these.")
    result = flowctl("finding", "--title", "Naming", "--severity", "low", "--no-blocking", env=env)
    assert result.returncode == 0 and "(2 pending for this run)" in result.stdout, result.stderr

    sha = human.task(code.key)["currentSha"]
    result = flowctl("complete", "--summary", "Two findings", "--sha", sha, "--verdict", "changes_required", env=env)
    assert result.returncode == 0, result.stderr
    findings = [f for f in human.task(code.key)["findings"] if f["type"] == "code-review"]
    assert sorted((f["title"], f["severity"], f["blocking"]) for f in findings) == [
        ("Naming", "low", False), ("Null check missing", "high", True)]
    [blocking] = [f for f in findings if f["blocking"]]
    assert (blocking["file"], blocking["line"], blocking["endLine"], blocking["description"], blocking["recommendation"]) == (
        "src/app.txt", 1, 2, "Crashes on empty input.", "Guard it.")


def test_finding_code_shows_the_reviewed_file_and_the_task_change(flowctl, reviewers, human, repo):
    code, _ = reviewers
    env = code.env()
    result = flowctl("finding", "--title", "Vague note", "--severity", "low", "--file", "./STUB_NOTES.md",
                     "--line", "1", env=env)
    assert result.returncode == 0, result.stderr
    sha = human.task(code.key)["currentSha"]
    result = flowctl("complete", "--summary", "One finding", "--sha", sha, "--verdict", "approved", env=env)
    assert result.returncode == 0, result.stderr
    [finding] = [f for f in human.task(code.key)["findings"] if f["type"] == "code-review"]
    assert (finding["line"], finding["endLine"]) == (1, None)

    base = git("rev-parse", "main", cwd=repo)
    added = {"kind": "add", "old": None, "new": 1, "text": "contract change"}
    assert human.ok("GET", f"/api/tasks/{code.key}/findings/{finding['dbId']}/code") == {
        "file": "./STUB_NOTES.md", "line": 1, "endLine": None, "reviewedSha": sha,
        "snapshot": {"sha": sha, "lines": ["contract change"]},
        "diff": {"from": base, "to": sha, "hunks": [{"header": "@@ -0,0 +1 @@", "oldStart": 0, "oldCount": 0,
                                                     "newStart": 1, "newCount": 1, "rows": [added]}]},
        "now": {"sha": sha, "lines": ["contract change"], "line": 1, "endLine": None, "changed": False, "hunks": []},
        "unavailable": {},
    }
    missing = human.get(f"/api/tasks/{code.key}/findings/999999/code")
    assert (missing.status_code, missing.json()["detail"]) == (404, "Finding 999999 not found")


def test_exited_reports_the_harness_exit(flowctl, reviewers, human):
    _, security = reviewers
    result = flowctl("exited", "--code", "3", env=security.env())
    assert (result.returncode, result.stderr) == (0, "")
    review = node(human.task(security.key), "security_review")
    # A harness that exits before completing fails its node; the exit code is kept on the run.
    assert (review["state"], review["message"]) == ("FAILED", "Harness exited (code 3)")
    assert review["error"] == "Harness exited with code 3 before reporting completion"
    assert review["lastRun"]["exitCode"] == 3


def test_a_harness_exit_is_retried_automatically_when_enabled(flowctl, launch, repo):
    with launch(workflow={"maxConcurrentAgents": 32, "maxAutomaticRetries": 1}, STUB_MANUAL="1") as backend:
        human = backend.human()
        key = new_key("FA")
        human.create_task(key, repo)
        human.run(key, "architect")
        first = agent_run(backend, human.wait_node(key, "architect", "RUNNING"), "architect")
        result = flowctl("exited", "--code", "3", env=first.env())
        assert (result.returncode, result.stderr) == (0, "")
        architect = node(human.task(key), "architect")
        assert (architect["state"], architect["message"]) == (
            "FAILED", "Harness exited (code 3); automatic retry 1 of 1 starts shortly")
        failed = architect["lastRun"]["id"]

        # After the pause the stage relaunches by itself, briefed on the failed attempt.
        snapshot = human.wait_task(key, lambda s: node(s, "architect")["state"] == "RUNNING"
                                   and node(s, "architect")["lastRun"]["id"] != failed, "automatic retry", 60)
        [event] = [e for e in snapshot["events"] if "automatic retry" in e["message"] and e["data"]]
        assert (event["message"], event["level"], event["data"]) == (
            "Architect: automatic retry 1 of 1 after the harness exited", "warning",
            {"automatic": True, "failedRunId": failed})
        second = agent_run(backend, snapshot, "architect")
        assert second.run_dir != first.run_dir
        assert "## Previous attempt at this stage" in (second.run_dir / "prompt.md").read_text(encoding="utf-8")

        # The allowance is used up: the next exit stays failed.
        result = flowctl("exited", "--code", "3", env=second.env())
        assert result.returncode == 0, result.stderr
        snapshot = human.task(key)
        assert (node(snapshot, "architect")["state"], node(snapshot, "architect")["message"]) == (
            "FAILED", "Harness exited (code 3)")
        assert any(e["message"] == "Architect not retried automatically: it has failed 2 times in a row in this "
                   "review round (workflow.maxAutomaticRetries is 1)" for e in snapshot["events"])


def test_status_json_prints_the_snapshot(flowctl, architect):
    result = flowctl("status", "--json", env=architect.env())
    assert result.returncode == 0, result.stderr
    text = result.stdout
    snapshot = json.loads(text[text.index("\n{") + 1:])
    assert snapshot["jiraKey"] == architect.key
