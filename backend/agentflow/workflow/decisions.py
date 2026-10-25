"""Decision-tree agent questions: what `flowctl question` may send, and the one line the agent
gets back.

A question is flat (`--message` / `--choice`: text choices, the answer is typed back as is) or a
decision tree (`--file`: object choices, an optional `context` and `risk`, and at most one level
of follow-up questions). A tree is stored as validated in `agent_questions.decision`; its
top-level labels also go into `choices`, so readers that only know flat questions keep working.

The human answers with a path through the tree, and the server builds the line from the stored
tree, so no surface can send a choice that does not exist:

    2: Pin pydantic to v2                                   a top-level leaf
    1.1: Soft-delete, keep invoices → After they close      a branch and its follow-up option
    other: <free text>                                      free text on a tree question

The Rust core (`app/crates/core/src/engine/decisions.rs`) mirrors this module message for
message.
"""

from __future__ import annotations

import re
from typing import Any, Sequence

# Between a branch and its follow-up option in an answer line. Labels may not contain its glyph,
# so a line splits back into exactly the labels that were picked.
ANSWER_SEPARATOR = " → "
OTHER_PREFIX = "other: "

RISKS = ("low", "medium", "high")
MAX_CHOICES = 8
MIN_FOLLOW_UP_CHOICES = 2
LABEL_LIMIT = 250
CONTEXT_LIMIT = 500
FOLLOW_UP_MESSAGE_LIMIT = 500
CHOICE_TEXT_LIMITS = {"detail": 200, "warning": 120, "note": 300, "source": 200}
CHOICE_FIELDS = ("label", "recommended", "detail", "warning", "note", "source", "followUp")
FOLLOW_UP_FIELDS = ("message", "choices")

NO_SUCH_CHOICE = "That choice does not exist"
NEEDS_FOLLOW_UP = "That choice has a follow-up question; pick one of its options"


def _text(value: Any, field: str, limit: int, required: bool = False) -> str | None:
    """One printable line of at most `limit` characters after trimming; blank counts as absent."""
    if value is None:
        if required:
            raise ValueError(f"{field}: field required")
        return None
    if not isinstance(value, str):
        raise ValueError(f"{field}: must be a string")
    value = value.strip()
    if not value:
        if required:
            raise ValueError(f"{field}: cannot be empty")
        return None
    if len(value) > limit:
        raise ValueError(f"{field}: must be at most {limit} characters")
    if any(not char.isprintable() for char in value):
        raise ValueError(f"{field}: must be a single line")
    return value


def source_allowed(source: str) -> bool:
    """A repository-relative path (no `..` segment, drive letter, scheme or leading slash) or an
    https URL. It is only displayed, never fetched or opened by AgentFlow itself."""
    if source.startswith("https://"):
        return len(source) > len("https://") and not any(char.isspace() for char in source)
    if source.startswith(("/", "\\")) or ":" in source:
        return False
    return ".." not in re.split(r"[\\/]", source)


def _unknown(raw: dict[str, Any], allowed: Sequence[str], field: str) -> None:
    unknown = sorted(key for key in raw if key not in allowed)
    if unknown:
        raise ValueError(f"{field}: unknown field '{unknown[0]}'")


def _choice(raw: Any, field: str, top_level: bool) -> dict[str, Any]:
    if not isinstance(raw, dict):
        raise ValueError(f"{field}: must be an object")
    _unknown(raw, CHOICE_FIELDS, field)
    label = _text(raw.get("label"), f"{field}.label", LABEL_LIMIT, required=True)
    if ANSWER_SEPARATOR.strip() in label:
        raise ValueError(f"{field}.label: cannot contain {ANSWER_SEPARATOR.strip()}")
    recommended = raw.get("recommended")
    if recommended is not None and not isinstance(recommended, bool):
        raise ValueError(f"{field}.recommended: must be true or false")
    choice: dict[str, Any] = {"label": label}
    if recommended:
        choice["recommended"] = True
    for key, limit in CHOICE_TEXT_LIMITS.items():
        value = _text(raw.get(key), f"{field}.{key}", limit)
        if value is not None:
            choice[key] = value
    if "source" in choice:
        if "note" not in choice:
            raise ValueError(f"{field}.source: needs a note")
        if not source_allowed(choice["source"]):
            raise ValueError(f"{field}.source: must be a repository-relative path (no '..', drive letter "
                             "or leading /) or an https:// URL")
    follow_up = raw.get("followUp")
    if follow_up is not None:
        if not top_level:
            raise ValueError(f"{field}.followUp: follow-up options cannot ask another follow-up "
                             "(one level only)")
        choice["followUp"] = _follow_up(follow_up, f"{field}.followUp")
    return choice


def _level(raw: Any, field: str, minimum: int, top_level: bool) -> list[dict[str, Any]]:
    if not isinstance(raw, list):
        raise ValueError(f"{field}: must be a list")
    if not minimum <= len(raw) <= MAX_CHOICES:
        raise ValueError(f"{field}: must have {minimum} to {MAX_CHOICES} choices")
    choices = [_choice(item, f"{field}[{index}]", top_level) for index, item in enumerate(raw)]
    if len({choice["label"].casefold() for choice in choices}) != len(choices):
        raise ValueError(f"{field}: labels must be unique")
    if sum(1 for choice in choices if choice.get("recommended")) > 1:
        raise ValueError(f"{field}: at most one choice can be recommended")
    return choices


def _follow_up(raw: Any, field: str) -> dict[str, Any]:
    if not isinstance(raw, dict):
        raise ValueError(f"{field}: must be an object")
    _unknown(raw, FOLLOW_UP_FIELDS, field)
    message = _text(raw.get("message"), f"{field}.message", FOLLOW_UP_MESSAGE_LIMIT, required=True)
    return {"message": message, "choices": _level(raw.get("choices"), f"{field}.choices",
                                                  MIN_FOLLOW_UP_CHOICES, top_level=False)}


def is_tree(choices: Sequence[Any], context: Any, risk: Any) -> bool:
    """Object choices, a context or a risk make a decision tree; text choices alone a flat question."""
    return context is not None or risk is not None or any(isinstance(choice, dict) for choice in choices)


def clean_decision(choices: Sequence[Any], context: Any, risk: Any) -> dict[str, Any]:
    """The stored tree `{context, risk, choices}`, or a ValueError naming the field at fault."""
    if any(not isinstance(choice, dict) for choice in choices):
        raise ValueError("choices: use objects for every choice of a decision tree "
                         "(or only text choices for a flat question)")
    if risk is not None and risk not in RISKS:
        raise ValueError("risk: must be low, medium or high")
    return {
        "context": _text(context, "context", CONTEXT_LIMIT),
        "risk": risk,
        "choices": _level(list(choices), "choices", 0, top_level=True),
    }


def answer_line(choices: Sequence[str], decision: dict[str, Any] | None,
                path: Sequence[int] | None, text: str | None) -> str:
    """What is typed into the agent's pane for a path (0-based indexes) or free text. Flat
    questions get the choice text or the free text unchanged; the caller still applies
    `clean_answer` to the result."""
    if path is None:
        text = text or ""
        return OTHER_PREFIX + text if decision is not None else text
    if decision is None:
        if len(path) != 1 or not 0 <= path[0] < len(choices):
            raise ValueError(NO_SUCH_CHOICE)
        return choices[path[0]]
    level = decision.get("choices") or []
    if not 1 <= len(path) <= 2 or not 0 <= path[0] < len(level):
        raise ValueError(NO_SUCH_CHOICE)
    branch = level[path[0]]
    follow_up = branch.get("followUp")
    if len(path) == 1:
        if follow_up:
            raise ValueError(NEEDS_FOLLOW_UP)
        return f"{path[0] + 1}: {branch['label']}"
    options = follow_up.get("choices") if follow_up else None
    if not options or not 0 <= path[1] < len(options):
        raise ValueError(NO_SUCH_CHOICE)
    return f"{path[0] + 1}.{path[1] + 1}: {branch['label']}{ANSWER_SEPARATOR}{options[path[1]]['label']}"
