"""Task lifecycle through the UI API: create, list, validate, cancel, archive and delete."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

import golden
from support import git, make_repository, new_key, node


@pytest.fixture(scope="module")
def backend(launch):
    with launch(STUB_MANUAL="1") as backend:
        yield backend


@pytest.fixture(scope="module")
def human(backend):
    return backend.human()


def test_cost_estimate_for_a_new_task(launch):
    # A fresh backend: no run history, so every stage says there is not enough of it.
    with launch(STUB_MANUAL="1") as fresh:
        human = fresh.human()
        direct = human.ok("POST", "/api/tasks/cost-estimate", json={"implementationMode": "direct"})
        assert direct["implementationMode"] == "direct"
        assert [(s["key"], s["label"], s["harness"]) for s in direct["stages"]] == [
            ("architect", "Architect", "stub"), ("implementer", "Implementer", "stub"),
            ("code_review", "Code Review", "stub"), ("security_review", "Security Review", "stub"),
            ("final_architect", "Final Architect Verification", "stub")]
        assert direct["stages"][0]["estimate"] == {"unit": "usd", "runs": 0, "enough": False, "typical": None,
                                                   "high": None, "costKind": None}
        assert direct["missing"] == [s["label"] for s in direct["stages"]]
        assert direct["onePass"] == {"usd": None, "tokens": None} and direct["perWorker"] is None

        # The form's overrides decide the planner, as they do when the task is created.
        parallel = human.ok("POST", "/api/tasks/cost-estimate", json={
            "implementationMode": "parallel", "agentOverrides": {"orchestrator": {"harness": "stub"}}})
        assert [(s["key"], s["label"]) for s in parallel["stages"]][1:3] == [
            ("orchestrator", "Orchestrator"), ("worker", "Worker")]
        assert parallel["perWorker"] == {"usd": None, "tokens": None}
        assert human.ok("POST", "/api/tasks/cost-estimate", json={})["implementationMode"] == "direct"
        golden.check_shape("cost-estimate", parallel)
        assert human.post("/api/tasks/cost-estimate", json={"implementationMode": "serial"}).status_code == 422


def test_create_makes_a_worktree_on_a_task_branch(human, repo):
    key = new_key("TK")
    snap = human.create_task(key, repo, projectRelativePath="src", additionalContext="ctx")
    assert snap["jiraKey"] == key and snap["status"] == "ACTIVE"
    assert snap["branchName"] == f"agent/{key}"
    assert snap["additionalContext"] == "ctx"
    worktree = Path(snap["worktreePath"])
    assert (worktree / "src" / "app.txt").read_text(encoding="utf-8") == "hello\n"
    assert git("rev-parse", "--abbrev-ref", "HEAD", cwd=worktree) == f"agent/{key}"
    assert Path(snap["harnessCwd"]) == worktree / "src"
    assert node(snap, "architect")["state"] == "READY"
    assert [n["key"] for n in snap["nodes"]] == ["architect", "requirements_gate", "implementer", "code_review",
                                                 "security_review", "consolidate", "final_architect", "ready_for_pr"]
    golden.check_shape("snapshot-created", snap)


@pytest.fixture
def behind_origin(tmp_path_factory) -> tuple[Path, str]:
    """A repository whose local `main` is behind its bare origin by one commit (returned)."""
    root = Path(tmp_path_factory.mktemp("behind")).resolve()
    repo = make_repository(root / "repo")
    remote = root / "origin.git"
    git("init", "-q", "--bare", str(remote), cwd=root)
    git("remote", "add", "origin", str(remote), cwd=repo)
    git("push", "-q", "origin", "main", cwd=repo)
    tree = git("rev-parse", "main^{tree}", cwd=repo)
    newer = git("-c", "user.name=contract", "-c", "user.email=contract@local",
                "commit-tree", tree, "-p", "main", "-m", "newer on origin", cwd=repo)
    git("push", "-q", "origin", f"{newer}:refs/heads/main", cwd=repo)
    git("update-ref", "refs/remotes/origin/main", "main", cwd=repo)
    return repo, newer


def test_create_branches_from_origin_when_the_local_base_is_behind(human, behind_origin):
    repo, newer = behind_origin
    local = git("rev-parse", "main", cwd=repo)
    snap = human.create_task(new_key("TK"), repo)
    worktree = Path(snap["worktreePath"])
    assert git("rev-parse", "HEAD", cwd=worktree) == newer
    assert git("rev-parse", "main", cwd=repo) == local
    assert snap["statusMessage"] is None
    upstream = subprocess.run(["git", "rev-parse", "--abbrev-ref", f"{snap['branchName']}@{{upstream}}"], cwd=repo,
                              capture_output=True, text=True)
    assert upstream.returncode != 0, upstream.stdout


def test_create_warns_when_the_base_could_not_be_fetched(human, behind_origin):
    repo, _ = behind_origin
    git("remote", "set-url", "origin", str(repo.parent / "missing.git"), cwd=repo)
    snap = human.create_task(new_key("TK"), repo)
    assert git("rev-parse", "HEAD", cwd=Path(snap["worktreePath"])) == git("rev-parse", "main", cwd=repo)
    warning = ("Couldn't fetch main from origin, so this task branched from the last fetched copy of main, "
               "which may be missing newer commits.")
    assert snap["statusMessage"] == warning
    assert any(e["level"] == "warning" and e["message"] == warning for e in snap["events"])


def test_local_task_from_a_prompt(human, backend, repo):
    response = human.post("/api/tasks", json={
        "taskSource": "local", "prompt": "  Tidy the readme\nKeep the links  ", "jiraKey": "ignored",
        "repositoryPath": str(repo), "baseBranch": "main", "additionalContext": "ctx"}, timeout=120)
    assert response.status_code == 201, response.text
    snap = response.json()
    key = snap["jiraKey"]
    backend.keys.add(key)
    assert re.fullmatch(r"LOCAL-[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}", key), key
    assert snap["taskSource"] == "local" and snap["jiraTitle"] == "Tidy the readme"
    assert snap["additionalContext"] == "Tidy the readme\nKeep the links\n\nctx"
    assert snap["branchName"] == f"agent/{key}"
    assert node(snap, "architect")["state"] == "READY"
    listed = [t for t in human.ok("GET", "/api/tasks") if t["jiraKey"] == key]
    assert [t["taskSource"] for t in listed] == ["local"]


@pytest.mark.parametrize("fields", [
    {"taskSource": "local", "prompt": "   "},
    {"taskSource": "local"},
    {"taskSource": "other", "prompt": "x"},
    {"jiraKey": "LOCAL-550E8400-E29B-41D4-A716-446655440000"},
    {"taskSource": "jira"},
])
def test_invalid_task_source_is_refused(human, repo, fields):
    response = human.post("/api/tasks", json={"repositoryPath": str(repo), "baseBranch": "main", **fields})
    assert response.status_code == 422, response.text


def test_jira_tasks_report_their_source(human, repo):
    assert human.create_task(new_key("TK"), repo)["taskSource"] == "jira"


def test_duplicate_key_is_refused(human, repo):
    key = new_key("TK")
    human.create_task(key, repo)
    response = human.post("/api/tasks", json={"jiraKey": key, "repositoryPath": str(repo), "baseBranch": "main"})
    assert response.status_code == 409


def test_create_rejects_a_path_that_is_not_a_repository(human, tmp_path):
    # Stop Git discovering an enclosing checkout when temp data stays in the workspace.
    (tmp_path / '.git').write_text('gitdir: missing-repository\n', encoding='utf-8')
    response = human.post("/api/tasks", json={"jiraKey": new_key("TK"), "repositoryPath": str(tmp_path), "baseBranch": "main"})
    assert 400 <= response.status_code < 500


def test_unknown_task_is_404(human):
    assert human.get("/api/tasks/NOPE-1").status_code == 404


def test_listing_includes_new_tasks(human, repo):
    key = new_key("TK")
    human.create_task(key, repo)
    listing = human.ok("GET", "/api/tasks")
    assert key in [t["jiraKey"] for t in listing]
    golden.check_shape("task-list-item", [t for t in listing if t["jiraKey"] == key][0])


def test_validate_repository(human, repo, tmp_path):
    good = human.ok("POST", "/api/tasks/validate-repository", json={"path": str(repo), "baseBranch": "main"})
    assert good["ok"] is True and "main" in good["branches"]
    (tmp_path / '.git').write_text('gitdir: missing-repository\n', encoding='utf-8')
    bad = human.ok("POST", "/api/tasks/validate-repository", json={"path": str(tmp_path)})
    assert bad["ok"] is False and bad["checks"]


def test_cancel_then_archive_and_unarchive(human, repo):
    key = new_key("TK")
    human.create_task(key, repo)
    # Only a finished task can be archived.
    assert human.post(f"/api/tasks/{key}/archive").status_code == 409
    assert human.ok("POST", f"/api/tasks/{key}/cancel")["status"] == "CANCELLED"
    human.ok("POST", f"/api/tasks/{key}/archive")
    assert key not in [t["jiraKey"] for t in human.ok("GET", "/api/tasks")]
    assert key in [t["jiraKey"] for t in human.ok("GET", "/api/tasks", params={"archived": "only"})]
    human.ok("POST", f"/api/tasks/{key}/unarchive")
    assert key in [t["jiraKey"] for t in human.ok("GET", "/api/tasks")]


def test_delete_removes_worktree_branch_and_task_folder(human, repo):
    key = new_key("TK")
    snap = human.create_task(key, repo)
    human.ok("POST", f"/api/tasks/{key}/cancel")
    preview = human.ok("GET", f"/api/tasks/{key}/delete-preview")
    assert preview["worktree"]["exists"] is True and preview["branch"]["exists"] is True
    assert preview["taskFolder"]["exists"] is True
    golden.check_shape("delete-preview", preview)

    human.ok("POST", f"/api/tasks/{key}/delete", json={
        "removeWorktree": True, "removeBranch": True, "removeTaskFolder": True, "expectedSha": preview["branch"]["sha"]})
    assert human.get(f"/api/tasks/{key}").status_code == 404
    assert not Path(snap["worktreePath"]).exists()
    assert not Path(snap["taskDir"]).exists()
    assert git("branch", "--list", f"agent/{key}", cwd=repo) == ""


def test_running_a_node_launches_its_agent(human, backend, repo):
    key = new_key("TK")
    human.create_task(key, repo)
    snap = human.run(key, "architect")
    assert node(snap, "architect")["state"] in ("RUNNING", "READY")
    snap = human.wait_node(key, "architect", "RUNNING")
    assert [s["role"] for s in snap["sessions"]] == ["architect"]
    # A node that is not ready cannot be run.
    assert human.post(f"/api/tasks/{key}/nodes/implementer/run", json={}).status_code == 409
