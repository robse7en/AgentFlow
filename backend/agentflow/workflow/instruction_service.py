"""Operator instruction operations, serialized with the engine's existing lifecycle."""
from pathlib import Path
from pydantic import BaseModel, ConfigDict, Field
from sqlalchemy import select
from ..database.models import NodeRun, Project
from . import instructions as guidance, workers
from .engine import WorkflowError
from .states import ROLE_RUN_DIRS

class SourceSave(BaseModel):
    model_config = ConfigDict(extra='forbid', strict=True)
    content: str
    expectedHash: str | None = Field(...)


class ApplyUpdate(BaseModel):
    model_config = ConfigDict(extra='forbid', strict=True)
    expectedVersion: str
    expectedSourceHashes: dict[str, str | None]
    freshConversations: bool


def project_root(session, project_id):
    project = session.get(Project, project_id)
    if project is None:
        raise WorkflowError('Project not found', 404)
    if not project.default_repository_path:
        raise WorkflowError('Choose a target repository in project defaults first', 409)
    return Path(project.default_repository_path)


async def project_instructions(engine, project_id: int):
    async with engine._projects_lock:
        with engine.db.session() as session:
            root = project_root(session, project_id)
            return {'repository': str(root), 'files': guidance.read_set(root),
                    'revision': await engine.git.head_sha(root),
                    'dirtyPaths': await engine.git.dirty_paths(root)}


async def save_project_source(engine, project_id: int, role: str, req: SourceSave):
    async with engine._projects_lock:
        with engine.db.session() as session:
            return guidance.save_source(project_root(session, project_id), role,
                                        req.content, req.expectedHash)


async def task_instructions(engine, jira_key: str, nodeKey: str, runId: int | None, preview: str):
    async with engine._lock(jira_key), engine._projects_lock:
        with engine.db.session() as session:
            task = engine._load_task(session, jira_key)
            workspace = engine.workspace(task)
            runs = session.scalars(select(NodeRun).where(NodeRun.task_id == task.id).order_by(NodeRun.id.desc())).all()
            # Historical inspection remains available even if the worktree was removed.
            if runId is not None:
                run = next((r for r in runs if r.id == runId), None)
                if run is None:
                    raise WorkflowError('Run not found', 404)
                metadata = Path(run.run_dir) / 'instructions.json' if run.run_dir else None
                return {'kind': 'historical', 'runId': run.id,
                        'metadata': guidance.read_json(metadata) if metadata and metadata.exists() else None,
                        'briefing': Path(run.prompt_path).read_bytes().decode('utf-8') if run.prompt_path else ''}
            root = Path(task.worktree_path)
            node = engine._node(task, nodeKey)
            snapshot = guidance.active_snapshot(workspace.root)
            guidance.role_path(node.role)
            source_error = None
            try:
                files = guidance.read_set(root)
            except guidance.InstructionError as exc:
                files, source_error = None, str(exc)
            effective = snapshot if preview == 'snapshot' and snapshot else {'files': files}
            briefing = None
            if effective['files'] is not None:
                # 0 is explicitly a placeholder; no run/artifact is created by preview.
                run = NodeRun(id=0, iteration=task.review_iteration)
                role_count = sum(r.node.role == node.role for r in runs)
                run_dir = workspace.runs_dir / ROLE_RUN_DIRS[node.role] / f'{role_count + 1:02d}'
                builtin = engine._build_prompt(session, task, node, run, run_dir)
                briefing = guidance.compose(builtin, effective, node.role, 0)
            return {'kind': 'preview', 'repository': str(root), 'files': files,
                    'sourceError': source_error, 'snapshot': snapshot, 'briefing': briefing,
                    'dirtyPaths': await engine.git.dirty_paths(root) if files is not None else [],
                    'differs': files != snapshot['files'] if snapshot and files is not None else None,
                    'runs': [{'id': r.id, 'nodeKey': r.node.key} for r in runs]}


async def save_task_source(engine, jira_key: str, role: str, req: SourceSave):
    async with engine._lock(jira_key):
        async with engine._projects_lock:
            with engine.db.session() as session:
                task = engine._load_task(session, jira_key)
                return guidance.save_source(Path(task.worktree_path), role, req.content, req.expectedHash)


async def apply_update(engine, jira_key: str, req: ApplyUpdate):
    async with engine._lock(jira_key), engine._projects_lock:
        with engine.db.session() as session:
            task = engine._load_task(session, jira_key)
            directory = engine.workspace(task).root
            previous = guidance.active_snapshot(directory)
            if previous is None or previous['version'] != req.expectedVersion:
                raise WorkflowError('Instruction version changed; reload before applying', 409)
            # A batch uses one version including workers that have not launched yet.
            if workers.work_items(session, task) and engine._node(task, 'implementer').state != 'COMPLETED':
                raise WorkflowError('Finish the parallel batch and integration before updating instructions', 409)
            files = guidance.read_set(Path(task.worktree_path))
            if {r: f['hash'] for r, f in files.items()} != req.expectedSourceHashes:
                raise WorkflowError('Instruction sources changed; reload before applying', 409)
            current = guidance.capture(directory, Path(task.worktree_path),
                                       await engine.git.head_sha(task.worktree_path),
                                       fresh=req.freshConversations, files=files)
            engine._log(session, task, 'Human applied repository instruction update',
                        data={'previousVersion': previous['version'], 'version': current['version'],
                              'freshConversations': req.freshConversations})
            return current
