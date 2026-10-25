"""Manual PR lifecycle through the public API; records no changes on GitHub."""
from support import agent_run, git, new_key


def test_pr_lifecycle_and_human_only_controls(launch, repo):
    with launch(STUB_FINDINGS_UNTIL="0") as backend:
        human = backend.human()
        key = new_key("PR")
        human.create_task(key, repo)
        human.run(key, "architect")
        human.wait_node(key, "requirements_gate", "WAITING_FOR_APPROVAL")
        human.approve_requirements(key)
        snap = human.wait_node(key, "ready_for_pr", "WAITING_FOR_APPROVAL")
        human.ok("POST", f"/api/tasks/{key}/nodes/ready_for_pr/approve",
                 json={"expectedSha": snap["currentSha"], "reason": "Stub fixture has no independent commands"})
        agent = agent_run(backend, snap, "architect")
        assert snap["prBaseBranch"] is None
        # Opening a PR pushes only to a GitHub origin: none here, then a local one, never pushed to.
        assert human.post(f"/api/tasks/{key}/pr/open").status_code == 409
        origin = repo.parent / "origin.git"
        git("init", "-q", "--bare", str(origin), cwd=repo.parent)
        git("remote", "add", "origin", str(origin), cwd=repo)
        refused = human.post(f"/api/tasks/{key}/pr/open")
        assert refused.status_code == 409 and "into main yourself" in refused.json()["detail"]
        assert git("--git-dir", str(origin), "for-each-ref", cwd=repo.parent) == ""
        assert human.post(f"/api/tasks/{key}/pr/retarget").status_code == 409
        assert human.post(f"/api/tasks/{key}/pr/merged").status_code == 409
        assert human.post(f"/api/tasks/{key}/pr/created", json={"prUrl": "javascript:alert(1)"}).status_code == 422
        url = "https://github.com/example/local/pull/42"
        snap = human.ok("POST", f"/api/tasks/{key}/pr/created", json={"prUrl": f" {url} "})
        assert snap["status"] == "PR_CREATED" and snap["prUrl"] == url and snap["prCreatedAt"]
        assert human.post(f"/api/tasks/{key}/nodes/implementer/run").status_code == 409
        assert agent.post("running", {}).status_code == 409
        snap = human.ok("POST", f"/api/tasks/{key}/pr/merged")
        assert snap["status"] == "MERGED" and snap["mergedAt"]
        assert human.ok("POST", f"/api/tasks/{key}/pr/merged/revert")["status"] == "PR_CREATED"
        snap = human.ok("POST", f"/api/tasks/{key}/pr/created/revert")
        assert snap["status"] == "READY_FOR_PR" and snap["prUrl"] is None and snap["prCreatedAt"] is None
        human.ok("POST", f"/api/tasks/{key}/cancel")
