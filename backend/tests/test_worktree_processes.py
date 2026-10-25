"""Real processes: stop_processes_in kills only what works inside the given directory."""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

from agentflow.services.worktree_processes import stop_processes_in

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="only Windows locks a process's working directory")
# The base interpreter, not a venv's python.exe launcher: that runs the real interpreter as a
# child process (and moves its own working directory elsewhere), so its pid isn't the one killed.
SLEEPER = [getattr(sys, "_base_executable", sys.executable), "-c", "import time; time.sleep(60)"]


@pytest.fixture
def folders():
    workspace = Path(__file__).resolve().parents[2]
    root = Path(tempfile.mkdtemp(prefix=".worktree-processes-", dir=workspace))
    inside, outside = root / "DE-1" / "nested", root / "DE-10"
    inside.mkdir(parents=True)
    outside.mkdir()
    yield root / "DE-1", inside, outside
    shutil.rmtree(root, ignore_errors=True)


@pytest.mark.asyncio
async def test_kills_processes_working_inside_the_directory_only(folders):
    worktree, inside, outside = folders
    held = subprocess.Popen(SLEEPER, cwd=inside)
    neighbour = subprocess.Popen(SLEEPER, cwd=outside)  # a sibling whose name shares the prefix
    try:
        stopped = await stop_processes_in(worktree)
        assert [proc.pid for proc in stopped] == [held.pid]
        assert Path(stopped[0].cwd).resolve() == inside.resolve()
        assert held.wait(timeout=5) is not None
        assert neighbour.poll() is None
        shutil.rmtree(worktree)  # nothing holds it any more
    finally:
        for proc in (held, neighbour):
            proc.kill()
            proc.wait()


@pytest.mark.asyncio
async def test_finds_nothing_in_an_unused_directory(folders):
    worktree, _, _ = folders
    assert await stop_processes_in(worktree) == []
