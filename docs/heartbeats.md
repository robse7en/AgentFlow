# Agent heartbeat visibility

AgentFlow accepts observational heartbeats from the current agent run in both the Rust
desktop/server and Python web backend. This adds health visibility without changing task
states, worker capacity, verification, reviews or approval rules.

## Agent reporting

New agent briefings request a report at the start and roughly every five minutes of active
work, including after a long tool returns:

```text
flowctl heartbeat --run <current-run-id> --phase testing
```

Allowed phases are `working` (default), `investigating`, `implementing`, `testing` and
`reviewing`. Both flowctl implementations use the existing scoped credential file. The
server checks task, role, run and current attempt; inactive or superseded runs are rejected.
Reports during a human wait are permitted but cannot clear that wait.

The latest receipt time and phase are stored per run in `run_heartbeats`. Receipt time is
server-generated UTC. Repeated reports update one row and do not append history events.
Snapshots broadcast the observation to connected task views. Deleting the run cascades to
its heartbeat. Existing databases gain the table when opened by the upgraded backend.

Existing running sessions keep their old briefings. They can use the new command once the
backend and flowctl are upgraded, or receive the instructions in their next generated briefing.

## Display and configuration

Agent nodes and expanded worker cards show the age of the last report and its phase. Before
the first report they show that a signal is awaited. An overdue signal is a warning to inspect
the pane, not a failure verdict. Existing terminal inspection/recovery controls remain available.

```json
{
  "workflow": {
    "heartbeatWarningSeconds": 600
  }
}
```

The default is ten minutes; the accepted range is 300 to 86400 seconds. Configure it in the
instance configuration and use the existing configuration reload/restart path. There is no new
Settings form. The agent reporting cadence remains approximately five minutes.

Warnings pause while the node waits for a human answer. A resumed node receives a fresh
warning window based on its state-change timestamp. Interrupted and settled nodes do not
show active heartbeat warnings. The UI updates ages locally every fifteen seconds, using
the server observation time plus monotonic elapsed time to avoid browser/server clock skew.

## Interpretation and limits

- A heartbeat proves only that the authenticated agent reported; it is not verified progress.
- No background watchdog is introduced. A model cannot send a report while blocked inside a
  long tool call. Quiet work can therefore become overdue even when it is healthy.
- Existing terminal/session observations are separate; a live shell cannot prove agent health.
- Warnings are displayed in task views, not emitted as Slack messages or per-ping history.
- Silence never terminates an agent, launches a retry, releases a worker slot or approves work.
- Machine sleep can delay timers and callbacks. On platforms where monotonic browser timers
  pause during sleep, a fresh snapshot/report is needed for an accurate age after waking.
- Receipt timestamps survive restart, while normal explicit interrupted-run recovery still
  applies. This does not add automatic recovery or claim live-provider hang detection.

## Validation

Regression coverage checks stale/inactive attempts, scoped API credentials, bounded phases,
coalesced reporting, preserved human waits and the UI warning boundary/resume grace. Config,
schema and prompt fixtures keep the Rust and Python implementations aligned. The local
contract test exercises the real HTTP API and both flowctl clients with a stub harness.
These checks do not substitute for visible desktop or authenticated provider acceptance.

Verified on 2026-10-16: 124 selected Python regressions, 104 Rust core checks (three
environment-dependent checks ignored), 13 Rust CLI checks and nine localhost contract cases
passed. The frontend production build and heartbeat component rendering checks passed.
The existing worker UI script passed its worker-card assertions, then failed an unrelated
execution-evidence assertion expecting `tokens unknown` where the UI renders `usage unknown`.
The same assertion fails in the unchanged main checkout; it was left outside this feature's scope.

Re-verified on 2026-10-21 after rebasing onto main 0.1.12 (worktree setup, merge-risk factors):
740 Python tests, 740 Rust tests with `cargo fmt --check` and `clippy -D warnings` clean, the
contract suite 97/97 against both the Python and Rust backends, the frontend production build
and the heartbeat component checks. Config and prompt fixtures were re-recorded from Python.
