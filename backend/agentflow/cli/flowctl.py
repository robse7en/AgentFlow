"""flowctl — the structured channel between agents and AgentFlow.

Agents run this inside their zellij tab. Task, role and run id come from environment
variables written by the launcher script (AGENTFLOW_TASK, AGENTFLOW_AGENT, AGENTFLOW_RUN, AGENTFLOW_API),
so the usual invocations are simply:

    flowctl question --message "Should this apply to all tenants?"
    flowctl question --file - <<'EOF'
    {"message": "...", "risk": "medium", "choices": [{"label": "...", "recommended": true}, ...]}
    EOF
    flowctl running
    flowctl complete --file - <<'EOF'
    {"status": "completed", "summary": "..."}
    EOF
    flowctl complete --file <run dir>/submission.json   (a result too long for a heredoc)
    flowctl finding --id CODE-001 --severity high --title "..." --file src/x.cs --line 12 --description "..."
    flowctl fail --message "Cannot continue because ..."
    flowctl status

It talks HTTP to the local backend only; it never touches the database.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

import httpx

DEFAULT_API = "http://127.0.0.1:8765"


def _api() -> str:
    return os.environ.get("AGENTFLOW_API", DEFAULT_API).rstrip("/")


def _common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--task", default=os.environ.get("AGENTFLOW_TASK"), help="Jira key (default: $AGENTFLOW_TASK)")
    parser.add_argument("--agent", default=os.environ.get("AGENTFLOW_AGENT"),
                        help="architect | orchestrator | implementer | code_reviewer | security_reviewer (default: $AGENTFLOW_AGENT)")
    parser.add_argument("--run", type=int, default=int(os.environ["AGENTFLOW_RUN"]) if os.environ.get("AGENTFLOW_RUN", "").isdigit() else None,
                        help="Node run id (default: $AGENTFLOW_RUN)")
    parser.add_argument("--api", default=None, help=f"Backend URL (default: $AGENTFLOW_API or {DEFAULT_API})")


def _require(args: argparse.Namespace) -> None:
    missing = [name for name in ("task", "agent") if not getattr(args, name)]
    if missing:
        sys.exit(f"flowctl: missing --{missing[0]} (or ${'AGENTFLOW_' + missing[0].upper()})")


def _post(args: argparse.Namespace, path: str, payload: dict[str, Any], *, on_response=None) -> dict[str, Any]:
    base = (args.api or _api()).rstrip("/")
    url = f"{base}/api/tasks/{args.task}/agent/{path}"
    body = {"role": args.agent, "runId": args.run, **payload}
    try:
        timeout = httpx.Timeout(30, read=None) if path == "complete" else 30
        response = httpx.post(url, json=body, headers=_headers(args), timeout=timeout)
    except httpx.HTTPError as exc:
        sys.exit(f"flowctl: cannot reach AgentFlow at {base}: {exc}")
    # A response - even a rejection - means the backend saw the request and made a decision;
    # only a transport failure above (no response at all) is worth preserving anything for.
    if on_response is not None:
        on_response()
    if response.status_code >= 400:
        try:
            detail = response.json().get("detail", response.text)
        except ValueError:
            detail = response.text
        sys.exit(f"flowctl: {path} rejected ({response.status_code}): {detail}")
    return response.json()


def _headers(args: argparse.Namespace) -> dict[str, str]:
    path = os.environ.get("AGENTFLOW_CREDENTIAL_FILE")
    if not path or args.run is None:
        sys.exit("flowctl: scoped credential file and explicit run ID required; restart older agent runs")
    credential = json.loads(Path(path).read_text(encoding="utf-8"))
    if credential.get("runId") == args.run:
        return {"Authorization": "Bearer " + credential["token"]}
    sys.exit("flowctl: no credential for this run; use --run from the current prompt")


def cmd_status(args: argparse.Namespace) -> None:
    base = (args.api or _api()).rstrip("/")
    if not args.task:
        sys.exit("flowctl: agents can only read their assigned task")
    response = httpx.get(f"{base}/api/tasks/{args.task}", headers=_headers(args), timeout=15)
    if response.status_code == 404:
        sys.exit(f"flowctl: task {args.task} not found")
    response.raise_for_status()
    snapshot = response.json()
    print(f"Task {snapshot['jiraKey']}: {snapshot['status']} (iteration {snapshot['reviewIteration']}/{snapshot['maxReviewIterations']})")
    if snapshot.get("statusMessage"):
        print(f"  {snapshot['statusMessage']}")
    for node in snapshot["nodes"]:
        extra = f" — {node['message']}" if node.get("message") else ""
        print(f"  {node['label']:<30} {node['state']:<22}{extra}")
        if node.get("error"):
            print(f"      error: {node['error']}")
    if args.json:
        print(json.dumps(snapshot, indent=2))


def cmd_question(args: argparse.Namespace) -> None:
    _require(args)
    if args.file is not None:
        if args.choice:
            sys.exit("flowctl: --choice cannot be combined with --file; put the choices in the JSON")
        # A decision tree (see `decisions`): one JSON object, read as UTF-8 whatever the console
        # code page, since labels often carry non-ASCII text.
        try:
            raw = (sys.stdin.buffer.read() if args.file == "-" else Path(args.file).read_bytes()).decode("utf-8-sig")
        except FileNotFoundError:
            sys.exit(f"flowctl: question file not found: {args.file}")
        except UnicodeDecodeError as exc:
            sys.exit(f"flowctl: question file is not UTF-8: {exc}")
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError as exc:
            sys.exit(f"flowctl: question is not valid JSON: {exc}")
        if not isinstance(payload, dict):
            sys.exit("flowctl: question must be a JSON object")
        payload = {key: value for key, value in payload.items() if key not in ("role", "runId", "run_id")}
    else:
        payload = {"message": args.message, "choices": args.choice}
    result = _post(args, "question", payload)
    print(f"Question recorded. Node is now {result['state']}. Wait for the human to reply in AgentFlow, then run: flowctl running")


def cmd_heartbeat(args: argparse.Namespace) -> None:
    _require(args)
    _post(args, "heartbeat", {"phase": args.phase})
    print("Agent heartbeat recorded.")


def cmd_running(args: argparse.Namespace) -> None:
    _require(args)
    result = _post(args, "running", {})
    print(f"Marked running ({result['state']}).")


def cmd_complete(args: argparse.Namespace) -> None:
    _require(args)
    if args.file == "-":
        # The usual path for a short result, e.g. `flowctl complete --file - <<'EOF' ... EOF`.
        # A harness passes the whole shell command, heredoc included, as one process argument,
        # so a long result exceeds Windows' ~32K command-line limit (ENAMETOOLONG) and has to
        # come through --file instead: the agent writes it to submission.json in its run dir.
        try:
            result = json.loads(sys.stdin.read())
        except json.JSONDecodeError as exc:
            sys.exit(f"flowctl: stdin is not valid JSON: {exc}")
    elif args.file:
        path = Path(args.file)
        if not path.exists():
            sys.exit(f"flowctl: result file not found: {path}")
        try:
            result = json.loads(path.read_text(encoding="utf-8-sig"))
        except json.JSONDecodeError as exc:
            sys.exit(f"flowctl: {path} is not valid JSON: {exc}")
    elif args.json:
        result = json.loads(args.json)
    else:
        result = {"status": "completed", "summary": args.summary or ""}
        if args.sha:
            result["reviewedSha"] = args.sha
        if args.verdict:
            result["verdict"] = args.verdict
    if not isinstance(result, dict):
        sys.exit("flowctl: result must be a JSON object")
    if args.agent != "orchestrator":
        result.setdefault("status", "completed")
    # Persist the result outside the repo (next to credential.json) before the network call:
    # this is flowctl's own trusted file I/O, not the agent's Write tool, and it never touches
    # the project/worktree. If the backend is unreachable at this exact instant, the finished
    # result survives on disk for AgentFlow to pick up on its next startup instead of being
    # lost for good. Once the backend has responded at all - even to reject it - there is
    # nothing left worth preserving, so it's cleared on any response, not just success.
    pending_path = _pending_complete_path()
    if pending_path is not None:
        pending_path.write_text(
            json.dumps({"role": args.agent, "runId": args.run, "result": result}, indent=2), encoding="utf-8"
        )
    clear_pending = (lambda: pending_path.unlink(missing_ok=True)) if pending_path is not None else None
    response = _post(args, "complete", {"result": result}, on_response=clear_pending)
    if response.get("error"):
        sys.exit(f"flowctl: completion REJECTED — {response['error']}\n"
                  f"Node state: {response['state']}; task status: {response['taskStatus']}.")
    # Kept on a rejection so the agent can fix it and resubmit. Only the designated file is
    # removed: --file can name any path, and a JSON object like package.json would parse.
    submission = _submission_path()
    if args.file not in (None, "-") and submission is not None and _same_file(Path(args.file), submission):
        submission.unlink(missing_ok=True)
    print(f"Completion accepted. Node state: {response['state']}; task status: {response['taskStatus']}.")


def _pending_complete_path() -> Path | None:
    credential = os.environ.get("AGENTFLOW_CREDENTIAL_FILE")
    return Path(credential).parent / "pending-complete.json" if credential else None


def _submission_path() -> Path | None:
    """Where the prompt tells an agent to write a result too long for a heredoc (prompts.submission_path)."""
    credential = os.environ.get("AGENTFLOW_CREDENTIAL_FILE")
    return Path(credential).parent / "submission.json" if credential else None


def _same_file(a: Path, b: Path) -> bool:
    try:
        return a.resolve() == b.resolve()
    except OSError:
        return False


def cmd_finding(args: argparse.Namespace) -> None:
    _require(args)
    finding = {
        "id": args.id, "severity": args.severity, "blocking": args.blocking, "file": args.file, "line": args.line,
        "title": args.title, "description": args.description, "recommendation": args.recommendation,
        "priorId": args.prior_id,
    }
    finding = {k: v for k, v in finding.items() if v is not None}
    response = _post(args, "finding", {"finding": finding})
    print(f"Finding recorded ({response['count']} pending for this run). Include them all in your final result or complete without findings to use these.")


def cmd_fail(args: argparse.Namespace) -> None:
    _require(args)
    response = _post(args, "fail", {"message": args.message})
    print(f"Failure recorded. Node state: {response['state']}.")


def cmd_exited(args: argparse.Namespace) -> None:
    _require(args)
    try:
        _post(args, "exited", {"exitCode": args.code})
    except SystemExit as exc:  # backend down at shutdown is not worth a stack trace
        print(exc, file=sys.stderr)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="flowctl", description="Signal AgentFlow from inside an agent session.")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("status", help="Show workflow state (all tasks, or the current task)")
    _common(p)
    p.add_argument("--json", action="store_true")
    p.set_defaults(func=cmd_status)

    p = sub.add_parser("question", help="Ask the human a question; node becomes WAITING_FOR_USER")
    _common(p)
    form = p.add_mutually_exclusive_group(required=True)
    form.add_argument("--message", "-m", help="The question (a flat question, with optional --choice)")
    form.add_argument("--file", "-f", help="Decision-tree question as a JSON file, or - to read it from stdin")
    p.add_argument("--choice", action="append", default=[], help="Selectable answer (repeat up to eight times)")
    p.set_defaults(func=cmd_question)

    p = sub.add_parser("heartbeat", help="Report agent responsiveness; does not change workflow state")
    _common(p)
    p.add_argument("--phase", default="working",
                   choices=["working", "investigating", "implementing", "testing", "reviewing"])
    p.set_defaults(func=cmd_heartbeat)

    p = sub.add_parser("running", help="Questions answered; node is RUNNING again")
    _common(p)
    p.set_defaults(func=cmd_running)

    p = sub.add_parser("complete", help="Submit the structured completion result")
    _common(p)
    p.add_argument("--file", "-f", help="JSON result file, or - to read JSON from stdin")
    p.add_argument("--json", help="Inline JSON result")
    p.add_argument("--summary", help="Short summary (when no file/json is given)")
    p.add_argument("--sha", help="Reviewed SHA (reviewers, when no file/json is given)")
    p.add_argument("--verdict", choices=["approved", "changes_required"])
    p.set_defaults(func=cmd_complete)

    p = sub.add_parser("finding", help="Record one review finding incrementally")
    _common(p)
    p.add_argument("--id")
    p.add_argument("--severity", choices=["info", "low", "medium", "high", "critical"], default="medium")
    p.add_argument("--blocking", action=argparse.BooleanOptionalAction, default=None)
    p.add_argument("--file")
    p.add_argument("--line", type=int)
    p.add_argument("--title", required=True)
    p.add_argument("--description")
    p.add_argument("--recommendation")
    p.add_argument("--prior-id", help="Id of your previous-round blocking finding this re-raises")
    p.set_defaults(func=cmd_finding)

    p = sub.add_parser("fail", help="Report that the agent cannot complete its task")
    _common(p)
    p.add_argument("--message", "-m", required=True)
    p.set_defaults(func=cmd_fail)

    p = sub.add_parser("exited", help="(internal) launcher reports the harness process exit code")
    _common(p)
    p.add_argument("--code", type=int, required=True)
    p.set_defaults(func=cmd_exited)
    return parser


def main(argv: list[str] | None = None) -> None:
    parser = build_parser()
    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
