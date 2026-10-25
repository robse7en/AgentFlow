"""Durable worker scheduling. Called only while the engine holds the task lock."""

from __future__ import annotations

from pathlib import Path
from uuid import uuid4

from sqlalchemy import select

from ..database.models import AgentSession, ImplementationWorkItem, NodeRun, WorkflowNode
from ..services.workspace import TaskWorkspace
from . import default_workflow as wf
from .models import ImplementationResult
from .states import ACTIVE_STATES, ExecutionMode, NodeKind, NodeState, Role
from .work_plans import WorkPlan, owns_path

MAX_WORKERS = 3


def work_items(session, task):
    return list(session.scalars(select(ImplementationWorkItem).where(
        ImplementationWorkItem.task_id == task.id)).all())


def work_item(session, node):
    return session.scalar(select(ImplementationWorkItem).where(
        ImplementationWorkItem.node_id == node.id))


def node_for(task, item):
    return next(node for node in task.nodes if node.id == item.node_id)


def completed_results(session, task, items):
    results = []
    remaining = list(items)
    done = set()
    while remaining:
        ready = [item for item in remaining if set(item.dependencies) <= done]
        if not ready:
            raise ValueError("Stored worker dependencies are invalid")
        for item in ready:
            node = node_for(task, item)
            run = session.scalar(select(NodeRun).where(NodeRun.node_id == node.id)
                                 .order_by(NodeRun.id.desc()))
            results.append({"key": item.plan_key, "sha": node.checkpoint_sha,
                            "summary": (run.result or {}).get("summary", "") if run else "",
                            "tests": (run.result or {}).get("tests") if run else None,
                            "knownIssues": (run.result or {}).get("knownIssues", []) if run else [],
                            "acceptanceCriteria": item.acceptance_criteria})
            remaining.remove(item)
            done.add(item.plan_key)
    return results


async def accept_plan(engine, session, task, node, run, raw):
    # flowctl adds "status": "completed" for every role but the Orchestrator, and the Architect
    # plans new tasks itself; the plan has no status of its own.
    if raw.get("status") == "completed":
        raw = {key: value for key, value in raw.items() if key != "status"}
    plan = WorkPlan.model_validate(raw)
    if work_items(session, task):
        raise ValueError("A plan already exists for this task")
    if await engine.git.is_dirty(task.worktree_path):
        raise ValueError("Planning changed the task worktree; inspect it before retrying")
    base = await engine.git.head_sha(task.worktree_path)
    for entry in plan.subtasks:
        uid = str(uuid4())
        child = WorkflowNode(task_id=task.id, key=f"worker_{uid}", kind=NodeKind.AGENT,
                             label=entry.title, role=Role.IMPLEMENTER, state=NodeState.PENDING,
                             execution_mode=ExecutionMode.AUTOMATIC)
        task.nodes.append(child)
        session.flush()
        session.add(ImplementationWorkItem(
            uid=uid, task_id=task.id, node_id=child.id, plan_key=entry.key,
            brief=entry.brief, owned_paths=entry.owned_paths,
            acceptance_criteria=entry.acceptance_criteria, dependencies=entry.dependencies,
            worktree_path=str(engine.config.worktree_dir / f"{task.jira_key}-worker-{uid}"),
            branch_name=f"agentflow-worker/{uid}", base_sha=base,
        ))
    run.result = plan.model_dump(by_alias=True)
    engine._finish_run(run, NodeState.COMPLETED)
    engine._set_state(session, task, node, NodeState.COMPLETED, plan.summary)
    # Persist the plan before launching external processes. Restart recovery never replans it.
    session.commit()
    await dispatch(engine, session, task)


def check_capacity(session, task, node):
    occupied = {other.id for other in task.nodes if other.key.startswith("worker_")
                and other.id != node.id
                and other.state in {*ACTIVE_STATES, NodeState.INTERRUPTED}}
    live_nodes = session.scalars(select(NodeRun.node_id).join(
        AgentSession, AgentSession.current_run_id == NodeRun.id).where(
            AgentSession.task_id == task.id, AgentSession.status == "running")).all()
    occupied.update(other.id for other in task.nodes if other.key.startswith("worker_")
                    and other.id != node.id and other.id in live_nodes)
    if len(occupied) >= MAX_WORKERS:
        raise ValueError("All three worker slots are occupied; wait for a worker to finish")


async def prepare(engine, session, task, node, item):
    check_capacity(session, task, node)
    items = work_items(session, task)
    dependencies = [other for other in items if other.plan_key in item.dependencies]
    if any(node_for(task, other).state != NodeState.COMPLETED for other in dependencies):
        raise ValueError("Worker dependencies have not completed")
    path = Path(item.worktree_path)
    if path.exists():
        await engine.git.verify_worker_worktree(task.repository_root, path, item.branch_name)
        if item.prepared:
            return
        if await engine.git.is_dirty(path):
            raise ValueError("Incomplete dependency preparation has local changes; inspect before retrying")
    else:
        await engine.git.create_worker_worktree(task.repository_root, path, item.branch_name, item.base_sha)
    for dependency in dependencies:
        await engine.git.merge_worker_checkpoint(path, node_for(task, dependency).checkpoint_sha)
    item.base_sha = await engine.git.head_sha(path)
    item.prepared = True
    session.commit()


async def dispatch(engine, session, task):
    items = work_items(session, task)
    if not items:
        return
    for item in items:
        node = node_for(task, item)
        if node.state != NodeState.PENDING:
            continue
        dependencies = [other for other in items if other.plan_key in item.dependencies]
        if any(node_for(task, other).state != NodeState.COMPLETED for other in dependencies):
            continue
        try:
            check_capacity(session, task, node)
        except ValueError:
            break
        engine._set_state(session, task, node, NodeState.READY)
        await engine._on_ready(session, task, node)
        session.commit()
    if all(node_for(task, item).state == NodeState.COMPLETED for item in items):
        integration = engine._node(task, wf.IMPLEMENTER)
        if integration.state == NodeState.PENDING:
            engine._set_state(session, task, integration, NodeState.READY,
                              "All workers completed; integrate checkpoints and run combined tests")
            await engine._on_ready(session, task, integration)


def recover_pending(engine, session, task):
    """Make a committed completion-to-dispatch gap recoverable without auto-launching."""
    items = work_items(session, task)
    if not items:
        return
    completed = {item.plan_key for item in items if node_for(task, item).state == NodeState.COMPLETED}
    for item in items:
        node = node_for(task, item)
        if node.state == NodeState.PENDING and set(item.dependencies) <= completed:
            engine._set_state(session, task, node, NodeState.READY, "Backend restarted; start this worker explicitly")
    integration = engine._node(task, wf.IMPLEMENTER)
    if len(completed) == len(items) and integration.state == NodeState.PENDING:
        engine._set_state(session, task, integration, NodeState.READY, "Workers complete; start integration explicitly")


async def complete(engine, session, task, node, run, raw):
    result = ImplementationResult.model_validate(raw)
    item = work_item(session, node)
    if result.status != "completed":
        engine._finish_run(run, NodeState.FAILED, result.summary)
        engine._set_state(session, task, node, NodeState.FAILED, result.summary)
        return
    await engine.git.verify_worker_worktree(task.repository_root, Path(item.worktree_path), item.branch_name)
    sha, _ = await engine.git.checkpoint(item.worktree_path, f"AgentFlow worker {item.uid}")
    paths = await engine.git.changed_paths(item.worktree_path, item.base_sha, sha)
    outside = [path for path in paths if not any(owns_path(owner, path) for owner in item.owned_paths)]
    if outside:
        raise ValueError("Worker changed files outside its ownership: " + ", ".join(outside))
    # Close only this worker's tracked pane before releasing its slot or integrating its output.
    # Not gracefully: the worker is still inside this `flowctl complete` call, so it can't exit.
    await engine._stop_sessions(session, task, node.role, node=node, graceful=False)
    node.checkpoint_sha = sha
    run.result = {**result.model_dump(by_alias=True), "checkpointSha": sha}
    if run.run_dir:
        TaskWorkspace.write_json(Path(run.run_dir) / "result.json", run.result)
    engine._finish_run(run, NodeState.COMPLETED)
    engine._set_state(session, task, node, NodeState.COMPLETED, result.summary)
    session.commit()
    await dispatch(engine, session, task)
