"""Thin asyncio subprocess helpers (prefer std primitives over wrapper libraries)."""

from __future__ import annotations

import asyncio
import os
import re
import shutil
import subprocess
import sys
import signal
from dataclasses import dataclass
from pathlib import Path


@dataclass
class CommandResult:
    argv: list[str]
    returncode: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.returncode == 0

    def check(self, what: str | None = None) -> "CommandResult":
        if not self.ok:
            detail = (self.stderr or self.stdout).strip()
            raise ProcessError(f"{what or self.argv[0]} failed (exit {self.returncode}): {detail}")
        return self


class ProcessError(RuntimeError):
    pass


# Environment variables that must never leak from the backend into child processes:
# a backend started inside a zellij pane would otherwise target the wrong session.
_ZELLIJ_VARS = ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "ZELLIJ_SERVER_MODE")
SAFE_ENV_NAMES = frozenset({"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "TEMP", "TMP",
    "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)",
    "PROGRAMDATA", "LANG", "LC_ALL", "TERM", "COLORTERM", "USERNAME", "USER", "LOGNAME",
    "HOMEDRIVE", "HOMEPATH",
    "COPILOT_API_HOME"})  # lets the Claude usage collector find a relocated Copilot-proxy ledger


def clean_env(extra: dict[str, str] | None = None) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k.upper() in SAFE_ENV_NAMES}
    if extra:
        env.update(extra)
    return env


def process_group_options() -> dict:
    return {"creationflags": 0x00000200} if sys.platform == "win32" else {"start_new_session": True}


async def terminate_process_tree(process: asyncio.subprocess.Process) -> None:
    """Stop a process group created for our own verifier/collector."""
    if process.returncode is not None:
        return
    if sys.platform == "win32":
        killer = await asyncio.create_subprocess_exec(
            "taskkill", "/PID", str(process.pid), "/T", "/F",
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL,
        )
        try:
            await asyncio.wait_for(killer.wait(), 10)
        except asyncio.TimeoutError:
            killer.kill()
            await killer.wait()
        if process.returncode is None:
            try:
                process.kill()
            except ProcessLookupError:
                pass
    else:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    await process.wait()


def resolve_executable(command: str) -> str | None:
    """Locate an executable by absolute path or via PATH (Windows aware)."""
    expanded = os.path.expanduser(os.path.expandvars(command))
    path = Path(expanded)
    if path.is_absolute() or os.sep in expanded or "/" in expanded:
        if path.is_file():
            return str(path)
        if sys.platform == "win32":
            for ext in (".exe", ".cmd", ".bat", ".ps1"):
                candidate = path.with_suffix(ext)
                if candidate.is_file():
                    return str(candidate)
        return None
    return shutil.which(expanded)


def _describe(argv: list[str]) -> str:
    """The executable plus its leading subcommand words (e.g. `zellij action new-tab`).

    Stops at the first flag or value so arbitrary arguments never reach error messages."""
    words = [argv[0]]
    for arg in argv[1:3]:
        if not re.fullmatch(r"[a-z][a-z-]*", arg):
            break
        words.append(arg)
    return " ".join(words)


async def run(
    argv: list[str],
    *,
    cwd: str | Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
    input_text: str | None = None,
) -> CommandResult:
    """Run a command to completion, capturing output."""
    proc = await asyncio.create_subprocess_exec(
        *argv,
        cwd=str(cwd) if cwd else None,
        env=clean_env(env),
        stdin=asyncio.subprocess.PIPE if input_text is not None else asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        out, err = await asyncio.wait_for(
            proc.communicate(input_text.encode() if input_text is not None else None), timeout=timeout
        )
    except asyncio.TimeoutError:
        proc.kill()
        await proc.wait()
        raise ProcessError(f"{_describe(argv)} timed out after {timeout}s")
    return CommandResult(argv, proc.returncode or 0, out.decode(errors="replace"), err.decode(errors="replace"))


async def spawn_detached(
    argv: list[str], *, cwd: str | Path | None = None, env: dict[str, str] | None = None,
    detach_from_console: bool = True, breakaway_from_job: bool = False,
) -> asyncio.subprocess.Process:
    """Start a background process with disconnected stdio and return its handle.

    breakaway_from_job matters for a process meant to outlive its caller: on Windows,
    a child normally stays in whatever job object its parent belongs to (e.g. the
    console/terminal hosting this backend), so closing that terminal can cascade-kill
    children that otherwise look fully detached. CREATE_BREAKAWAY_FROM_JOB asks Windows
    to exempt the child from that job, when the job's own policy allows it.
    """
    kwargs: dict = {}
    if sys.platform == "win32":
        creationflags = 0x00000200  # CREATE_NEW_PROCESS_GROUP
        if detach_from_console:
            creationflags |= 0x00000008  # DETACHED_PROCESS
        if breakaway_from_job:
            creationflags |= 0x01000000  # CREATE_BREAKAWAY_FROM_JOB
        kwargs["creationflags"] = creationflags
    else:
        kwargs["start_new_session"] = True
    return await asyncio.create_subprocess_exec(
        *argv,
        cwd=str(cwd) if cwd else None,
        env=clean_env(env),
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.DEVNULL,
        **kwargs,
    )


async def spawn_hidden_console(command: list[str] | str, *, env: dict[str, str] | None = None) -> asyncio.subprocess.Process:
    """Windows only: start a process in its own new console whose window is never shown.

    Unlike spawn_detached, stdio is deliberately left unredirected so the child uses its
    new console's own handles. A zellij server started with NUL std handles passes them on
    to its first pane's shell, which reads EOF and exits at once; the server then fails to
    reap it ("The parameter is incorrect", os error 87) and dies within a second.

    A string command runs through asyncio's shell mode, i.e. `cmd /c "<command>"`, which is
    what a `mode con ... & ...` chain needs; list2cmdline would re-quote an inner quoted
    path into something cmd can't parse. The process also breaks away from the hosting job
    (see spawn_detached) so it outlives the terminal running the backend.
    """
    startupinfo = subprocess.STARTUPINFO()
    startupinfo.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startupinfo.wShowWindow = 0  # SW_HIDE
    kwargs = {
        "env": clean_env(env),
        "startupinfo": startupinfo,
        # CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB
        "creationflags": 0x00000010 | 0x00000200 | 0x01000000,
    }
    if isinstance(command, str):
        return await asyncio.create_subprocess_shell(command, **kwargs)
    return await asyncio.create_subprocess_exec(*command, **kwargs)
