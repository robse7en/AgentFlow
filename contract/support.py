"""Black-box drivers for the contract suite: launch a backend, act as the human and as an agent.

Nothing here imports `agentflow`. The suite may only use what a client of the backend can see:
HTTP, `/api/ws`, subprocesses (the backend, flowctl, the stub agent) and the files a run leaves
under the task directory. That is what lets the same tests check the Python and the Rust backend.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

import httpx

ROOT = Path(__file__).resolve().parents[1]
STUB_AGENT = ROOT / "scripts" / "stub_agent.py"
PAIRING_PREFIX = "AgentFlow browser pairing code: "


def wait_for(predicate: Callable[[], Any], timeout: float = 90.0, what: str = "condition", interval: float = 0.25) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if value := predicate():
            return value
        time.sleep(interval)
    raise TimeoutError(f"timed out after {timeout:.0f}s waiting for {what}")


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def long_temp_dir(prefix: str) -> Path:
    # resolve(): %TEMP% can be an 8.3 short path (C:\Users\ROBROD~1), which worktree creation
    # rejects because it does not resolve to itself.
    return Path(tempfile.mkdtemp(prefix=prefix)).resolve()


def git(*args: str, cwd: Path) -> str:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()


def make_repository(path: Path) -> Path:
    """A throw-away repository with a `main` branch and one file under src/."""
    path.mkdir(parents=True)
    identity = ["-c", "user.name=contract", "-c", "user.email=contract@local"]
    git("init", "-q", "-b", "main", cwd=path)
    git(*identity, "commit", "-q", "--allow-empty", "-m", "init", cwd=path)
    (path / "src").mkdir()
    (path / "src" / "app.txt").write_text("hello\n", encoding="utf-8")
    git("add", "-A", cwd=path)
    git(*identity, "commit", "-q", "-m", "app", cwd=path)
    return path


def kill_processes_mentioning(text: str) -> list[int]:
    """Kill every process whose command line contains `text` (a test's temp directory).

    Agent processes run inside zellij for the Python target and can outlive `kill-session`.
    """
    processes: dict[int, tuple[int, str]] = {}  # pid -> (parent pid, command line)
    if sys.platform == "win32":
        listing = subprocess.run(["wmic", "process", "get", "CommandLine,ParentProcessId,ProcessId", "/format:csv"],
                                 capture_output=True, text=True, errors="replace").stdout
        # Rows are Node,CommandLine,ParentProcessId,ProcessId; only CommandLine can contain commas.
        for line in listing.splitlines():
            fields = line.split(",", 1)[-1].rsplit(",", 2)
            if len(fields) == 3 and fields[1].strip().isdigit() and fields[2].strip().isdigit():
                processes[int(fields[2])] = (int(fields[1]), fields[0])
    else:
        listing = subprocess.run(["ps", "-eo", "pid=,ppid=,args="], capture_output=True, text=True).stdout
        for line in listing.splitlines():
            pid, ppid, cmd = (line.split(None, 2) + [""])[:3]
            processes[int(pid)] = (int(ppid), cmd)
    # Never the test runner itself or the shell that started it, whose command line may name the path.
    mine, pid = set(), os.getpid()
    while pid in processes and pid not in mine:
        mine.add(pid)
        pid = processes[pid][0]
    pids = [pid for pid, (_, cmd) in processes.items() if text in cmd and pid not in mine]
    for pid in pids:
        if sys.platform == "win32":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)], capture_output=True)
        else:
            subprocess.run(["kill", "-9", str(pid)], capture_output=True)
    return pids


def new_key(prefix: str = "CT") -> str:
    return f"{prefix}-{uuid.uuid4().hex[:6].upper()}"


# --------------------------------------------------------------------------------------------
# Targets: how to start each backend implementation. Everything after startup is shared.
# --------------------------------------------------------------------------------------------

class Target:
    name: str

    def argv(self, config_file: Path, port: int) -> list[str]:
        raise NotImplementedError

    def cwd(self) -> Path:
        return ROOT

    def env(self) -> dict[str, str]:
        # Never inherit an ambient zellij session into the backend (start.sh does the same).
        return {k: v for k, v in os.environ.items() if not k.startswith("ZELLIJ")}

    def flowctl(self) -> list[str]:
        """The flowctl this implementation ships."""
        raise NotImplementedError

    def cleanup_task(self, key: str) -> None:
        """Release anything a task left outside its temp directories."""


class PythonTarget(Target):
    name = "python"

    def __init__(self, python: str) -> None:
        self.python = python

    def argv(self, config_file: Path, port: int) -> list[str]:
        return [self.python, "-m", "agentflow.main", "--config", str(config_file), "--port", str(port), "--skip-jira-check"]

    def cwd(self) -> Path:
        # Run from this checkout's backend/ so its code is imported, not an editable install elsewhere.
        return ROOT / "backend"

    def env(self) -> dict[str, str]:
        return {**super().env(), "PYTHONPATH": str(ROOT / "backend"), "PYTHONUNBUFFERED": "1"}

    def flowctl(self) -> list[str]:
        return [self.python, "-m", "agentflow.cli.flowctl"]

    def cleanup_task(self, key: str) -> None:
        zellij = shutil.which("zellij")
        if zellij:
            for args in (["kill-session", f"agentflow-{key}"], ["delete-session", f"agentflow-{key}", "--force"]):
                try:
                    subprocess.run([zellij, *args], capture_output=True, timeout=30, env=self.env())
                except subprocess.TimeoutExpired:
                    pass  # the processes are killed by temp-directory match afterwards


class RustTarget(Target):
    name = "rust"

    def __init__(self, binary: Path, flowctl: Path) -> None:
        self.binary = binary
        self.flowctl_binary = flowctl
        # zellij (as the Python backend) or embedded terminals.
        self.terminal = os.environ.get("CONTRACT_RUST_TERMINAL", "zellij")

    def argv(self, config_file: Path, port: int) -> list[str]:
        return [str(self.binary), "--config", str(config_file), "--port", str(port), "--skip-jira-check",
                "--terminal", self.terminal]

    def flowctl(self) -> list[str]:
        return [str(self.flowctl_binary)]

    def cleanup_task(self, key: str) -> None:
        if self.terminal == "zellij":
            PythonTarget.cleanup_task(self, key)  # type: ignore[arg-type]  # same sessions, same commands


# --------------------------------------------------------------------------------------------
# A running backend with its own data directory, worktree directory and configuration.
# --------------------------------------------------------------------------------------------

def stub_harness(python: str, **env: str) -> dict[str, Any]:
    return {
        "enabled": True, "type": "generic-cli", "displayName": "Stub Agent", "command": python,
        "args": [str(STUB_AGENT)], "modelArgument": "--model", "reasoningArgument": "--reasoning",
        "env": {"STUB_DELAY": "0.1", **env},
    }


def base_config(tmp: Path, port: int, harness: dict[str, Any]) -> dict[str, Any]:
    home = tmp / "home"
    return {
        "app": {"dataDirectory": str(home), "databasePath": str(home / "agentflow.db")},
        "server": {"host": "127.0.0.1", "port": port},
        "workspace": {"worktreeDirectory": str(tmp / "worktrees"), "taskDirectory": str(home / "tasks"),
                      "branchPrefix": "agent/"},
        "workflow": {"maxReviewIterations": 3, "implementationMode": "direct"},
        "harnesses": {"stub": harness},
        "agents": {role: {"harness": "stub", "reasoning": "high"}
                   for role in ("architect", "implementer", "codeReviewer", "securityReviewer")},
    }


@dataclass
class Backend:
    target: Target
    tmp: Path
    config: dict[str, Any]
    port: int
    process: subprocess.Popen | None = None
    output: list[str] = field(default_factory=list)
    keys: set[str] = field(default_factory=set)
    _pairing: list[str] = field(default_factory=list)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    @property
    def config_file(self) -> Path:
        return self.tmp / "home" / "config.json"

    def write_config(self) -> None:
        self.config_file.parent.mkdir(parents=True, exist_ok=True)
        self.config_file.write_text(json.dumps(self.config, indent=2), encoding="utf-8")

    def start(self) -> None:
        self.write_config()
        self._pairing.clear()  # each start prints a new single-use code
        self.process = subprocess.Popen(
            self.target.argv(self.config_file, self.port), cwd=self.target.cwd(), env=self.target.env(),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace",
        )
        threading.Thread(target=self._read_output, daemon=True).start()
        wait_for(self._healthy, 60, "backend /api/health")

    def _read_output(self) -> None:
        assert self.process and self.process.stdout
        for line in self.process.stdout:
            self.output.append(line.rstrip("\n"))
            if line.startswith(PAIRING_PREFIX):
                self._pairing.append(line[len(PAIRING_PREFIX):].strip())

    def _healthy(self) -> bool:
        if self.process and self.process.poll() is not None:
            raise RuntimeError(f"backend exited with {self.process.returncode}:\n" + "\n".join(self.output[-40:]))
        try:
            return httpx.get(f"{self.url}/api/health", timeout=2).status_code == 200
        except httpx.HTTPError:
            return False

    def pairing_code(self) -> str:
        return wait_for(lambda: self._pairing[-1] if self._pairing else None, 30, "pairing code on stdout")

    def human(self) -> "Human":
        """A paired browser session: the session cookie plus the UI's own Origin header."""
        client = httpx.Client(base_url=self.url, headers={"Origin": self.url}, timeout=60)
        client.post("/api/auth/pair", json={"code": self.pairing_code()}).raise_for_status()
        return Human(client, self)

    def stop(self) -> None:
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(15)
            except subprocess.TimeoutExpired:
                self.process.kill()
        for key in self.keys:
            self.target.cleanup_task(key)
        kill_processes_mentioning(str(self.tmp))
        kill_processes_mentioning(self.tmp.as_posix())

    def log_tail(self, lines: int = 60) -> str:
        return "\n".join(self.output[-lines:])


@dataclass
class Human:
    client: httpx.Client
    backend: Backend

    def get(self, path: str, **kw) -> httpx.Response:
        return self.client.get(path, **kw)

    def post(self, path: str, **kw) -> httpx.Response:
        return self.client.post(path, **kw)

    def put(self, path: str, **kw) -> httpx.Response:
        return self.client.put(path, **kw)

    def ok(self, method: str, path: str, **kw) -> Any:
        response = self.client.request(method, path, **kw)
        assert response.status_code < 400, f"{method} {path} -> {response.status_code}: {response.text}"
        return response.json()

    def task(self, key: str) -> dict[str, Any]:
        return self.ok("GET", f"/api/tasks/{key}")

    def create_task(self, key: str, repo: Path, **fields: Any) -> dict[str, Any]:
        self.backend.keys.add(key)
        response = self.post("/api/tasks", json={"jiraKey": key, "repositoryPath": str(repo), "baseBranch": "main", **fields},
                             timeout=120)
        assert response.status_code == 201, response.text
        return response.json()

    def run(self, key: str, node_key: str) -> dict[str, Any]:
        return self.ok("POST", f"/api/tasks/{key}/nodes/{node_key}/run", json={})

    def answer(self, key: str, question_uid: str, answer: str) -> dict[str, Any]:
        return self.ok("POST", f"/api/tasks/{key}/questions/{question_uid}/answer", json={"answer": answer})

    def approve_requirements(self, key: str) -> dict[str, Any]:
        digest = self.task(key)["requirementsDraftDigest"]
        return self.ok("POST", f"/api/tasks/{key}/nodes/requirements_gate/approve",
                       json={"expectedRequirementsDigest": digest})

    def wait_task(self, key: str, predicate: Callable[[dict[str, Any]], bool], what: str, timeout: float = 120) -> dict[str, Any]:
        """The first snapshot of the task that satisfies `predicate`."""
        def reached():
            snapshot = self.task(key)
            return snapshot if predicate(snapshot) else None
        return wait_for(reached, timeout, f"{key}: {what}")

    def wait_node(self, key: str, node_key: str, *states: str, timeout: float = 120) -> dict[str, Any]:
        return self.wait_task(key, lambda s: node(s, node_key)["state"] in states, f"{node_key} in {states}", timeout)


def node(snapshot: dict[str, Any], key: str) -> dict[str, Any]:
    return next(n for n in snapshot["nodes"] if n["key"] == key)


# --------------------------------------------------------------------------------------------
# Acting as an agent: with the stub in STUB_MANUAL mode the test itself is the agent, using the
# credential and prompt the backend wrote for the run.
# --------------------------------------------------------------------------------------------

@dataclass
class AgentRun:
    backend: Backend
    key: str
    role: str  # the flowctl --agent name, e.g. code_reviewer
    run_dir: Path

    @property
    def run_id(self) -> int:
        match = re.search(r"run id \(`(\d+)`\)", (self.run_dir / "prompt.md").read_text(encoding="utf-8"))
        assert match, f"no run id in {self.run_dir / 'prompt.md'}"
        return int(match.group(1))

    @property
    def credential_file(self) -> Path:
        return self.run_dir / "credential.json"

    @property
    def token(self) -> str:
        return json.loads(self.credential_file.read_text(encoding="utf-8"))["token"]

    def post(self, action: str, payload: dict[str, Any], *, token: str | None = None, key: str | None = None) -> httpx.Response:
        body = {"role": self.role, "runId": self.run_id, **payload}
        return httpx.post(f"{self.backend.url}/api/tasks/{key or self.key}/agent/{action}", json=body,
                          headers={"Authorization": f"Bearer {token if token is not None else self.token}"}, timeout=60)

    def env(self) -> dict[str, str]:
        """The variables the launcher gives an agent, for running flowctl as that agent."""
        return {**os.environ, "AGENTFLOW_API": self.backend.url, "AGENTFLOW_TASK": self.key, "AGENTFLOW_AGENT": self.role,
                "AGENTFLOW_RUN": str(self.run_id), "AGENTFLOW_CREDENTIAL_FILE": str(self.credential_file)}


def agent_run(backend: Backend, snapshot: dict[str, Any], role_dir: str, role: str | None = None,
              timeout: float = 60) -> AgentRun:
    """The newest run directory for a role (runs/<role_dir>/NN) once its prompt and credential exist.
    `role` is the agent name when it differs from the directory (code-review -> code_reviewer)."""
    runs = Path(snapshot["taskDir"]) / "runs" / role_dir

    def ready():
        dirs = sorted(p for p in runs.glob("*") if p.is_dir()) if runs.is_dir() else []
        if dirs and (dirs[-1] / "prompt.md").exists() and (dirs[-1] / "credential.json").exists():
            return dirs[-1]
        return None
    run_dir = wait_for(ready, timeout, f"run directory under {runs}")
    return AgentRun(backend, snapshot["jiraKey"], role or role_dir, run_dir)


def write_requirements(task_dir: Path) -> None:
    """The documents an Architect discovery must leave before completing."""
    req = task_dir / "requirements"
    req.mkdir(exist_ok=True)
    for name, body in {
        "requirements.md": "# Requirements\n\n- Add STUB_NOTES.md describing the change.\n",
        "acceptance-criteria.md": "# Acceptance criteria\n\n- STUB_NOTES.md exists.\n",
        "assumptions.md": "# Assumptions\n\n- None.\n",
        "implementation-plan.md": "# Plan\n\n1. Create STUB_NOTES.md.\n",
        "risks.md": "# Risks\n\n- None.\n",
    }.items():
        (req / name).write_text(body, encoding="utf-8")


ARCHITECT_RESULT = {"status": "completed", "summary": "Contract discovery complete", "complexity": "Low",
                    "affectedComponents": ["STUB_NOTES.md"], "assumptions": [], "risks": [], "openQuestions": []}


IMPLEMENTER_RESULT = {"status": "completed", "summary": "Contract implementation: added STUB_NOTES.md",
                      "filesChanged": ["STUB_NOTES.md"], "tests": {"executed": 1, "passed": 1, "failed": 0},
                      "assumptions": [], "knownIssues": []}


def reach_reviews(backend: Backend, human: Human, repo: Path, prefix: str) -> tuple[AgentRun, AgentRun]:
    """A new task taken (in STUB_MANUAL mode, the test acting as each agent) through discovery,
    approval and implementation, until both reviewers are running. Returns (code, security)."""
    key = new_key(prefix)
    human.create_task(key, repo)
    human.run(key, "architect")
    architect = agent_run(backend, human.wait_node(key, "architect", "RUNNING"), "architect")
    write_requirements(architect.run_dir.parents[2])
    assert architect.post("complete", {"result": ARCHITECT_RESULT}).status_code == 200
    human.approve_requirements(key)

    snap = human.wait_node(key, "implementer", "RUNNING")
    implementer = agent_run(backend, snap, "implementer")
    (Path(snap["worktreePath"]) / "STUB_NOTES.md").write_text("contract change\n", encoding="utf-8")
    response = implementer.post("complete", {"result": IMPLEMENTER_RESULT})
    assert response.status_code == 200 and not response.json().get("error"), response.text

    snap = human.wait_task(key, lambda s: node(s, "code_review")["state"] == "RUNNING"
                           and node(s, "security_review")["state"] == "RUNNING", "both reviews running")
    return (agent_run(backend, snap, "code-review", "code_reviewer"),
            agent_run(backend, snap, "security-review", "security_reviewer"))


def python_executable() -> str:
    return os.environ.get("CONTRACT_PYTHON") or sys.executable
