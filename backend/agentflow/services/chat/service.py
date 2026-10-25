"""Chat notifications, read-only commands and question answering for the one AgentFlow operator.

The service listens to every task snapshot the engine publishes, but only queues it; a single
worker does all chat I/O, so a slow or unreachable chat platform never delays or fails a
workflow action. Each task gets one thread in the operator's direct messages. What was
already announced is stored in `chat_threads`, so a restart repeats nothing, and a restart
catches up on anything that became true while chat was down.

Answers go through `engine.answer_question`, the same path as the browser, so its validation
and staleness checks apply. They are accepted only from the operator, only in their direct
messages, and choices are resolved by index against the engine's copy of the question.

Slash commands and messages typed in the operator's DM share one command router. A reply in a
task's thread is about that task, so it needs no key. Typed text is never delivered to an agent
as it is: in a thread with an open question it is offered back with a control to send it.

Tasks can be created from a form (`allowActions: create`). The form only does cheap checks while
the platform waits; `engine.create_task` runs afterwards, and its result opens the task's thread
(through the worker, like every other thread) or, on failure, is posted as a direct message.
A READY stage can be started from a button (`allowActions: run`), never one that needs API launch
approval or is over budget; the engine re-checks READY-only under its lock (`ready_only`).
Requirements can be sent back to the Architect (`requestChanges`), a task cancelled (`cancel`) and
its pull request marked created or merged (`markPr`). A typed command only ever posts the button;
acting always takes a click, with a confirmation where it cannot be undone from chat.
"""

from __future__ import annotations

import asyncio
import dataclasses
import logging
import re
import time
from collections import deque
from datetime import datetime, timedelta
from typing import Any, Callable, Collection
from urllib.parse import quote

from pydantic import ValidationError
from sqlalchemy import func, select

from ... import diagnostics
from ...config import SlackConfig
from ...database.models import ChatThread, Task
from ...services.git_workspace import GitError
from ...services.jira_client import build_service as build_jira_service
from ...workflow import default_workflow as wf
from ...workflow.budgets import daily_spend, launch_blocker
from ...workflow.decisions import NEEDS_FOLLOW_UP, NO_SUCH_CHOICE
from ...workflow.engine import NotFound, WorkflowEngine, WorkflowError
from ...workflow.models import (MAX_ANSWER_LENGTH, MAX_FEEDBACK_LENGTH, MAX_PR_URL_LENGTH, CreateTaskRequest,
                                clean_answer, clean_pr_url)
from ...workflow.notifications import LIVE_STATUSES, notices
from ...workflow.states import NodeState, TaskStatus
from ...workflow.task_details import DEFAULT_ACTIVITY, ERROR_CHARS, REQUIREMENT_TITLES, SECTIONS, task_details
from .ask import MAX_QUESTION, AskError, HarnessAsker
from .base import (ActionRequest, AnswerOffer, AnswerPrompt, AnswerRequest, Button, ChatAdapter, ChatHandlers,
                   MessageRequest, Option, OptionsRequest, PermanentChatError, Reply, TaskDraft, TaskForm, TextForm,
                   TextSubmission)

log = logging.getLogger(__name__)

MAX_RETRY_DELAY = 300.0
# Commands that act: typed, they only post a button (with a confirmation where it matters).
ACTION_COMMANDS = {"run", "changes", "approve", "cancel", "pr"}
# First words that make a typed message a command (anything else may be an answer, a question for
# the model, or gets help). "new" is matched separately, and only as "new" or "new KEY".
COMMANDS = {"list", "status", "details", "activity", "digest", "help", "ask", *ACTION_COMMANDS}
# allowActions entry and wording for each command that acts.
ACTION_SETTINGS = {"run": ("run", "Starting stages"), "changes": ("requestChanges", "Requesting changes"),
                   "approve": ("approveRequirements", "Approving requirements"),
                   "cancel": ("cancel", "Cancelling tasks"), "pr": ("markPr", "Marking pull requests")}
# Engine refusals for actions that may be posted as they are: fixed text about the task's state.
# Anything else (git, zellij or Jira output) is reported generically; details stay in the log.
POSTABLE_ACTION_ERRORS = ("Task already reached", "Task must have a PR created", "Task must be Ready for PR",
                          "PR URL must", "Feedback", "Requirements gate is not waiting", "Maximum requirement revisions",
                          "Task no longer accepts", "Task is not marked", "Requirements changed since",
                          "Refresh and review", "Requirements already approved", "A nonempty", "Stop active agents")
POSTABLE_ACTION_ENDINGS = ("is no longer ready to start", "is not waiting for approval")
# Requirements longer than this are reviewed in AgentFlow: posting them would take dozens of messages.
MAX_REVIEW_CHARS = 60000
DIGEST = re.compile(r"^[0-9a-f]{64}$")
# The daily digest's clock is checked this often rather than slept on for a day: a sleeping
# laptop pauses the event loop's clock, so one long sleep could post hours late.
DIGEST_POLL_SECONDS = 60.0


def next_digest_at(now: datetime, at: str) -> datetime:
    """The first `at` ("HH:MM", on `now`'s clock) strictly after `now`."""
    hour, minute = map(int, at.split(":"))
    due = now.replace(hour=hour, minute=minute, second=0, microsecond=0)
    return due if due > now else due + timedelta(days=1)
MAX_OPTIONS = 100  # Slack's cap for a select's options
BRANCH_CACHE_SECONDS = 60.0
JIRA_KEY = re.compile(r"^[A-Za-z][A-Za-z0-9_]*-\d+$")
# Plain-words requests for the create-task button: "create a new task", "new task for ABC-12",
# "can you make a task?". The whole message must match, so a longer sentence is still a question
# for the model (or an answer in a thread).
NEW_TASK_PHRASE = re.compile(
    r"^(?:(?:please|can you|could you)\s+)?(?:new|(?:create|make|start|open|add)(?:\s+(?:me\s+)?an?)?(?:\s+new)?)"
    r"\s+task(?:\s+(?:for\s+)?([A-Za-z][A-Za-z0-9_]*-\d+))?(?:,?\s+please)?\s*[.!?]*$", re.IGNORECASE)
STARTED_STATES = {NodeState.RUNNING, NodeState.WAITING_FOR_USER, NodeState.WAITING_FOR_APPROVAL, NodeState.COMPLETED}

# Why a launch did not start, as the engine records it on the node: fixed text, safe to post.
POSTABLE_LAUNCH_REASONS = {
    "Global agent capacity reached; start again when a slot is free",
    "API launch needs approval: review the selected harness and model, and enter a reason",
}

# Engine refusals that may be shown in chat as they are. Anything else (e.g. a zellij failure,
# whose message carries command output) is reported generically; details stay in the log.
POSTABLE_ANSWER_ERRORS = {
    "This question has already been answered",
    "The agent is no longer waiting for this answer",
    "Question is not attached to an active agent run",
    "The waiting agent pane is unavailable",
    "The waiting agent pane is no longer active",
    "The Zellij session for this agent is unavailable",
    "Answer must be a single line",
    "Answer cannot be empty",
    f"Answer must be at most {MAX_ANSWER_LENGTH} characters",
}


class ChatService:
    def __init__(self, engine: WorkflowEngine, adapter: ChatAdapter, settings: SlackConfig,
                 retry_delay: float = 5.0) -> None:
        self.engine = engine
        self.adapter = adapter
        self.settings = settings
        self.retry_delay = retry_delay
        self.channel: str | None = None
        # jira key -> latest published snapshot (None = read it fresh). Newer publishes overwrite
        # older ones, so a burst of updates for one task costs one round of chat calls.
        self._pending: dict[str, dict[str, Any] | None] = {}
        self._record_silently: set[str] = set()
        # Tasks created from chat whose thread has not been opened yet.
        self._created: set[str] = set()
        self._background: set[asyncio.Task] = set()
        # repository path -> (fetched at, branches), so typing in the branch field stays fast.
        self._branches: dict[str, tuple[float, list[str]]] = {}
        # repository path -> (fetched at, folders), for the project sub-directory field.
        self._folders: dict[str, tuple[float, list[str]]] = {}
        self._attempts: dict[str, int] = {}
        self._wake = asyncio.Event()
        self._unsubscribe = None
        self._worker: asyncio.Task | None = None
        self._digest_task: asyncio.Task | None = None
        # Local wall-clock time (naive): "09:00" stays 09:00 across a daylight-saving change.
        self._now: Callable[[], datetime] = datetime.now
        self.digest_poll_seconds = DIGEST_POLL_SECONDS
        # Free-text questions: one at a time, and the last few turns per thread (memory only).
        self._asker = HarnessAsker(engine.config, settings.ask) if settings.ask.enabled else None
        self._ask_lock = asyncio.Lock()
        self._ask_history: dict[str, deque[tuple[str, str]]] = {}

    # lifecycle ------------------------------------------------------------------------
    def start(self) -> None:
        """Subscribe before connecting so nothing published meanwhile is missed."""
        self._unsubscribe = self.engine.broadcaster.subscribe(self._on_broadcast)
        self._queue_catch_up()
        self._worker = asyncio.create_task(self._run(), name=f"chat-{self.adapter.platform}")
        if self._asker and (problem := self._asker.problem()):
            log.error("Slack questions are enabled but cannot be answered: %s", problem)
        if self.settings.digest_at:
            self._digest_task = asyncio.create_task(self._digest_loop(self.settings.digest_at),
                                                    name=f"chat-digest-{self.adapter.platform}")

    def _queue_catch_up(self) -> None:
        """Queue every task, so anything that became true while chat was off is announced.

        A task with no chat record that is already finished (e.g. the first time chat is
        enabled) is recorded silently instead of DMed as history. This is decided from its
        status now, before connecting: a task that fails while Slack is still connecting is
        judged by what it was at startup, and still announced."""
        platform = self.adapter.platform
        with self.engine.db.session() as session:
            rows = session.execute(select(Task.jira_key, Task.status, ChatThread.id).outerjoin(
                ChatThread, (ChatThread.task_id == Task.id) & (ChatThread.platform == platform))).all()
        for key, status, thread in rows:
            self._pending.setdefault(key, None)
            if thread is None and status not in LIVE_STATUSES:
                self._record_silently.add(key)

    async def stop(self) -> None:
        if self._unsubscribe:
            self._unsubscribe()
        for task in (self._worker, self._digest_task):
            if task:
                task.cancel()
                try:
                    await task
                except (asyncio.CancelledError, Exception):  # noqa: BLE001 - shutting down
                    pass
        try:
            await self.adapter.disconnect()
        except Exception:  # noqa: BLE001 - shutting down
            log.debug("chat disconnect failed", exc_info=True)

    async def _run(self) -> None:
        await self._connect()
        self._wake.set()
        while True:
            await self._wake.wait()
            self._wake.clear()
            while self._pending:
                key = next(iter(self._pending))
                snapshot = self._pending.pop(key)
                await self._process_safely(key, snapshot)

    async def _connect(self) -> None:
        delay = self.retry_delay
        while True:
            try:
                await self.adapter.connect(ChatHandlers(
                    self._on_command, self._on_message, self._on_answer, self._on_action,
                    self._on_new_task, self._on_options, self._on_submit_task,
                    self._on_text_form, self._on_submit_text))
                self.channel = await self.adapter.open_direct(self.settings.user_id)
                diagnostics.event("chat.connected", platform=self.adapter.platform)
                log.info("%s connected; notifications go to a direct message", self.adapter.platform)
                return
            except Exception as exc:  # noqa: BLE001 - keep retrying; the workflow runs regardless
                diagnostics.event("chat.connect_failed", platform=self.adapter.platform, error=type(exc).__name__)
                log.warning("%s connection failed (%s); retrying in %.0fs", self.adapter.platform, exc, delay)
                try:
                    await self.adapter.disconnect()
                except Exception:  # noqa: BLE001
                    pass
                await asyncio.sleep(delay)
                delay = min(delay * 2, MAX_RETRY_DELAY)

    # notifications --------------------------------------------------------------------
    def _on_broadcast(self, message: dict[str, Any]) -> None:
        task = message.get("task") if message.get("type") == "task" else None
        if isinstance(task, dict) and task.get("jiraKey"):
            self._pending[task["jiraKey"]] = task
            self._wake.set()

    def _retry_later(self, key: str) -> None:
        attempt = self._attempts.get(key, 0) + 1
        self._attempts[key] = attempt
        delay = min(self.retry_delay * 2 ** (attempt - 1), MAX_RETRY_DELAY)

        def requeue() -> None:
            self._pending.setdefault(key, None)
            self._wake.set()
        asyncio.get_running_loop().call_later(delay, requeue)

    async def _process_safely(self, key: str, snapshot: dict[str, Any] | None) -> None:
        try:
            if snapshot is None:
                try:
                    snapshot = self.engine.snapshot(key)
                except WorkflowError:
                    return  # task no longer exists
            await self.process(snapshot, record_silently=key in self._record_silently)
            self._record_silently.discard(key)
            self._attempts.pop(key, None)
        except Exception as exc:  # noqa: BLE001 - retried; never raised into the workflow
            diagnostics.event("chat.notify_failed", platform=self.adapter.platform, error=type(exc).__name__)
            log.warning("%s notification for %s failed (%s); will retry", self.adapter.platform, key, exc)
            self._retry_later(key)

    def task_url(self, jira_key: str) -> str:
        return f"{self.settings.ui_base_url.rstrip('/')}/tasks/{quote(jira_key)}"

    async def process(self, snapshot: dict[str, Any], record_silently: bool = False) -> None:
        """Announce conditions that became true, refresh the thread title, forget cleared ones."""
        key = snapshot["jiraKey"]
        current = notices(snapshot, self.settings.notify, self.settings.include_question_text)
        created = key in self._created  # created from chat: open its thread even with nothing to announce
        platform = self.adapter.platform
        with self.engine.db.session() as session:
            task = session.scalar(select(Task).where(Task.jira_key == key))
            if task is None:
                return
            task_id = task.id
            row = session.scalar(select(ChatThread).where(ChatThread.task_id == task_id, ChatThread.platform == platform))
            if row is None:
                if record_silently:
                    session.add(ChatThread(task_id=task_id, platform=platform, notified={k: None for k in current}))
                    return
                if not current and not created:
                    return
                row = ChatThread(task_id=task_id, platform=platform, notified={})
                session.add(row)
            same_channel = row.channel == self.channel
            thread_id = row.thread_id if same_channel else None
            shown_title = row.title if same_channel else None
            announced = dict(row.notified or {})

        def save(**fields: Any) -> None:
            with self.engine.db.session() as session:
                stored = session.scalar(select(ChatThread).where(ChatThread.task_id == task_id, ChatThread.platform == platform))
                for name, value in fields.items():
                    setattr(stored, name, value)

        title = self.adapter.render_title(snapshot, self.task_url(key))
        fresh = [k for k in current if k not in announced]
        if (fresh or created) and thread_id is None:
            thread_id = await self.adapter.send(self.channel, title)
            shown_title = title
            # Saved before any reply, so a failure below never starts a second thread.
            save(channel=self.channel, thread_id=thread_id, title=title)
        elif thread_id is not None and title != shown_title:
            # Cosmetic, so best effort: a title that cannot be edited (e.g. the message was
            # deleted) must not hold up the notices below.
            try:
                await self.adapter.update(self.channel, thread_id, title)
            except Exception as exc:  # noqa: BLE001
                log.warning("Could not update the %s thread title for %s (%s)", platform, key, exc)
            save(title=title)
        if created:
            # Kept until posted: a failed send is retried with the rest of this snapshot.
            try:
                await self.adapter.send(self.channel, self.adapter.render_created(snapshot, self._run_refusals(snapshot)),
                                        thread_id, buttons=self._run_buttons(snapshot))
            except PermanentChatError as exc:
                log.error("%s rejected the created-task message for %s (%s)", platform, key, exc)
            self._created.discard(key)
        answering = self.settings.answering
        questions = {q["uid"]: q for q in snapshot.get("questions", [])}
        for condition in fresh:
            notice = current[condition]
            # Answering implies question text may be posted, a tree's notes included.
            prompt = (AnswerPrompt(key, notice.question_uid, notice.choices,
                                   (questions.get(notice.question_uid) or {}).get("decision"))
                      if answering and notice.question_uid else None)
            try:
                announced[condition] = await self.adapter.send(
                    self.channel, self.adapter.render_notice(notice, answerable=prompt is not None), thread_id, prompt,
                    buttons=() if prompt else self._notice_buttons(snapshot, condition))
            except PermanentChatError as exc:
                # Retrying the same content cannot succeed; record it so the queue moves on.
                diagnostics.event("chat.notice_rejected", platform=platform, error=str(exc))
                log.error("%s rejected a notice for %s (%s); see AgentFlow instead", platform, key, exc)
                announced[condition] = None
            save(notified=dict(announced))
        if same_channel:
            await self._resolve_questions(snapshot, [k for k in announced if k not in current], announced)
        remaining = {k: announced.get(k) for k in current}
        if remaining != announced:
            save(notified=remaining)

    async def _resolve_questions(self, snapshot: dict[str, Any], cleared: list[str], announced: dict[str, Any]) -> None:
        """Edit question notices that no longer await an answer, so no stale controls remain.

        Best effort: a failed edit is logged and not retried. A leftover control is harmless,
        because the engine refuses answers to questions that are no longer open."""
        questions = {q["uid"]: q for q in snapshot.get("questions", [])}
        for condition in cleared:
            message_id = announced.get(condition)
            if not condition.startswith("question:") or not message_id:
                continue
            text = self.adapter.render_resolved(questions.get(condition.removeprefix("question:")),
                                                self.settings.include_question_text)
            try:
                await self.adapter.update(self.channel, message_id, text)
            except Exception as exc:  # noqa: BLE001 - cosmetic; see docstring
                log.warning("Could not update a resolved %s question for %s (%s)",
                            self.adapter.platform, snapshot["jiraKey"], exc)

    # answers --------------------------------------------------------------------------
    async def _on_answer(self, request: AnswerRequest) -> str | AnswerPrompt | None:
        """Deliver an answer from chat; None on success, the prompt to show instead when a decision
        tree's branch has a follow-up (or for "Change branch"), else a reason safe to post back."""
        platform = self.adapter.platform
        if request.user_id != self.settings.user_id:
            diagnostics.event("chat.answer_rejected", platform=platform, reason="user")
            log.warning("Rejected a %s answer from unauthorized user %s", platform, request.user_id)
            return "You are not authorized to answer AgentFlow questions."
        if not self.settings.answering:
            return "Answering from Slack is turned off; answer in AgentFlow."
        if self.channel is None or request.channel != self.channel:
            diagnostics.event("chat.answer_rejected", platform=platform, reason="channel")
            return "Answer from your AgentFlow direct messages."
        try:
            snapshot = self.engine.snapshot(request.jira_key)
        except WorkflowError:
            return "That task no longer exists."
        question = next((q for q in snapshot.get("questions", []) if q["uid"] == request.question_uid), None)
        if question is None:
            return "That question is no longer open."
        decision = question.get("decision")
        answer: str | None = None
        path: list[int] | None = None
        if decision is not None and (request.choice_index is not None or request.path is not None or request.back):
            if not question.get("actionable"):
                return "That question is no longer open."
            prompt = AnswerPrompt(snapshot["jiraKey"], question["uid"], tuple(question.get("choices") or ()), decision)
            if request.back:
                return prompt
            branches = decision.get("choices") or []
            branch = request.path[0] if request.path is not None else request.choice_index
            if not 0 <= branch < len(branches):
                return "That choice is no longer offered."
            if request.path is None and branches[branch].get("followUp"):
                return dataclasses.replace(prompt, branch=branch)
            path = list(request.path) if request.path is not None else [branch]
        elif request.choice_index is not None:
            choices = question.get("choices") or []
            if not 0 <= request.choice_index < len(choices):
                return "That choice is no longer offered."
            answer = choices[request.choice_index]
        elif request.path is not None or request.back:
            return "That choice is no longer offered."
        else:
            answer = request.text or ""
        try:
            await self.engine.answer_question(snapshot["jiraKey"], question["uid"], answer,
                                              via=self.adapter.display_name, path=path)
        except NotFound:
            return "That question is no longer open."
        except WorkflowError as exc:
            if str(exc) in (NO_SUCH_CHOICE, NEEDS_FOLLOW_UP):
                return "That choice is no longer offered."
            if str(exc) in POSTABLE_ANSWER_ERRORS:
                return f"Not sent: {exc}."
            log.warning("%s answer for %s was not delivered (%s)", platform, snapshot["jiraKey"], exc)
            return "Not sent: AgentFlow could not deliver the answer. Check AgentFlow."
        diagnostics.event("chat.answered", platform=platform)
        return None

    # commands and messages ------------------------------------------------------------
    async def _on_command(self, user_id: str, text: str) -> Reply:
        if user_id != self.settings.user_id:
            diagnostics.event("chat.command_rejected", platform=self.adapter.platform)
            log.warning("Rejected a %s command from unauthorized user %s", self.adapter.platform, user_id)
            return Reply("You are not authorized to use AgentFlow.")
        reply = await self._command_reply(text)
        if not reply.buttons:
            return reply
        # A slash command answers wherever it was typed, privately; buttons act only in the
        # operator's DM (and a private reply cannot be edited to show what happened). So the
        # buttons go to the DM, and the command's own reply points there.
        if self.channel is None:
            return Reply("AgentFlow is still connecting; try again shortly.")
        try:
            await self.adapter.send(self.channel, reply.text, buttons=reply.buttons)
        except Exception:  # noqa: BLE001 - reported to the operator; details in the log
            log.warning("Could not post buttons to the %s DM", self.adapter.platform, exc_info=True)
            return Reply("AgentFlow could not post the button to your direct messages; try again.")
        return Reply("The button is in your direct messages with AgentFlow.")

    async def _on_message(self, request: MessageRequest) -> Reply | None:
        """A message typed in a direct message with the app. Only the operator's own DM gets a
        reply; anything else is dropped silently, so the app never converses with anyone else."""
        if request.user_id != self.settings.user_id or self.channel is None or request.channel != self.channel:
            diagnostics.event("chat.message_ignored", platform=self.adapter.platform)
            return None
        key = self._thread_task(request.thread_id) if request.thread_id else None
        words = request.text.split()
        # Only "new" or "new KEY": "new approach, please" in a thread may be an answer.
        if words and words[0].lower() == "new" and (len(words) == 1 or len(words) == 2 and JIRA_KEY.match(words[1])):
            return self._new_task_reply(words[1:])
        offer = self._offer_answer(key, request.text) if key is not None and words else None
        if phrase := NEW_TASK_PHRASE.match(request.text.strip()):
            # The button, and in a question thread the text as a possible answer too.
            reply = self._new_task_reply([phrase.group(1)] if phrase.group(1) else [])
            if offer is not None and offer.offer is not None:
                return Reply(reply.text, offer.offer, reply.buttons)
            return reply
        if words and words[0].lower() in COMMANDS:
            reply = await self._command_reply(request.text, key, request.thread_id)
            # "status quo is fine" may be an answer that starts with a command word: run the
            # command, but still offer the text. A bare read-only command word is never an answer;
            # a bare "cancel" or "run" may be ("Proceed or cancel?"), and its reply is only a button.
            # An explicit "ask …" is a question for AgentFlow, never an answer for the agent.
            maybe_answer = len(words) > 1 or words[0].lower() in ACTION_COMMANDS
            if maybe_answer and words[0].lower() != "ask" and offer is not None and offer.offer is not None:
                return Reply(reply.text, offer.offer, reply.buttons)
            return reply
        if offer is not None:
            return offer
        # Anything else that reads like a question goes to the model, when questions are on; a
        # single stray word ("thanks") gets the help instead of costing a model call.
        if self._asker and words and (len(words) > 1 or request.text.rstrip().endswith("?")):
            return await self._ask(request.text, key, request.thread_id)
        return Reply(self.adapter.render_help())

    async def _command_reply(self, text: str, key: str | None = None, thread_id: str | None = None) -> Reply:
        words = text.split()
        command = words[0].lower() if words else ""
        if command in ACTION_COMMANDS:
            return self._action_reply(command, words[1:], key)
        if command == "ask":
            if self._asker is None:
                return Reply("Questions are off. Set slack.ask.enabled in config.json to ask AgentFlow in plain words.")
            return await self._ask(text.strip()[len(words[0]):], key, thread_id)
        return Reply(await self._route(text, key))

    # questions -----------------------------------------------------------------------
    async def _ask(self, question: str, key: str | None, thread_id: str | None) -> Reply:
        """A free-text question, answered by the operator's harness from the data chat may post."""
        question = question.strip()
        if not question:
            return Reply("Ask a question after `ask`, e.g. `ask why is JR-12 stuck?`")
        if len(question) > MAX_QUESTION:
            return Reply(f"Questions are limited to {MAX_QUESTION} characters.")
        context = await self._ask_context(question, key)
        history = list(self._ask_history.get(thread_id, ())) if thread_id else []
        async with self._ask_lock:  # one at a time: each runs a model CLI
            try:
                result = await self._asker.ask(context, question, history)
            except AskError as exc:
                return Reply(self.adapter.render_note(str(exc)))
        if thread_id:
            self._ask_history.setdefault(thread_id, deque(maxlen=3)).append((question, result.text))
        diagnostics.event("chat.asked", platform=self.adapter.platform, seconds=round(result.seconds, 1),
                          costUsd=result.cost_usd)
        return Reply(self.adapter.render_ai_answer(result.text))

    @staticmethod
    def _task_brief(task: dict[str, Any]) -> dict[str, Any]:
        return {"jiraKey": task["jiraKey"], "jiraTitle": task.get("jiraTitle"), "status": task.get("status"),
                "phase": task.get("phase"), "createdAt": task.get("createdAt"), "updatedAt": task.get("updatedAt"),
                "needsYou": [{"stage": item.get("label"), "action": item.get("action"), "since": item.get("since"),
                              "questions": item.get("questions")} for item in task.get("attention") or []]}

    async def _ask_context(self, question: str, key: str | None) -> dict[str, Any]:
        """What the model may know: every task's phase and what needs the operator (as `list`
        shows), and, for the task in question, its stages, open agent questions (if their text may
        be posted) and the `slack.details` sections allowed (so its pull request and Jira ticket
        only with "pr" and "ticket"). Nothing else: not status messages, node messages or errors
        unless "errors" is allowed, and no local paths."""
        allowed = self.settings.details
        context: dict[str, Any] = {"allowedDetails": list(allowed),
                                   "tasks": [self._task_brief(task) for task in (await self.engine.list_tasks())[:50]]}
        mentioned = (word.upper() for word in re.findall(r"[A-Za-z][A-Za-z0-9_]*-\d+", question))
        target = key or next((word for word in mentioned if self._task_exists(word)), None)
        snapshot = self._snapshot(target) if target else None
        if snapshot is not None:
            context["task"] = {
                **self._task_brief(snapshot),
                "stages": [{"stage": node.get("label"), "state": node.get("state")} for node in snapshot.get("nodes", [])],
                "openAgentQuestions": [{"stage": q.get("nodeKey"), "question": q.get("message"), "choices": q.get("choices")}
                                       for q in snapshot.get("questions", []) if q.get("actionable")]
                if self.settings.include_question_text else "not shared",
                "details": task_details(self._with_ticket(snapshot, allowed), allowed),
            }
        return context

    def _thread_task(self, thread_id: str) -> str | None:
        """The task whose notification thread this is, if any."""
        with self.engine.db.session() as session:
            return session.scalar(select(Task.jira_key).join(ChatThread, ChatThread.task_id == Task.id).where(
                ChatThread.platform == self.adapter.platform, ChatThread.channel == self.channel,
                ChatThread.thread_id == thread_id))

    def _offer_answer(self, key: str, text: str) -> Reply | None:
        """Typed text in a thread whose task has one open question is probably meant for it. It is
        offered back with a send control rather than delivered, so a stray message never reaches
        an agent's terminal."""
        if not self.settings.answering:
            return None
        snapshot = self._snapshot(key)
        pending = [q for q in (snapshot or {}).get("questions", []) if q.get("actionable")]
        if not pending:
            return None
        if len(pending) > 1:
            return Reply(f"{key} has {len(pending)} open questions; use Reply… on the one you mean.")
        try:
            answer = clean_answer(text)
        except ValueError as exc:
            return Reply(f"Not offered as an answer: {exc}.")
        return Reply(f"{key} is waiting for your answer.", AnswerOffer(key, pending[0]["uid"], answer))

    def _with_ticket(self, snapshot: dict[str, Any], wanted: Collection[str]) -> dict[str, Any]:
        """The snapshot plus its synced Jira ticket, read only when the "ticket" section is both
        allowed and wanted."""
        if "ticket" not in self.settings.details or "ticket" not in wanted:
            return snapshot
        try:
            issue = self.engine.jira_issue(snapshot["jiraKey"])
        except (WorkflowError, OSError, UnicodeDecodeError):
            log.warning("Could not read the synced Jira ticket of %s", snapshot["jiraKey"], exc_info=True)
            issue = None
        return {**snapshot, "jiraIssue": issue}

    def _snapshot(self, key: str) -> dict[str, Any] | None:
        try:
            return self.engine.snapshot(key)
        except WorkflowError:
            return None

    @staticmethod
    def _no_task(key: str) -> str:
        return f"No AgentFlow task {re.sub(r'[^A-Za-z0-9_-]', '', key)[:64].upper()}."

    async def _route(self, text: str, key: str | None = None) -> str:
        """Run one command. `key` is the task whose thread the command was typed in, if any."""
        words = text.split()
        command, args = (words[0].lower(), words[1:]) if words else ("help", [])
        if command == "list" or (command == "status" and not args and key is None):
            return self.adapter.render_list(await self.engine.list_tasks())
        if command == "status":
            target = args[0] if args else key
            snapshot = self._snapshot(target)
            if snapshot is None:
                return self._no_task(target)
            return self.adapter.render_status(snapshot, self.task_url(snapshot["jiraKey"]))
        if command in ("details", "activity"):
            return self._details(command, args, key)
        if command == "digest":
            return await self._digest()
        if command == "new":  # the adapter opens the form itself where it can
            return "Type `new` in your direct messages with AgentFlow to create a task."
        return self.adapter.render_help()

    def _details(self, command: str, args: list[str], key: str | None) -> str:
        allowed = self.settings.details
        if not allowed:
            return "Task details are off. Choose what may be posted with slack.details in config.json."
        sections, target, limit = (["activity"] if command == "activity" else []), key, DEFAULT_ACTIVITY
        for arg in args:
            if arg.lower() in SECTIONS:
                sections.append(arg.lower())
            elif command == "activity" and arg.isdigit():
                limit = int(arg)
            else:
                target = arg
        if target is None:
            return f"Which task? Use `{command} KEY`, or ask in the task's thread."
        refused = [section for section in sections if section not in allowed]
        if refused:
            return f"Not enabled in slack.details: {', '.join(refused)}."
        snapshot = self._snapshot(target)
        if snapshot is None:
            return self._no_task(target)
        snapshot = self._with_ticket(snapshot, sections or allowed)
        details = task_details(snapshot, allowed, sections or None, limit)
        return self.adapter.render_details(snapshot, details, self.task_url(snapshot["jiraKey"]))

    async def _digest(self) -> str:
        tasks = await self.engine.list_tasks()
        spend = None
        if "cost" in self.settings.details:
            with self.engine.db.session() as session:
                spend = daily_spend(session, self.engine.config.workflow.cost_controls)
        return self.adapter.render_digest(tasks, spend)

    async def _digest_loop(self, at: str) -> None:
        """Post the digest daily at `at`. A backend that starts after `at` waits for tomorrow's."""
        while True:
            due = next_digest_at(self._now(), at)
            while (remaining := (due - self._now()).total_seconds()) > 0:
                await asyncio.sleep(min(remaining, self.digest_poll_seconds))
            await self._post_daily_digest()

    async def _post_daily_digest(self) -> None:
        """A top-level direct message, skipped when no task is active. Never raises."""
        if self.channel is None:
            log.warning("Daily %s digest skipped: not connected", self.adapter.platform)
            return
        try:
            tasks = await self.engine.list_tasks()
            if not any(task.get("status") not in (TaskStatus.MERGED, TaskStatus.CANCELLED) for task in tasks):
                return
            await self.adapter.send(self.channel, await self._digest())
            diagnostics.event("chat.digest", platform=self.adapter.platform)
        except Exception:  # noqa: BLE001 - tomorrow's digest is attempted regardless
            log.warning("Daily %s digest failed", self.adapter.platform, exc_info=True)

    # task creation --------------------------------------------------------------------
    def _authorized(self, user_id: str, what: str) -> bool:
        if user_id == self.settings.user_id:
            return True
        diagnostics.event("chat.action_rejected", platform=self.adapter.platform, action=what)
        log.warning("Rejected a %s %s from unauthorized user %s", self.adapter.platform, what, user_id)
        return False

    def _off(self, what: str, setting: str) -> str:
        return f'{what} from {self.adapter.display_name} is turned off. Add "{setting}" to slack.allowActions in config.json.'

    def _creation_off(self) -> str:
        return self._off("Creating tasks", "create")

    def _new_task_reply(self, args: list[str]) -> Reply:
        """A typed message cannot open a form, so it gets a button that can."""
        if not self.settings.creating:
            return Reply(self._creation_off())
        key = args[0].upper() if args and JIRA_KEY.match(args[0]) else ""
        return Reply("Fill in the form to create a task.",
                     buttons=(Button("new_task", f"Create {key}" if key else "Create a task", key),))

    async def _on_new_task(self, user_id: str, jira_key: str | None) -> TaskForm | str:
        if not self._authorized(user_id, "task form"):
            return "You are not authorized to use AgentFlow."
        if not self.settings.creating:
            return self._creation_off()
        repositories = self._repositories()
        key = (jira_key or "").strip()
        return TaskForm(key.upper() if JIRA_KEY.match(key) else None, repositories[0] if repositories else None,
                        build_jira_service(self.engine.config.jira) is not None,
                        self.engine.config.workflow.implementation_mode)

    def _repositories(self) -> list[str]:
        """The default repository, then those of past tasks, most recently used first."""
        with self.engine.db.session() as session:
            used = session.scalars(select(Task.repository_path).group_by(Task.repository_path)
                                   .order_by(func.max(Task.created_at).desc())).all()
        default = self.engine.config.workspace.default_repository_directory
        return list(dict.fromkeys([*([default] if default else []), *used]))

    async def _on_options(self, request: OptionsRequest) -> list[Option]:
        """Choices for a searchable form field. The platform waits only a few seconds, so every
        lookup is bounded; what the operator typed is always offered too."""
        if not self._authorized(request.user_id, "form search") or not self.settings.creating:
            return []
        query = request.query.strip()
        if request.field == "ticket":
            return await self._ticket_options(query)
        if request.field == "repository":
            found = [(path, path) for path in self._repositories() if query.casefold() in path.casefold()]
            typed = [(query, f"{query} (as typed)")] if ("/" in query or "\\" in query) and query not in dict(found) else []
            return (typed + found)[:MAX_OPTIONS]
        if request.field == "branch":
            return await self._branch_options(request.repository, query)
        if request.field == "project":
            return await self._folder_options(request.repository, query)
        return []

    async def _ticket_options(self, query: str) -> list[Option]:
        options: list[Option] = []
        jira = build_jira_service(self.engine.config.jira)
        if jira and query:
            try:
                results = await asyncio.wait_for(jira.search_issues(query), 2.5)
                options = [(result.key, f"{result.key} · {result.summary}") for result in results]
            except Exception as exc:  # noqa: BLE001 - the typed key must still be offered when Jira misbehaves
                log.warning("Jira search for the %s task form failed (%s)", self.adapter.platform, exc)
        if JIRA_KEY.match(query) and query.upper() not in dict(options):
            options.insert(0, (query.upper(), f"{query.upper()} (as typed)"))
        return options[:MAX_OPTIONS]

    async def _branch_options(self, repository: str | None, query: str) -> list[Option]:
        if not repository:
            return []
        branches = await self._repository_branches(repository)
        folded = query.casefold()
        found = sorted((b for b in branches if folded in b.casefold()), key=lambda b: (not b.casefold().startswith(folded), b))
        typed = [(query, f"{query} (as typed)")] if query and query not in branches else []
        return [(b, b) for b in found][:MAX_OPTIONS - len(typed)] + typed

    async def _repository_branches(self, repository: str) -> list[str]:
        cached = self._branches.get(repository)
        if cached and time.monotonic() - cached[0] < BRANCH_CACHE_SECONDS:
            return cached[1]
        try:
            info = await asyncio.wait_for(self.engine.git.inspect_repository(repository), 2.5)
        except (GitError, OSError, asyncio.TimeoutError):
            return []  # not a repository (yet): only the typed branch is offered
        self._branches[repository] = (time.monotonic(), info.branches)
        return info.branches

    async def _folder_options(self, repository: str | None, query: str) -> list[Option]:
        """Committed folders of the repository, top levels first. "." (the whole repository) lets
        a chosen folder be taken back, as a select cannot be cleared."""
        if not repository:
            return []
        folders = await self._repository_folders(repository)
        query = query.replace("\\", "/").strip("/")
        folded = query.casefold()
        found = sorted((f for f in folders if folded in f.casefold()),
                       key=lambda f: (not f.casefold().startswith(folded), f.count("/"), f))
        whole = [] if query else [(".", "Whole repository")]
        typed = [(query, f"{query} (as typed)")] if query and query not in folders else []
        return whole + [(f, f) for f in found][:MAX_OPTIONS - len(whole) - len(typed)] + typed

    async def _repository_folders(self, repository: str) -> list[str]:
        cached = self._folders.get(repository)
        if cached and time.monotonic() - cached[0] < BRANCH_CACHE_SECONDS:
            return cached[1]

        async def lookup() -> list[str]:
            info = await self.engine.git.inspect_repository(repository)
            return await self.engine.git.directories(info.root)  # the project path is relative to the root
        try:
            folders = await asyncio.wait_for(lookup(), 2.5)
        except (GitError, OSError, asyncio.TimeoutError):
            return []  # not a repository (yet): only the typed folder is offered
        # Hidden folders and node_modules are left out, as in the web form's folder picker.
        folders = [f for f in folders if not any(part.startswith(".") or part.lower() == "node_modules"
                                                 for part in f.split("/"))]
        self._folders[repository] = (time.monotonic(), folders)
        return folders

    async def _on_submit_task(self, draft: TaskDraft) -> dict[str, str]:
        """Only checks that answer within the platform's few seconds; creation runs afterwards."""
        if not self._authorized(draft.user_id, "task creation"):
            return {"ticket": "You are not authorized to use AgentFlow."}
        if not self.settings.creating:
            return {"ticket": self._creation_off()}
        errors = {}
        if not draft.repository_path.strip():
            errors["repository"] = "Choose a repository."
        if not draft.base_branch.strip():
            errors["branch"] = "Choose a base branch."
        fields = {"jiraKey": "ticket", "repositoryPath": "repository", "baseBranch": "branch",
                  "projectRelativePath": "project"}
        try:
            request = CreateTaskRequest(
                jiraKey=draft.jira_key, repositoryPath=draft.repository_path.strip(),
                baseBranch=draft.base_branch.strip(), projectRelativePath=(draft.project_relative_path or "").strip() or None,
                implementationMode=draft.implementation_mode or None,
                additionalContext=(draft.additional_context or "").strip() or None)
        except ValidationError as exc:
            for error in exc.errors():
                field = fields.get(str(error["loc"][0]) if error["loc"] else "", "ticket")
                errors.setdefault(field, error["msg"].removeprefix("Value error, "))
        if errors:
            return errors
        with self.engine.db.session() as session:
            existing = session.execute(select(Task.id, Task.archived_at).where(Task.jira_key == request.jira_key)).first()
            if existing and existing.archived_at:
                return {"ticket": f"A task for {request.jira_key} exists but is archived. Unarchive or delete it in AgentFlow first."}
            if existing:
                return {"ticket": f"A task for {request.jira_key} already exists."}
        job = asyncio.create_task(self._create(request), name=f"chat-create-{request.jira_key}")
        self._background.add(job)
        job.add_done_callback(self._background.discard)
        return {}

    async def _create(self, request: CreateTaskRequest) -> None:
        key, platform = request.jira_key, self.adapter.platform
        try:
            await self.engine.create_task(request)
        except Exception as exc:  # noqa: BLE001 - reported in chat; details stay in the log
            diagnostics.event("chat.create_failed", platform=platform, error=type(exc).__name__)
            log.warning("Creating %s from %s failed (%s)", key, platform, exc)
            # Git and Jira errors can quote paths and command output: posted only when errors may be.
            detail = str(exc)[:ERROR_CHARS] if isinstance(exc, WorkflowError) and "errors" in self.settings.details else None
            if self.channel is not None:
                try:
                    await self.adapter.send(self.channel, self.adapter.render_creation_failed(key, detail))
                except Exception:  # noqa: BLE001 - nothing more to tell the operator through chat
                    log.warning("Could not post a failed creation of %s to %s", key, platform, exc_info=True)
            return
        diagnostics.event("chat.created", platform=platform)
        # The worker opens the thread, so thread creation never races the create_task publish.
        self._created.add(key)
        self._pending.setdefault(key, None)
        self._wake.set()

    # starting stages ------------------------------------------------------------------
    def _launch_refusal(self, snapshot: dict[str, Any], node: dict[str, Any]) -> str | None:
        """Why chat may not start this stage. Only a READY agent stage of an ACTIVE task qualifies:
        retrying a failure or resuming a stopped review loop are decisions for the UI, and so is
        anything that needs API launch approval or is over budget."""
        label = node.get("label") or node.get("key")
        if snapshot.get("status") != TaskStatus.ACTIVE:
            return f"{snapshot['jiraKey']} is {snapshot.get('status')}; start stages in AgentFlow."
        config = node.get("effectiveConfig")
        if node.get("state") != NodeState.READY or not config:
            return f"{label} is {node.get('state')}; only a READY stage can be started from {self.adapter.display_name}."
        if not config.get("harnessAvailable", True):
            return f"{label}'s harness is unavailable; choose another in AgentFlow."
        if config.get("billingMode") != "subscription":
            if config.get("requiresMeteredApproval"):
                return f"{label} needs API launch approval; start it in AgentFlow."
            blocker = launch_blocker(snapshot["budget"]) if snapshot.get("budget") else None
            if blocker:
                return f"Not started: {blocker}."
        return None

    def _run_refusals(self, snapshot: dict[str, Any]) -> tuple[str, ...]:
        """Why a READY stage gets no start button, so its absence is explained."""
        if not self.settings.running:
            return ()
        return tuple(refusal for node in snapshot.get("nodes", [])
                     if node.get("state") == NodeState.READY and (refusal := self._launch_refusal(snapshot, node)))

    def _run_buttons(self, snapshot: dict[str, Any], only: Collection[str] | None = None) -> tuple[Button, ...]:
        """A start button for each stage chat may start right now (of `only`, if given)."""
        if not self.settings.running:
            return ()
        buttons = []
        for node in snapshot.get("nodes", []):
            if only is not None and node["key"] not in only:
                continue
            if node.get("state") != NodeState.READY or self._launch_refusal(snapshot, node):
                continue
            label = node.get("label") or node["key"]
            confirm = f"Start {label} for {snapshot['jiraKey']}?"
            transition = self.engine.config.jira.auto_transition_status
            if snapshot.get("taskSource") != "local" and node["key"] == wf.ARCHITECT and transition and build_jira_service(self.engine.config.jira):
                confirm += f" This also moves the Jira ticket to {transition}."
            buttons.append(Button("run", f"Run {label}", f"{snapshot['jiraKey']}|{node['key']}", confirm))
        return tuple(buttons)

    async def _on_action(self, request: ActionRequest) -> str | None:
        """A "run", "cancel" or "mark_merged" button. None when done, else a reason safe to post back."""
        if not self._authorized(request.user_id, "action"):
            return "You are not authorized to use AgentFlow."
        if self.channel is None or request.channel != self.channel:
            return "Use AgentFlow's buttons in your direct messages."
        if request.kind == "run":
            return await self._start_stage(request.value)
        if request.kind == "review_requirements":
            return await self._review_requirements(request.value)
        if request.kind == "approve_requirements":
            return await self._approve_requirements(request.value)
        allowed = {"cancel": self.settings.cancelling, "mark_merged": self.settings.marking_pr}
        if request.kind not in allowed:
            return "That action is not available."
        if not allowed[request.kind]:
            setting, what = ACTION_SETTINGS["cancel" if request.kind == "cancel" else "pr"]
            return self._off(what, setting)
        if self._snapshot(request.value) is None:
            return "That task no longer exists."
        try:
            if request.kind == "cancel":
                await self.engine.cancel_task(request.value)
            else:
                await self.engine.mark_pr_merged(request.value)
        except WorkflowError as exc:
            return self._refusal(exc, "Not done", request.value)
        diagnostics.event(f"chat.{request.kind}", platform=self.adapter.platform)
        return None

    def _refusal(self, exc: WorkflowError, prefix: str, key: str) -> str:
        """An engine refusal as it may be posted: its own words only when they are about state."""
        log.warning("%s action on %s refused (%s)", self.adapter.platform, key, exc)
        message = str(exc)
        if message.startswith(POSTABLE_ACTION_ERRORS) or message.endswith(POSTABLE_ACTION_ENDINGS):
            return f"{prefix}: {message}."
        return f"{prefix}: AgentFlow could not do it. Check AgentFlow."

    async def _start_stage(self, value: str) -> str | None:
        if not self.settings.running:
            return f"Starting stages from {self.adapter.display_name} is turned off; start it in AgentFlow."
        key, _, node_key = value.partition("|")
        snapshot = self._snapshot(key)
        if snapshot is None:
            return "That task no longer exists."
        node = next((n for n in snapshot.get("nodes", []) if n["key"] == node_key), None)
        if node is None:
            return "That stage no longer exists."
        # For a clear answer only: the engine enforces READY-only itself (ready_only) under its lock.
        refusal = self._launch_refusal(snapshot, node)
        if refusal:
            return refusal
        try:
            await self.engine.run_node(snapshot["jiraKey"], node_key, ready_only=True)
        except WorkflowError as exc:
            return self._refusal(exc, "Not started", key)
        # A launch the engine declines (capacity, approval) is recorded on the node, not raised.
        after = next((n for n in (self._snapshot(key) or {}).get("nodes", []) if n["key"] == node_key), {})
        if after.get("state") in STARTED_STATES:
            diagnostics.event("chat.started", platform=self.adapter.platform)
            return None
        message = after.get("message")
        return f"Not started: {message}." if message in POSTABLE_LAUNCH_REASONS else "Not started. Check AgentFlow."

    # commands that act ---------------------------------------------------------------
    def _task_exists(self, key: str) -> bool:
        with self.engine.db.session() as session:
            return session.scalar(select(Task.id).where(Task.jira_key == key.upper())) is not None

    def _action_reply(self, command: str, args: list[str], key: str | None) -> Reply:
        """`run`, `changes`, `cancel` and `pr` typed: the matching button, never the action itself."""
        setting, what = ACTION_SETTINGS[command]
        if setting not in self.settings.allow_actions:
            return Reply(self._off(what, setting))
        # The first argument names a task, unless typed in a thread and it is not one ("run architect").
        target, rest = (args[0], args[1:]) if args and (key is None or self._task_exists(args[0])) else (key, args)
        if target is None:
            return Reply(f"Which task? Use `{command} KEY`, or ask in the task's thread.")
        snapshot = self._snapshot(target)
        if snapshot is None:
            return Reply(self._no_task(target))
        key, status = snapshot["jiraKey"], snapshot.get("status")
        note = self.adapter.render_note
        if command == "run":
            return self._run_reply(snapshot, rest[0] if rest else None)
        if command in ("changes", "approve"):
            if not self._gate_waiting(snapshot):
                return Reply(note(f"{key}'s requirements are not waiting for approval."))
            if command == "approve":
                return Reply(note(f"Review {key}'s requirements in full, then approve them:"),
                             buttons=(Button("review_requirements", "Review requirements", key),))
            return Reply(note(f"Send {key}'s requirements back to the Architect with your feedback:"),
                         buttons=(Button("request_changes", "Request changes…", key),))
        if command == "cancel":
            if status in (TaskStatus.MERGED, TaskStatus.CANCELLED):
                return Reply(note(f"{key} is {status}; it can no longer be cancelled."))
            return Reply(note(f"Cancel {key}?"), buttons=(Button(
                "cancel", f"Cancel {key}", key,
                f"Cancel {key}? Its agents are stopped. The worktree, branch and files are kept."),))
        if status == TaskStatus.READY_FOR_PR:
            return Reply(note(f"Record {key}'s pull request:"), buttons=(Button("mark_pr", "Mark PR created…", key),))
        if status == TaskStatus.PR_CREATED:
            return Reply(note(f"Mark {key}'s pull request as merged?"),
                         buttons=(Button("mark_merged", "Mark merged", key, f"Mark {key}'s pull request as merged?"),))
        return Reply(note(f"{key} is {status}; pull requests are tracked once it is Ready for PR."))

    def _run_reply(self, snapshot: dict[str, Any], stage: str | None) -> Reply:
        key, note = snapshot["jiraKey"], self.adapter.render_note
        nodes = [n for n in snapshot.get("nodes", []) if n.get("effectiveConfig")]
        if stage:
            wanted = stage.casefold().replace("_", " ").replace("-", " ")
            nodes = [n for n in nodes if wanted in (n["key"].casefold().replace("_", " "), (n.get("label") or "").casefold())]
            if not nodes:
                return Reply(note(f"{key} has no stage {stage}."))
        buttons = self._run_buttons(snapshot, [n["key"] for n in nodes])
        if buttons:
            return Reply(note(f"Start a stage of {key}:"), buttons=buttons)
        # Say why nothing can start: the named stage, else every READY one.
        reasons = [self._launch_refusal(snapshot, n) for n in nodes if stage or n.get("state") == NodeState.READY]
        return Reply(note("\n".join(r for r in reasons if r) or f"Nothing in {key} is ready to start."))

    def _notice_buttons(self, snapshot: dict[str, Any], condition: str) -> tuple[Button, ...]:
        """The action a notice invites, when chat may take it."""
        key = snapshot["jiraKey"]
        kind, _, rest = condition.partition(":")
        if kind == "node":
            node_key, _, state = rest.rpartition(":")
            if state == NodeState.READY:
                return self._run_buttons(snapshot, [node_key])
            if node_key == wf.REQUIREMENTS_GATE and state == NodeState.WAITING_FOR_APPROVAL:
                return self._gate_buttons(key)
        if condition == f"status:{TaskStatus.READY_FOR_PR}" and self.settings.marking_pr:
            return (Button("mark_pr", "Mark PR created…", key),)
        return ()

    # one-field forms: requirement feedback and the PR link ---------------------------
    async def _on_text_form(self, user_id: str, kind: str, key: str) -> TextForm | str:
        """The form a "request_changes" or "mark_pr" button opens, if it applies now."""
        if not self._authorized(user_id, "form"):
            return "You are not authorized to use AgentFlow."
        snapshot = self._snapshot(key)
        if snapshot is None:
            return "That task no longer exists."
        key = snapshot["jiraKey"]
        if kind == "request_changes":
            if not self.settings.requesting_changes:
                return self._off("Requesting changes", "requestChanges")
            if not self._gate_waiting(snapshot):
                return f"{key}'s requirements are not waiting for approval."
            return TextForm(kind, key, "Request changes",
                            f"{key}: the requirements go back to the Architect with your feedback, to be revised.",
                            "What should change?", True, MAX_FEEDBACK_LENGTH)
        if kind == "mark_pr":
            if not self.settings.marking_pr:
                return self._off("Marking pull requests", "markPr")
            if snapshot.get("status") != TaskStatus.READY_FOR_PR:
                return f"{key} is {snapshot.get('status')}; a PR can be recorded only when it is Ready for PR."
            return TextForm(kind, key, "Mark PR created", f"Record the pull request for {key}.",
                            "Pull request URL", False, MAX_PR_URL_LENGTH)
        return "That form is not available."

    async def _on_submit_text(self, submission: TextSubmission) -> dict[str, str]:
        """Checks that answer within the platform's few seconds; the engine call runs afterwards
        and its result is posted in the task's thread."""
        form = await self._on_text_form(submission.user_id, submission.kind, submission.value)
        if isinstance(form, str):
            return {"text": form}
        text = submission.text.strip()
        if form.kind == "request_changes" and not text:
            return {"text": "Feedback cannot be empty."}
        if form.kind == "mark_pr":
            try:
                text = clean_pr_url(text)
            except ValueError as exc:
                return {"text": str(exc)}
        job = asyncio.create_task(self._apply_text(form.kind, form.value, text), name=f"chat-{form.kind}-{form.value}")
        self._background.add(job)
        job.add_done_callback(self._background.discard)
        return {}

    async def _apply_text(self, kind: str, key: str, text: str) -> None:
        buttons: tuple[Button, ...] = ()
        try:
            if kind == "request_changes":
                await self.engine.request_requirements_changes(key, text)
                note = "Requirements sent back to the Architect."
                # Offer to start the revision straight away (unless it already started itself).
                snapshot = self._snapshot(key)
                buttons = self._run_buttons(snapshot, [wf.ARCHITECT]) if snapshot else ()
            else:
                await self.engine.mark_pr_created(key, text)
                note = "Pull request recorded."
            diagnostics.event(f"chat.{kind}", platform=self.adapter.platform)
        except WorkflowError as exc:
            note = self._refusal(exc, "Not done", key)
        except Exception:  # noqa: BLE001 - reported generically; details in the log
            log.exception("%s %s on %s failed", self.adapter.platform, kind, key)
            note = "Not done: AgentFlow could not do it. Check AgentFlow."
        await self._post_to_task(key, note, buttons)

    def _task_thread(self, key: str) -> str | None:
        """The task's thread in the operator's DM, if it has one."""
        with self.engine.db.session() as session:
            return session.scalar(select(ChatThread.thread_id).join(Task, ChatThread.task_id == Task.id).where(
                Task.jira_key == key, ChatThread.platform == self.adapter.platform, ChatThread.channel == self.channel))

    async def _post_to_task(self, key: str, text: str, buttons: tuple[Button, ...] = ()) -> None:
        """A result in the task's thread (a direct message if it has none). Plain `text`."""
        if self.channel is None:
            return
        try:
            await self.adapter.send(self.channel, self.adapter.render_note(text), self._task_thread(key), buttons=buttons)
        except Exception:  # noqa: BLE001 - nothing more to tell the operator through chat
            log.warning("Could not post a result for %s to %s", key, self.adapter.platform, exc_info=True)

    # requirements approval -----------------------------------------------------------
    @staticmethod
    def _gate_waiting(snapshot: dict[str, Any]) -> bool:
        gate = next((n for n in snapshot.get("nodes", []) if n["key"] == wf.REQUIREMENTS_GATE), {})
        return gate.get("state") == NodeState.WAITING_FOR_APPROVAL

    def _gate_buttons(self, key: str) -> tuple[Button, ...]:
        buttons = []
        if self.settings.approving_requirements:
            buttons.append(Button("review_requirements", "Review requirements", key))
        if self.settings.requesting_changes:
            buttons.append(Button("request_changes", "Request changes…", key))
        return tuple(buttons)

    async def _review_requirements(self, key: str) -> str | None:
        """Post the requirements in full in the task's thread, then an approve button bound to the
        digest of exactly those documents. The engine refuses the approval if they change after."""
        if not self.settings.approving_requirements:
            return self._off("Approving requirements", "approveRequirements")
        snapshot = self._snapshot(key)
        if snapshot is None:
            return "That task no longer exists."
        key = snapshot["jiraKey"]
        if not self._gate_waiting(snapshot):
            return f"{key}'s requirements are not waiting for approval."
        # The digest covers every document the engine reads, so every one of those is shown (unless
        # empty), including any this module has no title for.
        files = snapshot.get("requirements") or {}
        digest = snapshot["requirementsDraftDigest"]
        documents = [(REQUIREMENT_TITLES.get(name, name), text) for name, text in files.items() if (text or "").strip()]
        if not documents:
            return f"{key} has no requirements to review yet."
        if sum(len(text) for _, text in documents) > MAX_REVIEW_CHARS:
            return f"{key}'s requirements are too long to review in {self.adapter.display_name}; review and approve them in AgentFlow."
        thread = self._task_thread(key)
        try:
            for message in self.adapter.render_requirements(key, documents):
                await self.adapter.send(self.channel, message, thread)
            buttons = [Button("approve_requirements", "Approve requirements", f"{key}|{digest}",
                              f"Approve the requirements posted above for {key}? The next stage can then start.")]
            if self.settings.requesting_changes:
                buttons.append(Button("request_changes", "Request changes…", key))
            await self.adapter.send(self.channel, self.adapter.render_note(f"Approve these requirements for {key}?"),
                                    thread, buttons=tuple(buttons))
        except Exception:  # noqa: BLE001 - no approve button is posted unless every document was
            log.warning("Could not post the requirements of %s to %s", key, self.adapter.platform, exc_info=True)
            return "Could not post the requirements in full, so there is nothing to approve. Try again, or use AgentFlow."
        diagnostics.event("chat.requirements_reviewed", platform=self.adapter.platform)
        return None

    async def _approve_requirements(self, value: str) -> str | None:
        if not self.settings.approving_requirements:
            return self._off("Approving requirements", "approveRequirements")
        key, _, digest = value.partition("|")
        if not DIGEST.match(digest):
            return "That approval button is not valid; use Review requirements again."
        try:
            await self.engine.approve(key, wf.REQUIREMENTS_GATE, expected_requirements_digest=digest,
                                      via=self.adapter.display_name)
        except WorkflowError as exc:
            return self._refusal(exc, "Not approved", key)
        diagnostics.event("chat.requirements_approved", platform=self.adapter.platform)
        # Offer the next stage, unless a READY notice (which carries the same button) is coming.
        if "READY" not in self.settings.notify:
            snapshot = self._snapshot(key)
            buttons = self._run_buttons(snapshot) if snapshot else ()
            if buttons:
                await self._post_to_task(key, "Requirements approved. Start the next stage:", buttons)
        return None
