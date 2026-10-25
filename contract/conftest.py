"""Target selection and backend fixtures for the contract suite (see README.md)."""

from __future__ import annotations

import os
import shutil
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator

import pytest

from support import (ROOT, Backend, PythonTarget, RustTarget, Target, base_config, free_port, long_temp_dir,
                     make_repository, python_executable, stub_harness)

_running: list[Backend] = []


def _rust_binary(name: str, override: str) -> Path | None:
    if explicit := os.environ.get(override):
        return Path(explicit)
    for profile in ("release", "debug"):
        candidate = ROOT / "app" / "target" / profile / f"{name}.exe"
        if candidate.exists() or (candidate := candidate.with_suffix("")).exists():
            return candidate
    return None


@pytest.fixture(scope="session")
def target() -> Target:
    name = os.environ.get("CONTRACT_TARGET", "python")
    if name == "python":
        return PythonTarget(python_executable())
    if name == "rust":
        binary = _rust_binary("agentflow", "CONTRACT_RUST_BIN")
        # flowctl from the same build as the backend (whose bin/flowctl shims point beside it),
        # unless named explicitly.
        flowctl = Path(os.environ["CONTRACT_RUST_FLOWCTL"]) if os.environ.get("CONTRACT_RUST_FLOWCTL") else (
            binary.with_name("flowctl" + binary.suffix) if binary else None)
        if binary is None or flowctl is None or not flowctl.exists():
            pytest.exit("CONTRACT_TARGET=rust needs the Rust backend and flowctl built (cargo build in app/)", 2)
        return RustTarget(binary, flowctl)
    pytest.exit(f"Unknown CONTRACT_TARGET {name!r}; use python or rust", 2)


def rust_flowctl() -> Path | None:
    return _rust_binary("flowctl", "CONTRACT_RUST_FLOWCTL")


@pytest.fixture(scope="session")
def launch(target: Target):
    """`with launch(**stub_env) as backend:` starts an isolated backend that uses the stub harness
    for every role. `workflow=` overrides workflow settings, `agents=` adds or replaces role
    assignments; `stub_env` configures the stub agent."""

    @contextmanager
    def start(workflow: dict[str, Any] | None = None, jira: dict[str, Any] | None = None,
              slack: dict[str, Any] | None = None, agents: dict[str, Any] | None = None,
              **stub_env: str) -> Iterator[Backend]:
        tmp = long_temp_dir(f"agentflow-contract-{target.name}-")
        port = free_port()
        if flowctl := os.environ.get("CONTRACT_STUB_FLOWCTL"):
            stub_env.setdefault("STUB_FLOWCTL", flowctl)
        config = base_config(tmp, port, stub_harness(python_executable(), **stub_env))
        config["workflow"].update(workflow or {})
        config["agents"].update(agents or {})
        if jira is not None:
            config["jira"] = jira
        if slack is not None:
            config["slack"] = slack
        backend = Backend(target, tmp, config, port)
        _running.append(backend)
        try:
            backend.start()
            yield backend
        finally:
            backend.stop()
            _running.remove(backend)
            if not os.environ.get("CONTRACT_KEEP"):
                shutil.rmtree(tmp, ignore_errors=True)

    return start


@pytest.fixture
def repo(tmp_path_factory) -> Path:
    return make_repository(Path(tmp_path_factory.mktemp("repo")).resolve() / "repo")


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    report = outcome.get_result()
    if report.failed:
        for backend in _running:
            report.sections.append((f"backend {backend.target.name} on :{backend.port} (data in {backend.tmp})",
                                    backend.log_tail()))
