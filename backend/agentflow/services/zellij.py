"""Zellij session / tab / pane control.

All calls target a *named* session by setting ZELLIJ_SESSION_NAME in the child environment,
after scrubbing any inherited zellij variables (see process.clean_env).
"""

from __future__ import annotations

import asyncio
import json
import re
import shlex
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import AsyncIterator

from ..config import ZellijConfig
from .. import diagnostics
from .process import (ProcessError, clean_env, resolve_executable, run, spawn_detached, spawn_hidden_console,
                      terminate_process_tree)


class ZellijError(RuntimeError):
    pass


# Interpolated into a `cmd /c` line for the hidden client, so kept to characters cmd leaves alone.
_SAFE_SESSION_NAME = re.compile(r"[A-Za-z0-9._-]+")

# Width of a background session that has no client attached.
_CLIENTLESS_COLUMNS = 50

# `subscribe --format json`, `write-chars --pane-id`, `list-panes --json`, `attach --create-background`.
MIN_VERSION = (0, 44, 0)

# Session states from `list-sessions` (see ZellijService._session_states).
RUNNING = "running"
EXITED = "exited"


def version_supported(version: str | None) -> bool:
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", version or "")
    return match is not None and tuple(map(int, match.groups())) >= MIN_VERSION


@dataclass
class PaneInfo:
    pane_id: str  # terminal_<n>
    tab_id: int
    tab_name: str
    exited: bool
    exit_status: int | None
    command: str | None


class ZellijService:
    def __init__(self, config: ZellijConfig) -> None:
        self.config = config
        # session name -> the hidden `zellij attach` console keeping it sized (Windows)
        self._sizing_clients: dict[str, asyncio.subprocess.Process] = {}

    # detection ---------------------------------------------------------------------
    def executable(self) -> str | None:
        return resolve_executable(self.config.command)

    async def version(self) -> str | None:
        exe = self.executable()
        if not exe:
            return None
        try:
            return (await run([exe, "--version"], timeout=15)).stdout.strip()
        except ProcessError:
            return None

    async def require_subscribe_support(self) -> None:
        version = await self.version()
        if not version_supported(version):
            found = version or "unavailable"
            raise ZellijError(f"The embedded terminal requires Zellij 0.44 or newer (found {found})")

    def shell_command(self) -> list[str]:
        """Shell used to run generated launcher scripts inside tabs."""
        if self.config.shell:
            return [self.config.shell]
        if sys.platform == "win32":
            for candidate in ("pwsh", "powershell"):
                if resolve_executable(candidate):
                    return [candidate]
            return ["powershell"]
        return ["sh"]

    # low level ---------------------------------------------------------------------
    async def _zellij(self, *args: str, session: str | None = None, timeout: float = 30.0):
        exe = self.executable()
        if not exe:
            raise ZellijError("zellij executable not found (configure zellij.command)")
        env = {"ZELLIJ_SESSION_NAME": session} if session else None
        return await run([exe, *args], env=env, timeout=timeout)

    async def _session_states(self) -> dict[str, str]:
        """Every session `list-sessions` knows, as "running" or "exited".

        An exited session ("EXITED - attach to resurrect") is only saved layout: no server
        is running it. It must never count as running, because `zellij action` against it
        (with ZELLIJ_SESSION_NAME) hangs until our timeout rather than failing."""
        result = await self._zellij("list-sessions", "--no-formatting")
        if not result.ok:
            detail = (result.stderr or result.stdout).strip()
            if "no active zellij sessions" in detail.lower() or "no active sessions" in detail.lower():
                return {}
            raise ZellijError(f"Could not inspect Zellij sessions: {detail}")
        states = {}
        for line in result.stdout.splitlines():
            line = line.strip()
            if line:
                states[line.split()[0]] = EXITED if "(EXITED" in line.upper() else RUNNING
        return states

    async def list_sessions(self) -> list[str]:
        """Running sessions only (see _session_states)."""
        return [name for name, state in (await self._session_states()).items() if state == RUNNING]

    async def session_state(self, name: str) -> str | None:
        """RUNNING, EXITED, or None when Zellij doesn't know the session at all."""
        return (await self._session_states()).get(name)

    async def session_exists(self, name: str) -> bool:
        """Whether the session is running, i.e. its tabs and panes can be queried."""
        return await self.session_state(name) == RUNNING

    async def _delete_session(self, name: str) -> None:
        result = await self._zellij("delete-session", name, "--force")
        if not result.ok:
            detail = (result.stderr or result.stdout).strip()
            if "no session" not in detail.lower() and "not found" not in detail.lower():
                raise ZellijError(f"Could not delete Zellij session {name}: {detail}")

    async def ensure_session(self, name: str) -> bool:
        diagnostics.event('zellij.session.requested', session=name)
        try:
            created = await self._ensure_session(name)
        except Exception:
            diagnostics.log.exception('zellij.session.failed', extra={'details': {'session': name}})
            raise
        diagnostics.event('zellij.session.ready', session=name, created=created)
        return created

    async def _ensure_session(self, name: str) -> bool:
        """Create a session if needed. Returns True when newly created."""
        state = await self.session_state(name)
        if state == RUNNING:
            return False
        if state == EXITED:
            # Start fresh rather than resurrect: a resurrected layout brings back the old
            # agent tabs under their names, which untracked-tab matching could take for live
            # agents. AgentFlow relaunches or resumes its agents itself.
            diagnostics.event('zellij.session.exited_replaced', session=name)
            await self._delete_session(name)
        exe = self.executable()
        if not exe:
            raise ZellijError("zellij executable not found (configure zellij.command)")
        try:
            if sys.platform == "win32":
                # Headless: see spawn_hidden_console for why this can't use spawn_detached.
                process = await spawn_hidden_console([exe, "attach", "--create-background", name])
            else:
                # A captured run can hang because the background server inherits its
                # pipes, so disconnect stdio and poll for the session instead.
                process = await spawn_detached(
                    [exe, "attach", "--create-background", name], detach_from_console=False
                )
        except OSError as exc:
            raise ZellijError(f"Could not start Zellij session {name}: {exc}") from exc
        for _ in range(40):
            await asyncio.sleep(0.25)
            if await self.session_exists(name):
                if sys.platform == "win32":
                    await self._size_new_session(exe, name)
                return True
            exit_code = process.returncode
            if exit_code not in (None, 0):
                diagnostics.event('zellij.launch.exited', session=name, exitCode=exit_code)
                raise ZellijError(
                    f"Zellij exited with code {exit_code} before creating session {name}"
                )
        diagnostics.event('zellij.launch.timeout', session=name, exitCode=process.returncode, timeoutSeconds=10)
        if process.returncode == 0:
            raise ZellijError(
                f"Zellij exited successfully, but session {name} did not appear within 10s"
            )
        raise ZellijError(f"zellij session {name} did not appear within 10s")

    async def _size_new_session(self, exe: str, name: str) -> None:
        """Attach the sizing client to a just-created session, confirmed by geometry.

        `list-clients` can't confirm it here: a new background session lists a phantom
        client (the launcher's own) for an unbounded time, seconds at least, which looks
        exactly like a real one and sizes nothing. Instead, wait for the viewport to grow past
        the 50 columns a session has with no client. That is the client's actual window width,
        which can be narrower than headlessCols: a console window can't be wider than the
        screen, and a disconnected or locked Windows session has a small one (about 118
        columns), so waiting for the full headlessCols would never succeed there.
        On failure the half-made session is killed so a retry starts clean
        rather than trusting whatever client the next check happens to see.
        """
        try:
            await self._start_sizing_client(exe, name, attached=self._viewport_sized)
        except ZellijError:
            try:
                await self.kill_session(name)
            except ZellijError:
                diagnostics.log.exception('zellij.session.cleanup_failed', extra={'details': {'session': name}})
            raise

    async def _viewport_sized(self, session: str) -> bool:
        result = await self._zellij("action", "list-panes", "--json", "--all", session=session)
        try:
            panes = json.loads(result.stdout or "[]") if result.ok else []
        except json.JSONDecodeError:
            return False
        return max((int(p.get("pane_columns") or 0) for p in panes), default=0) > _CLIENTLESS_COLUMNS

    async def _client_listed(self, session: str) -> bool:
        return await self.client_count(session) > 0

    async def client_count(self, session: str) -> int:
        result = await self._zellij("action", "list-clients", session=session)
        if not result.ok:
            return 0
        lines = [line for line in result.stdout.splitlines() if line.strip()]
        return max(0, len(lines) - 1)  # first line is the CLIENT_ID header

    async def ensure_client_attached(self, session: str) -> None:
        """Make sure a client is sizing `session` before adding a pane (Windows only).

        Zellij lays a new tab out against an attached client's viewport. With no client it
        still spawns the tab's command, but fails the layout ("Not enough room for panes"):
        the tab vanishes and its process keeps running invisibly until the whole session is
        killed. So a client must be attached before every new_tab, and failing to confirm
        one is an error, never something to retry past (a retry would run the command twice).

        The client is a hidden console running `zellij attach`, one per session, started by
        `_ensure_session` and tracked here. `list-clients` isn't trusted for a tracked client
        (it has missed a working hidden client), only its process's liveness. Without a
        tracked client (e.g. after a backend restart) any listed client counts: the previous
        run's hidden client, or a window the human opened.
        """
        if sys.platform != "win32":
            return
        client = self._sizing_clients.get(session)
        if client is not None and client.returncode is None:
            return
        if client is None and await self.client_count(session) > 0:
            return
        exe = self.executable()
        if not exe:
            raise ZellijError("zellij executable not found (configure zellij.command)")
        # No phantom client here (that only haunts a just-created session), so a listed
        # client is good enough to confirm the replacement.
        await self._start_sizing_client(exe, session, attached=self._client_listed)

    async def _start_sizing_client(self, exe: str, session: str, *, attached) -> None:
        """Start the hidden, sized `zellij attach` console for `session` and wait until `attached`."""
        if not _SAFE_SESSION_NAME.fullmatch(session):
            raise ZellijError(f"Unsupported Zellij session name: {session!r}")
        restarted = session in self._sizing_clients
        diagnostics.event('zellij.client.starting', session=session, restarted=restarted)
        cols, rows = self.config.headless_cols, self.config.headless_rows
        try:
            client = await spawn_hidden_console(f'mode con: cols={cols} lines={rows} & "{exe}" attach {session}')
        except OSError as exc:
            raise ZellijError(f"Could not start a hidden Zellij client for {session}: {exc}") from exc
        self._sizing_clients[session] = client
        for _ in range(40):
            await asyncio.sleep(0.25)
            if client.returncode is not None:
                raise ZellijError(f"Hidden Zellij client for {session} exited with code {client.returncode}")
            if await attached(session):
                return
        await terminate_process_tree(client)
        raise ZellijError(f"Hidden Zellij client did not attach to session {session} within 10s")

    async def open_window(self, session: str, tab_name: str | None = None) -> None:
        """Open a visible, interactive window attached to `session`, on `tab_name` (Windows only).

        A newly attached client joins whichever tab the existing (hidden) client is on, so
        focusing the tab first is what lands the window on it. `/max` keeps the window no
        smaller than the hidden client: a tab is laid out at the smallest viewing client's
        size and doesn't grow back once that client leaves.

        Deliberately not Windows Terminal: `wt` re-tokenizes its command line internally and
        has been observed mangling an exe path with a space in it (ERROR_BAD_EXE_FORMAT).
        Plain `cmd /c start` is a single, well-understood parsing layer. breakaway_from_job
        so the window survives closing whatever terminal hosts the backend.
        """
        if sys.platform != "win32":
            raise ZellijError(f"Opening a terminal window is only supported on Windows; run: {self.attach_command(session)}")
        exe = self.executable()
        if not exe:
            raise ZellijError("zellij executable not found (configure zellij.command)")
        if tab_name:
            await self.focus_tab(session, tab_name)
        try:
            await spawn_detached(["cmd", "/c", "start", "/max", "", exe, "attach", session], breakaway_from_job=True)
        except OSError as exc:
            raise ZellijError(f"Could not open a terminal window for {session}: {exc}") from exc

    async def list_panes(self, session: str) -> list[PaneInfo]:
        result = await self._zellij("action", "list-panes", "--json", "--all", session=session)
        if not result.ok:
            raise ZellijError(f"list-panes failed for {session}: {(result.stderr or result.stdout).strip()}")
        panes = []
        try:
            raw = json.loads(result.stdout or "[]")
        except json.JSONDecodeError as exc:
            raise ZellijError(f"list-panes returned invalid JSON: {exc}") from exc
        for item in raw:
            if item.get("is_plugin"):
                continue
            panes.append(
                PaneInfo(
                    pane_id=f"terminal_{item['id']}",
                    tab_id=int(item.get("tab_id", -1)),
                    tab_name=item.get("tab_name", ""),
                    exited=bool(item.get("exited")),
                    exit_status=item.get("exit_status"),
                    command=item.get("pane_command") or item.get("terminal_command"),
                )
            )
        return panes

    async def new_tab(self, session: str, name: str, cwd: Path, command: list[str] | None = None) -> tuple[int, str | None]:
        """Create a tab, optionally running a command. Returns (tab_id, pane_id)."""
        await self.ensure_client_attached(session)
        args = ["action", "new-tab", "--name", name, "--cwd", str(cwd)]
        if command:
            args += ["--", *command]
        result = await self._zellij(*args, session=session)
        if not result.ok:
            raise ZellijError(f"new-tab failed: {(result.stderr or result.stdout).strip()}")
        tab_id = -1
        out = (result.stdout or "").strip().splitlines()
        if out and out[-1].strip().isdigit():
            tab_id = int(out[-1].strip())
        # The pane (and sometimes the tab id itself) shows up slightly after new-tab returns.
        pane_id = None
        for _ in range(12):
            if tab_id < 0:
                tab_id = await self.tab_id_by_name(session, name)
            if tab_id >= 0:
                pane_id = await self.pane_for_tab(session, tab_id)
                if pane_id:
                    break
            await asyncio.sleep(0.25)
        if pane_id is None:
            # Usually the layout failed for want of a sizing client (see ensure_client_attached).
            diagnostics.event('zellij.tab.paneless', session=session, tab=name, tabId=tab_id)
        return tab_id, pane_id

    async def tab_id_by_name(self, session: str, name: str) -> int:
        """Highest tab id carrying `name` (tabs are created in id order), or -1."""
        result = await self._zellij("action", "list-tabs", session=session)
        best = -1
        if result.ok:
            for line in result.stdout.splitlines()[1:]:
                parts = line.split(None, 2)
                if len(parts) == 3 and parts[0].isdigit() and parts[2].strip() == name:
                    best = max(best, int(parts[0]))
        return best

    async def pane_for_tab(self, session: str, tab_id: int) -> str | None:
        try:
            for pane in await self.list_panes(session):
                if pane.tab_id == tab_id:
                    return pane.pane_id
        except ZellijError:
            return None
        return None

    async def pane_state(self, session: str, pane_id: str) -> PaneInfo | None:
        for pane in await self.list_panes(session):
            if pane.pane_id == pane_id:
                return pane
        return None

    async def screen(self, session: str, pane_id: str) -> list[str]:
        """The pane's current viewport, one string per row."""
        result = await self._zellij("action", "dump-screen", "--pane-id", pane_id, session=session)
        if not result.ok:
            raise ZellijError(f"dump-screen failed: {(result.stderr or result.stdout).strip()}")
        return (result.stdout or "").splitlines()

    async def write_line(self, session: str, pane_id: str, text: str) -> None:
        """Type a single line into a pane and press Enter."""
        result = await self._zellij("action", "write-chars", "--pane-id", pane_id, text, session=session)
        if not result.ok:
            raise ZellijError(f"write-chars failed: {(result.stderr or result.stdout).strip()}")
        result = await self._zellij("action", "write", "--pane-id", pane_id, "13", session=session)
        if not result.ok:
            raise ZellijError(f"write failed: {(result.stderr or result.stdout).strip()}")

    async def subscribe(self, session: str, pane_id: str) -> AsyncIterator[bytes]:
        """Stream the rendered output of one tracked pane as Zellij NDJSON."""
        exe = self.executable()
        if not exe:
            yield (json.dumps({"event": "error", "message": "zellij executable not found"}) + "\n").encode()
            return
        process = None
        try:
            process = await asyncio.create_subprocess_exec(
                exe,
                "--session",
                session,
                "subscribe",
                "--pane-id",
                pane_id,
                "--scrollback",
                "100",
                "--format",
                "json",
                env=clean_env(),
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL,
            )
            assert process.stdout is not None
            while True:
                line = await process.stdout.readline()
                if not line:
                    break
                yield line
            return_code = await process.wait()
            if return_code:
                message = f"Zellij pane stream exited with code {return_code}"
                yield (json.dumps({"event": "error", "message": message}) + "\n").encode()
        except OSError as exc:
            message = f"Could not start Zellij pane stream: {exc}"
            yield (json.dumps({"event": "error", "message": message}) + "\n").encode()
        finally:
            if process is not None and process.returncode is None:
                try:
                    process.terminate()
                except ProcessLookupError:
                    pass
                try:
                    await asyncio.wait_for(process.wait(), timeout=2)
                except asyncio.TimeoutError:
                    process.kill()
                    await process.wait()

    @staticmethod
    def parse_pane_update(line: bytes) -> list[str] | None:
        """Rendered viewport rows from one `subscribe --format json` NDJSON line, or None
        for anything else on that stream (a non-pane_update event, or the synthetic
        {"event": "error", ...} lines `subscribe` itself yields on failure)."""
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            return None
        if not isinstance(event, dict) or event.get("event") != "pane_update":
            return None
        viewport = event.get("viewport")
        return viewport if isinstance(viewport, list) else None

    async def focus_tab(self, session: str, tab_name: str) -> None:
        await self._zellij("action", "go-to-tab-name", tab_name, session=session)

    async def kill_session(self, name: str) -> None:
        """Kill and delete a whole session, verifying it's actually gone.

        `_stop_sessions` only ever closes tabs it tracks in the database (agent tabs), so
        any other pane in the task's session - e.g. the "Tests"/"Shell" convenience tabs
        `create_task` opens with a cwd inside the worktree - keeps holding that directory
        open. On Windows that blocks `git worktree remove` with "Permission denied" no
        matter how many agent tabs got closed first. Killing the whole session (instead of
        hunting down every non-agent pane individually) is the only way to be sure nothing
        is left with its cwd inside the worktree before it's removed.
        """
        state = await self.session_state(name)
        if state is None:
            return
        if state == RUNNING:
            result = await self._zellij("kill-session", name)
            if not result.ok:
                detail = (result.stderr or result.stdout).strip()
                if "no session" not in detail.lower() and "not found" not in detail.lower():
                    raise ZellijError(f"Could not kill Zellij session {name}: {detail}")
        await self._delete_session(name)  # also clears an exited session's saved layout
        for _ in range(40):
            if await self.session_state(name) is None:
                await self._stop_sizing_client(name)
                return
            await asyncio.sleep(0.25)
        raise ZellijError(f"Zellij session {name} still exists after kill+delete")

    async def _stop_sizing_client(self, session: str) -> None:
        """The hidden client exits on its own once its session is gone; make sure of it."""
        client = self._sizing_clients.pop(session, None)
        if client is not None and client.returncode is None:
            try:
                await asyncio.wait_for(client.wait(), timeout=2)
            except asyncio.TimeoutError:
                await terminate_process_tree(client)

    async def close_tab(self, session: str, tab_id: int) -> None:
        if tab_id < 0:
            raise ZellijError("Cannot stop an agent without a known tab ID")
        result = await self._zellij("action", "close-tab-by-id", str(tab_id), session=session)
        if not result.ok:
            raise ZellijError(f"Could not stop agent tab {tab_id}: {result.stderr or result.stdout}")
        for _ in range(20):
            if not await self.session_exists(session):
                return
            if not any(p.tab_id == tab_id for p in await self.list_panes(session)):
                return
            await asyncio.sleep(0.1)
        raise ZellijError(f"Agent tab {tab_id} did not stop")

    # attaching ----------------------------------------------------------------------
    def attach_command(self, session: str) -> str:
        return f"{self.config.command} attach {shlex.quote(session)}"

    def web_url(self, session: str) -> str | None:
        if not self.config.web_client_enabled:
            return None
        return f"{self.config.web_client_base_url.rstrip('/')}/{session}"
