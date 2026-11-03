# `flowctl`: how agents talk to AgentFlow

Agents run these inside their Zellij pane (task/role/run id come from the environment):

```
flowctl status                                   # workflow state
flowctl question --message "Should this apply to all tenants?"   # → WAITING_FOR_USER
flowctl question --file - <<'EOF'                # a decision tree (see "Agent questions" in workflow.md)
{"message": "...", "risk": "medium", "choices": [{"label": "...", "recommended": true}, {"label": "..."}]}
EOF
flowctl running                                  # optional: AgentFlow resumes the agent after the last answer
flowctl complete --file result.json              # structured completion (see prompt for the schema)
flowctl finding --id CODE-001 --severity high --title "..." --file src/x.cs --line 12 --end-line 14   # incremental findings
flowctl finding --prior-id CODE-001 --blocking --title "..."    # re-raise a previous-round blocker
flowctl fail --message "Blocked because ..."
```

`flowctl` is a thin HTTP client for `http://127.0.0.1:<port>/api/tasks/<KEY>/agent/*`; it never touches the database.
The prompts written to `runs/<role>/NN/prompt.md` tell each agent exactly which command and JSON shape to use.
