"""Golden files recorded from the Python backend, which every target must reproduce.

- Shapes: the structure of a JSON response (keys and value types, not values), so the Vue UI keeps
  receiving exactly what `frontend/src/api/types.ts` describes.
- Prompts: the prompt.md each agent run receives, with run-specific values (paths, keys, SHAs,
  run ids, ports) replaced by placeholders. Agents read these files, so their text is contract.

Record with `CONTRACT_UPDATE_GOLDEN=1` against the Python target; review the diff before committing.
"""

from __future__ import annotations

import difflib
import json
import os
import re
from pathlib import Path
from typing import Any

import pytest

GOLDEN = Path(__file__).resolve().parent / "golden"


def updating() -> bool:
    if not os.environ.get("CONTRACT_UPDATE_GOLDEN"):
        return False
    if os.environ.get("CONTRACT_TARGET", "python") != "python":
        pytest.fail("Golden files are recorded from the Python backend only")
    return True


# ---------------------------------------------------------------------------- shapes

def shape(value: Any) -> Any:
    if isinstance(value, dict):
        return {k: shape(v) for k, v in sorted(value.items())}
    if isinstance(value, list):
        return [_merge([shape(v) for v in value])] if value else []
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, (int, float)):
        return "number"  # JSON has one number type; 1 vs 1.0 is not a contract difference
    if value is None:
        return "null"
    return "str"


def _merge(shapes: list[Any]) -> Any:
    """One shape describing every item of a list."""
    dicts = [s for s in shapes if isinstance(s, dict)]
    lists = [s for s in shapes if isinstance(s, list)]
    scalars = sorted({s for s in shapes if isinstance(s, str)})
    merged: list[Any] = []
    if dicts:
        keys = sorted({k for d in dicts for k in d})
        merged.append({k: _merge([d[k] for d in dicts if k in d]) for k in keys})
    if lists:
        merged.append([_merge([item for lst in lists for item in lst])] if any(lists) else [])
    if scalars:
        merged.append("|".join(scalars))
    return merged[0] if len(merged) == 1 else {"anyOf": merged}


def check_shape(name: str, value: Any) -> None:
    path = GOLDEN / "shapes" / f"{name}.json"
    actual = shape(value)
    if updating():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(actual, indent=2) + "\n", encoding="utf-8")
        return
    if not path.exists():
        pytest.fail(f"No golden shape {path.name}; record it with CONTRACT_UPDATE_GOLDEN=1 against the Python target")
    differences = _diff(json.loads(path.read_text(encoding="utf-8")), actual)
    assert not differences, f"{name} shape differs from {path}:\n" + "\n".join(differences)


def _diff(expected: Any, actual: Any, where: str = "$") -> list[str]:
    # Intended Rust difference: persist the assigned implementation checkpoint. Python's ORM
    # retains the pre-verification null; test_workflow checks Rust's value against currentSha.
    if (os.environ.get("CONTRACT_TARGET") == "rust"
            and where == "$.nodes[].lastRun.anyOf[].result.checkpointSha"
            and expected == "null" and actual == "str"):
        return []
    # Embedded terminals are viewed in the app; there is no zellij command to attach with.
    if (os.environ.get("CONTRACT_TARGET") == "rust"
            and os.environ.get("CONTRACT_RUST_TERMINAL") == "embedded"
            and where == "$.zellij.attachCommand" and expected == "str" and actual == "null"):
        return []
    if isinstance(expected, dict) and isinstance(actual, dict):
        lines = [f"{where}.{k}: missing" for k in expected if k not in actual]
        lines += [f"{where}.{k}: unexpected" for k in actual if k not in expected]
        for k in expected.keys() & actual.keys():
            lines += _diff(expected[k], actual[k], f"{where}.{k}")
        return lines
    if isinstance(expected, list) and isinstance(actual, list) and expected and actual:
        return _diff(expected[0], actual[0], f"{where}[]")
    if where in SET_BY_TIMING and isinstance(expected, str) and isinstance(actual, str):
        a, b = set(expected.split("|")) - {"null"}, set(actual.split("|")) - {"null"}
        if not a or not b or a == b:
            return []
    return [] if expected == actual else [f"{where}: expected {expected!r}, got {actual!r}"]


# Fields a background job fills in, so whether they are still null when the snapshot is taken is
# timing, not structure. Everywhere else a null where the golden has a type is a difference.
SET_BY_TIMING = {
    "$.sessions[].screenStateAt",  # the pane screen poller
}


# ---------------------------------------------------------------------------- event traces

def event_trace(events: list[dict[str, Any]], *, key: str, roots: dict[str, Path]) -> dict[str, list[str]]:
    """The task's event log as one line per event, grouped by node: nodes run concurrently (the
    two reviews), so only the order within each node, and of task-level events, is deterministic."""
    trace: dict[str, list[str]] = {}
    for event in events:
        data = "" if event.get("data") is None else " " + json.dumps(event["data"], sort_keys=True)
        line = normalize(f"{event['level']}: {event['message']}{data}", key=key, roots=roots)
        line = re.sub(r"\b(agentflow-)?terminal_\d+|\bpty_\d+", "<PANE>", line)
        line = re.sub(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", "<UID>", line)
        line = re.sub(r"\b[0-9a-f]{10}\b", "<SHORT-SHA>", line)
        trace.setdefault(event.get("nodeKey") or "(task)", []).append(line)
    return dict(sorted(trace.items()))


def check_events(name: str, events: list[dict[str, Any]], *, key: str, roots: dict[str, Path]) -> None:
    path = GOLDEN / "events" / f"{name}.json"
    actual = event_trace(events, key=key, roots=roots)
    if updating():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(actual, indent=2) + "\n", encoding="utf-8")
        return
    if not path.exists():
        pytest.fail(f"No golden event trace {path.name}; record it with CONTRACT_UPDATE_GOLDEN=1 against the Python target")
    expected = json.loads(path.read_text(encoding="utf-8"))
    problems = []
    for group in sorted(expected.keys() | actual.keys()):
        want, got = expected.get(group, []), actual.get(group, [])
        if want != got:
            diff = "".join(difflib.unified_diff([f"{line}\n" for line in want], [f"{line}\n" for line in got],
                                                f"golden {group}", f"actual {group}"))
            problems.append(diff)
    assert not problems, f"event trace {name} differs from {path}:\n" + "\n".join(problems)


# ---------------------------------------------------------------------------- prompts

def normalize(text: str, *, key: str, roots: dict[str, Path]) -> str:
    """Replace each root directory with <NAME>, in native, POSIX and JSON-escaped spelling."""
    for name, root in sorted(roots.items(), key=lambda item: len(str(item[1])), reverse=True):
        native = str(root)
        for form in (native.replace("\\", "\\\\"), native, root.as_posix()):
            text = text.replace(form, f"<{name}>")
    text = text.replace(key, "<KEY>")
    text = re.sub(r"http://127\.0\.0\.1:\d+", "<API>", text)
    text = re.sub(r"\b[0-9a-f]{40}\b", "<SHA>", text)
    text = re.sub(r"\bcheckpoint [0-9a-f]{10}\b", "checkpoint <SHORT-SHA>", text)
    text = re.sub(r"run id \(`\d+`\)", "run id (`<RUN>`)", text)
    text = re.sub(r"--run \d+", "--run <RUN>", text)
    return text.replace("\r\n", "\n")


def check_prompt(task_dir: Path, run: str, *, key: str, roots: dict[str, Path]) -> None:
    """One run's prompt.md (e.g. "architect/01") against its golden, partway through a flow.
    Read-only: check_prompts records the whole set at the end of the flow."""
    golden_path = GOLDEN / "prompts" / f"{run}.md"
    if updating():
        return
    actual = normalize((task_dir / "runs" / run / "prompt.md").read_text(encoding="utf-8"), key=key, roots=roots)
    expected = golden_path.read_text(encoding="utf-8")
    if actual != expected:
        diff = "".join(difflib.unified_diff(expected.splitlines(keepends=True), actual.splitlines(keepends=True),
                                            f"golden/prompts/{run}.md", "actual"))
        pytest.fail(f"prompt {run} differs from golden/prompts/{run}.md:\n{diff}")


def check_prompts(task_dir: Path, *, key: str, roots: dict[str, Path]) -> None:
    """Every runs/<role>/<NN>/prompt.md against golden/prompts/<role>/<NN>.md."""
    actual = {
        f"{p.parent.parent.name}/{p.parent.name}.md": normalize(p.read_text(encoding="utf-8"), key=key, roots=roots)
        for p in sorted((task_dir / "runs").glob("*/*/prompt.md"))
    }
    base = GOLDEN / "prompts"
    if updating():
        for rel, text in actual.items():
            (base / rel).parent.mkdir(parents=True, exist_ok=True)
            (base / rel).write_text(text, encoding="utf-8", newline="\n")
        return
    if not base.is_dir():
        pytest.fail("No golden prompts; record them with CONTRACT_UPDATE_GOLDEN=1 against the Python target")
    expected = {p.relative_to(base).as_posix(): p.read_text(encoding="utf-8") for p in sorted(base.glob("*/*.md"))}
    assert sorted(actual) == sorted(expected), "the set of agent runs differs from the golden prompts"
    for rel, text in expected.items():
        if actual[rel] != text:
            diff = "".join(difflib.unified_diff(text.splitlines(keepends=True), actual[rel].splitlines(keepends=True),
                                                f"golden/prompts/{rel}", "actual"))
            pytest.fail(f"prompt {rel} differs from golden/prompts/{rel}:\n{diff}")
