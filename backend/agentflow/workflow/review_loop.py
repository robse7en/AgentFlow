"""Contract checks that keep the review loop from resolving anything silently.

From the second round on, a reviewer must rule on every one of its own prior blocking findings and
the implementer must answer every blocking finding it was given. Nothing is then marked fixed,
dropped or conceded without an explicit, recorded decision. A result that breaks these rules is
refused while the agent is still running, with a message it can act on, so it resubmits instead of
failing the run.
"""

from __future__ import annotations

from typing import Any

from .models import FINDING_RESPONSE_ACTIONS, PRIOR_FINDING_STATUSES, Finding

# A blocking finding raised this many times without being fixed stops the loop for a human.
REPEAT_ESCALATION = 3


def blocking_ids(findings: list[dict[str, Any]]) -> list[str]:
    return list(dict.fromkeys(str(f["id"]) for f in findings if f.get("blocking") and f.get("id")))


def _entries(raw: dict[str, Any], key: str) -> tuple[list[dict[str, Any]], list[str]]:
    value = raw.get(key)
    if value is None:
        return [], []
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        return [], [f'"{key}" must be a list of objects']
    return value, []


def _text(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def review_problems(raw: dict[str, Any], prior_blocking: list[str] | None, reported: list[Finding]) -> list[str]:
    """`prior_blocking` is None on a first review; `reported` is what will be stored, including
    findings recorded incrementally with `flowctl finding`."""
    problems: list[str] = []
    reraised: set[str] = set()
    for finding in reported:
        if not finding.prior_id:
            continue
        prior_id = finding.prior_id
        if prior_blocking is None:
            problems.append(f'finding "{finding.title}" has priorId {prior_id}, but there is no previous '
                            "review to re-raise from; remove priorId")
        elif prior_id not in prior_blocking:
            problems.append(f"priorId {prior_id} is not one of your previous blocking findings "
                            f"({', '.join(prior_blocking) or 'there were none'})")
        elif prior_id in reraised:
            problems.append(f"{prior_id} is re-raised more than once; report it as a single finding "
                            "(findings recorded with flowctl finding are already included)")
        else:
            reraised.add(prior_id)
            if finding.blocking is not True:
                problems.append(f're-raised {prior_id} must keep "blocking": true; if it no longer needs to block, '
                                'give it status "withdrawn" with evidence instead of re-raising it')
    if prior_blocking is None:
        return problems

    verdicts, problems_in_shape = _entries(raw, "priorFindings")
    problems += problems_in_shape
    ruled: dict[str, Any] = {}
    for verdict in verdicts:
        verdict_id = verdict.get("id")
        if verdict_id not in prior_blocking:
            problems.append(f"priorFindings entry {verdict_id!r} is not one of your previous blocking findings")
            continue
        if verdict_id in ruled:
            problems.append(f"{verdict_id} has more than one priorFindings entry")
            continue
        status = verdict.get("status")
        ruled[verdict_id] = status
        if status not in PRIOR_FINDING_STATUSES:
            problems.append(f"{verdict_id}: status must be one of {', '.join(PRIOR_FINDING_STATUSES)} (got {status!r})")
        if not _text(verdict.get("evidence")):
            problems.append(f"{verdict_id}: evidence is required (the file:line or test that shows it)")
    missing = [finding_id for finding_id in prior_blocking if finding_id not in ruled]
    if missing:
        problems.append(f"no verdict for prior blocking finding(s): {', '.join(missing)}")
    for finding_id, status in ruled.items():
        if status == "not_fixed" and finding_id not in reraised:
            problems.append(f'{finding_id} is "not_fixed" but is not re-reported in "findings" with "priorId": "{finding_id}"')
        elif status in ("fixed", "withdrawn") and finding_id in reraised:
            problems.append(f'{finding_id} is "{status}" but is also re-raised; use "not_fixed" if it is still broken')
    return problems


def review_rejection(problems: list[str], prior_blocking: list[str] | None) -> str:
    lines = ["Review result not accepted. This run is still active: fix the result and run flowctl complete again."]
    lines += [f"- {problem}" for problem in problems]
    if prior_blocking:
        lines += [
            'Every blocking finding from your previous review needs exactly one verdict in "priorFindings", e.g.:',
            f'  "priorFindings": [{{"id": "{prior_blocking[0]}", "status": "fixed | not_fixed | withdrawn", '
            '"evidence": "the file:line or test that shows it"}]',
            'A "not_fixed" finding must also be re-reported in "findings" with "priorId": "<old id>" and "blocking": true.',
        ]
    return "\n".join(lines)


def response_problems(raw: dict[str, Any], given: list[dict[str, Any]]) -> list[str]:
    """`given` is the consolidated finding list the implementer was asked to fix."""
    blocking = blocking_ids(given)
    known = {str(f.get("id")) for f in given}
    responses, problems = _entries(raw, "findingResponses")
    answered: set[str] = set()
    for response in responses:
        finding_id = response.get("id")
        if not isinstance(finding_id, str) or finding_id not in known:
            problems.append(f"findingResponses entry {finding_id!r} is not a finding from the last review")
            continue
        if finding_id in answered:
            problems.append(f"{finding_id} has more than one findingResponses entry")
            continue
        answered.add(finding_id)
        action = response.get("action")
        if action not in FINDING_RESPONSE_ACTIONS:
            problems.append(f"{finding_id}: action must be one of {', '.join(FINDING_RESPONSE_ACTIONS)} (got {action!r})")
        elif action == "deferred" and finding_id in blocking:
            problems.append(f'{finding_id} is blocking, so it cannot be "deferred": fix it, or dispute it with a rationale')
        if not _text(response.get("rationale")):
            problems.append(f"{finding_id}: rationale is required (what you changed and where, or why the finding is wrong)")
    missing = [finding_id for finding_id in blocking if finding_id not in answered]
    if missing:
        problems.append(f"no response for blocking finding(s): {', '.join(missing)}")
    return problems


def response_rejection(problems: list[str], given: list[dict[str, Any]]) -> str:
    example = (blocking_ids(given) or ["CODE-001"])[0]
    return "\n".join([
        "Implementation result not accepted. This run is still active: fix the result and run flowctl complete again.",
        *(f"- {problem}" for problem in problems),
        'Every blocking finding needs exactly one entry in "findingResponses", e.g.:',
        f'  "findingResponses": [{{"id": "{example}", "action": "fixed | disputed", '
        '"rationale": "what changed and where, or why the finding is wrong"}]',
        '"deferred" is allowed only for non-blocking findings.',
    ])
