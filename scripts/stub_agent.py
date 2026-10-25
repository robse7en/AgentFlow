#!/usr/bin/env python
"""Stub coding harness for smoke-testing AgentFlow end to end.

Configure it as a `generic-cli` harness (see scripts/smoke_config.json). It behaves like an
interactive CLI agent: it receives the initial prompt reference on argv, performs a canned action
for the role described in the prompt, reports through `flowctl`, and then keeps reading stdin for
further prompt lines (so the persistent-Architect path is exercised too).

Environment knobs:
    STUB_FINDINGS_UNTIL   reviewers report one blocking finding while iteration <= N (default 1)
    STUB_ASK_QUESTION     "1" → the architect asks a question first and waits for Enter
    STUB_EXIT_AFTER       "1" → exit after handling the first prompt (tests fresh-launch/resume path)
    STUB_DELAY            seconds to sleep before acting (default 0.5)
    STUB_SECURITY_LAG     extra seconds the security reviewer waits before completing, so the parallel
                          reviews always finish code-first (default 1.5)
    STUB_FLOWCTL          flowctl executable to call (default: `flowctl` on PATH, else the Python module)
    STUB_MANUAL           "1" → do nothing and idle on stdin, so a test can act as the agent itself
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path


def log(msg: str) -> None:
    print(f"[stub-agent] {msg}", flush=True)


def flowctl_command() -> list[str]:
    """Whichever flowctl the backend under test provides, so the stub works against any backend.

    STUB_FLOWCTL (set through the harness `env` map) wins; then the `flowctl` shim the launcher
    puts on PATH (flowctl.cmd on Windows); then the Python module as a last resort.
    $AGENTFLOW_FLOWCTL is deliberately not used: on Windows it names the extensionless sh shim.
    """
    if explicit := os.environ.get("STUB_FLOWCTL"):
        return [explicit]
    if found := shutil.which("flowctl"):
        return [found]
    return [sys.executable, "-m", "agentflow.cli.flowctl"]


def flowctl(*args: str) -> int:
    log("flowctl " + " ".join(args))
    return subprocess.call([*flowctl_command(), *args])


def git(worktree: str, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=worktree, capture_output=True, text=True, check=True).stdout.strip()


def extract_prompt_path(line: str) -> Path | None:
    match = re.search(r"Read the file (.+?prompt\.md) and follow", line)
    return Path(match.group(1)) if match else None


def json_block_after(prompt: str, heading: str) -> list[dict]:
    """The first ```json block under a prompt heading, or [] when the prompt has no such section."""
    match = re.search(rf"## {re.escape(heading)}.*?```json\n(.*?)\n```", prompt, re.S)
    return json.loads(match.group(1)) if match else []


def findings_to_fix(prompt: str) -> list[dict]:
    return json_block_after(prompt, "Review findings to fix")


def prior_findings(prompt: str) -> list[dict]:
    """A follow-up review's findings from the previous round."""
    return json_block_after(prompt, "Follow-up review")


def handle(prompt_path: Path) -> None:
    text = prompt_path.read_text(encoding="utf-8")
    run_dir = prompt_path.parent
    run_id = re.search(r"run id \(`(\d+)`\)", text)
    run_args = ["--run", run_id.group(1)] if run_id else []
    title = text.splitlines()[0]
    task_dir = Path(os.environ["AGENTFLOW_TASK_DIR"])
    worktree = os.environ["AGENTFLOW_WORKTREE"]
    time.sleep(float(os.environ.get("STUB_DELAY", "0.5")))
    result_path = run_dir / "result.json"

    if "Architect discovery" in title:
        if os.environ.get("STUB_ASK_QUESTION") == "1":
            flowctl("question", "--message", "Stub question: should this apply to all tenants?", *run_args)
            log("waiting for the human to press Enter ...")
            sys.stdin.readline()
            flowctl("running", *run_args)
        req = task_dir / "requirements"
        req.mkdir(exist_ok=True)
        (req / "requirements.md").write_text("# Requirements\n\n- Add STUB_NOTES.md describing the change.\n", encoding="utf-8")
        (req / "acceptance-criteria.md").write_text("# Acceptance criteria\n\n- STUB_NOTES.md exists.\n", encoding="utf-8")
        (req / "assumptions.md").write_text("# Assumptions\n\n- None.\n", encoding="utf-8")
        (req / "implementation-plan.md").write_text("# Plan\n\n1. Create STUB_NOTES.md.\n", encoding="utf-8")
        (req / "risks.md").write_text("# Risks\n\n- None.\n", encoding="utf-8")
        result = {"status": "completed", "summary": "Stub discovery complete", "complexity": "Low",
                  "affectedComponents": ["STUB_NOTES.md"], "assumptions": [], "risks": [], "openQuestions": []}
    elif "Implementer" in title:
        notes = Path(worktree) / "STUB_NOTES.md"
        iteration = re.search(r"iteration (\d+) of", text)
        with notes.open("a", encoding="utf-8") as fh:
            fh.write(f"stub change at {time.time()} (iteration {iteration.group(1) if iteration else 1})\n")
        result = {"status": "completed", "summary": "Stub implementation: appended to STUB_NOTES.md",
                  "filesChanged": ["STUB_NOTES.md"], "tests": {"executed": 1, "passed": 1, "failed": 0},
                  "assumptions": [], "knownIssues": []}
        # A fix iteration must answer every blocking finding the prompt lists.
        blocking = [f["id"] for f in findings_to_fix(text) if f.get("blocking")]
        if blocking:
            result["findingResponses"] = [{"id": fid, "action": "fixed", "rationale": "Stub: appended a new line to STUB_NOTES.md"}
                                          for fid in blocking]
    elif "Code Review" in title or "Security Review" in title:
        sha = git(worktree, "rev-parse", "HEAD")
        iteration = int(re.search(r"iteration (\d+),", title).group(1))
        until = int(os.environ.get("STUB_FINDINGS_UNTIL", "1"))
        prefix = "CODE" if "Code Review" in title else "SEC"
        still_broken = iteration <= until
        # A follow-up round rules on every prior blocking finding; one still broken is raised
        # again under its old id.
        prior = [f for f in prior_findings(text) if f.get("blocking")]
        findings = []
        if still_broken:
            finding = {"id": f"{prefix}-001", "severity": "medium", "blocking": True, "file": "STUB_NOTES.md", "line": 1,
                       "title": f"Stub finding iteration {iteration}", "description": "Canned finding", "recommendation": "Fix it"}
            if prior:
                finding["priorId"] = prior[0]["id"]
            findings.append(finding)
        result = {"status": "completed", "reviewedSha": sha, "verdict": "changes_required" if findings else "approved",
                  "summary": "Stub review", "findings": findings}
        if prior:
            result["priorFindings"] = [{"id": f["id"], "status": "not_fixed" if still_broken else "fixed",
                                        "evidence": "STUB_NOTES.md:1"} for f in prior]
        if prefix == "SEC":
            # The two reviews run in parallel, and the backend stores findings in completion order;
            # finishing second keeps that order (CODE before SEC) the same on every run.
            time.sleep(float(os.environ.get("STUB_SECURITY_LAG", "1.5")))
    elif "Final Architect" in title:
        final = task_dir / "final"
        final.mkdir(exist_ok=True)
        (final / "assessment.md").write_text("# Stub assessment\n\nAll good.\n", encoding="utf-8")
        result = {"status": "completed", "taskSummary": "Stub task", "complexity": "Low", "complexityScore": 2,
                  "overallRisk": "LOW", "regressionRisk": "LOW", "securityRisk": "LOW", "requirementsVerified": True,
                  "testsPerformed": "One stub assertion passed; no live application tests.",
                  "humanReviewRecommendation": "LOW: normal PR review is sufficient"}
    else:
        flowctl("fail", "--message", f"stub does not understand prompt: {title}", *run_args)
        return
    result_path.write_text(json.dumps(result, indent=2), encoding="utf-8")
    flowctl("complete", "--file", str(result_path), *run_args)


def main() -> None:
    args = [a for a in sys.argv[1:]]
    # swallow --model/--reasoning style options the adapter may add
    prompt_line = args[-1] if args else ""
    log(f"started with args {args}")
    if os.environ.get("STUB_MANUAL") == "1":
        log("manual mode; idling on stdin")
        for line in sys.stdin:
            log(f"received: {line.rstrip()}")
        return
    first = extract_prompt_path(prompt_line)
    if first:
        handle(first)
    if os.environ.get("STUB_EXIT_AFTER") == "1":
        log("exiting after first prompt")
        return
    log("idle; waiting for further prompts on stdin")
    for line in sys.stdin:
        path = extract_prompt_path(line)
        if path:
            handle(path)
        elif line.strip() in ("exit", "quit"):
            break


if __name__ == "__main__":
    main()
