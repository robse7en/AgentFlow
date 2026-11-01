# Slack notifications and answers

AgentFlow can DM you in Slack when a task needs you:
- an agent asked a question;
- a stage is waiting for approval, failed or was interrupted;
- a ticket needs human review or is ready for PR.

You can answer an agent's question from that DM. You can also type commands there, or after
`/agentflow`: `list`, `status`, `digest`, and, if you allow it, `details` and `activity` (see
[Chatting with AgentFlow](#chatting-with-agentflow)).

It is **off by default**. Only the one Slack user you configure receives DMs or may use the
command or answer. Each person who runs AgentFlow creates their own Slack app with its own slash
command (see [Several people in one workspace](#several-people-in-one-workspace)); this page writes
it as `/agentflow`, the default. Slack is reached over **Socket Mode**, an outbound connection from the backend, so
AgentFlow still listens only on loopback: no public URL, webhook or tunnel.

What Slack may do is set by `allowActions`. By default it can only answer questions. Add
`"create"` to [create tasks](#creating-tasks), `"run"` to [start stages](#starting-stages), and
`"requestChanges"`, `"approveRequirements"`, `"cancel"` or `"markPr"` for the
[other actions](#other-actions). Final approval, review overrides, retrying a failed stage,
resuming a stopped review loop and worktree cleanup stay in the AgentFlow UI; links in Slack point
there (`uiBaseUrl`).

## What is posted

Each task gets one DM thread. The parent message shows the ticket key, the Jira title and the
current phase, and is edited as the phase changes. Replies announce each new condition once, and
a restart does not repeat them. A condition that clears and comes back is announced again. When
Slack is enabled for the first time, finished tasks are recorded silently, so the history does not
arrive as a burst of DMs.

Notifications contain the ticket key, Jira title, phase, node labels, and agent questions with
their choices (unless `includeQuestionText` is `false`). They **never** contain review findings,
task status messages (which can quote findings), node errors or command output. Those are posted
only when you ask for them with `details`, and only the sections you enabled in `slack.details`.
Agent and Jira text is escaped, so it cannot create Slack links or mentions.

## Answering questions

A question arrives with one button per choice and a **Reply…** button, which opens a one-line text
box. Your answer goes through the same path as the AgentFlow UI: it is typed into the agent's
terminal, and the task's event log records it as answered *via Slack*. Once the question is
answered, from Slack or the UI, its message is edited to show the answer and the buttons go away.
If the agent stops waiting first, the message says so.

A decision tree (see the README's *Agent questions*) arrives with numbered branch buttons, the
agent's pick in green and `›` on a branch with a follow-up, and its risk, context, consequences and
agent's notes in a line under the question. Clicking a branch with a follow-up swaps the notice's
buttons for that follow-up's options and **‹ Change branch** (which brings the branches back); nothing
is sent until you click an option or a branch without a follow-up. The notice itself changes, so
there is no second message to clean up, and nothing is stored: the path rides in the button.

Safeguards:
- Only your `userId` can answer, and only in your DM with the app.
- A button names its choice by position (a branch, or branch and option), and AgentFlow builds the
  answer from its own copy of the question.
- Answers must be one line. An answer to a question that was already answered, or that the agent
  no longer waits for, is refused. The reason is posted in the thread.
- If delivery fails, the reply in Slack is a generic message. The details, which may include
  command output, stay in the backend log.

Answering needs `includeQuestionText: true`, because you have to see the question. Set
`"allowActions": []` to keep notifications but turn answering off. Questions posted before
answering was enabled keep their text-only messages; answer those in AgentFlow.

## Chatting with AgentFlow

Type a command in your DM with AgentFlow, or after `/agentflow` anywhere if your app has a slash
command (that reply is visible only to you). Buttons work only in your DM with AgentFlow, so when
`/agentflow` would reply with one, the button is posted there and the reply says so. A message you
type in the DM gets a 👀 reaction while AgentFlow works on it, swapped for ✅ once the reply is
posted (Slack does not let apps show "is typing"):

| Command | Reply |
|---|---|
| `list` | Active tasks and what needs you |
| `status KEY` | One task's phase and pending actions |
| `digest` | Every active task, what is waiting on you and for how long; today's API spend if `cost` is allowed |
| `details KEY [section]` | The task's details: all allowed sections, or one of `requirements`, `findings`, `errors`, `cost`, `activity`, `pr`, `ticket` |
| `activity KEY [N]` | The task's last N events (default 10, at most 30); needs `activity` allowed |
| `new [KEY]` | Create a task (needs `create` in `allowActions`). In the DM, plain words such as "create a new task" or "new task for KEY" work too |
| `run [KEY] [STAGE]` | Buttons to start the task's READY stages, or the named one (needs `run`) |
| `changes [KEY]` | A button to send the requirements back to the Architect (needs `requestChanges`) |
| `cancel [KEY]` | A button to cancel the task (needs `cancel`) |
| `pr [KEY]` | A button to mark the pull request created, or merged (needs `markPr`) |
| `ask QUESTION` | A plain-words answer from AgentFlow's data (needs `slack.ask`; see [Asking questions](#asking-questions)) |
| anything else | With `slack.ask` on, a question for the model; otherwise this list |

Commands that act never act on the text alone: they post a button, and the button asks for
confirmation where it matters. In a thread, `run architect` names the stage of that thread's task.

**Daily digest.** Set `"digestAt": "09:00"` (this machine's local time) to get the digest as a DM
every day. It is skipped when no task is active. A backend started after that time posts the next
day's digest, not a late one.

**In a task's thread** the key is optional: `status` or `details` there means that task. If the
agent has an open question, other text you type in the thread comes back quoted, with a **Send as
answer** button. If your text starts with a command word ("status quo is fine"), the command runs
and the button is offered too. Nothing reaches the agent until you click it. The answer then goes through the
same checks as the buttons above. Text you type outside a thread is never taken as an answer.

Only your `userId`, in your DM with the app, gets a reply. Messages from anyone else, from bots
(including the app itself) and edits are ignored.

### Task details (`slack.details`)

Details are **off by default** (`"details": []`). List the sections Slack may post:

| Section | What is posted |
|---|---|
| `requirements` | Each requirement document's title and first few lines, and whether they are approved |
| `findings` | Open review findings: severity, blocking, title, file:line (at most 15, blocking first) |
| `errors` | The task's status message and each node's error, cut to a few lines |
| `cost` | Runs, billed and estimated cost and tokens, per task and per stage; the ticket's API spend against its limit |
| `activity` | Recent event-log messages |
| `pr` | The task's pull request: its URL, when it was created and merged, and whether GitHub PR sync is paused |
| `ticket` | The Jira ticket as AgentFlow last synced it: the description (first 15 lines) and the latest three comments, each cut to a few lines |

Anything posted stays in your Slack workspace's history. Findings and errors can quote code,
file paths and command output, and `ticket` copies Jira text into Slack. Enable only what you are
comfortable keeping there.

`ticket` is AgentFlow's copy, not live Jira: it is synced when the task is created and before each
Architect run, and its sync time is shown. Comments are included only when `jira.fetchComments` is on.

## Creating tasks

With `"create"` in `allowActions`, `/agentflow new [KEY]` opens a form. In your DM, typing
`new [KEY]` posts a **Create a task** button that opens it (Slack opens forms only from a command or a
click). A message that is only a request to create a task, such as "create a new task", "make me a
task" or "new task for ABC-12", gets the same button. A longer sentence ("create a task for the
login bug") is a question for the model, as below. In a thread where an agent waits for an answer,
the request is also offered as the answer. The form asks for:

- **Jira ticket**: searches Jira when it is configured, else a plain key field. A key you type is
  always offered, even if the search does not find it.
- **Repository**: your `defaultRepositoryDirectory` and the repositories of past tasks, most
  recent first. Paste a path to use another one, then pick it (it is listed "(as typed)"). It must
  be a folder on the AgentFlow machine. The field starts preselected with the default repository,
  so check it before creating.
- **Base branch**: starts as `main`. The list shows the chosen repository's local and remote
  branches; a branch you type is offered too, e.g. one that exists only on the remote so far.
- Optional **project sub-directory**: the folders committed in the chosen repository, top levels
  first (hidden folders and `node_modules` are left out). Type to search, or type a path to use a
  folder that is not committed yet. **Whole repository** undoes a choice.
- Optional **implementation route** and **additional context**.

The form checks only what it can check at once: a valid key, a repository, a branch, and that the
task does not already exist. Whether the branch or a typed folder exists is checked only at
creation, so change the base branch for a repository without `main`. Creating the task (worktree, Jira sync, zellij session) happens after
the form closes. When it is done, the task's DM thread opens. If creation fails, a DM says so. The
reason (which can quote git output) is included only when `errors` is in `slack.details`;
otherwise it is in the backend log. Agent overrides are not in the form; set them in AgentFlow.

## Starting stages

With `"run"` in `allowActions`, a task created from Slack opens with a **Run Architect** button,
which asks for confirmation. The confirmation says so when starting the Architect also moves
the Jira ticket (`jira.autoTransitionStatus`). Slack only starts a **READY** stage of an
**active** task. It refuses, and points you to AgentFlow, when:
- the stage needs API launch approval (`requireMeteredApproval`, or a Copilot role on a metered harness);
- a ticket or daily API budget would be exceeded;
- the harness is unavailable;
- the stage failed (retry) or the review loop stopped (resume). Both are decisions for the UI.

If the engine declines the launch itself, for example because every agent slot is busy, the
thread says why. Once a stage starts, its button message changes to *… started* and the thread
carries its notifications as usual. AgentFlow checks "READY stage of an active task" again under
the task's lock, so a button clicked after the task moved on is refused.

Besides `run STAGE`, a **Run** button appears on a "… is ready to start" notice. Those notices
are off by default; add `"READY"` to `notify` to get them.

## Other actions

- **Request requirement changes** (`"requestChanges"`). The Requirements Approval
  "waiting for your approval" notice gets a **Request changes…** button; so does `changes KEY`. It
  opens a form for your feedback (up to 4000 characters). The requirements go back to the
  Architect, the same as *Request changes* in AgentFlow, and the thread offers **Run Architect**
  to start the revision.
- **Approve requirements** (`"approveRequirements"`). See
  [Approving requirements](#approving-requirements).
- **Cancel** (`"cancel"`). `cancel KEY` posts a **Cancel KEY** button that asks for confirmation.
  Agents stop; the worktree, branch and files are kept, as in AgentFlow.
- **Pull requests** (`"markPr"`). The "Ready for PR" notice gets **Mark PR created…**, which asks
  for the link (it must start with `http://` or `https://`); so does `pr KEY`. Once a PR is recorded,
  `pr KEY` offers **Mark merged**, with a confirmation. Undoing either stays in AgentFlow.

Results and refusals are posted in the task's thread. A refusal about the task's state (for example
"Task must be Ready for PR") is shown as it is; anything else says to check AgentFlow, and the
details stay in the backend log.

## Approving requirements

With `"approveRequirements"` in `allowActions`, the Requirements Approval notice gets a
**Review requirements** button; so does `approve KEY`. Approval always follows a full review:

1. **Review requirements** posts every requirement document **in full** in the task's thread,
   never summarized or cut. A long document is split across several messages. Empty documents are
   left out.
2. Then comes **Approve requirements**, with a confirmation, and **Request changes…**. The approve
   button is bound to the exact text that was posted: if any document changes afterwards, the
   approval is refused ("Requirements changed since they were displayed"), and you review again.
   If any document could not be posted, no approve button is posted either.
3. The approval is recorded in the event log as *Requirements approved via Slack*. With `run`
   allowed, the thread then offers the next stage.

Requirements over 60,000 characters in total are left to AgentFlow. Posting them would take dozens
of messages. Enabling this action puts the full requirement documents in your Slack history,
whatever `slack.details` says.

Final approval (Ready for PR), review overrides and worktree cleanup stay in AgentFlow: they need
the diff and the evidence, which Slack does not show.

## Asking questions

With `slack.ask` enabled, you can ask in plain words: "why is JR-12 stuck?", "what's waiting on
me?", "what did the reviewers find on JR-9?". Ask in your DM, in a task's thread (the question is
then about that task), or with `ask QUESTION`. Anything you type that isn't a command is a
question, unless it is a single word, or it is in a thread where an agent waits for an answer.
There it is offered as the answer; use `ask` to ask instead.

```json
"ask": { "enabled": true, "harness": "claude", "model": null, "timeoutSeconds": 90, "allowMetered": false }
```

How it answers:
- **Which model runs:** the CLI of the harness named by `harness`, headless, on your own login.
  It must be a Claude Code (`"type": "claude"`) or GitHub Copilot CLI (`"type": "copilot"`) harness
  in `config.json`. Codex is not supported yet. AgentFlow itself holds no API key. `model` is
  passed as `--model` (e.g. `"sonnet"` or `"haiku"` for Claude Code, `"claude-sonnet-5"` or
  `"gpt-5.6-luna"` for Copilot); `null` uses the CLI's default.
- **No tools:** every tool and MCP server is off, and your and projects' settings, hooks and
  permissions are not loaded. It runs in an empty temporary folder with the backend's safe
  environment. The harness's own `args` are never used. It cannot read files, run commands or change
  anything; it can only write text. Anything it suggests doing is text: use a command or button.
- **With Copilot:** each question runs with a new, empty `COPILOT_HOME`, so none of your Copilot
  MCP servers, hooks, plugins, skills, custom instructions or memories load, and the session is
  deleted afterwards. The login still works because the CLI keeps it in the OS credential store.
  This was checked on Windows with Copilot CLI 1.0.89, not yet on macOS or Linux. If questions fail
  there with a login error, put a token in the harness's `env` as `COPILOT_GITHUB_TOKEN`. Copilot's
  way of turning tools off was checked on 1.0.89. These runs use `--no-auto-update`, so they use the
  installed binary, not a version Copilot downloaded by itself. After upgrading that binary (e.g.
  with winget or npm), run the live test from `backend/`:
  `AGENTFLOW_LIVE_ASK=copilot pytest tests/test_chat_ask.py -k live_copilot` (two premium requests).
  If a run shows any tool use, the answer is discarded and the backend logs an error.
- **What it knows:** every task's phase and what needs you (as `list` shows), and, for the task in
  question, its stages, open agent questions (if `includeQuestionText`) and the `slack.details`
  sections you allowed. It never gets a section you haven't allowed. With `pr` and `ticket` allowed,
  it can answer about that task's pull request and Jira ticket, from AgentFlow's copy. It cannot look
  anything up on GitHub or Jira, so it knows nothing about PRs or tickets that aren't AgentFlow tasks.
- **Memory:** in a thread it remembers the last three questions and answers. This is in memory
  only and gone after a restart.
- **Limits:** one question at a time; `timeoutSeconds` per question (90 by default); questions up
  to 2000 characters, answers cut at 3500.
- **Cost:** it uses that harness's subscription. If its `billingMode` isn't `"subscription"`,
  questions are refused unless you set `allowMetered`, because each question is then billed. With
  Copilot, each question uses one premium request (AI credits on the newer billing), even on a
  subscription. Premium requests beyond your plan's allowance are billed. The example `copilot`
  harness has no `billingMode`, so it counts as `unknown` and is refused until you set one.

Answers are marked *AI answer … it can be wrong*. Jira, agent and review text in the context is
untrusted. With no tools, the worst it can do is mislead the answer, so check AgentFlow before
acting on one. The CLI tells the model its working directory and OS; the prompt says not to repeat
them, but a determined question may still get them. Answers are shown only to you: in your DM, or
privately where you typed `/agentflow ask`. With questions on, every message of two or more
words that isn't a command is one model call on your subscription.

## Setup

`agentflow setup` (or `setup.ps1` / `setup.sh`) covers steps 1 and 6 and helps with step 2: choose
Slack when it asks, and it installs the libraries (with your consent), asks you to name your agent,
which names your Slack app and makes your slash command `/agentflow-<name>` (it suggests your user
name; type a whole command such as `/team-bot` to pick another), asks whether to add that slash
command at all, prints the app manifest, then asks for both tokens and your member ID. You still create the app (steps 2 to 5) and restart (step 7).

1. **Install the optional libraries** into the backend's environment. `agentflow setup` offers to
   do this when you choose Slack. By hand, from the project root (a venv made by uv has no pip):
   ```
   uv pip install --python backend/.venv/Scripts/python.exe -e "backend[slack]"   # Windows
   uv pip install --python backend/.venv/bin/python -e "backend[slack]"           # macOS / Linux
   ```
2. **Create the Slack app** at <https://api.slack.com/apps> → *Create New App* → *From a
   manifest*, choose your workspace, and paste the manifest. `agentflow setup` names the app and
   command after your agent and prints the manifest as JSON, ready to paste (select *JSON* in Slack's
   editor). By hand, use this YAML with your agent's name in place of `Jarvis` and `jarvis`. In the
   app `name`, use only letters, digits, spaces, `.`, `-` and `_`: Slack refuses symbols such as
   brackets there. The bot's `display_name`, which messages and the DM show, accepts them; keep its
   quotes, since YAML reads an unquoted `[` as the start of a list.
   ```yaml
   display_information:
     name: AgentFlow Jarvis
   features:
     app_home:
       messages_tab_enabled: true
       messages_tab_read_only_enabled: false
     bot_user:
       display_name: "[AgentFlow] Jarvis"
     slash_commands:
       - command: /agentflow-jarvis
         description: AgentFlow task status
         usage_hint: "list | status KEY | details KEY | digest | new"
   oauth_config:
     scopes:
       bot: [chat:write, im:write, im:history, reactions:write, commands]
   settings:
     socket_mode_enabled: true
     interactivity:
       is_enabled: true
     event_subscriptions:
       bot_events: [message.im]
   ```
   **No slash command?** Delete the `slash_commands` block and the `commands` scope, and set
   `slack.command` to `null` (step 6). See [Several people in one workspace](#several-people-in-one-workspace).

   **Verify:** this manifest and the scopes have not yet been tried against a live workspace. If
   Slack rejects a field, create the app by hand with the same settings: Socket Mode on,
   Interactivity on (no request URL is needed with Socket Mode), Event Subscriptions on with the
   bot event `message.im`, the Messages tab enabled and **not** read-only, bot scopes `chat:write`,
   `im:write`, `im:history`, `reactions:write` and `commands`, and your slash command.

   **Already created the app from an earlier manifest?** Update it to match. Without
   *Interactivity & Shortcuts* the answer buttons do nothing. Without the `message.im` event,
   `im:history` and a writable Messages tab, typing in the DM gets no reply. Without
   `reactions:write`, replies still arrive but your message gets no 👀 or ✅
   (the backend logs `missing_scope`). After adding a scope, **reinstall** the app
   (*Install App → Reinstall*).
3. **Install it to the workspace** (*Install App*), then copy the **Bot User OAuth Token** (`xoxb-…`).
4. **Create an app-level token**: *Basic Information → App-Level Tokens → Generate*, with scope
   `connections:write`. Copy it (`xapp-…`).
5. **Find your member ID**: in Slack, open your profile → ⋮ → *Copy member ID* (`U…`).
6. **Configure AgentFlow** (`agentflow setup` does this, writing the tokens to `slack.botToken` /
   `slack.appToken` and the command to `slack.command`). By hand, put the tokens in the backend's environment:
   ```
   AGENTFLOW_SLACK_BOT_TOKEN=xoxb-…
   AGENTFLOW_SLACK_APP_TOKEN=xapp-…
   ```
   Then add this to `config.json`:
   ```json
   "slack": {
     "enabled": true,
     "userId": "U…",
     "command": "/agentflow-jarvis",
     "includeQuestionText": true,
     "notify": ["WAITING_FOR_USER", "WAITING_FOR_APPROVAL", "FAILED", "INTERRUPTED",
                "NEEDS_HUMAN_REVIEW", "READY_FOR_PR"],
     "uiBaseUrl": "http://127.0.0.1:5173",
     "allowActions": ["answer"],
     "details": []
   }
   ```
   `command` must match the slash command in the app's manifest: lowercase letters, digits, `-`
   or `_`, at most 32 characters including the `/`. It defaults to `/agentflow`; the wizard sets
   `/agentflow-<agent name>`. To change it later, edit `slack.command`, rename the command in the
   app's *Slash Commands* to match, reinstall the app and restart. `"command": null` means no slash
   command: AgentFlow then takes commands only in your DM with the app.
   Add sections to `details` (e.g. `["requirements", "cost", "activity"]`) to allow them; see
   [Task details](#task-details-slackdetails). Add `"create"`, `"run"`, `"requestChanges"`,
   `"approveRequirements"`, `"cancel"` and `"markPr"` to `allowActions` for the actions you want
   Slack to take.
   `botToken`/`appToken` in `config.json` work as a fallback. `config.json` is gitignored, but it
   is plaintext. The environment variables are never passed to agents, because harness launches
   inherit only an allowlist of variables.
7. **Restart the backend.** A config reload won't do; it refuses Slack changes. The log says
   `slack connected`. If Slack is misconfigured or unreachable, the backend still starts. It
   logs why and keeps retrying the connection. If it logs `Slack is off: slack.enabled is not true
   in <path>`, the backend read a different config file than the one you edited; put the `slack`
   block in that file.

### Several people in one workspace

Everyone runs AgentFlow on their own machine, so everyone creates their **own** Slack app, with its
own tokens and its own slash command, such as `/agentflow-jarvis` and `/agentflow-friday`.

- **Don't share one command name.** Slash commands are shared by every app in the workspace. When
  two apps register `/agentflow`, Slack sends each use to just one of them, and the other person's
  AgentFlow replies *"You are not authorized to use AgentFlow."* because its `userId` is theirs.
- **Don't share one app's tokens.** Socket Mode gives each event to one open connection, not all
  of them, so commands, button clicks and DMs would land on whichever machine Slack picks.
- **DMs never clash.** Each app has its own Messages tab, so typing `list` or `new` there always
  reaches your AgentFlow, whatever the command is called.
- **Or go without a slash command.** A slash command shows in the `/` menu of everyone in the
  workspace. To keep your app out of it, answer *no* when `agentflow setup` asks to add the
  command (or set `"command": null`, delete `slash_commands` and the `commands` scope from the
  manifest, and reinstall). Everything still works from your DM with the app. The app itself is
  still listed under *Apps*, but only your `userId` can use it.

**Moving an existing app to its own command:** in the app's settings, open *Slash Commands* and
rename `/agentflow` (or re-run `agentflow setup` and paste the manifest it prints into *App
Manifest*). Reinstall the app (*Install App → Reinstall*), set `slack.command` to the new name, and
restart the backend. The log line for a refused user names their member ID, so a clash shows up in
the log of the AgentFlow that refused it.

If your workspace requires admin approval for new apps, each person's app needs that approval.

`notify` may also include `READY` (a stage is ready to start; noisy in manual mode) and
`CANCELLED`. Use `uiBaseUrl` `http://127.0.0.1:8765` when you run the built frontend from the
backend instead of the Vite dev server.

## Smoke test

1. With the backend running, run `/agentflow list` in any conversation (your DM with yourself
   works). The reply is visible only to you.
2. Open the app's Messages tab and type `list`, then `digest`. Both get a reply in the DM.
3. Run a stage until an agent asks a question. A DM thread appears with the question and its
   buttons. In that thread, type `status`: the reply is about that task. Type `Gradual`: it comes
   back quoted with **Send as answer**, and the agent's terminal is unchanged until you click it.
4. Click a choice. The agent's terminal receives it, and the message changes to *Answered:*.
5. At the next question, use **Reply…**, type `a < b & c` and press Send. **Verify** that the
   agent's terminal shows exactly `a < b & c`, not `&lt;` or `&amp;`. Choice buttons can't have
   this problem, because their text comes from AgentFlow; typed answers come from Slack. Repeat
   by typing `a < b & c https://example.com` in the thread and clicking **Send as answer**. The
   link must arrive as plain text, not `<https://…>`.
6. Click a button on an already-answered question, e.g. from an older notice. The thread shows
   why it was refused.
7. With `"details": ["activity"]`, type `details` in the task's thread: only *Recent activity*
   appears, and `details findings` is refused.
8. With `"allowActions": ["answer", "create", "run"]`, run `/agentflow new`. **Verify** that the
   ticket, repository and branch fields list options as you type. They load over Socket Mode
   (`block_suggestion`), which has not been tried against a live workspace. Submit with an existing
   key: the ticket field shows the error. Submit a new one: the form closes and a thread opens
   with **Run Architect**. Click it and confirm: the Architect starts and the button message says so.
9. With `requestChanges`, `cancel` and `markPr` allowed:
   - at a Requirements Approval notice, use **Request changes…** and send feedback: the thread
     confirms it and offers **Run Architect**;
   - `pr KEY` on a Ready-for-PR task opens the link form. A link without `https://` is refused on
     the form itself;
   - `cancel KEY` asks for confirmation before anything happens.
10. With `approveRequirements` allowed, at a Requirements Approval notice use **Review
    requirements**. Check that every document appears in full, then approve. Then review again,
    edit a requirement file in AgentFlow's task folder, and click the new approve button: it is
    refused because the requirements changed.
11. With `"ask": {"enabled": true}`, type `what's waiting on me?` in the DM. The answer arrives in
    a few seconds, marked as an AI answer. In a task's thread, ask `ask why is it stuck?`, then a
    follow-up; the follow-up should build on the first answer.
12. Restart the backend. Nothing is re-posted.

Troubleshooting: *"You are not authorized to use AgentFlow."* means the AgentFlow that answered
doesn't have your member ID as `slack.userId`. Check your own `userId` first (the backend log names
the member ID it refused). If it's right, another person's app answered your command: pick your app
in the command's autocomplete list, or give each app its own command
([Several people in one workspace](#several-people-in-one-workspace)).
*"Slack is enabled but its libraries are missing"* in the backend log, even after a
`pip install -e "backend[slack]"`, means that `pip` installed into a different Python than the one
the backend runs from. Use the venv's own interpreter, from the project root, then restart:
`backend\.venv\Scripts\python.exe -m pip install -e "backend[slack]"` (Windows) or
`backend/.venv/bin/python -m pip install -e "backend[slack]"` (macOS / Linux).
`invalid_auth` means the bot token is wrong. `not_allowed_token_type` means the
bot token and app token are swapped. If you don't see the slash command, reinstall the app after
changing the manifest. If buttons show a warning icon when clicked, Interactivity is off. If
typing in the DM gets no reply, check the `message.im` event and the `im:history` scope, then
reinstall.

If Slack says **"Sending messages to this app has been turned off"**, the manifest alone may not
be enough. Open *App Home → Show Tabs*, tick **"Allow users to send Slash commands and messages
from the messages tab"**, then quit and reopen Slack. The desktop client caches the old setting.
Slash commands work without this, even in a DM with yourself.
