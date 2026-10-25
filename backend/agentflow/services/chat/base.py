"""The contract a chat platform implements (Slack today; Discord/Matrix could follow).

An adapter is transport plus formatting: it connects, sends and edits messages, delivers
slash commands, typed direct messages and answers to the service, and renders content in its
platform's markup.
Deciding what to announce, who may act, and what was already announced belongs to `ChatService`.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import Any, Awaitable, Callable, Literal

from ...workflow.notifications import Notice


@dataclass(frozen=True)
class AnswerPrompt:
    """Offer answer controls on a question notice. Controls carry the choice's index, never its
    text: the answer is always read back from the engine's copy of the question."""

    jira_key: str
    question_uid: str
    choices: tuple[str, ...]
    # A decision tree (workflow.decisions), only when question text may be posted. With
    # `branch`, the controls are that branch's follow-up options instead of the top-level choices.
    decision: dict[str, Any] | None = None
    branch: int | None = None


@dataclass(frozen=True)
class AnswerRequest:
    """Someone used an answer control: a choice (`choice_index`), a follow-up option (`path`:
    branch and option), "Change branch" (`back`), or free text (`text`)."""

    user_id: str
    channel: str
    jira_key: str
    question_uid: str
    choice_index: int | None = None
    text: str | None = None
    path: tuple[int, int] | None = None
    back: bool = False


@dataclass(frozen=True)
class MessageRequest:
    """Someone typed in a direct message with the app. `thread_id` is set for a reply in a thread;
    `text` is plain text, with the platform's markup already removed by the adapter."""

    user_id: str
    channel: str
    thread_id: str | None
    text: str


@dataclass(frozen=True)
class AnswerOffer:
    """Offer to send typed text as the answer to one open question. Nothing reaches the agent
    until the operator uses the control, which comes back as an `AnswerRequest` with `text`."""

    jira_key: str
    question_uid: str
    text: str


ButtonKind = Literal["new_task", "request_changes", "mark_pr", "run", "cancel", "mark_merged",
                     "review_requirements", "approve_requirements"]
# Buttons that open a form; the others act when clicked (after `confirm`, if set).
FORM_BUTTONS = frozenset({"new_task", "request_changes", "mark_pr"})


@dataclass(frozen=True)
class Button:
    """A control on a message. `kind` says what it does, `value` what it acts on:
    - "new_task" opens the create-task form, prefilled with ticket key `value` ("" for none);
    - "request_changes" / "mark_pr" open a text form for task `value` (`ChatHandlers.text_form`);
    - "run" starts a stage, `value` "KEY|node_key"; "cancel" / "mark_merged" act on task `value`;
      "review_requirements" posts task `value`'s requirements in full, followed by an
      "approve_requirements" button whose `value` "KEY|digest" names exactly the documents posted
      (all through `ChatHandlers.action`).
    `confirm` asks before acting."""

    kind: ButtonKind
    label: str
    value: str
    confirm: str | None = None


@dataclass(frozen=True)
class Reply:
    """A reply to a typed message: mrkdwn `text` (from the adapter's `render_*`), with an optional
    answer offer or buttons."""

    text: str
    offer: AnswerOffer | None = None
    buttons: tuple[Button, ...] = ()


@dataclass(frozen=True)
class ActionRequest:
    """Someone used a button that acts (any `Button` kind outside FORM_BUTTONS)."""

    user_id: str
    channel: str
    kind: str
    value: str


@dataclass(frozen=True)
class TaskForm:
    """What the create-task form starts with. Options for its searchable fields come from
    `ChatHandlers.options`."""

    jira_key: str | None
    repository: str | None       # preselected: the default repository, else the last one used
    search_tickets: bool         # Jira is configured, so the ticket field searches it
    implementation_mode: str     # the configured default, "direct" or "parallel"
    base_branch: str = "main"    # preselected; any other branch can still be chosen


@dataclass(frozen=True)
class OptionsRequest:
    """A searchable form field wants options: `field` is "ticket", "repository", "branch" or
    "project". `repository` is the one chosen so far (branches and folders depend on it)."""

    user_id: str
    field: str
    query: str
    repository: str | None = None


@dataclass(frozen=True)
class TaskDraft:
    """A submitted create-task form."""

    user_id: str
    jira_key: str
    repository_path: str
    base_branch: str
    project_relative_path: str | None = None
    implementation_mode: str | None = None
    additional_context: str | None = None


@dataclass(frozen=True)
class TextForm:
    """A one-field form: feedback for the Architect, or a pull request URL. `intro` is plain text."""

    kind: str          # the button kind that opened it
    value: str         # the task key
    title: str         # at most 24 characters
    intro: str
    label: str
    multiline: bool
    max_length: int


@dataclass(frozen=True)
class TextSubmission:
    user_id: str
    kind: str
    value: str
    text: str


# (value, label) for one choice in a searchable field.
Option = tuple[str, str]

# (user_id, command text) -> reply shown only to that user.
CommandHandler = Callable[[str, str], Awaitable[Reply]]
# -> the reply to post, or None to stay silent (e.g. the message is not from the operator's DM).
MessageHandler = Callable[[MessageRequest], Awaitable[Reply | None]]
# -> None when the answer was delivered, a prompt to show in the notice instead (a branch's
# follow-up, or the top-level choices again), else a short reason to post back in the thread.
AnswerHandler = Callable[[AnswerRequest], Awaitable["str | AnswerPrompt | None"]]
# -> None when the action was done, else a short reason to post back.
ActionHandler = Callable[[ActionRequest], Awaitable[str | None]]
# (user_id, ticket key to prefill) -> the form to open, or a reason it cannot be opened.
NewTaskHandler = Callable[[str, str | None], Awaitable[TaskForm | str]]
OptionsHandler = Callable[[OptionsRequest], Awaitable[list[Option]]]
# -> errors by field ("ticket", "repository", "branch", "project"); {} means accepted, and the
# task is created afterwards (the result is posted as a direct message).
SubmitHandler = Callable[[TaskDraft], Awaitable[dict[str, str]]]
# (user_id, button kind, task key) -> the form to open, or a reason it cannot be opened.
TextFormHandler = Callable[[str, str, str], Awaitable[TextForm | str]]
# -> {"text": error} to show on the field; {} means accepted (the result is posted in the task's thread).
TextSubmitHandler = Callable[[TextSubmission], Awaitable[dict[str, str]]]


@dataclass(frozen=True)
class ChatHandlers:
    """What the service does with each kind of inbound interaction."""

    command: CommandHandler
    message: MessageHandler
    answer: AnswerHandler
    action: ActionHandler
    new_task: NewTaskHandler
    options: OptionsHandler
    submit_task: SubmitHandler
    text_form: TextFormHandler
    submit_text: TextSubmitHandler


class PermanentChatError(Exception):
    """The platform rejected the content itself (too long, malformed); retrying cannot help."""


class ChatAdapter(ABC):
    platform: str
    # Shown in the task's event log, e.g. "Human replied to question … via Slack".
    display_name: str

    @abstractmethod
    async def connect(self, handlers: ChatHandlers) -> None:
        """Open the connection; raise if the credentials are rejected."""

    @abstractmethod
    async def disconnect(self) -> None: ...

    @abstractmethod
    async def open_direct(self, user_id: str) -> str:
        """The direct-message channel with `user_id`."""

    @abstractmethod
    async def send(self, channel: str, text: str, thread_id: str | None = None,
                   prompt: AnswerPrompt | None = None, buttons: tuple[Button, ...] = ()) -> str:
        """Post a message (as a thread reply when `thread_id` is set), with answer controls when
        `prompt` is given, else `buttons`; returns its id. Raises PermanentChatError for rejected content."""

    @abstractmethod
    async def update(self, channel: str, message_id: str, text: str) -> None:
        """Replace a message's content, removing any answer controls."""

    # Rendering. `url` links to the task in the AgentFlow UI.
    @abstractmethod
    def render_title(self, snapshot: dict[str, Any], url: str) -> str: ...

    @abstractmethod
    def render_notice(self, notice: Notice, answerable: bool = False) -> str: ...

    @abstractmethod
    def render_resolved(self, question: dict[str, Any] | None, include_text: bool) -> str:
        """A question notice once it no longer awaits an answer (`question` None: unknown)."""

    @abstractmethod
    def render_status(self, snapshot: dict[str, Any], url: str) -> str: ...

    @abstractmethod
    def render_list(self, tasks: list[dict[str, Any]]) -> str: ...

    @abstractmethod
    def render_details(self, snapshot: dict[str, Any], details: dict[str, Any], url: str) -> str:
        """`details` is `workflow.task_details.task_details` output: only allowed sections."""

    @abstractmethod
    def render_digest(self, tasks: list[dict[str, Any]], spend: dict[str, Any] | None) -> str:
        """Active tasks and what they need; `spend` (today's API spend) only when cost is allowed."""

    @abstractmethod
    def render_created(self, snapshot: dict[str, Any], notes: tuple[str, ...] = ()) -> str:
        """The first reply in the thread of a task created from chat. `notes`: plain-text reasons a
        READY stage cannot be started from chat."""

    @abstractmethod
    def render_requirements(self, jira_key: str, documents: list[tuple[str, str]]) -> list[str]:
        """Requirement documents (title, untrusted text) in full, never shortened, as the messages
        to post in order. An approval is bound to exactly this text."""

    @abstractmethod
    def render_ai_answer(self, text: str) -> str:
        """A model's answer (untrusted), marked as one."""

    @abstractmethod
    def render_note(self, text: str) -> str:
        """Plain text from AgentFlow (a result or refusal), made safe for the platform's markup."""

    @abstractmethod
    def render_creation_failed(self, jira_key: str, detail: str | None) -> str:
        """`detail` (untrusted: may quote git output) only when errors may be posted."""

    @abstractmethod
    def render_help(self) -> str: ...
