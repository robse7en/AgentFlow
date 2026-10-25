"""Requirements approval and independently collected verification evidence."""

import asyncio
import hashlib
import json
from pathlib import Path

from sqlalchemy import select

from ..database.models import RequirementsApproval
from ..services.process import clean_env, process_group_options, terminate_process_tree


def documents_digest(documents: dict) -> str:
    return hashlib.sha256(json.dumps(documents, sort_keys=True).encode()).hexdigest()


def approve_requirements(session, task, workspace):
    documents = workspace.requirement_files()
    for name in ("requirements", "acceptanceCriteria", "implementationPlan"):
        if not documents.get(name, "").strip():
            raise ValueError(f"A nonempty {name} document is required")
    approval = session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id))
    if approval:
        raise ValueError("Requirements already approved; create a new task for changed scope")
    approval = RequirementsApproval(task_id=task.id, documents=documents, digest=documents_digest(documents))
    session.add(approval)
    session.flush()
    return approval


def check_requirements(session, task, workspace, documents=None):
    """`documents` lets a caller that already read the requirement files skip a second disk read."""
    approval = session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id))
    if not approval:
        raise ValueError(
            "Requirements need approval under the current policy: this task predates the requirements-approval "
            "gate and has no recorded approval. Open the Requirements tab and click \"Approve requirements\" to "
            "record it, then retry."
        )
    if documents is None:
        documents = workspace.requirement_files()
    if documents_digest(documents) != approval.digest:
        raise ValueError("Approved requirements changed; restore the approved documents or create a new task")
    return approval


_OUTPUT_LIMIT = 16000
_OUTPUT_HEAD = 2000
_TRUNCATED = b"\n[... output truncated: only the start and the end are kept ...]\n"


def _bounded(head: bytearray, tail: bytearray) -> str:
    """At most _OUTPUT_LIMIT bytes, keeping the end: test runners print their summary last."""
    if len(head) < _OUTPUT_LIMIT or head == tail:
        return bytes(head).decode(errors="replace")
    keep = _OUTPUT_LIMIT - _OUTPUT_HEAD - len(_TRUNCATED)
    return (bytes(head[:_OUTPUT_HEAD]) + _TRUNCATED + bytes(tail[-keep:])).decode(errors="replace")


async def verify_commands(commands: list[list[str]], cwd: Path, sha: str) -> dict:
    """Run only human-configured argv, without a shell, and bound retained output."""
    results = []
    for argv in commands:
        if not argv or not all(isinstance(part, str) and part for part in argv):
            raise ValueError("Verification commands must be nonempty argv arrays")
        process = await asyncio.create_subprocess_exec(
            *argv, cwd=str(cwd), env=clean_env(), stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
            **process_group_options(),
        )
        head, tail = bytearray(), bytearray()

        async def drain():
            while chunk := await process.stdout.read(8192):
                head.extend(chunk[:max(0, _OUTPUT_LIMIT - len(head))])
                tail.extend(chunk)
                del tail[:-_OUTPUT_LIMIT]
            return await process.wait()

        try:
            code = await asyncio.wait_for(drain(), timeout=300)
        except (asyncio.TimeoutError, asyncio.CancelledError):
            await terminate_process_tree(process)
            raise
        results.append({"command": argv, "exitCode": code, "output": _bounded(head, tail)})
    return {"sha": sha, "commands": results, "passed": bool(results) and all(r["exitCode"] == 0 for r in results)}
