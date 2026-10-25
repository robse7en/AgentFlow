"""The agent API (`/api/tasks/{key}/agent/*`) that flowctl calls, driven directly over HTTP.

The stub runs in STUB_MANUAL mode, so it only idles in its terminal and the test acts as the
agent with the credential and prompt the backend wrote for the run.
"""

from __future__ import annotations

from pathlib import Path

import httpx
import pytest

import golden
from support import ARCHITECT_RESULT, agent_run, new_key, node, write_requirements


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
    """A running Architect discovery on a new task."""
    key = new_key("AG")
    snap = human.create_task(key, repo)
    human.run(key, "architect")
    snap = human.wait_node(key, "architect", "RUNNING")
    return agent_run(backend, snap, "architect")


def test_run_directory_holds_the_prompt_and_a_scoped_credential(architect, human):
    prompt = (architect.run_dir / "prompt.md").read_text(encoding="utf-8")
    assert architect.key in prompt and f"run id (`{architect.run_id}`)" in prompt
    assert architect.token
    # Credentials live under task data, never in the worktree the agent edits.
    worktree = Path(human.task(architect.key)["worktreePath"]).resolve()
    assert not architect.credential_file.resolve().is_relative_to(worktree)


def test_callbacks_need_the_runs_credential(architect):
    url = f"{architect.backend.url}/api/tasks/{architect.key}/agent/running"
    body = {"role": "architect", "runId": architect.run_id}
    assert httpx.post(url, json=body).status_code == 403
    assert architect.post("running", {}, token="forged").status_code == 403


def test_credential_is_scoped_to_its_task(architect, human, repo):
    other = new_key("AG")
    human.create_task(other, repo)
    assert architect.post("running", {}, key=other).status_code == 403
    headers = {"Authorization": f"Bearer {architect.token}"}
    assert httpx.get(f"{architect.backend.url}/api/tasks/{architect.key}", headers=headers).status_code == 200
    assert httpx.get(f"{architect.backend.url}/api/tasks/{other}", headers=headers).status_code == 403


def test_agents_cannot_perform_human_actions(architect):
    headers = {"Authorization": f"Bearer {architect.token}", "Origin": architect.backend.url}
    url = architect.backend.url
    for method, path in (("POST", f"/api/tasks/{architect.key}/cancel"),
                         ("POST", f"/api/tasks/{architect.key}/nodes/requirements_gate/approve"),
                         ("GET", "/api/tasks"), ("GET", "/api/config")):
        assert httpx.request(method, url + path, headers=headers, json={}).status_code == 403, path


def test_agent_post_must_be_a_json_object(architect):
    url = f"{architect.backend.url}/api/tasks/{architect.key}/agent/running"
    headers = {"Authorization": f"Bearer {architect.token}"}
    assert httpx.post(url, content=b"not json", headers=headers).status_code == 422
    assert httpx.post(url, json=[1, 2], headers=headers).status_code == 422


def test_question_round_trip(architect, human):
    response = architect.post("question", {"message": "Which tenants?", "choices": ["All", "Some"]})
    assert response.status_code == 200 and response.json()["state"] == "WAITING_FOR_USER"
    snap = human.task(architect.key)
    assert node(snap, "architect")["state"] == "WAITING_FOR_USER"
    [question] = snap["questions"]
    assert (question["message"], question["choices"], question["answered"]) == ("Which tenants?", ["All", "Some"], False)
    assert question["runId"] == architect.run_id and question["actionable"] is True

    snap = human.answer(architect.key, question["uid"], "All")
    assert node(snap, "architect")["state"] == "RUNNING"
    [question] = snap["questions"]
    assert question["answered"] is True and question["answer"] == "All"
    # Answering the same question again with the same text is idempotent; a different text is not.
    human.answer(architect.key, question["uid"], "All")
    conflict = human.post(f"/api/tasks/{architect.key}/questions/{question['uid']}/answer", json={"answer": "Some"})
    assert conflict.status_code == 409


TREE = {
    "message": "312 inactive accounts still own open invoices. What should the cleanup do?",
    "context": "4,108 accounts inactive · billing reads invoice.owner_id",
    "risk": "high",
    "choices": [
        {"label": "Soft-delete, keep invoices", "recommended": True, "followUp": {
            "message": "When are they purged for good?", "choices": [
                {"label": "After their invoices close", "recommended": True, "detail": "A nightly job purges"},
                {"label": "Never", "warning": "Table grows", "note": "Legal asked for a purge date.",
                 "source": "docs/privacy.md"}]}},
        {"label": "Hard-delete everything", "warning": "Breaks history on 312 invoices"},
    ],
}


def test_decision_tree_round_trip(architect, human):
    response = architect.post("question", {**TREE, "choices": [*TREE["choices"], {"label": " Skip ", "detail": " "}]})
    assert response.status_code == 200, response.text
    [question] = human.task(architect.key)["questions"]
    assert question["choices"] == ["Soft-delete, keep invoices", "Hard-delete everything", "Skip"]
    assert question["decision"] == {**{k: TREE[k] for k in ("context", "risk")},
                                    "choices": [*TREE["choices"], {"label": "Skip"}]}

    [listed] = [q for q in human.ok("GET", "/api/questions?open=true") if q["uid"] == question["uid"]]
    assert {k: listed[k] for k in ("jiraKey", "nodeKey", "nodeLabel", "actionable", "decision")} == {
        "jiraKey": architect.key, "nodeKey": "architect", "nodeLabel": "Architect", "actionable": True,
        "decision": question["decision"]}
    golden.check_shape("open-question", listed)
    assert human.get("/api/questions?open=false").status_code == 422

    answer = f"/api/tasks/{architect.key}/questions/{question['uid']}/answer"
    for body in ({"path": [0]}, {"path": [3]}, {"path": [0, 2]}, {"path": [0, 0, 0]}, {"path": []},
                 {"path": ["0"]}, {"path": [-1]}, {"path": [0], "answer": "x"}, {}):
        assert human.post(answer, json=body).status_code == 422, body
    line = "1.2: Soft-delete, keep invoices \u2192 Never"
    snap = human.ok("POST", answer, json={"path": [0, 1]})
    assert snap["questions"][0]["answer"] == line and node(snap, "architect")["state"] == "RUNNING"
    assert human.ok("POST", answer, json={"path": [0, 1]})["questions"][0]["answer"] == line  # idempotent
    assert human.post(answer, json={"path": [1]}).status_code == 409
    assert human.post(answer, json={"answer": "Never"}).status_code == 409
    assert question["uid"] not in [q["uid"] for q in human.ok("GET", "/api/questions")]


def test_decision_tree_free_text_and_flat_paths(architect, human):
    assert architect.post("question", {"message": "Anything to add?", "risk": "low"}).status_code == 200
    assert architect.post("question", {"message": "Which tenants?", "choices": ["All", "Some"]}).status_code == 200
    tree, flat = human.task(architect.key)["questions"]
    assert (tree["decision"], flat["decision"]) == ({"context": None, "risk": "low", "choices": []}, None)
    snap = human.answer(architect.key, tree["uid"], "Ask legal")
    assert snap["questions"][0]["answer"] == "other: Ask legal" and node(snap, "architect")["state"] == "WAITING_FOR_USER"
    assert human.post(f"/api/tasks/{architect.key}/questions/{flat['uid']}/answer", json={"path": [0, 0]}).status_code == 422
    snap = human.ok("POST", f"/api/tasks/{architect.key}/questions/{flat['uid']}/answer", json={"path": [1]})
    assert snap["questions"][1]["answer"] == "Some" and node(snap, "architect")["state"] == "RUNNING"


def _changed(path: tuple, **fields):
    data = {**TREE, "choices": [dict(c) for c in TREE["choices"]]}
    data["choices"][0]["followUp"] = {**TREE["choices"][0]["followUp"],
                                      "choices": [dict(c) for c in TREE["choices"][0]["followUp"]["choices"]]}
    target = data
    for step in path:
        target = target[step]
    target.update(fields)
    return data


@pytest.mark.parametrize("payload, problem", [
    ({**TREE, "choices": ["Plain", {"label": "Object"}]}, "choices: use objects for every choice"),
    ({**TREE, "risk": "severe"}, "risk: must be low, medium or high"),
    ({**TREE, "context": "x" * 501}, "context: must be at most 500 characters"),
    (_changed(("choices", 1), label="A \u2192 B"), "choices[1].label: cannot contain \u2192"),
    (_changed(("choices", 1), label=" soft-delete, KEEP invoices"), "choices: labels must be unique"),
    (_changed(("choices", 1), recommended=True), "choices: at most one choice can be recommended"),
    (_changed(("choices", 1), source="docs/a.md"), "choices[1].source: needs a note"),
    (_changed(("choices", 1), note="n", source="../secrets"), "choices[1].source: must be a repository-relative path"),
    (_changed(("choices", 1), note="n", source="C:/x"), "choices[1].source: must be a repository-relative path"),
    (_changed(("choices", 1), note="n", source="http://x.test"), "choices[1].source: must be a repository-relative path"),
    (_changed(("choices", 1), warn="typo"), "choices[1]: unknown field 'warn'"),
    (_changed(("choices", 1), followUp={"message": "Then?", "choices": [{"label": "Only"}]}),
     "choices[1].followUp.choices: must have 2 to 8 choices"),
    (_changed(("choices", 0, "followUp", "choices", 1),
              followUp={"message": "Deeper?", "choices": [{"label": "A"}, {"label": "B"}]}),
     "choices[0].followUp.choices[1].followUp: follow-up options cannot ask another follow-up"),
    (_changed(("choices", 0, "followUp", "choices", 1), recommended=True),
     "choices[0].followUp.choices: at most one choice can be recommended"),
])
def test_decision_tree_rejections_name_the_field(architect, human, payload, problem):
    response = architect.post("question", payload)
    assert response.status_code == 422 and problem in response.text, response.text
    assert human.task(architect.key)["questions"] == []


def test_completion_moves_discovery_to_requirements_approval(architect, human):
    write_requirements(Path(human.task(architect.key)["taskDir"]))
    response = architect.post("complete", {"result": ARCHITECT_RESULT})
    assert response.status_code == 200, response.text
    body = response.json()
    assert body["state"] == "COMPLETED" and not body.get("error")

    snap = human.task(architect.key)
    assert node(snap, "requirements_gate")["state"] == "WAITING_FOR_APPROVAL"
    assert "STUB_NOTES" in snap["requirements"]["requirements"]
    assert len(snap["requirementsDraftDigest"]) == 64


def test_completed_run_rejects_further_callbacks(architect, human):
    write_requirements(Path(human.task(architect.key)["taskDir"]))
    assert architect.post("complete", {"result": ARCHITECT_RESULT}).status_code == 200
    for action, payload in (("running", {}), ("question", {"message": "late?"}),
                            ("complete", {"result": {**ARCHITECT_RESULT, "summary": "changed my mind"}})):
        assert architect.post(action, payload).status_code == 409, action
    # Resending the identical result (a retry after a lost response) is accepted again.
    again = architect.post("complete", {"result": ARCHITECT_RESULT})
    assert again.status_code == 200 and again.json()["state"] == "COMPLETED"


def test_failure_report_fails_the_node(architect, human):
    response = architect.post("fail", {"message": "Cannot continue: contract test"})
    assert response.status_code == 200, response.text
    snap = human.task(architect.key)
    assert node(snap, "architect")["state"] == "FAILED"
    assert "contract test" in (node(snap, "architect")["error"] or node(snap, "architect")["message"])
