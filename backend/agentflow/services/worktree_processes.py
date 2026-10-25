"""Find and stop processes whose working directory is inside a worktree (Windows).

Windows refuses to delete a directory that any process has as its working directory, so a
shell or agent the human started inside a task's worktree (in their own terminal or zellij
session, which AgentFlow doesn't track) blocks cleanup. Nothing exposes another process's
working directory, so this reads it from the process's PEB, as Process Explorer does.
POSIX systems delete such directories without complaint, so there this finds nothing.
"""

from __future__ import annotations

import asyncio
import ctypes
import os
import struct
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class WorktreeProcess:
    pid: int
    name: str
    cwd: str
    command_line: str = ""


async def stop_processes_in(directory: str | Path) -> list[WorktreeProcess]:
    """Kill every process working inside `directory` and return the ones that were killed.

    Never touches this process or its ancestors: killing a shell that launched the backend
    would take the backend down with it."""
    if sys.platform != "win32" or struct.calcsize("P") != 8:
        return []
    return await asyncio.to_thread(_stop_processes_in, Path(directory))


@dataclass(frozen=True)
class WorktreeHolder:
    """A process that probably still has a worktree open: working inside it, or started with it
    on its command line (an editor with the folder open holds it without working there). Only
    reported, never stopped: closing someone's editor is their call."""
    pid: int
    name: str


async def find_holders(directory: str | Path) -> list[WorktreeHolder]:
    """The processes that look like they hold `directory` (best effort: a window opened on the
    folder after the editor started leaves no trace on its command line)."""
    if sys.platform != "win32" or struct.calcsize("P") != 8:
        return []
    return await asyncio.to_thread(_find_holders, Path(directory))


def mentions(command_line: str, directory: str) -> bool:
    """Whether `command_line` names `directory` itself or something inside it, ignoring case,
    slash direction and a `\\\\?\\` prefix. `DE-1` is not found in `DE-12`."""
    def normalized(text: str) -> str:
        return text.replace("/", "\\").lower().replace("\\\\?\\", "")

    line, target = normalized(command_line), normalized(directory).rstrip("\\")
    if not target:
        return False
    start = line.find(target)
    while start != -1:
        end = start + len(target)
        if end == len(line) or line[end] in "\\\"' ":
            return True
        start = line.find(target, start + 1)
    return False


def _find_holders(directory: Path) -> list[WorktreeHolder]:
    target = os.path.normcase(os.path.normpath(str(directory.resolve())))
    snapshot = _snapshot()
    spared = _ancestry(snapshot)
    holders = []
    for pid, (_, name) in snapshot.items():
        if pid <= 4 or pid in spared:
            continue
        found = _inspect(pid, name)
        if found is None:
            continue
        if _within(found.cwd, target) or mentions(found.command_line, target):
            holders.append(WorktreeHolder(pid, name))
    return sorted(holders, key=lambda holder: holder.pid)


def _within(cwd: str, directory: str) -> bool:
    cwd = os.path.normcase(os.path.normpath(cwd))
    return cwd == directory or cwd.startswith(directory.rstrip(os.sep) + os.sep)


def _stop_processes_in(directory: Path) -> list[WorktreeProcess]:
    target = os.path.normcase(os.path.normpath(str(directory.resolve())))
    stopped: list[WorktreeProcess] = []
    # Kills land in arbitrary order, so a parent (an agent CLI) can relaunch a child it just
    # lost (an MCP server) into the same directory; rescan until a pass finds nothing.
    for _ in range(3):
        snapshot = _snapshot()
        spared = _ancestry(snapshot)
        killed = []
        for pid, (_, name) in snapshot.items():
            if pid <= 4 or pid in spared:  # 0 and 4 are the idle and System processes
                continue
            found = _inspect(pid, name)
            if found is not None and _within(found.cwd, target) and _terminate(pid):
                killed.append(found)
        if not killed:
            break
        stopped.extend(killed)
    return stopped


# Win32 plumbing ------------------------------------------------------------------------

_PROCESS_TERMINATE = 0x0001
_PROCESS_QUERY_INFORMATION = 0x0400
_PROCESS_VM_READ = 0x0010
_SYNCHRONIZE = 0x00100000
_TH32CS_SNAPPROCESS = 0x00000002
_PEB_PROCESS_PARAMETERS = 0x20  # PEB.ProcessParameters (x64)
_PARAMS_CURRENT_DIRECTORY = 0x38  # RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath (x64)
_PARAMS_COMMAND_LINE = 0x70  # RTL_USER_PROCESS_PARAMETERS.CommandLine (x64)


if sys.platform == "win32":
    from ctypes import wintypes

    class _BasicInformation(ctypes.Structure):
        _fields_ = [("exit_status", ctypes.c_void_p), ("peb", ctypes.c_void_p),
                    ("affinity", ctypes.c_void_p), ("priority", ctypes.c_void_p),
                    ("pid", ctypes.c_void_p), ("parent_pid", ctypes.c_void_p)]

    class _ProcessEntry(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD), ("pid", wintypes.DWORD),
                    ("heap", ctypes.c_void_p), ("module", wintypes.DWORD), ("threads", wintypes.DWORD),
                    ("parent_pid", wintypes.DWORD), ("priority", ctypes.c_long), ("flags", wintypes.DWORD),
                    ("exe", ctypes.c_wchar * 260)]

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _ntdll = ctypes.WinDLL("ntdll")
    _kernel32.OpenProcess.restype = wintypes.HANDLE
    _kernel32.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    _kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
    _kernel32.ReadProcessMemory.argtypes = (wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p,
                                            ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t))
    _kernel32.IsWow64Process.argtypes = (wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL))
    _kernel32.TerminateProcess.argtypes = (wintypes.HANDLE, wintypes.UINT)
    _kernel32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
    _kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    _kernel32.CreateToolhelp32Snapshot.argtypes = (wintypes.DWORD, wintypes.DWORD)
    _kernel32.Process32FirstW.argtypes = (wintypes.HANDLE, ctypes.POINTER(_ProcessEntry))
    _kernel32.Process32NextW.argtypes = (wintypes.HANDLE, ctypes.POINTER(_ProcessEntry))
    _ntdll.NtQueryInformationProcess.argtypes = (wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                                 wintypes.ULONG, ctypes.POINTER(wintypes.ULONG))


def _snapshot() -> dict[int, tuple[int, str]]:
    """pid -> (parent pid, executable name) for every running process."""
    entries: dict[int, tuple[int, str]] = {}
    snap = _kernel32.CreateToolhelp32Snapshot(_TH32CS_SNAPPROCESS, 0)
    if not snap or snap == wintypes.HANDLE(-1).value:
        return entries
    try:
        entry = _ProcessEntry()
        entry.size = ctypes.sizeof(entry)
        more = _kernel32.Process32FirstW(snap, ctypes.byref(entry))
        while more:
            entries[entry.pid] = (entry.parent_pid, entry.exe)
            more = _kernel32.Process32NextW(snap, ctypes.byref(entry))
    finally:
        _kernel32.CloseHandle(snap)
    return entries


def _ancestry(snapshot: dict[int, tuple[int, str]]) -> set[int]:
    spared, pid = set(), os.getpid()
    while pid and pid not in spared:
        spared.add(pid)
        pid = snapshot.get(pid, (0, ""))[0]
    return spared


def _read(handle, address: int, size: int) -> bytes | None:
    buffer = ctypes.create_string_buffer(size)
    read = ctypes.c_size_t()
    if not _kernel32.ReadProcessMemory(handle, ctypes.c_void_p(address), buffer, size, ctypes.byref(read)):
        return None
    return buffer.raw[:read.value]


def _inspect(pid: int, name: str) -> WorktreeProcess | None:
    """The process's working directory, or None when it can't be read (another user's or an
    elevated process, a 32-bit one whose live directory sits in a separate 32-bit PEB)."""
    handle = _kernel32.OpenProcess(_PROCESS_QUERY_INFORMATION | _PROCESS_VM_READ, False, pid)
    if not handle:
        return None
    try:
        wow64 = wintypes.BOOL()
        if not _kernel32.IsWow64Process(handle, ctypes.byref(wow64)) or wow64.value:
            return None
        info = _BasicInformation()
        if _ntdll.NtQueryInformationProcess(handle, 0, ctypes.byref(info), ctypes.sizeof(info), None) != 0:
            return None
        if not info.peb:
            return None
        raw = _read(handle, info.peb + _PEB_PROCESS_PARAMETERS, 8)
        if not raw or len(raw) != 8:
            return None
        parameters = int.from_bytes(raw, "little")
        cwd = _unicode_string(handle, parameters + _PARAMS_CURRENT_DIRECTORY)
        if not cwd:
            return None
        return WorktreeProcess(pid, name, cwd, _unicode_string(handle, parameters + _PARAMS_COMMAND_LINE) or "")
    finally:
        _kernel32.CloseHandle(handle)


def _unicode_string(handle, address: int) -> str | None:
    """The UNICODE_STRING at `address` in the process's memory."""
    unicode_string = _read(handle, address, 16)
    if not unicode_string or len(unicode_string) != 16:
        return None
    length = int.from_bytes(unicode_string[0:2], "little")
    text = _read(handle, int.from_bytes(unicode_string[8:16], "little"), length) if length else None
    return text.decode("utf-16-le", errors="replace") if text else None


def _terminate(pid: int) -> bool:
    """Kill the process and wait (briefly) for it to exit so its directory handle is released."""
    handle = _kernel32.OpenProcess(_PROCESS_TERMINATE | _SYNCHRONIZE, False, pid)
    if not handle:
        return False
    try:
        if not _kernel32.TerminateProcess(handle, 1):
            return False
        _kernel32.WaitForSingleObject(handle, 5000)
        return True
    finally:
        _kernel32.CloseHandle(handle)
