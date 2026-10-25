#!/usr/bin/env python
"""End-to-end smoke test: drives the full AgentFlow workflow with the stub harness.

Requires: git, zellij, and the backend venv (run with backend/.venv python).

    backend/.venv/Scripts/python scripts/smoke_test.py            # Windows
    backend/.venv/bin/python scripts/smoke_test.py                # POSIX

It creates a throw-away Git repository, an isolated AGENTFLOW_HOME, starts the backend on a
spare port, and walks a task from creation to READY_FOR_PR — including a review/fix loop.
Everything is cleaned up afterwards (zellij session, worktree, temp dirs).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import threading
import uuid
from pathlib import Path

import httpx

ROOT = Path(__file__).resolve().parents[1]
PORT = int(os.environ.get("SMOKE_PORT", "8799"))
API = f"http://127.0.0.1:{PORT}"
HUMAN = httpx.Client(headers={"Origin": API})


def sh(*args: str, cwd: str | Path | None = None) -> str:
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()


def wait_for(predicate, timeout: float = 90.0, what: str = "condition"):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = predicate()
        if last:
            return last
        time.sleep(0.5)
    raise TimeoutError(f"timed out waiting for {what}")


def node(snapshot: dict, key: str) -> dict:
    return next(n for n in snapshot["nodes"] if n["key"] == key)


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    # resolve(): %TEMP% can be an 8.3 short path (C:\Users\ROBROD~1), which worktree creation
    # rejects because it does not resolve to itself.
    tmp = Path(tempfile.mkdtemp(prefix="agentflow-smoke-")).resolve()
    home = tmp / "home"
    repo = tmp / "repo"
    key = f"SMOKE-{uuid.uuid4().hex[:6].upper()}"
    backend: subprocess.Popen | None = None
    ok = False
    try:
        # 1. throw-away repository
        repo.mkdir()
        sh("git", "init", "-q", "-b", "main", cwd=repo)
        sh("git", "-c", "user.name=smoke", "-c", "user.email=smoke@local", "commit", "-q", "--allow-empty", "-m", "init", cwd=repo)
        (repo / "src").mkdir()
        (repo / "src" / "app.txt").write_text("hello\n")
        sh("git", "add", "-A", cwd=repo)
        sh("git", "-c", "user.name=smoke", "-c", "user.email=smoke@local", "commit", "-q", "-m", "app", cwd=repo)

        # 2. isolated configuration using the stub harness for every role
        home.mkdir()
        stub = {
            "enabled": True, "type": "generic-cli", "displayName": "Stub Agent", "command": sys.executable,
            "args": [str(ROOT / "scripts" / "stub_agent.py")], "modelArgument": "--model", "reasoningArgument": "--reasoning",
            "env": {"STUB_FINDINGS_UNTIL": "1", "STUB_DELAY": "0.2", "STUB_ASK_QUESTION": "1"},
        }
        config = {
            "app": {"dataDirectory": str(home), "databasePath": str(home / "agentflow.db")},
            "server": {"host": "127.0.0.1", "port": PORT},
            "workspace": {"worktreeDirectory": str(tmp / "worktrees"), "taskDirectory": str(home / "tasks"), "branchPrefix": "agent/"},
            "workflow": {"maxReviewIterations": 3, "implementationMode": "direct",
                         # verificationCommands run in the harness cwd (worktree/<projectRelativePath>,
                         # i.e. "src" here), but STUB_NOTES.md lives at the worktree root - locate it
                         # via `git rev-parse --show-toplevel` instead of assuming the command's cwd.
                         "verificationCommands": [[sys.executable, "-c",
                                                   "import subprocess, pathlib; root = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip(); "
                                                   "assert pathlib.Path(root, 'STUB_NOTES.md').exists(), 'STUB_NOTES.md missing'"]]},
            "harnesses": {"stub": stub, "claude": {"type": "claude", "command": "claude", "displayName": "Claude Code"}},
            "agents": {
                "architect": {"harness": "stub", "reasoning": "high"},
                "implementer": {"harness": "stub", "reasoning": "high"},
                "codeReviewer": {"harness": "stub", "reasoning": "high"},
                "securityReviewer": {"harness": "stub", "reasoning": "high"},
            },
        }
        config_file = home / "config.json"
        config_file.write_text(json.dumps(config, indent=2))

        # 3. backend
        env = {k: v for k, v in os.environ.items() if not k.startswith("ZELLIJ")}
        # The stub workflow never talks to Jira.
        backend = subprocess.Popen([sys.executable, "-m", "agentflow.main", "--config", str(config_file),
                                    "--skip-jira-check"],
                                   cwd=ROOT / "backend", env=env, stdout=subprocess.PIPE, text=True)
        pairing = {}

        def read_output():
            for line in backend.stdout:
                if line.startswith("AgentFlow browser pairing code: "):
                    pairing["code"] = line.split(": ", 1)[1].strip()
                else:
                    print(line, end="")

        threading.Thread(target=read_output, daemon=True).start()
        wait_for(lambda: _health(), 30, "backend health")
        wait_for(lambda: pairing.get("code"), 30, "pairing code")
        HUMAN.post(f"{API}/api/auth/pair", json={"code": pairing.pop("code")}).raise_for_status()
        print(f"backend up on {API}")

        harnesses = HUMAN.get(f"{API}/api/harnesses").json()
        assert any(h["key"] == "stub" and h["available"] for h in harnesses), harnesses

        # 4. create task
        response = HUMAN.post(f"{API}/api/tasks", json={
            "jiraKey": key, "repositoryPath": str(repo), "projectRelativePath": "src", "baseBranch": "main",
            "additionalContext": "Smoke test context",
        }, timeout=120)
        assert response.status_code == 201, response.text
        snap = response.json()
        assert Path(snap["worktreePath"]).is_dir(), "worktree missing"
        assert snap["branchName"] == f"agent/{key}"
        assert node(snap, "architect")["state"] == "READY"
        print(f"task {key} created; worktree {snap['worktreePath']}; zellij {snap['zellijSession']}")

        # duplicate creation must be refused, not overwrite
        dup = HUMAN.post(f"{API}/api/tasks", json={"jiraKey": key, "repositoryPath": str(repo), "baseBranch": "main"}, timeout=60)
        assert dup.status_code == 409, dup.text

        get = lambda: HUMAN.get(f"{API}/api/tasks/{key}", timeout=30).json()  # noqa: E731

        # 5. architect discovery
        run(key, "architect")
        wait_for(lambda: node(get(), "architect")["state"] == "WAITING_FOR_USER", what="architect question")
        snap = get()
        assert node(snap, "architect")["openQuestions"] == 1 and len([q for q in snap["questions"] if not q["answered"]]) == 1
        question = next(q for q in snap["questions"] if not q["answered"])
        print("architect asked a question -> WAITING_FOR_USER; answering through the UI API")
        # The backend types the answer into the agent's pane; the stub reads it from stdin.
        HUMAN.post(f"{API}/api/tasks/{key}/questions/{question['uid']}/answer",
                   json={"answer": "Yes, all tenants"}, timeout=30).raise_for_status()
        wait_for(lambda: node(get(), "requirements_gate")["state"] == "WAITING_FOR_APPROVAL", what="requirements gate")
        assert all(q["answered"] for q in get()["questions"])
        snap = get()
        assert node(snap, "architect")["state"] == "COMPLETED"
        assert "STUB_NOTES" in snap["requirements"]["requirements"]
        print("architect discovery complete → requirements approval")
        HUMAN.post(f"{API}/api/tasks/{key}/nodes/requirements_gate/approve",
                   json={"expectedRequirementsDigest": HUMAN.get(f"{API}/api/tasks/{key}").json()["requirementsDraftDigest"]}, timeout=30).raise_for_status()
        print("requirements approved; Implementer/Reviews/Final run automatically from here - waiting for the pipeline")

        # 6. implementation iteration 1 → checkpoint → reviews all run automatically
        # (ExecutionMode.AUTOMATIC); calling `run` on them ourselves would 409.
        wait_for(lambda: get()["reviewIteration"] == 2 and node(get(), "implementer")["state"] in ("READY", "RUNNING", "COMPLETED"),
                 timeout=180, what="fix loop")
        snap = get()
        sha1 = node(snap, "code_review")["checkpointSha"] or snap["currentSha"]
        blocking = [f for f in snap["findings"] if f["blocking"]]
        assert len(blocking) == 2, snap["findings"]
        assert node(snap, "consolidate")["state"] == "COMPLETED"
        print(f"implementation 1 complete, checkpoint {sha1[:10]}; iteration 1 reviews: {len(snap['findings'])} findings → fix loop")

        # 7. fix iteration 2 → clean reviews → final architect, all still automatic
        wait_for(lambda: node(get(), "ready_for_pr")["state"] == "WAITING_FOR_APPROVAL", timeout=180, what="final approval")
        snap = get()
        sha2 = snap["currentSha"]
        assert sha2 and sha2 != sha1
        # The round-2 reviewers ruled both prior findings fixed.
        assert all(f["status"] == "resolved" for f in snap["findings"]), snap["findings"]
        print(f"implementation 2 complete, checkpoint {sha2[:10]}; iteration 2 reviews clean → final architect ran automatically")

        # 8. final verification ran through a fresh scoped architect process
        assert len([s for s in snap["sessions"] if s["role"] == "architect"]) == 2, "final verification needs a fresh scoped process"
        assert snap["assessment"]["overallRisk"] == "LOW"
        HUMAN.post(f"{API}/api/tasks/{key}/nodes/ready_for_pr/approve", json={"expectedSha": sha2}, timeout=30).raise_for_status()
        snap = get()
        assert snap["status"] == "READY_FOR_PR", snap["status"]
        print("READY_FOR_PR reached")

        # 9. artifacts on disk
        task_dir = Path(snap["taskDir"])
        for rel in ("task.json", "requirements/requirements.md", "findings/consolidated-01.json", "findings/consolidated-02.json",
                    "final/assessment.json", "runs/architect/01/prompt.md", "runs/architect/02/prompt.md", "runs/implementer/02/result.json"):
            assert (task_dir / rel).exists(), f"missing artifact {rel}"
        events = [e["message"] for e in snap["events"]]
        assert any("Task created" in m for m in events) and any("READY_FOR_PR" in m for m in events)
        print(f"{len(events)} events logged; artifacts present under {task_dir}")

        # 10. second task: reach the review limit, then authorize another numbered attempt.
        key2 = key + "B"
        stub["env"] = {"STUB_FINDINGS_UNTIL": "9", "STUB_DELAY": "0.2"}
        config["workflow"]["maxReviewIterations"] = 2
        config_file.write_text(json.dumps(config, indent=2))
        HUMAN.post(f"{API}/api/config/reload", timeout=30).raise_for_status()
        response = HUMAN.post(f"{API}/api/tasks", json={"jiraKey": key2, "repositoryPath": str(repo), "baseBranch": "main"}, timeout=120)
        assert response.status_code == 201, response.text
        get2 = lambda: HUMAN.get(f"{API}/api/tasks/{key2}", timeout=30).json()  # noqa: E731
        run(key2, "architect")
        wait_for(lambda: node(get2(), "requirements_gate")["state"] == "WAITING_FOR_APPROVAL", what="gate 2")
        HUMAN.post(f"{API}/api/tasks/{key2}/nodes/requirements_gate/approve",
                   json={"expectedRequirementsDigest": HUMAN.get(f"{API}/api/tasks/{key2}").json()["requirementsDraftDigest"]}, timeout=30).raise_for_status()
        # Both review iterations run automatically; every finding blocks (STUB_FINDINGS_UNTIL=9),
        # so after maxReviewIterations=2 the engine stops and hands control back to a human.
        wait_for(lambda: get2()["status"] == "NEEDS_HUMAN_REVIEW", timeout=240, what="review cap")
        snap = get2()
        assert "Maximum review iterations reached" in snap["statusMessage"] and "CODE-001" in snap["statusMessage"]
        assert node(snap, "implementer")["state"] == "READY"
        print(f"iteration cap: {snap['statusMessage']}")

        # Human continues explicitly; task becomes ACTIVE and the iteration advances, then the
        # (still automatic) reviews run again on their own.
        run(key2, "implementer")
        wait_for(lambda: get2()["reviewIteration"] == 3 and get2()["status"] == "ACTIVE", what="iteration 3")
        # Exact SHA rejection is covered deterministically by test_workflow_safety.py.
        ok = True
    finally:
        if backend:
            backend.terminate()
            try:
                backend.wait(10)
            except subprocess.TimeoutExpired:
                backend.kill()
        for session in (f"agentflow-{key}", f"agentflow-{key}B"):
            subprocess.run(["zellij", "kill-session", session], capture_output=True, timeout=30)
            subprocess.run(["zellij", "delete-session", session, "--force"], capture_output=True, timeout=30)
        time.sleep(0.5)
        shutil.rmtree(tmp, ignore_errors=True)
    print("SMOKE TEST PASSED" if ok else "SMOKE TEST FAILED")
    return 0 if ok else 1


def _health() -> bool:
    try:
        return HUMAN.get(f"{API}/api/health", timeout=2).status_code == 200
    except httpx.HTTPError:
        return False


def run(key: str, node_key: str) -> None:
    response = HUMAN.post(f"{API}/api/tasks/{key}/nodes/{node_key}/run", json={}, timeout=60)
    assert response.status_code == 200, f"run {node_key}: {response.status_code} {response.text}"


if __name__ == "__main__":
    sys.exit(main())
