"""Worker scheduling, ownership and integration through the public API and real local Git."""
from pathlib import Path

from support import ARCHITECT_RESULT, AgentRun, agent_run, git, new_key, node, write_requirements


def plan(count=4):
    return {"summary": "Implement isolated parts", "subtasks": [
        {"key": f"part-{i}", "title": f"Part {i}", "brief": f"Implement part {i}",
         "ownedPaths": [f"src/part-{i}"], "acceptanceCriteria": ["The part exists"],
         "dependencies": []} for i in range(count)]}


def begin(backend, human, repo, value, planner="architect"):
    """Discovery, approval and a submitted plan. With no Orchestrator configured the Architect plans:
    the planning run sits beside its discovery run and reports as the Architect, whose flowctl adds
    a status to every result."""
    key = new_key("WK")
    human.create_task(key, repo, implementationMode="parallel")
    human.run(key, "architect")
    architect = agent_run(backend, human.task(key), "architect")
    write_requirements(Path(human.task(key)["taskDir"]))
    assert architect.post("complete", {"result": ARCHITECT_RESULT}).json()["ok"]
    human.approve_requirements(key)
    planning = node(human.wait_node(key, "orchestrator", "RUNNING"), "orchestrator")
    assert planning["role"] == planner
    assert planning["label"] == ("Architect Planning" if planner == "architect" else "Orchestrator")
    run_dir = Path(planning["lastRun"]["runDir"])
    assert run_dir.parent.name == planner
    result = {"status": "completed", **value} if planner == "architect" else value
    assert AgentRun(backend, key, planner, run_dir).post("complete", {"result": result}).json()["ok"]
    return key


def workers(snap):
    return [n for n in snap["nodes"] if n.get("worker")]


def finish(backend, key, child, path=None):
    tree = Path(child["worker"]["worktreePath"])
    changed = tree / (path or f"src/{child['worker']['key']}/result.txt")
    changed.parent.mkdir(parents=True, exist_ok=True)
    changed.write_text(f"{child['key']}\n", encoding="utf-8")
    run = AgentRun(backend, key, "implementer", Path(child["lastRun"]["runDir"]))
    result = {"status": "completed", "summary": "Done", "tests": {"executed": 1, "passed": 1, "failed": 0}}
    return run, result, run.post("complete", {"result": result}).json()


def test_workers_capacity_ownership_and_integration(launch, repo):
    with launch(workflow={"implementationMode": "parallel"}, STUB_MANUAL="1") as backend:
        human = backend.human()
        key = begin(backend, human, repo, plan())
        children = workers(human.task(key))
        assert [n["state"] for n in children] == ["RUNNING", "RUNNING", "RUNNING", "PENDING"]
        assert len({n["worker"]["worktreePath"] for n in children}) == 4
        first_worker = AgentRun(backend, key, 'implementer', Path(children[0]['lastRun']['runDir']))
        assert first_worker.post('heartbeat', {'phase': 'implementing'}).status_code == 200
        assert first_worker.post('heartbeat', {
            'runId': children[1]['lastRun']['id'], 'phase': 'testing',
        }).status_code == 403
        assert [n['state'] for n in workers(human.task(key))] == [
            'RUNNING', 'RUNNING', 'RUNNING', 'PENDING',
        ]
        first_run, result, response = finish(backend, key, children[0])
        assert response["ok"]
        assert first_run.post("complete", {"result": result}).json()["ok"]
        children = workers(human.task(key))
        assert children[3]["state"] == "RUNNING"
        for child in children[1:]:
            assert finish(backend, key, child)[2]["ok"]
        snap = human.task(key)
        assert node(snap, "implementer")["state"] == "RUNNING"
        assert node(snap, "code_review")["state"] == "PENDING"
        # The coordinator must include every worker checkpoint before reviewers may start.
        tree = Path(snap["worktreePath"])
        for child in workers(snap):
            git("merge", "--no-edit", child["checkpointSha"], cwd=tree)
        integrator = agent_run(backend, snap, "implementer")
        assert integrator.post("complete", {"result": result}).json()["ok"]
        assert node(human.task(key), "code_review")["state"] == "RUNNING"
        human.ok("POST", f"/api/tasks/{key}/cancel")


def test_worker_dependencies_and_ownership_failure(launch, repo):
    with launch(STUB_MANUAL="1") as backend:
        human = backend.human()
        value = plan(2)
        value["subtasks"][1]["dependencies"] = ["part-0"]
        key = begin(backend, human, repo, value)
        children = workers(human.task(key))
        assert children[1]["state"] == "PENDING"
        assert finish(backend, key, children[0])[2]["ok"]
        children = workers(human.task(key))
        assert children[1]["state"] == "RUNNING"
        assert (Path(children[1]["worker"]["worktreePath"]) / "src/part-0/result.txt").exists()
        assert finish(backend, key, children[1], "src/unassigned.txt")[2]["state"] == "FAILED"
        assert node(human.task(key), "code_review")["state"] == "PENDING"
        human.ok("POST", f"/api/tasks/{key}/cancel")


def test_a_configured_orchestrator_plans_in_its_own_conversation(launch, repo):
    orchestrator = {"orchestrator": {"harness": "stub", "reasoning": "high"}}
    with launch(agents=orchestrator, STUB_MANUAL="1") as backend:
        human = backend.human()
        key = begin(backend, human, repo, plan(1), planner="orchestrator")
        assert len(workers(human.task(key))) == 1
        human.ok("POST", f"/api/tasks/{key}/cancel")
