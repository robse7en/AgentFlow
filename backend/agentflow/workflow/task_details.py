"""The parts of a task snapshot the operator may ask about from chat, filtered by what is allowed.

Pure and platform-neutral: a snapshot and the allowed sections (`slack.details`) go in, plain data
comes out. A section that is not allowed is never read, so anything built from this output
(a chat reply or a model's context) cannot contain it. The text is untrusted (agent, review and
Jira output) and is trimmed here; adapters must still escape it.

The Jira ticket is not part of an engine snapshot: callers that may show `ticket` add it as
`jiraIssue` (`Engine.jira_issue`), and only then read the file.
"""

from __future__ import annotations

import re
from typing import Any, Collection

SECTIONS = ("requirements", "findings", "errors", "cost", "activity", "pr", "ticket")

REQUIREMENT_TITLES = {
    "requirements": "Requirements", "acceptanceCriteria": "Acceptance criteria", "assumptions": "Assumptions",
    "implementationPlan": "Implementation plan", "risks": "Risks",
}
EXCERPT_LINES = 6
EXCERPT_CHARS = 600
ERROR_CHARS = 500
MAX_FINDINGS = 15
DEFAULT_ACTIVITY = 10
MAX_ACTIVITY = 30
DESCRIPTION_LINES = 15
DESCRIPTION_CHARS = 1000
LATEST_COMMENTS = 3
COMMENT_CHARS = 300
# jira-issue.md as services/jira_client._issue_markdown writes it.
_DESCRIPTION_HEADING = re.compile(r"\A\s*## Description[ \t]*\n")
_COMMENTS_HEADING = re.compile(r"^## Comments(?: \((\d+)\))?[ \t]*$", re.M)
_COMMENT_HEADING = re.compile(r"^### Comment \S+ - (.+?)[ \t]*$", re.M)
SEVERITY_ORDER = {"critical": 0, "high": 1, "medium": 2, "low": 3, "info": 4}


def _trim(text: str, chars: int, lines: int | None = None) -> str:
    kept = [line for line in text.strip().splitlines() if line.strip()]
    cut = lines is not None and len(kept) > lines
    text = "\n".join(kept[:lines] if lines is not None else kept)
    if len(text) > chars:
        text, cut = text[:chars].rstrip(), True
    return text + ("…" if cut else "")


def _requirements(snapshot: dict[str, Any]) -> dict[str, Any]:
    documents = [{"title": title, "excerpt": _trim(text, EXCERPT_CHARS, EXCERPT_LINES)}
                 for key, title in REQUIREMENT_TITLES.items()
                 if (text := (snapshot.get("requirements") or {}).get(key)) and text.strip()]
    # The files on disk are what is shown; they only count as approved while they still match.
    approved = snapshot.get("requirementsDigest")
    state = ("draft" if approved is None
             else "approved" if approved == snapshot.get("requirementsDraftDigest") else "changed since approval")
    return {"documents": documents, "state": state}


def _findings(snapshot: dict[str, Any]) -> dict[str, Any]:
    open_findings = [f for f in snapshot.get("findings") or [] if f.get("status") == "open"]
    open_findings.sort(key=lambda f: (not f.get("blocking"), SEVERITY_ORDER.get(f.get("severity"), 5)))
    shown = [{"severity": f.get("severity"), "blocking": bool(f.get("blocking")), "title": _trim(f.get("title") or "", 200),
              "file": f.get("file"), "line": f.get("line"), "type": f.get("type")}
             for f in open_findings[:MAX_FINDINGS]]
    return {"open": shown, "more": max(0, len(open_findings) - MAX_FINDINGS),
            "blocking": sum(1 for f in open_findings if f.get("blocking"))}


def _errors(snapshot: dict[str, Any]) -> dict[str, Any]:
    nodes = [{"label": n.get("label") or n.get("key"), "error": _trim(n["error"], ERROR_CHARS, EXCERPT_LINES)}
             for n in snapshot.get("nodes") or [] if n.get("error")]
    message = snapshot.get("statusMessage")
    return {"statusMessage": _trim(message, ERROR_CHARS, EXCERPT_LINES) if message else None, "nodes": nodes}


def _cost(snapshot: dict[str, Any]) -> dict[str, Any]:
    nodes = [{"label": n.get("label") or n.get("key"), "usage": n["usage"]}
             for n in snapshot.get("nodes") or [] if n.get("usage")]
    return {"total": snapshot.get("totalUsage"), "nodes": nodes, "budget": snapshot.get("budget")}


def _activity(snapshot: dict[str, Any], limit: int) -> dict[str, Any]:
    events = snapshot.get("events") or []  # oldest first
    return {"events": [{"timestamp": e.get("timestamp"), "level": e.get("level"), "message": _trim(e.get("message") or "", 300)}
                       for e in events[-limit:]]}


def _pr(snapshot: dict[str, Any]) -> dict[str, Any]:
    return {"url": snapshot.get("prUrl"), "createdAt": snapshot.get("prCreatedAt"), "mergedAt": snapshot.get("mergedAt"),
            "syncPaused": bool(snapshot.get("prSyncPaused"))}


def _ticket(snapshot: dict[str, Any]) -> dict[str, Any]:
    """The synced ticket's description and latest comments, trimmed. `comments` is None when they
    were not synced (off, or the fetch failed)."""
    issue = snapshot.get("jiraIssue")
    if not issue:
        return {"synced": False}
    markdown = issue.get("markdown") or ""
    body = markdown.split("\n", 1)[1] if markdown.startswith("# ") else markdown  # the title line
    # Tasks synced before comments were fetched have the description right after the title.
    body = _DESCRIPTION_HEADING.sub("", body, count=1)
    heading = _COMMENTS_HEADING.search(body)
    description = body[:heading.start()] if heading else body
    comments = None
    if heading and heading.group(1) is not None:
        parts = _COMMENT_HEADING.split(body[heading.end():])[1:]  # [by, text, by, text, ...]
        entries = list(zip(parts[::2], parts[1::2]))
        comments = {"count": int(heading.group(1)),
                    "latest": [{"by": _trim(by, 100), "excerpt": _trim(text, COMMENT_CHARS, EXCERPT_LINES)}
                               for by, text in entries[-LATEST_COMMENTS:]]}
    return {"synced": True, "syncedAt": issue.get("syncedAt"),
            "description": _trim(description, DESCRIPTION_CHARS, DESCRIPTION_LINES), "comments": comments}


def task_details(snapshot: dict[str, Any], allowed: Collection[str], sections: Collection[str] | None = None,
                 activity_limit: int = DEFAULT_ACTIVITY) -> dict[str, Any]:
    """`sections` (default: all) intersected with `allowed`, in `SECTIONS` order."""
    wanted = [s for s in SECTIONS if s in allowed and (sections is None or s in sections)]
    builders = {"requirements": _requirements, "findings": _findings, "errors": _errors, "cost": _cost,
                "activity": lambda snap: _activity(snap, max(1, min(activity_limit, MAX_ACTIVITY))),
                "pr": _pr, "ticket": _ticket}
    return {section: builders[section](snapshot) for section in wanted}
