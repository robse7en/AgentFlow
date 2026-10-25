"""End-to-end workflow with the stub agent playing every role: discovery with a question,
requirements approval, a review/fix loop, final verification and approval, and the review cap."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

import golden
from support import new_key, node

# The stub's verification command: STUB_NOTES.md must exist at the worktree root. Commands run in
# the harness cwd (worktree/<projectRelativePath>), so the root is found through git.
VERIFY = [sys.executable, "-c",
          "import subprocess, pathlib; root = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip(); "
          "assert pathlib.Path(root, 'STUB_NOTES.md').exists(), 'STUB_NOTES.md missing'"]


@pytest.fixture(scope="module")
def backend(launch):
    with launch(workflow={"maxReviewIterations": 3, "verificationCommands": [VERIFY]},
                STUB_FINDINGS_UNTIL="1", STUB_ASK_QUESTION="1") as backend:
        yield backend


@pytest.fixture(scope="module")
def human(backend):
    return backend.human()


def test_full_workflow_reaches_ready_for_pr(backend, human, repo):
    key = new_key("FLOW")
    snap = human.create_task(key, repo, projectRelativePath="src", additionalContext="Contract test context")
    assert snap["branchName"] == f"agent/{key}"
    assert Path(snap["worktreePath"]).is_dir()
    assert node(snap, "architect")["state"] == "READY"

    # Discovery: the stub asks one question and waits for the answer on its terminal.
    human.run(key, "architect")
    snap = human.wait_node(key, "architect", "WAITING_FOR_USER")
    assert node(snap, "architect")["openQuestions"] == 1
    [question] = [q for q in snap["questions"] if not q["answered"]]
    assert question["role"] == "architect" and question["message"].startswith("Stub question")
    human.answer(key, question["uid"], "Yes, all tenants")

    snap = human.wait_node(key, "requirements_gate", "WAITING_FOR_APPROVAL")
    assert node(snap, "architect")["state"] == "COMPLETED"
    assert [q["answer"] for q in snap["questions"]] == ["Yes, all tenants"]
    assert "STUB_NOTES" in snap["requirements"]["requirements"]
    golden.check_shape("snapshot-requirements-gate", snap)
    roots = {"TMP": backend.tmp, "REPO": repo}
    golden.check_events("requirements-gate", snap["events"], key=key, roots=roots)
    golden.check_prompt(Path(snap["taskDir"]), "architect/01", key=key, roots=roots)

    # Everything after requirements approval runs on its own: implementation, both reviews,
    # consolidation, one fix iteration (STUB_FINDINGS_UNTIL=1), clean reviews, final architect.
    human.approve_requirements(key)
    snap = human.wait_task(key, lambda s: s["reviewIteration"] == 2, "fix iteration", 180)
    assert len([f for f in snap["findings"] if f["blocking"]]) == 2, snap["findings"]
    assert node(snap, "consolidate")["state"] == "COMPLETED"
    first_sha = snap["currentSha"]

    snap = human.wait_node(key, "ready_for_pr", "WAITING_FOR_APPROVAL", timeout=240)
    assert snap["currentSha"] and snap["currentSha"] != first_sha
    # The round-2 reviewers ruled both prior findings fixed.
    assert sorted((f["id"], f["status"]) for f in snap["findings"]) == [("CODE-001", "resolved"), ("SEC-001", "resolved")]
    # Final verification runs in a fresh, scoped architect process.
    assert len([s for s in snap["sessions"] if s["role"] == "architect"]) == 2
    assert snap["assessment"]["overallRisk"] == "LOW"
    if backend.target.name == "rust":
        assert node(snap, "implementer")["lastRun"]["result"]["checkpointSha"] == snap["currentSha"]
    golden.check_shape("snapshot-final-approval", snap)

    human.ok("POST", f"/api/tasks/{key}/nodes/ready_for_pr/approve", json={"expectedSha": snap["currentSha"]})
    snap = human.task(key)
    assert snap["status"] == "READY_FOR_PR"

    task_dir = Path(snap["taskDir"])
    for rel in ("task.json", "requirements/requirements.md", "findings/consolidated-01.json",
                "findings/consolidated-02.json", "final/assessment.json", "runs/architect/01/prompt.md",
                "runs/architect/02/prompt.md", "runs/implementer/02/result.json"):
        assert (task_dir / rel).exists(), f"missing artifact {rel}"
    messages = [e["message"] for e in snap["events"]]
    assert any("Task created" in m for m in messages) and any("READY_FOR_PR" in m for m in messages)
    golden.check_prompts(task_dir, key=key, roots=roots)
    golden.check_events("ready-for-pr", snap["events"], key=key, roots=roots)


def test_review_cap_hands_control_back_to_the_human(launch, repo):
    with launch(workflow={"maxReviewIterations": 2}, STUB_FINDINGS_UNTIL="9") as backend:
        human = backend.human()
        key = new_key("CAP")
        human.create_task(key, repo)
        human.run(key, "architect")
        human.wait_node(key, "requirements_gate", "WAITING_FOR_APPROVAL")
        human.approve_requirements(key)
        # Every review blocks, so after two iterations the engine stops and asks the human.
        snap = human.wait_task(key, lambda s: s["status"] == "NEEDS_HUMAN_REVIEW", "review cap", 240)
        assert "Maximum review iterations reached" in snap["statusMessage"] and "CODE-001" in snap["statusMessage"]
        assert node(snap, "implementer")["state"] == "READY"

        # Running the implementer again is the human's explicit decision to continue.
        human.run(key, "implementer")
        human.wait_task(key, lambda s: s["reviewIteration"] == 3 and s["status"] == "ACTIVE", "iteration 3")
