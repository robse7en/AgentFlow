# Slack setup, step by step

Setup takes about 15 minutes: create a Slack app, copy two tokens and your member ID, add them to
AgentFlow, then restart the backend. Once it's done, AgentFlow sends you a DM when a task needs you,
and you can answer agent questions from Slack.

- **Off by default.** Only the one Slack user you configure gets DMs or can use your slash command.
- **One app per person.** Everyone who runs AgentFlow creates their own Slack app with its own
  slash command, such as `/agentflow-jarvis`. Slash commands are shared by the whole workspace, so two
  apps both using `/agentflow` get each other's commands. See
  [Several people in one workspace](slack.md#several-people-in-one-workspace).
- **No public URL.** Slack connects over Socket Mode, an outbound connection, so the backend still
  listens only on loopback.
- **You need:** a Slack workspace where you can create apps, the AgentFlow backend venv, and about
  15 minutes.
- **Full reference:** every setting is described in [slack.md](slack.md). This guide covers only the
  path to a working setup.

## Step 1 — Install the Slack libraries

The backend needs the optional `slack` extra, which installs `slack_bolt` and `aiohttp`. You can let
the setup wizard do it or install it yourself.

**Option A — the wizard.** Run `powershell -ExecutionPolicy Bypass -File setup.ps1` or
`backend/.venv/Scripts/agentflow setup`. Answer **yes** to *Use Slack DMs?*, then **yes** to
*Install it now?*. It then asks you to name your agent (it suggests your user name). The name
becomes your Slack app's name and your slash command, `/agentflow-<name>`: an agent called Jarvis is
the app *AgentFlow Jarvis*, whose bot is *[AgentFlow] Jarvis*, with the command `/agentflow-jarvis`.
To pick a different command, type it whole, e.g. `/team-bot`. It then asks whether to add the
slash command at all: everyone in the workspace sees it in their `/` menu, so answer **no** to keep
it to yourself and type commands in your DM with the app instead. It prints the app manifest as JSON.
Leave the wizard open there. It asks for the tokens next, and you create them in steps 2–5.

**Option B — by hand**, from the project root. A venv made by uv has no pip, so use `uv pip`:

```
# Windows
uv pip install --python backend/.venv/Scripts/python.exe -e "backend[slack]"

# macOS / Linux
uv pip install --python backend/.venv/bin/python -e "backend[slack]"
```

## Step 2 — Create the Slack app

1. Go to <https://api.slack.com/apps> and click **Create New App → From a manifest**.
2. Choose your workspace.
3. Paste the manifest and click **Next → Create**:
   - **From the wizard:** switch the format to **JSON** and paste what the wizard printed.
   - **By hand:** switch the format to **YAML** and paste the manifest below, with your agent's name
     in place of `Jarvis` and `jarvis`, keeping the quotes. Pick a name nobody else in the
     workspace uses. The app `name` can't contain symbols such as brackets; the bot's
     `display_name` can.

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

**No slash command?** Delete the `slash_commands` block and the `commands` scope before pasting,
and set `"command": null` in step 6.

This manifest hasn't been tested against a live workspace yet. **If Slack rejects it**, create the
app **From scratch** and set these by hand:

| Where in the app settings | Setting |
| --- | --- |
| Socket Mode | On |
| Interactivity & Shortcuts | On (no Request URL needed with Socket Mode) |
| Event Subscriptions | On, bot event `message.im` |
| OAuth & Permissions → Bot Token Scopes | `chat:write`, `im:write`, `im:history`, `reactions:write`, `commands` (only with a slash command) |
| Slash Commands | Your command, e.g. `/agentflow-jarvis`, or none |
| App Home → Show Tabs | Messages Tab on, and **Allow users to send Slash commands and messages from the messages tab** ticked |

If you're updating an app made from an older manifest, make it match this table, then reinstall it
(**Install App → Reinstall**). Without Interactivity, the buttons do nothing. Without `message.im`,
`im:history` or a writable Messages tab, messages you type in the DM get no reply.

## Steps 3–5 — Collect two tokens and your member ID

You need three values. Keep the tokens private: anyone who has them can post to your workspace as
the app.

| Step | Value | Looks like | Where to get it |
| --- | --- | --- | --- |
| 3 | Bot User OAuth Token | `xoxb-…` | **Install App → Install to Workspace**, then **Allow**. Copy the token shown on the page. |
| 4 | App-level token | `xapp-…` | **Basic Information → App-Level Tokens → Generate Token and Scopes**. Name it (e.g. `socket`), add the scope `connections:write`, then **Generate**. |
| 5 | Your member ID | `U0123ABCD` | In Slack, open your profile, click **⋮ (More) → Copy member ID**. |

Don't mix up the two tokens. If they're swapped, the backend logs `not_allowed_token_type`.

## Step 6 — Configure AgentFlow

The settings go in the `slack` section of `config.json`, which sits in the project root of a source
checkout. The tokens can go there too, or in environment variables.

**6a. With the wizard.** Go back to the wizard from step 1, or run
`backend/.venv/Scripts/agentflow setup`. Paste the `xoxb-` token, the `xapp-` token and your member
ID when asked. The wizard sets `enabled`, `command`, `botToken`, `appToken` and `userId`. It **doesn't** change
`allowActions` or `details`, so check those in 6c.

**6b. Or by hand: tokens.** Environment variables take precedence over `slack.botToken` /
`slack.appToken`. AgentFlow doesn't read a `.env` file, so set them in the shell that starts the
backend, or set them permanently for your user:

```powershell
# This PowerShell session only
$env:AGENTFLOW_SLACK_BOT_TOKEN = "xoxb-..."
$env:AGENTFLOW_SLACK_APP_TOKEN = "xapp-..."

# Permanently for your user; open a new terminal afterwards
setx AGENTFLOW_SLACK_BOT_TOKEN "xoxb-..."
setx AGENTFLOW_SLACK_APP_TOKEN "xapp-..."
```

Agents never see these variables: harness launches pass on only an allowlist of variables.
`config.json` is gitignored but stored in plain text, which is why environment variables are the
preferred place.

**6c. The `slack` section.** Start small. This is the safe minimum: DMs, plus answering agent
questions.

```json
"slack": {
  "enabled": true,
  "userId": "U0123ABCD",
  "command": "/agentflow-jarvis",
  "includeQuestionText": true,
  "notify": ["WAITING_FOR_USER", "WAITING_FOR_APPROVAL", "FAILED", "INTERRUPTED",
             "NEEDS_HUMAN_REVIEW", "READY_FOR_PR"],
  "uiBaseUrl": "http://127.0.0.1:5173",
  "allowActions": ["answer"],
  "details": []
}
```

- `userId` must match `U…` or `W…`, using uppercase letters and digits only.
- `command` must be the slash command in your app's manifest: lowercase letters, digits, `-` or
  `_`, at most 32 characters including the `/`. Without it, AgentFlow listens on `/agentflow`. You
  can change it any time: edit `command`, rename the command in the app's **Slash Commands** to
  match, reinstall the app, and restart the backend. Set it to `null` if your app has no slash
  command.
- `uiBaseUrl` is where Slack's links go: `http://127.0.0.1:5173` for the Vite dev server,
  `http://127.0.0.1:8765` for the built UI served by the backend.
- `allowActions` sets what Slack may do. Add `create`, `run`, `requestChanges`,
  `approveRequirements`, `cancel` or `markPr` one at a time, as you need them.
- `details` sets what Slack may post when you ask: `requirements`, `findings`, `errors`, `cost`,
  `activity`, `pr`, `ticket`.

**Watch out if you copied `config.example.json`.** Its `slack` section turns on **every** action and
**every** detail section; it's disabled only because `enabled` is `false`. If you just flip
`enabled` to `true`, review findings, errors, Jira text and full requirement documents can be posted
into your Slack history. Trim both lists to what you want before enabling.

## Step 7 — Restart and verify

Stop the backend and start it again. A config reload isn't enough: it refuses Slack changes.

```powershell
cd backend
.venv/Scripts/python.exe -m agentflow.main
```

Or run `./start.sh` from Git Bash, which stops the previous instance for you. Then check these four
things in order:

1. **`agentflow doctor`** reports **Slack: configured**. It checks the settings and the libraries,
   but **doesn't** test the tokens.
2. **The backend log** shows `slack connected`. If Slack is misconfigured, the backend still starts,
   logs why, and keeps retrying.
3. **In Slack**, run your command, e.g. `/agentflow-jarvis list`, in any conversation. A DM with yourself works. The reply is
   visible only to you.
4. **Open the AgentFlow app's Messages tab** and type `list`, then `digest`. Each gets a reply, and
   your message gets a 👀 reaction, then ✅.

If all four pass, you're done. The next agent question arrives as a DM with one button per choice.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| Log shows `invalid_auth` | The bot token is wrong. Copy the `xoxb-` token again from **Install App**. |
| Log shows `not_allowed_token_type` | The bot and app tokens are swapped. |
| No slash command in Slack | Reinstall the app after any manifest change (**Install App → Reinstall**). |
| "You are not authorized to use AgentFlow." | The AgentFlow that answered doesn't have your member ID. Check `userId` first; the backend log names the member ID it refused. If yours is right, another person's app answered: give your app its own command ([Several people in one workspace](slack.md#several-people-in-one-workspace)). |
| The command does nothing, or Slack says it failed | `slack.command` doesn't match the command in the app's manifest. Make them the same and restart. |
| Buttons show a warning icon when clicked | Interactivity is off. Turn it on under **Interactivity & Shortcuts**. |
| Typing in the DM gets no reply | Check the `message.im` event and the `im:history` scope, then reinstall the app. |
| "Sending messages to this app has been turned off" | **App Home → Show Tabs**: tick **Allow users to send Slash commands and messages from the messages tab**. Then quit and reopen Slack, because the desktop client caches the old setting. |
| Replies arrive but there's no 👀 or ✅ (log shows `missing_scope`) | Add the `reactions:write` scope and reinstall. |
| Nothing changed after editing `config.json` | Restart the backend fully; a config reload doesn't apply Slack changes. |

## Optional, once it works

Each of these is described in [slack.md](slack.md):

- **Daily digest:** `"digestAt": "09:00"`, in this machine's local time.
- **Stage-ready notices:** add `"READY"` to `notify`. It's noisy in manual mode.
- **Plain-language questions:** `"ask": {"enabled": true, "harness": "claude"}`. It runs your own
  harness CLI with every tool off, and each question is one model call on your subscription.
- **Full smoke test:** the [Smoke test](slack.md#smoke-test) section of `slack.md` covers answering,
  forms, requirement approval and restarts.
