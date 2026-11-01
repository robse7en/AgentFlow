# Headless Zellij on Windows

Status: **implemented** (`feat/zellij-headless`). Verified on Windows 10 with Zellij 0.45.1.

Before this, every task on Windows opened a real console window (`cmd /c start /max`, then
minimized by a PowerShell/user32 hack), because `attach --create-background` appeared to crash there.
Sessions now run with no visible window at all.

## How it works

Two hidden processes per session, both started by `spawn_hidden_console`
(`CREATE_NEW_CONSOLE` + `SW_HIDE` + `CREATE_BREAKAWAY_FROM_JOB`, **no stdio redirection**):

1. **Server**: `zellij attach --create-background <session>` (`ZellijService._ensure_session`).
2. **Sizing client**: `cmd /c "mode con: cols=C lines=R & "<zellij>" attach <session>"`, one per
   session. `_ensure_session` starts it as soon as the server is up, and it's kept alive and tracked in
   `ZellijService._sizing_clients`. `ensure_client_attached` runs before every `new_tab` and respawns it if it died. `C`×`R` come from
   `zellij.headlessCols`/`headlessRows` (default 200×50), so agent panes are 200×48 (minus tab bar and status bar).

Both outlive the backend. After a restart, an untracked session that already lists a client is
reused as-is, so no duplicate client is spawned (verified live).

## Why each piece is needed (verified by experiment)

- **The "crash" was ours, not Zellij's.** With NUL std handles (what `spawn_detached` always passes), the
  server's first pane's shell reads EOF and exits at once. The server then fails to reap it (`failed to kill pid …
  The parameter is incorrect. (os error 87)`) and dies within a second. The same command without
  redirection, in a hidden new console, stays up, including after its Python parent exits.
- **A client is needed at tab-creation time only.** With no client the session is 50×50, and `new-tab`
  logs `Failed to apply layout: Not enough room for panes`. Once a tab exists, `subscribe`, rendering and `write-chars`
  all work with no client attached.
- **A paneless tab is worse than a failed launch.** Its command still runs, but the tab vanishes from
  `list-tabs` (so it can't be closed) and the process lives on invisibly until `kill-session`. So
  `new_tab` is never retried. If a client can't be confirmed, `ensure_client_attached` raises instead.
  A paneless result is recorded as the `zellij.tab.paneless` diagnostics event.
- **`list-clients` is unreliable both ways.**
  - It missed a working hidden client (its tab got a real pane).
  - After `--create-background` it lists a phantom launcher client that sizes nothing. It was gone after 1.5s in one
    probe and still listed after ~5s in another, and it looks identical to a real client.

  So a new session's first client is confirmed **by geometry**: the widest pane grows past the 50-column clientless
  default. It is not compared with `headlessCols`: a console window can't be wider than the screen, so in a
  disconnected or locked Windows session (a screen of about 900 px) `mode con: cols=200` still yields a window of
  about 118 columns, and agent panes get that width instead. If it can't be confirmed, the half-made
  session is killed so a retry starts clean. After that, a tracked client is judged by process liveness.
  `list-clients` is only used to confirm a *respawned* client, when no phantom exists.
- **`mode con` sets the size.** Without it a hidden console is 120×30. The chain has to go through
  `cmd /c "…"` as a raw string (asyncio shell mode), because list-form argv re-quotes the inner exe path into something
  cmd can't parse.

## Interactive access

The terminal drawer's **Open window** button → `POST /api/tasks/{key}/zellij/window?node=` →
`ZellijService.open_window`:

- It calls `go-to-tab-name` first. A newly attached client joins whichever tab the existing (hidden)
  client is on, so this lands the window on the agent's tab.
- The window is opened maximized (`start /max`). A tab is laid out at its smallest viewing client's
  size and doesn't grow back after that client leaves, so the window must not be smaller than the hidden client.

## Known limits

- On Windows 11 with Windows Terminal set as the default terminal, `CREATE_NEW_CONSOLE` may be handed
  to Windows Terminal, which ignores `SW_HIDE`. This is untested. The fallback would be hosting the sizing client in
  a backend-owned ConPTY (`pywinpty`), which gives exact size control and has no console at all.
- One hidden `cmd` + `zellij` client pair stays running per live task session. Sharing one session
  across tasks would reduce that to one.
