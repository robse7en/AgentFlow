"""find_holders names what still has a worktree open without stopping it (an editor with the folder open)."""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

from agentflow.services.worktree_processes import find_holders, mentions

windows_only = pytest.mark.skipif(sys.platform != "win32", reason="holders are only looked for on Windows")
# The base interpreter, not a venv's python.exe launcher (see test_worktree_processes.py).
INTERPRETER = getattr(sys, "_base_executable", sys.executable)
SLEEP = "import time; time.sleep(60)"


def test_a_command_line_names_the_directory_or_something_inside_it():
    directory = r"D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400"
    code = r'"C:\Program Files\Microsoft VS Code\Code.exe" "D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400\Acme.Web"'
    assert mentions(code, directory)
    assert mentions(r"node D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400", directory)
    assert mentions("code d:/dev/agentflow/data/.agentflow/worktrees/af-15400/src", directory)
    assert mentions(r"tool \\?\D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400 --x", directory)


def test_a_longer_name_or_another_directory_is_not_a_mention():
    directory = r"D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400"
    assert not mentions(r"code D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-154001", directory)
    assert not mentions(r"code D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15121", directory)
    assert not mentions("", directory)
    assert not mentions("code anything", "")


@pytest.fixture
def folders():
    workspace = Path(__file__).resolve().parents[2]
    root = Path(tempfile.mkdtemp(prefix=".worktree-holders-", dir=workspace))
    worktree, neighbour = root / "DE-1", root / "DE-10"
    (worktree / "nested").mkdir(parents=True)
    neighbour.mkdir()
    yield worktree, neighbour
    shutil.rmtree(root, ignore_errors=True)


@windows_only
@pytest.mark.asyncio
async def test_reports_a_process_started_on_the_folder_without_stopping_it(folders):
    worktree, neighbour = folders
    # What `code <folder>` looks like: the folder is an argument, the working directory is elsewhere.
    editor = subprocess.Popen([INTERPRETER, "-c", SLEEP, str(worktree / "nested")], cwd=neighbour)
    unrelated = subprocess.Popen([INTERPRETER, "-c", SLEEP, str(neighbour)], cwd=neighbour)
    try:
        holders = await find_holders(worktree)
        assert [holder.pid for holder in holders] == [editor.pid]
        assert editor.poll() is None  # reported, not stopped
        assert unrelated.poll() is None
    finally:
        for proc in (editor, unrelated):
            proc.kill()
            proc.wait()


@windows_only
@pytest.mark.asyncio
async def test_reports_a_process_working_inside_the_folder(folders):
    worktree, _ = folders
    shell = subprocess.Popen([INTERPRETER, "-c", SLEEP], cwd=worktree / "nested")
    try:
        assert [holder.pid for holder in await find_holders(worktree)] == [shell.pid]
    finally:
        shell.kill()
        shell.wait()


@windows_only
@pytest.mark.asyncio
async def test_finds_no_holders_of_an_unused_directory(folders):
    worktree, _ = folders
    assert await find_holders(worktree) == []
