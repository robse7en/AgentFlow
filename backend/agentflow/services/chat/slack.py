"""Slack over Socket Mode: an outbound WebSocket, so AgentFlow still listens only on loopback.

Needs the optional dependencies: `pip install -e "backend[slack]"`. They are imported when the
adapter is created, so AgentFlow runs without them while Slack is disabled.
"""

from __future__ import annotations

import json
import logging
import re
from datetime import datetime, timezone
from typing import Any

from ...workflow.models import MAX_ANSWER_LENGTH
from ...workflow.notifications import Notice
from ...workflow.states import TaskStatus
from .base import (ActionRequest, AnswerHandler, AnswerOffer, AnswerPrompt, AnswerRequest, Button, ChatAdapter,
                   ChatHandlers, CommandHandler, FORM_BUTTONS, MessageHandler, MessageRequest, Option, OptionsRequest,
                   PermanentChatError, Reply, TaskDraft, TaskForm, TextForm, TextSubmission)

log = logging.getLogger(__name__)

COMMAND = "/agentflow"
MAX_LISTED = 25
FINISHED = {TaskStatus.MERGED, TaskStatus.CANCELLED}
CHOICE_ACTION = "agentflow_answer_choice_"
FOLLOW_ACTION = "agentflow_answer_follow_"
BACK_ACTION = "agentflow_answer_back"
FOLLOW_UP_BLOCK = "followup"  # the "You picked …" section shown with a branch's follow-up
RISK_LABELS = {"low": "Low risk", "medium": "Medium risk", "high": "High risk"}
REPLY_ACTION = "agentflow_answer_reply"
TYPED_ACTION = "agentflow_answer_typed"
MODAL_CALLBACK = "agentflow_answer"
NEW_TASK_ACTION = "agentflow_new_task"
RUN_ACTION = "agentflow_run"
TASK_FORM_CALLBACK = "agentflow_task_form"
TEXT_FORM_CALLBACK = "agentflow_text_form"
WORKING_REACTION = "eyes"  # on a DM message while AgentFlow works on it
DONE_REACTION = "white_check_mark"  # replaces it once the reply is posted
# The create-task form's searchable fields: block id -> action id. Options load over the socket.
SEARCHABLE = {"ticket": "agentflow_task_ticket", "repository": "agentflow_task_repository",
              "branch": "agentflow_task_branch", "project": "agentflow_task_project"}
BUTTON_ACTIONS = {"new_task": NEW_TASK_ACTION, "run": RUN_ACTION, "request_changes": "agentflow_request_changes",
                  "mark_pr": "agentflow_mark_pr", "cancel": "agentflow_cancel", "mark_merged": "agentflow_mark_merged",
                  "review_requirements": "agentflow_review_requirements",
                  "approve_requirements": "agentflow_approve_requirements"}
ACTION_KINDS = {action_id: kind for kind, action_id in BUTTON_ACTIONS.items()}
PRIMARY_BUTTONS = {"run", "approve_requirements"}
# What an acting button's message says once the action is done ({label} for "run"). Buttons not
# listed keep their message (Review requirements can be used again).
DONE = {"run": "*{label} started.*", "cancel": "*Task cancelled.*", "mark_merged": "*Marked merged.*",
        "approve_requirements": "*Requirements approved.*"}
# Slack's Block Kit limits: section text 3000 characters, button label 75, button value 2000,
# option text 75 and value 150, confirm dialog text 300, input hint 2000.
SECTION_LIMIT = 2900
BUTTON_LABEL_LIMIT = 75
BUTTON_VALUE_LIMIT = 2000
OPTION_TEXT_LIMIT = 75
OPTION_VALUE_LIMIT = 150
CONTEXT_LIMIT = 3000
CONTEXT_ELEMENTS = 10  # elements in one context block
# One message of a requirements review. Sections are cut at a line break in their second half, so
# this stays under 50 sections (and under the 40,000-character message text).
REQUIREMENTS_MESSAGE_LIMIT = 20000
# Errors about the content itself: the same message would be rejected again on retry.
PERMANENT_ERRORS = {"invalid_blocks", "invalid_blocks_format", "msg_too_long", "invalid_arguments", "no_text"}


def escape(text: str) -> str:
    """Slack control characters; without this, untrusted text could post links or mentions."""
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


_MARKUP = re.compile(r"<([^<>|]*)(?:\|([^<>]*))?>")


def unslack(text: str) -> str:
    """Typed text as the person wrote it. Slack delivers `&`, `<` and `>` escaped and wraps links
    and mentions in `<target|label>`; an answer must reach the agent without either."""
    text = _MARKUP.sub(lambda m: m.group(2) or m.group(1).lstrip("!"), text)
    return text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")


def _heading(snapshot: dict[str, Any]) -> str:
    title = snapshot.get("jiraTitle")
    return f"*{escape(snapshot['jiraKey'])}*" + (f" · {escape(title)}" if title else "")


def render_title(snapshot: dict[str, Any], url: str) -> str:
    return f"{_heading(snapshot)}\n{escape(str(snapshot.get('phase') or snapshot.get('status')))} · <{url}|Open in AgentFlow>"


def _quote(text: str) -> list[str]:
    return [f"> {line}" for line in escape(text).splitlines() or [""]]


def render_notice(notice: Notice, answerable: bool = False) -> str:
    lines = [escape(notice.summary)]
    if notice.question:
        lines += _quote(notice.question)
    if notice.choices and not answerable:  # answerable notices show the choices as buttons
        lines.append("Choices: " + " / ".join(f"`{escape(choice).replace('`', chr(39))}`" for choice in notice.choices))
    if notice.key.startswith("question:"):
        lines.append("Answer below, or in AgentFlow." if answerable else "Answer it in AgentFlow.")
    return "\n".join(lines)


def render_resolved(question: dict[str, Any] | None, include_text: bool) -> str:
    if question and question.get("answered"):
        lines = [f"*Answered:* {escape(question.get('answer') or '')}" if include_text else "*Answered.*"]
    else:
        lines = ["*No longer waiting for an answer.*"]
    if include_text and question:
        lines += _quote(question.get("message") or "")
    return "\n".join(lines)


def chunks(text: str, limit: int = SECTION_LIMIT) -> list[str]:
    """Split rendered mrkdwn into pieces Slack accepts in one section, preferring line breaks and
    never cutting an escape sequence such as `&amp;` in half."""
    pieces = []
    while len(text) > limit:
        cut = text.rfind("\n", limit // 2, limit)
        if cut <= 0:
            cut = limit
            amp = text.rfind("&", cut - 5, cut)
            if amp != -1 and ";" not in text[amp:cut]:
                cut = amp
        pieces.append(text[:cut])
        text = text[cut:].lstrip("\n")
    if text or not pieces:
        pieces.append(text)
    return pieces


def _sections(text: str) -> list[dict[str, Any]]:
    return [{"type": "section", "text": {"type": "mrkdwn", "text": piece}} for piece in chunks(text)]


def _label(choice: str) -> str:
    return choice if len(choice) <= BUTTON_LABEL_LIMIT else choice[:BUTTON_LABEL_LIMIT - 1] + "…"


def notice_blocks(text: str, prompt: AnswerPrompt) -> list[dict[str, Any]]:
    """The notice with one button per choice plus "Reply…". A button's value names the choice by
    index only; the service reads the answer text back from the engine."""
    return _sections(text) + answer_blocks(prompt)


def _cut(text: str, limit: int = SECTION_LIMIT) -> str:
    """Rendered mrkdwn shortened to `limit`, never cutting an escape sequence such as `&amp;`."""
    if len(text) <= limit:
        return text
    cut = limit - 1
    amp = text.rfind("&", cut - 5, cut)
    if amp != -1 and ";" not in text[amp:cut]:
        cut = amp
    return text[:cut] + "…"


def _choice_notes(number: str, choice: dict[str, Any]) -> str | None:
    """A choice's consequences and agent's note as one context line, e.g. `3: … · ⚠ …`."""
    parts = [escape(choice["detail"])] if choice.get("detail") else []
    if choice.get("warning"):
        parts.append(f"⚠ {escape(choice['warning'])}")
    if choice.get("note"):
        source = f" ({escape(choice['source'])})" if choice.get("source") else ""
        parts.append(f"⚠ Note on \"{escape(choice['label'])}\": {escape(choice['note'])}{source}")
    return f"{number}: " + " · ".join(parts) if parts else None


def answer_blocks(prompt: AnswerPrompt) -> list[dict[str, Any]]:
    """The answer controls under a question notice. A flat question gets one button per choice; a
    decision tree its numbered branches (the agent's pick in green, `›` where a follow-up comes
    next) with their consequences in a context block, or, with `prompt.branch`, that branch's
    follow-up options and "‹ Change branch". Buttons carry the path (`1`, `0.1`, `back`), never
    text: the service builds the answer from the engine's copy of the question."""
    reference = f"{prompt.jira_key}|{prompt.question_uid}"

    def button(action_id: str, label: str, value: str, primary: bool = False) -> dict[str, Any]:
        element = {"type": "button", "action_id": action_id, "text": {"type": "plain_text", "text": _label(label)},
                   "value": f"{reference}|{value}"}
        if primary:
            element["style"] = "primary"
        return element

    blocks: list[dict[str, Any]] = []
    lines: list[str | None] = []
    decision = prompt.decision
    if decision is None:
        buttons = [button(f"{CHOICE_ACTION}{index}", choice, str(index)) for index, choice in enumerate(prompt.choices)]
    elif prompt.branch is None:
        choices = decision.get("choices") or []
        buttons = [button(f"{CHOICE_ACTION}{index}", f"{index + 1} · {choice['label']}" + (" ›" if choice.get("followUp") else ""),
                          str(index), bool(choice.get("recommended"))) for index, choice in enumerate(choices)]
        heading = [f"*{RISK_LABELS[decision['risk']]}*"] if decision.get("risk") in RISK_LABELS else []
        if decision.get("context"):
            heading.append(escape(decision["context"]))
        legend = []
        if any(choice.get("recommended") for choice in choices):
            legend.append("Green = the agent's pick")
        if any(choice.get("followUp") for choice in choices):
            legend.append("› asks a follow-up")
        lines += [" · ".join(heading), " · ".join(legend)]
        lines += [_choice_notes(str(index + 1), choice) for index, choice in enumerate(choices)]
    else:
        branch = decision["choices"][prompt.branch]
        follow_up = branch["followUp"]
        blocks.append({"type": "section", "block_id": FOLLOW_UP_BLOCK, "text": {"type": "mrkdwn", "text": _cut(
            f"You picked *{escape(branch['label'])}*, one more choice:\n*{escape(follow_up['message'])}*")}})
        options = follow_up["choices"]
        buttons = [button(f"{FOLLOW_ACTION}{index}", f"{prompt.branch + 1}.{index + 1} · {option['label']}",
                          f"{prompt.branch}.{index}", bool(option.get("recommended")))
                   for index, option in enumerate(options)]
        buttons.append(button(BACK_ACTION, "‹ Change branch", "back"))
        lines.append("Green = the agent's pick" if any(o.get("recommended") for o in options) else None)
        lines += [_choice_notes(f"{prompt.branch + 1}.{index + 1}", option) for index, option in enumerate(options)]
    buttons.append({"type": "button", "action_id": REPLY_ACTION,
                    "text": {"type": "plain_text", "text": "Reply…"}, "value": f"{reference}|reply"})
    context = [{"type": "mrkdwn", "text": _cut(line)} for line in lines if line][:CONTEXT_ELEMENTS]
    if context:
        blocks.append({"type": "context", "block_id": "notes", "elements": context})
    return blocks + [{"type": "actions", "block_id": "answer", "elements": buttons}]


def parse_path(choice: str) -> tuple[int, int] | None:
    """A follow-up option's `branch.option` from a button value."""
    match = re.fullmatch(r"([0-9]{1,3})\.([0-9]{1,3})", choice)
    return (int(match.group(1)), int(match.group(2))) if match else None


OFFER_PROMPT = "Send this to the agent as your answer?"
OFFER_TOO_LONG = "That is too long to send from here; use Reply… on the question, or answer in AgentFlow."


def offer_blocks(offer: AnswerOffer) -> list[dict[str, Any]]:
    """The typed text quoted back with a "Send as answer" button. Text that does not fit in a
    button value (the service checked the answer limit, not Slack's) gets a pointer instead."""
    value = f"{offer.jira_key}|{offer.question_uid}|{offer.text}"
    if len(value) > BUTTON_VALUE_LIMIT:
        return _sections(OFFER_TOO_LONG)
    button = {"type": "button", "action_id": TYPED_ACTION, "style": "primary",
              "text": {"type": "plain_text", "text": "Send as answer"}, "value": value}
    return _sections("\n".join([OFFER_PROMPT, *_quote(offer.text)])) + [{"type": "actions", "block_id": "offer", "elements": [button]}]


def reply_blocks(reply: Reply) -> list[dict[str, Any]] | None:
    """The reply's text, then its buttons, then any answer offer; None when plain text will do."""
    if not reply.buttons and reply.offer is None:
        return _sections(reply.text) if len(reply.text) > SECTION_LIMIT else None
    blocks = _sections(reply.text)
    if reply.buttons:
        blocks.append(buttons_block(reply.buttons))
    if reply.offer is not None:
        blocks += offer_blocks(reply.offer)
    return blocks


def _plain(text: str, limit: int) -> dict[str, Any]:
    return {"type": "plain_text", "text": text if len(text) <= limit else text[:limit - 1] + "…"}


def buttons_block(buttons: tuple[Button, ...]) -> dict[str, Any]:
    elements = []
    for button in buttons:
        element = {"type": "button", "action_id": BUTTON_ACTIONS[button.kind],
                   "text": _plain(button.label, BUTTON_LABEL_LIMIT)}
        if button.value:  # Slack rejects an empty value; a click then carries none
            element["value"] = button.value[:BUTTON_VALUE_LIMIT]
        if button.kind in PRIMARY_BUTTONS:
            element["style"] = "primary"
        elif button.kind == "cancel":
            element["style"] = "danger"
        if button.confirm:
            element["confirm"] = {"title": _plain("Are you sure?", 100), "text": _plain(button.confirm, 300),
                                  "confirm": _plain("Yes", 30), "deny": _plain("Cancel", 30)}
        elements.append(element)
    # Distinct action ids per message: a second "run" button gets its index as a suffix.
    for index, element in enumerate(elements):
        element["action_id"] += f"_{index}"
    return {"type": "actions", "block_id": "buttons", "elements": elements}


def option(value: str, label: str) -> dict[str, Any] | None:
    """A select option, or None when the value is too long for Slack to carry."""
    if not value or len(value) > OPTION_VALUE_LIMIT:
        return None
    # Long labels (paths) keep their end, which is the part that tells them apart.
    text = label if len(label) <= OPTION_TEXT_LIMIT else "…" + label[-(OPTION_TEXT_LIMIT - 1):]
    return {"text": {"type": "plain_text", "text": text}, "value": value}


def options_payload(options: list[Option]) -> list[dict[str, Any]]:
    return [o for o in (option(value, label) for value, label in options) if o is not None]


def task_modal(form: TaskForm) -> dict[str, Any]:
    def searchable(block: str, label: str, placeholder: str, initial: str | None = None, hint: str | None = None):
        element = {"type": "external_select", "action_id": SEARCHABLE[block], "min_query_length": 0,
                   "placeholder": _plain(placeholder, 150)}
        if initial and (initial_option := option(initial, initial)):
            element["initial_option"] = initial_option
        block_ = {"type": "input", "block_id": block, "label": _plain(label, 2000), "element": element}
        if hint:
            block_["hint"] = _plain(hint, 2000)
        return block_

    def text_input(block: str, label: str, initial: str | None = None, multiline: bool = False, max_length: int = 500):
        element = {"type": "plain_text_input", "action_id": "value", "multiline": multiline, "max_length": max_length}
        if initial:
            element["initial_value"] = initial
        return {"type": "input", "block_id": block, "label": _plain(label, 2000), "element": element}

    ticket = (searchable("ticket", "Jira ticket", "Search Jira or type a key", form.jira_key)
              if form.search_tickets else text_input("ticket", "Jira key", form.jira_key, max_length=64))
    modes = {"direct": "Direct: one implementer", "parallel": "Parallel workers"}
    mode_options = [{"text": _plain(text, OPTION_TEXT_LIMIT), "value": value} for value, text in modes.items()]
    project = searchable("project", "Project sub-directory", "Search the repository's folders or type one",
                         hint="Relative to the repository root; choose the repository first. Leave empty, "
                              "or choose Whole repository, for all of it.")
    project["optional"] = True
    context = text_input("context", "Additional context for the Architect", multiline=True, max_length=CONTEXT_LIMIT)
    context["optional"] = True
    return {
        "type": "modal", "callback_id": TASK_FORM_CALLBACK,
        "title": _plain("New AgentFlow task", 24),
        "submit": _plain("Create", 24), "close": _plain("Cancel", 24),
        "blocks": [
            ticket,
            searchable("repository", "Repository", "Search past repositories or paste a path", form.repository,
                       "A folder on the AgentFlow machine."),
            searchable("branch", "Base branch", "Search branches or type one", form.base_branch,
                       hint="Choose the repository first; the list shows its local and remote branches."),
            project,
            {"type": "input", "block_id": "mode", "label": _plain("Implementation route", 2000),
             "element": {"type": "static_select", "action_id": "value", "options": mode_options,
                         "initial_option": next(o for o in mode_options if o["value"] == form.implementation_mode)}},
            context,
        ],
    }


def _selected(values: dict[str, Any], block: str) -> str:
    """The value of a form field, whatever kind of input it is."""
    for element in (values.get(block) or {}).values():
        chosen = element.get("selected_option")
        return (chosen or {}).get("value") or element.get("value") or ""
    return ""


def task_draft(user_id: str, values: dict[str, Any]) -> TaskDraft:
    return TaskDraft(user_id, _selected(values, "ticket"), _selected(values, "repository"), _selected(values, "branch"),
                     _selected(values, "project") or None, _selected(values, "mode") or None,
                     _selected(values, "context") or None)


def parse_offer(value: str) -> tuple[str, str, str] | None:
    parts = value.split("|", 2)  # the answer itself may contain "|"
    return (parts[0], parts[1], parts[2]) if len(parts) == 3 and all(parts) else None


def answer_modal(question_blocks: list[dict[str, Any]], metadata: dict[str, str]) -> dict[str, Any]:
    return {
        "type": "modal", "callback_id": MODAL_CALLBACK,
        "title": {"type": "plain_text", "text": "Answer the agent"},
        "submit": {"type": "plain_text", "text": "Send"},
        "close": {"type": "plain_text", "text": "Cancel"},
        "private_metadata": json.dumps(metadata),
        "blocks": question_blocks + [{
            "type": "input", "block_id": "answer",
            "label": {"type": "plain_text", "text": "Your answer (one line)"},
            "element": {"type": "plain_text_input", "action_id": "value", "multiline": False,
                        "max_length": MAX_ANSWER_LENGTH},
        }],
    }


def parse_reference(value: str) -> tuple[str, str, str] | None:
    parts = value.split("|")
    return (parts[0], parts[1], parts[2]) if len(parts) == 3 and all(parts) else None


def _needs_you(item: dict[str, Any]) -> str:
    questions = item.get("questions") or 0
    suffix = f" ({questions} question{'s' if questions != 1 else ''})" if questions else ""
    return f"{escape(item.get('label') or item.get('nodeKey') or '')}: {escape(item.get('action') or '')}{suffix}"


def render_status(snapshot: dict[str, Any], url: str) -> str:
    lines = [_heading(snapshot), f"Phase: {escape(str(snapshot.get('phase') or snapshot.get('status')))}"]
    attention = snapshot.get("attention") or []
    if attention:
        lines.append("Needs you:")
        lines += [f"• {_needs_you(item)}" for item in attention]
    lines.append(f"<{url}|Open in AgentFlow>")
    return "\n".join(lines)


def render_list(tasks: list[dict[str, Any]]) -> str:
    live = [task for task in tasks if task.get("status") not in FINISHED]
    if not live:
        return "No active AgentFlow tasks."
    lines = []
    for task in live[:MAX_LISTED]:
        line = f"• {_heading(task)} · {escape(str(task.get('phase') or task.get('status')))}"
        if task.get("attention"):
            line += f" · needs you: {_needs_you(task['attention'][0])}"
        lines.append(line)
    if len(live) > MAX_LISTED:
        lines.append(f"…and {len(live) - MAX_LISTED} more")
    return "\n".join(lines)


def _money(value: float | None) -> str:
    return "unknown" if value is None else f"${value:,.2f}"


def _usage(usage: dict[str, Any]) -> str:
    runs = usage.get("runs") or 0
    parts = [f"{runs} run{'s' if runs != 1 else ''}"]
    if usage.get("billedCostUsd") is not None:
        parts.append(f"billed {_money(usage['billedCostUsd'])}")
    if usage.get("estimatedCostUsd") is not None:
        parts.append(f"estimated {_money(usage['estimatedCostUsd'])}")
    if usage.get("totalTokens"):
        parts.append(f"{usage['totalTokens']:,} tokens")
    return " · ".join(parts)


def _spend(label: str, group: dict[str, Any]) -> str:
    line = f"{label}: {_money(group.get('knownUsd'))}"
    if group.get("limitUsd"):
        line += f" of {_money(group['limitUsd'])}"
    if group.get("unknownRuns"):
        line += f" ({group['unknownRuns']} metered run{'s' if group['unknownRuns'] != 1 else ''} without a cost yet)"
    return line + (" · near the limit" if group.get("warning") else "")


def _age(since: str | None) -> str:
    try:
        started = datetime.fromisoformat((since or "").replace("Z", "+00:00"))
    except ValueError:
        return ""
    minutes = int((datetime.now(timezone.utc) - started.astimezone(timezone.utc)).total_seconds() // 60)
    if minutes < 60:
        return f" for {max(minutes, 0)}m"
    return f" for {minutes // 60}h" if minutes < 24 * 60 else f" for {minutes // (24 * 60)}d"


def _code(text: str) -> str:
    return f"`{escape(text).replace('`', chr(39))}`"


def render_details(snapshot: dict[str, Any], details: dict[str, Any], url: str) -> str:
    lines = [_heading(snapshot), f"Phase: {escape(str(snapshot.get('phase') or snapshot.get('status')))}"]
    if "requirements" in details:
        requirements = details["requirements"]
        lines.append(f"\n*Requirements* ({requirements['state']})")
        if not requirements["documents"]:
            lines.append("None written yet.")
        for document in requirements["documents"]:
            lines.append(f"_{escape(document['title'])}_")
            lines += _quote(document["excerpt"])
    if "findings" in details:
        findings = details["findings"]
        lines.append(f"\n*Open findings* ({findings['blocking']} blocking)")
        if not findings["open"]:
            lines.append("None open.")
        for finding in findings["open"]:
            where = ""
            if finding.get("file"):
                location = finding["file"] + (f":{finding['line']}" if finding.get("line") else "")
                where = f" · {_code(location)}"
            tag = escape(str(finding.get("severity") or "")) + (", blocking" if finding["blocking"] else "")
            lines.append(f"• [{tag}] {escape(finding['title'])}{where}")
        if findings["more"]:
            lines.append(f"…and {findings['more']} more")
    if "errors" in details:
        errors = details["errors"]
        lines.append("\n*Errors*")
        if not errors["statusMessage"] and not errors["nodes"]:
            lines.append("None.")
        if errors["statusMessage"]:
            lines.append("Task:")
            lines += _quote(errors["statusMessage"])
        for node in errors["nodes"]:
            lines.append(f"{escape(node['label'])}:")
            lines += _quote(node["error"])
    if "cost" in details:
        cost = details["cost"]
        lines.append("\n*Cost*")
        lines.append(f"Total: {_usage(cost['total'])}" if cost["total"] else "No runs yet.")
        lines += [f"• {escape(node['label'])}: {_usage(node['usage'])}" for node in cost["nodes"]]
        if cost.get("budget"):
            lines.append(_spend("Ticket API spend", cost["budget"]["task"]))
    if "activity" in details:
        lines.append("\n*Recent activity*")
        events = details["activity"]["events"]
        if not events:
            lines.append("Nothing yet.")
        for event in events:
            when = escape((event.get("timestamp") or "")[5:16].replace("T", " "))
            flag = "⚠ " if event.get("level") in ("warning", "error") else ""
            lines.append(f"• {when} {flag}{escape(event['message'])}")
    if "pr" in details:
        pr = details["pr"]
        lines.append("\n*Pull request*")
        if pr["url"]:
            lines.append(escape(pr["url"]))  # plain text: Slack links it, but it cannot hide another target
            dates = [f"{label} {escape(value[:10])}" for label, value in
                     (("created", pr.get("createdAt")), ("merged", pr.get("mergedAt"))) if value]
            if dates:
                lines.append(", ".join(dates).capitalize())
        else:
            lines.append("None recorded yet.")
        if pr["syncPaused"]:
            lines.append("GitHub PR sync is paused for this task.")
    if "ticket" in details:
        ticket = details["ticket"]
        synced = escape((ticket.get("syncedAt") or "")[:16].replace("T", " "))
        lines.append("\n*Jira ticket*" + (f" (synced {synced} UTC)" if synced else ""))
        if not ticket["synced"]:
            lines.append("Not synced from Jira yet.")
        else:
            lines += _quote(ticket["description"] or "(no description)")
            comments = ticket["comments"]
            if comments is None:
                lines.append("Comments were not synced.")
            elif not comments["count"]:
                lines.append("No comments.")
            else:
                lines.append(f"_Latest comments_ ({len(comments['latest'])} of {comments['count']})")
                for comment in comments["latest"]:
                    lines.append(f"{escape(comment['by'])}:")
                    lines += _quote(comment["excerpt"])
    lines.append(f"\n<{url}|Open in AgentFlow>")
    return "\n".join(lines)


def render_digest(tasks: list[dict[str, Any]], spend: dict[str, Any] | None) -> str:
    live = [task for task in tasks if task.get("status") not in FINISHED]
    waiting = sum(1 for task in live if task.get("attention"))
    lines = [f"*AgentFlow digest*: {len(live)} active task{'s' if len(live) != 1 else ''}, "
             f"{waiting} waiting on you"]
    for task in sorted(live, key=lambda task: not task.get("attention"))[:MAX_LISTED]:
        line = f"• {_heading(task)} · {escape(str(task.get('phase') or task.get('status')))}"
        if task.get("attention"):
            item = task["attention"][0]
            line += f" · needs you{_age(item.get('since'))}: {_needs_you(item)}"
        lines.append(line)
    if len(live) > MAX_LISTED:
        lines.append(f"…and {len(live) - MAX_LISTED} more")
    if spend is not None:
        lines.append(_spend("Today's API spend", spend))
    return "\n".join(lines)


def render_created(snapshot: dict[str, Any], notes: tuple[str, ...] = ()) -> str:
    return "\n".join([f"Created from Slack: branch {_code(str(snapshot.get('branchName') or ''))} "
                      f"from {_code(str(snapshot.get('baseBranch') or ''))}.", *map(escape, notes)])


def text_modal(form: TextForm) -> dict[str, Any]:
    return {
        "type": "modal", "callback_id": TEXT_FORM_CALLBACK,
        "title": _plain(form.title, 24), "submit": _plain("Send", 24), "close": _plain("Cancel", 24),
        "private_metadata": json.dumps({"kind": form.kind, "value": form.value}),
        "blocks": [
            {"type": "section", "text": _plain(form.intro, SECTION_LIMIT)},
            {"type": "input", "block_id": "text", "label": _plain(form.label, 2000),
             "element": {"type": "plain_text_input", "action_id": "value", "multiline": form.multiline,
                         "max_length": form.max_length}},
        ],
    }


def render_requirements(jira_key: str, documents: list[tuple[str, str]]) -> list[str]:
    """Every document in full, split only across messages (each well within Slack's block limits),
    never shortened: an approval is bound to exactly this text."""
    count = len(documents)
    messages = [f"*Requirements for {escape(jira_key)}*, in full: {count} document{'s' if count != 1 else ''}. "
                "The approve button follows the last one."]
    for title, text in documents:
        messages += chunks(f"*{escape(title)}*\n{escape(text.strip())}", REQUIREMENTS_MESSAGE_LIMIT)
    return messages


def render_ai_answer(text: str) -> str:
    """Escaped like any untrusted text (no links or mentions), with Markdown bold turned into Slack's."""
    body = re.sub(r"\*\*(.+?)\*\*", r"*\1*", escape(text))
    return f"{body}\n_AI answer from AgentFlow's data; it can be wrong. Check AgentFlow before acting on it._"


def render_creation_failed(jira_key: str, detail: str | None) -> str:
    lines = [f"*Could not create {escape(jira_key)}.*"]
    lines += _quote(detail) if detail else ["The reason is in the AgentFlow backend log."]
    return "\n".join(lines)


def render_help(command: str | None = COMMAND) -> str:
    return ("`list`: active tasks and what needs you\n"
            "`status KEY`: one task's phase and pending actions\n"
            "`details KEY [requirements|findings|errors|cost|activity|pr|ticket]`: task details, as allowed by slack.details\n"
            "`activity KEY [N]`: the task's last N events\n"
            "`digest`: every active task, what is waiting on you and today's spend\n"
            "`new [KEY]` (or \"create a new task\"): create a task\n"
            "`run [KEY] [STAGE]`, `changes [KEY]`, `approve [KEY]`, `cancel [KEY]`, `pr [KEY]`: a button to "
            "start a stage, request requirement changes, review and approve the requirements, cancel the task, "
            "or mark its PR created or merged\n"
            "(`new` and those five need slack.allowActions.)\n"
            "`ask QUESTION`: a plain-words answer from AgentFlow's data, if slack.ask is enabled\n"
            f"Type these in your DM with AgentFlow{f', or after `{command}`' if command else ''}. In a task's thread the key is "
            "optional, and other text can be sent as the answer to the agent's open question.")


ANSWER_CONTROL = re.compile(rf"^({re.escape(CHOICE_ACTION)}\d+|{re.escape(FOLLOW_ACTION)}\d+|"
                            rf"{re.escape(BACK_ACTION)}|{re.escape(REPLY_ACTION)})$")


class SlackAdapter(ChatAdapter):
    platform = "slack"
    display_name = "Slack"

    def __init__(self, bot_token: str, app_token: str, command: str | None = COMMAND) -> None:
        from slack_bolt.async_app import AsyncApp
        from slack_sdk.http_retry.builtin_async_handlers import AsyncRateLimitErrorRetryHandler
        from slack_sdk.web.async_client import AsyncWebClient

        self._app_token = app_token
        self._command = command
        self._client = AsyncWebClient(token=bot_token,
                                      retry_handlers=[AsyncRateLimitErrorRetryHandler(max_retry_count=2)])
        self._app = AsyncApp(client=self._client)
        self._handler = None
        self._on_command: CommandHandler | None = None
        self._on_message: MessageHandler | None = None
        self._on_answer: AnswerHandler | None = None
        self._on_action = self._on_new_task = self._on_options = self._on_submit_task = None
        self._on_text_form = self._on_submit_text = None

        async def _command(ack, command, respond, client):
            # Slack expects the ack within ~3 s; the reply may take longer (list may read Jira).
            await ack()
            user_id, text = command.get("user_id", ""), unslack(command.get("text", ""))
            words = text.split()
            try:
                if words and words[0].lower() == "new":
                    reason = await self._open_task_form(client, command.get("trigger_id", ""), user_id,
                                                        words[1] if len(words) > 1 else None)
                    reply = Reply(escape(reason)) if reason else None
                else:
                    reply = (await self._on_command(user_id, text) if self._on_command
                             else Reply("AgentFlow is still connecting."))
            except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                log.exception("Slack command failed")
                reply = Reply("AgentFlow could not answer that; check the backend log.")
            if reply:
                await respond(text=reply.text, response_type="ephemeral", blocks=reply_blocks(reply))

        if command:  # none: the app has no slash command; commands are typed in the DM
            self._app.command(command)(_command)

        @self._app.action(re.compile(rf"^{NEW_TASK_ACTION}_\d+$"))
        async def _new_task_button(ack, body, action, client):
            await ack()
            channel = (body.get("channel") or {}).get("id", "")
            message = body.get("message") or {}
            try:
                reason = await self._open_task_form(client, body.get("trigger_id", ""),
                                                    (body.get("user") or {}).get("id", ""), action.get("value") or None)
            except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                log.exception("Could not open the Slack task form")
                reason = "AgentFlow could not open the form; check the backend log."
            if reason and channel:
                await self._post_quietly(channel, escape(reason), message.get("thread_ts"))

        @self._app.options(re.compile(rf"^({'|'.join(map(re.escape, SEARCHABLE.values()))})$"))
        async def _options(ack, body):
            field = next((block for block, action_id in SEARCHABLE.items() if action_id == body.get("action_id")), "")
            values = ((body.get("view") or {}).get("state") or {}).get("values") or {}
            request = OptionsRequest((body.get("user") or {}).get("id", ""), field, body.get("value") or "",
                                     _selected(values, "repository") or None)
            try:
                options = await self._on_options(request) if self._on_options else []
            except Exception:  # noqa: BLE001 - an empty list; details in the backend log
                log.exception("Slack option lookup failed")
                options = []
            await ack(options=options_payload(options))

        @self._app.view(TASK_FORM_CALLBACK)
        async def _task_submitted(ack, body, view):
            # Answered within Slack's ~3 s: errors show on the form's fields, and creation runs afterwards.
            draft = task_draft((body.get("user") or {}).get("id", ""), (view.get("state") or {}).get("values") or {})
            try:
                errors = (await self._on_submit_task(draft) if self._on_submit_task
                          else {"ticket": "AgentFlow is still connecting; try again shortly."})
            except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                log.exception("Slack task form failed")
                errors = {"ticket": "AgentFlow could not check this form; see the backend log."}
            if errors:
                await ack(response_action="errors", errors=errors)
            else:
                await ack()

        acting = "|".join(re.escape(action_id) for kind, action_id in BUTTON_ACTIONS.items() if kind not in FORM_BUTTONS)

        @self._app.action(re.compile(rf"^({acting})_\d+$"))
        async def _acting_button(ack, body, action):
            await ack()
            kind = ACTION_KINDS[action.get("action_id", "").rsplit("_", 1)[0]]
            channel = (body.get("channel") or {}).get("id", "")
            message = body.get("message") or {}
            request = ActionRequest((body.get("user") or {}).get("id", ""), channel, kind, action.get("value", ""))
            try:
                reply = (await self._on_action(request) if self._on_action
                         else "AgentFlow is still connecting; try again shortly.")
            except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                log.exception("Slack %s action failed", kind)
                reply = "Not done: AgentFlow could not do it. Check AgentFlow."
            if reply is None and kind in DONE and message.get("ts"):
                # Keep the message, replace its buttons with what happened.
                label = ((action.get("text") or {}).get("text") or "").removeprefix("Run ") or "Stage"
                sections = [block["text"]["text"] for block in message.get("blocks", [])
                            if block.get("type") == "section" and isinstance(block.get("text"), dict)]
                try:
                    await self.update(channel, message["ts"], "\n".join([*sections, DONE[kind].format(label=escape(label))]))
                except Exception:  # noqa: BLE001 - cosmetic; the engine refuses a repeated action anyway
                    log.warning("Could not update a Slack %s button", kind, exc_info=True)
            elif reply and channel:
                await self._post_quietly(channel, escape(reply), message.get("thread_ts") or message.get("ts"))

        text_forms = "|".join(re.escape(BUTTON_ACTIONS[kind]) for kind in FORM_BUTTONS if kind != "new_task")

        @self._app.action(re.compile(rf"^({text_forms})_\d+$"))
        async def _text_form_button(ack, body, action, client):
            await ack()
            kind = ACTION_KINDS[action.get("action_id", "").rsplit("_", 1)[0]]
            channel = (body.get("channel") or {}).get("id", "")
            message = body.get("message") or {}
            try:
                # trigger_id expires within ~3 s; the service's check is a quick lookup.
                form = (await self._on_text_form((body.get("user") or {}).get("id", ""), kind, action.get("value", ""))
                        if self._on_text_form else "AgentFlow is still connecting; try again shortly.")
                if not isinstance(form, str):
                    await client.views_open(trigger_id=body.get("trigger_id", ""), view=text_modal(form))
                    return
            except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                log.exception("Could not open a Slack %s form", kind)
                form = "AgentFlow could not open the form; check the backend log."
            if channel:
                await self._post_quietly(channel, escape(form), message.get("thread_ts") or message.get("ts"))

        @self._app.view(TEXT_FORM_CALLBACK)
        async def _text_submitted(ack, body, view):
            try:
                metadata = json.loads(view.get("private_metadata") or "{}")
                text = view["state"]["values"]["text"]["value"]["value"] or ""
            except (ValueError, KeyError, TypeError):
                log.warning("Ignored a malformed Slack form submission")
                await ack()
                return
            submission = TextSubmission((body.get("user") or {}).get("id", ""), str(metadata.get("kind", "")),
                                        str(metadata.get("value", "")), text)
            try:
                errors = (await self._on_submit_text(submission) if self._on_submit_text
                          else {"text": "AgentFlow is still connecting; try again shortly."})
            except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                log.exception("Slack form submission failed")
                errors = {"text": "AgentFlow could not check this form; see the backend log."}
            if errors:
                await ack(response_action="errors", errors=errors)
            else:
                await ack()

        @self._app.event("message")
        async def _message(event):
            # Only people typing in a DM. Edits, joins, file shares and every bot message
            # (including this app's own replies) carry a subtype or bot_id; answering those would loop.
            if event.get("bot_id") or event.get("subtype") or event.get("channel_type") != "im":
                return
            request = MessageRequest(event.get("user", ""), event.get("channel", ""), event.get("thread_ts"),
                                     unslack(event.get("text") or ""))
            # 👀 on the message while AgentFlow works on it (bots cannot show "is typing" outside
            # RTM), swapped for ✅ once the reply is posted.
            ts = event.get("ts")
            seen = await self._react("reactions_add", request.channel, ts, WORKING_REACTION)
            replied = False
            try:
                try:
                    reply = await self._on_message(request) if self._on_message else None
                except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
                    log.exception("Slack message failed")
                    reply = Reply("AgentFlow could not answer that; check the backend log.")
                if reply is not None:
                    try:
                        await self._post_reply(request.channel, request.thread_id, reply)
                        replied = True
                    except Exception:  # noqa: BLE001 - nothing more to tell the user through Slack
                        log.warning("Could not post a reply to Slack", exc_info=True)
            finally:
                if seen:
                    await self._react("reactions_remove", request.channel, ts, WORKING_REACTION)
                    if replied:
                        await self._react("reactions_add", request.channel, ts, DONE_REACTION)

        @self._app.action(TYPED_ACTION)
        async def _send_typed(ack, body, action):
            await ack()
            offer = parse_offer(action.get("value", ""))
            if offer is None:
                return
            jira_key, question_uid, text = offer
            channel = (body.get("channel") or {}).get("id", "")
            message = body.get("message") or {}
            sent = await self._deliver(AnswerRequest((body.get("user") or {}).get("id", ""), channel, jira_key,
                                                     question_uid, text=text),
                                       message.get("thread_ts") or message.get("ts"))
            if sent and message.get("ts"):
                try:  # drop the button so the same text cannot be sent twice
                    await self.update(channel, message["ts"], "\n".join(["*Sent as your answer.*", *_quote(text)]))
                except Exception:  # noqa: BLE001 - cosmetic; the engine refuses a second answer anyway
                    log.warning("Could not update a sent Slack answer offer", exc_info=True)

        @self._app.action(ANSWER_CONTROL)
        async def _answer_control(ack, body, action, client):
            await ack()  # before anything slow: delivering an answer runs several zellij calls
            reference = parse_reference(action.get("value", ""))
            if reference is None:
                return
            jira_key, question_uid, choice = reference
            channel = (body.get("channel") or {}).get("id", "")
            message = body.get("message") or {}
            thread = message.get("thread_ts") or message.get("ts")
            if action.get("action_id") == REPLY_ACTION:
                # trigger_id expires within ~3 s, so open the modal before any lookup; the answer
                # itself is authorized when it is submitted.
                question = [block for block in message.get("blocks", []) if block.get("type") == "section"]
                await client.views_open(trigger_id=body["trigger_id"], view=answer_modal(
                    question, {"key": jira_key, "uid": question_uid, "channel": channel, "thread": thread or ""}))
                return
            user = (body.get("user") or {}).get("id", "")
            if choice.isdigit():
                request = AnswerRequest(user, channel, jira_key, question_uid, choice_index=int(choice))
            elif choice == "back":
                request = AnswerRequest(user, channel, jira_key, question_uid, back=True)
            elif (path := parse_path(choice)) is not None:
                request = AnswerRequest(user, channel, jira_key, question_uid, path=path)
            else:
                return
            await self._deliver(request, thread, message)

        @self._app.view(MODAL_CALLBACK)
        async def _answer_submitted(ack, body, view):
            await ack()  # close the modal; problems are posted in the task's thread instead
            try:
                metadata = json.loads(view.get("private_metadata") or "{}")
                text = view["state"]["values"]["answer"]["value"]["value"] or ""
            except (ValueError, KeyError, TypeError):
                log.warning("Ignored a malformed Slack answer submission")
                return
            await self._deliver(AnswerRequest((body.get("user") or {}).get("id", ""), metadata.get("channel", ""),
                                              metadata.get("key", ""), metadata.get("uid", ""), text=text),
                                metadata.get("thread") or None)

    async def _deliver(self, request: AnswerRequest, thread: str | None, message: dict[str, Any] | None = None) -> bool:
        """True when the answer was delivered; otherwise the reason is posted in the thread. A
        decision tree's branch (or "Change branch") instead swaps the controls of `message`, the
        notice clicked, so the follow-up step needs no state of its own."""
        try:
            reply = (await self._on_answer(request) if self._on_answer
                     else "AgentFlow is still connecting; try again shortly.")
        except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
            log.exception("Slack answer failed")
            reply = "Not sent: AgentFlow could not deliver the answer. Check AgentFlow."
        if isinstance(reply, AnswerPrompt):
            reply = await self._show_prompt(request.channel, message or {}, reply)
        if reply and request.channel:
            try:
                await self.send(request.channel, escape(reply), thread)
            except Exception:  # noqa: BLE001 - nothing more to tell the user through Slack
                log.warning("Could not post an answer result to Slack", exc_info=True)
        return reply is None

    async def _show_prompt(self, channel: str, message: dict[str, Any], prompt: AnswerPrompt) -> str | None:
        """Edit the notice in place: its question text, then `prompt`'s controls. None when shown."""
        if not message.get("ts"):
            return "Not shown: answer it in AgentFlow."
        sections = [block for block in message.get("blocks") or []
                    if block.get("type") == "section" and block.get("block_id") != FOLLOW_UP_BLOCK]
        try:
            await self._call(self._client.chat_update, channel=channel, ts=message["ts"],
                             text=message.get("text") or "AgentFlow question", blocks=sections + answer_blocks(prompt))
        except Exception:  # noqa: BLE001 - reported without internals; details in the backend log
            log.warning("Could not show a Slack follow-up question", exc_info=True)
            return "Not shown: Slack refused the follow-up. Answer it in AgentFlow."
        return None

    async def _open_task_form(self, client, trigger_id: str, user_id: str, jira_key: str | None) -> str | None:
        """Open the create-task form; None when opened, else the reason it was not. trigger_id
        expires within ~3 s, so nothing slow may run before views_open."""
        form = (await self._on_new_task(user_id, jira_key) if self._on_new_task
                else "AgentFlow is still connecting; try again shortly.")
        if isinstance(form, str):
            return form
        await client.views_open(trigger_id=trigger_id, view=task_modal(form))
        return None

    async def _post_quietly(self, channel: str, text: str, thread: str | None) -> None:
        try:
            await self.send(channel, text, thread)
        except Exception:  # noqa: BLE001 - nothing more to tell the user through Slack
            log.warning("Could not post to Slack", exc_info=True)

    async def _react(self, method: str, channel: str, ts: str | None, name: str) -> bool:
        """Add or remove a reaction; True when Slack accepted it. Never raises: the reaction is
        a courtesy, and an app installed before reactions:write simply goes without."""
        if not ts:
            return False
        try:
            await getattr(self._client, method)(channel=channel, timestamp=ts, name=name)
            return True
        except Exception as exc:  # noqa: BLE001 - e.g. missing_scope until the app is reinstalled
            log.warning("Could not update the Slack reaction (%s): %s", method, exc)
            return False

    async def _post_reply(self, channel: str, thread: str | None, reply: Reply) -> None:
        await self._call(self._client.chat_postMessage, channel=channel, text=reply.text, blocks=reply_blocks(reply),
                         thread_ts=thread, unfurl_links=False, unfurl_media=False)

    async def connect(self, handlers: ChatHandlers) -> None:
        from slack_bolt.adapter.socket_mode.async_handler import AsyncSocketModeHandler

        self._on_command, self._on_message, self._on_answer = handlers.command, handlers.message, handlers.answer
        self._on_action, self._on_new_task = handlers.action, handlers.new_task
        self._on_options, self._on_submit_task = handlers.options, handlers.submit_task
        self._on_text_form, self._on_submit_text = handlers.text_form, handlers.submit_text
        await self._client.auth_test()  # fail fast on a bad bot token
        self._handler = AsyncSocketModeHandler(self._app, self._app_token)
        await self._handler.connect_async()

    async def disconnect(self) -> None:
        if self._handler is not None:
            handler, self._handler = self._handler, None
            await handler.close_async()

    async def open_direct(self, user_id: str) -> str:
        response = await self._client.conversations_open(users=user_id)
        return response["channel"]["id"]

    async def _call(self, method, **kwargs):
        from slack_sdk.errors import SlackApiError

        try:
            return await method(**kwargs)
        except SlackApiError as exc:
            code = exc.response.get("error") if exc.response is not None else None
            if code in PERMANENT_ERRORS:
                raise PermanentChatError(code) from exc
            raise

    async def send(self, channel: str, text: str, thread_id: str | None = None,
                   prompt: AnswerPrompt | None = None, buttons: tuple[Button, ...] = ()) -> str:
        if prompt:
            blocks = notice_blocks(text, prompt)
        elif buttons:
            blocks = _sections(text) + [buttons_block(buttons)]
        else:
            blocks = _sections(text) if len(text) > SECTION_LIMIT else None
        response = await self._call(self._client.chat_postMessage, channel=channel, text=text, blocks=blocks,
                                    thread_ts=thread_id, unfurl_links=False, unfurl_media=False)
        return response["ts"]

    async def update(self, channel: str, message_id: str, text: str) -> None:
        # Explicit blocks replace the old ones, which removes any answer buttons.
        await self._call(self._client.chat_update, channel=channel, ts=message_id, text=text, blocks=_sections(text))

    def render_title(self, snapshot: dict[str, Any], url: str) -> str:
        return render_title(snapshot, url)

    def render_notice(self, notice: Notice, answerable: bool = False) -> str:
        return render_notice(notice, answerable)

    def render_resolved(self, question: dict[str, Any] | None, include_text: bool) -> str:
        return render_resolved(question, include_text)

    def render_status(self, snapshot: dict[str, Any], url: str) -> str:
        return render_status(snapshot, url)

    def render_list(self, tasks: list[dict[str, Any]]) -> str:
        return render_list(tasks)

    def render_details(self, snapshot: dict[str, Any], details: dict[str, Any], url: str) -> str:
        return render_details(snapshot, details, url)

    def render_digest(self, tasks: list[dict[str, Any]], spend: dict[str, Any] | None) -> str:
        return render_digest(tasks, spend)

    def render_created(self, snapshot: dict[str, Any], notes: tuple[str, ...] = ()) -> str:
        return render_created(snapshot, notes)

    def render_creation_failed(self, jira_key: str, detail: str | None) -> str:
        return render_creation_failed(jira_key, detail)

    def render_requirements(self, jira_key: str, documents: list[tuple[str, str]]) -> list[str]:
        return render_requirements(jira_key, documents)

    def render_ai_answer(self, text: str) -> str:
        return render_ai_answer(text)

    def render_note(self, text: str) -> str:
        return escape(text)

    def render_help(self) -> str:
        return render_help(self._command)
