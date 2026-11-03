# Repository agent instructions

AgentFlow can add repository-owned role guidance to every harness's existing `prompt.md`
briefing. Desktop and Python/web use the same layout in the **target repository root**:

```text
.agentflow-instructions/
    architect.md
    orchestrator.md
    implementer.md
    code-reviewer.md
    security-reviewer.md
```

No config setting, database instruction-text field, global library, or installer resource is
needed. These files belong to the project being worked on, independently of AgentFlow upgrades.
Provider-native configuration remains useful for provider capabilities. This additional text
has no provider-native system-message priority and does not guarantee identical model behavior.

## Role mapping and composition

| File | Applies to |
| --- | --- |
| `architect.md` | Discovery, requirements revisions, Architect Planning (new Parallel tasks), final verification |
| `orchestrator.md` | Planning by a separately configured Orchestrator (`agents.orchestrator` or an Orchestrator override), and by tasks created before planning moved to the Architect |
| `implementer.md` | Ordinary implementation, fix rounds, parallel workers, integration |
| `code-reviewer.md` | Every code review and retry |
| `security-reviewer.md` | Every security review and retry |

Generated workflow instructions remain intact. AgentFlow first rewrites its own flowctl
examples with the run ID, then appends a clearly labelled guidance section. Custom text is
never rewritten, templated, interpreted as a command, or followed to execute linked scripts.
Existing permissions, reporting schemas, approved scope, ownership checks, approvals and
completion checks remain enforced by the engine. Use `flowctl question` for conflicting guidance.

All five conventional sources are optional on the initial capture; there is no arbitrary
source-selection or required-file setting. An absent role is recorded as `content: null` and
`hash: null`. Empty or absent guidance contributes no extra prompt section, preserving existing
briefings and their native file line endings. Captured non-empty guidance preserves its exact
UTF-8 content, including line endings; generated text and custom content are written without
another newline conversion. Files without an instruction directory need no configuration.

## Editing and preview

Use **Projects → project actions → Agent instructions…** to edit the project's default
repository. Choose a target repository in project defaults first. This editor changes that
repository's working copy, rather than a task's isolated worktree.

Use **Task → Agent instructions** for the task worktree. Select the role/stage and choose:

- **Edit source / preview saved source**: current repository content, optional starter creation,
  save with a concurrency check, and the briefing that would result from the saved sources.
- **Future run using task snapshot**: the frozen version that the next run will receive, its
  captured content and revision, and whether sources differ.
- **Historical run**: a read-only view of a previous run's actual briefing, source, instruction
  version, repository revision and briefing SHA-256. Legacy runs are explicitly identified.

Draft a starter explicitly, edit it, and press **Save source** to create the file. Saving never
commits. The editor shows uncommitted instruction paths and explains that Git creates new
worktrees from committed files: uncommitted edits in a project's main checkout do **not** travel
to a new task worktree. Commit instructions using your normal Git review process before creating
new tasks, or edit a task's own source files and explicitly apply them there.

Saved-source preview excludes unsaved draft changes. Task previews use run ID `0` as a visible
placeholder and the next role run directory. They use the same template builder and composer as
launch; workflow state, Jira synchronization and assigned run ID can change before actual launch.
Project editing has no workflow briefing until a task exists. Historical inspection does not
depend on the worktree still existing.

## Lessons proposed by agents

Agents can propose additions to their own role's file. Every briefing tells them to use
`flowctl lesson --message "…"` for something durable about the repository, such as a build quirk, a
convention or a check that gives false alarms, rather than something about one task. A proposal is
only a record on the task (`<task>/lessons.json`). It changes nothing for the current run.

- **Review:** the task's **Lessons** tab lists proposals. Edit the text if needed, then
  **Approve into guidance** or **Dismiss**.
- **What approval does:** it appends the lesson as one bullet under a `## Lessons from past tasks`
  heading. The heading is added when it isn't the file's last heading. The file is the role's
  `.agentflow-instructions/<role>.md` in the task's **original repository checkout** (its repository
  root), never the task worktree.
- **What approval doesn't do:** it never commits. The tab shows the checkout's uncommitted paths.
  Commit the file with your normal review, and tasks created from that commit capture it in their
  instruction snapshot. Until then no agent sees it.
- **Saving:** approval uses the same validated save as the editor: role allowlist, size limits, the
  hash check against concurrent edits, and an atomic write. An approval that doesn't fit, or that
  races an external edit, is refused with `409` and the lesson stays proposed.
- **Text rules:** lesson text is agent output. It is normalised to a single line, at most 300
  characters, so it can't add headings or other structure. It is shown as plain text, and the
  existing file's line endings are kept.
- **Limits:** five proposals per run and 50 per task. A lesson can be decided once; deciding it
  again returns `409`. Agents' credentials can propose but never approve or dismiss.

## Snapshots, updates and conversations

The first agent launch captures all five roles from the task worktree, including the exact
contents, relative paths, SHA-256 hashes and current Git revision. Later stages, queued workers,
integration and retries reuse this immutable instruction set; editing or deleting a source
file cannot change it. Backend restart does not recapture it.

**Apply saved instruction set to future runs** explicitly creates a new UUID version, using
the version and source hashes you reviewed. It retains every previous version and run artifact.
It never sends text to, stops, restarts or releases an agent. To keep a parallel batch consistent,
updates are refused from creation of the worker plan until integration completes, including
queued or failed workers. Finish/retry that batch with its original guidance first. This also
means a cancelled unfinished batch cannot be updated; create a new task for a different plan.

The fresh-conversation checkbox defaults on. After applying with it enabled, each role's next
launch starts fresh if its previous conversation used an older instruction version. Later
launches on the same version may resume normally. With it disabled, normal exact-session resume
rules apply, and older instructions may remain in conversation history. Existing reviewer-round
freshness rules and worker fresh launches still apply. Resuming a process that survived a backend
restart continues its original run/version; an update cannot erase its conversation. Use the
existing human-controlled recovery/retry controls when you decide to stop or replace it.

Under the configured task artifact directory:

```text
<task>/instructions-required.json       # required-snapshot marker
<task>/instructions/active.json         # atomic pointer to the current version
<task>/instructions/<uuid>.json         # immutable complete captured role set
<task>/runs/<role>/<number>/prompt.md   # exact delivered briefing
<task>/runs/<role>/<number>/instructions.json
```

Snapshots use schema `1` in both backends. Run metadata records version, role, source content/path/
hash, revision and SHA-256 of the **actual prompt file bytes**. These are delivery evidence, not
proof the model obeyed the guidance. Preserve these artifacts together with the task database
when backing up/moving an instance. No database migration is needed. Filesystem artifacts are
the historical store; editable source text remains exclusively in the target repository.

An absent initial conventional source differs from a required stored snapshot that disappears
or becomes invalid: the latter blocks future launches/updates with a conflict instead of silently
recapturing new instructions. Missing pointer/version, invalid schema, invalid paths, content
hash mismatch and oversized artifacts fail closed. Restore the task artifacts from backup if
they are lost. An interrupted initial capture keeps a validated pending snapshot in the required marker.
Retry completes that exact capture, even if repository sources have changed. Before any launch
can use it, the marker becomes established; subsequent artifact loss requires backup recovery. Historical prompts remain inspectable independently.

## Operator APIs and limits

These endpoints use existing paired operator-session authentication and mutation-origin checks.
Agent bearer credentials cannot read or mutate them, even for their own task.

| Endpoint | Purpose |
| --- | --- |
| `GET /api/projects/{id}/instructions` | Project repository sources, revision and dirty paths |
| `PUT /api/projects/{id}/instructions/{role}` | Save/create a project source |
| `GET /api/tasks/{key}/instructions?nodeKey=architect&preview=snapshot` | Effective task preview and version |
| `GET /api/tasks/{key}/instructions?nodeKey=implementer&preview=source` | Saved-source future preview |
| `GET /api/tasks/{key}/instructions?runId=123` | Historical briefing and metadata, task-scoped |
| `PUT /api/tasks/{key}/instructions/{role}` | Save/create a task-worktree source |
| `POST /api/tasks/{key}/instructions/apply` | Explicit version update for future launches |
| `POST /api/tasks/{key}/lessons/{uid}/approve` | Append a proposed lesson (`{ "text": "…" }`, optional) to the role's file in the original checkout |
| `POST /api/tasks/{key}/lessons/{uid}/dismiss` | Dismiss a proposed lesson |

API role keys are `architect`, `orchestrator`, `implementer`, `code_reviewer`, `security_reviewer`.
A source save requires `{ "content": "...", "expectedHash": "<sha256>" }`; use an explicit
`null` hash only when the file is absent. Omitted hashes are rejected. Existing content changed
externally yields `409`; reload before saving. Apply requires `expectedVersion`, all five
`expectedSourceHashes` (including nulls) and `freshConversations`. Stale versions or source hashes
yield `409`. Initial capture happens at launch; apply requires an existing captured version.

Supported sources are regular UTF-8 files without BOM or NUL, at most **65,536 bytes per role**
and **262,144 bytes total**. Invalid UTF-8, surrogate text, oversized content, directories in place
of files and unavailable repositories fail closed. Roles are an allowlist: no user-supplied path,
parent discovery, traversal, absolute/UNC/device source path or override hierarchy is supported.
Both supported path components are checked canonically; symlinks, Windows junctions and other
reparse points are rejected even if their target is inside the repository. UNC repositories are
unsupported. The selected repository root comes from existing validated project/task records.

Writes use temporary files in the destination directory and atomic replacement. Existing task
and project locks serialize in-process edits, capture and updates; hash comparisons detect
concurrent external edits before replacement. As with the rest of AgentFlow, these are application
controls rather than OS isolation from a malicious process with the same filesystem account.
Markdown is always displayed as escaped text. This feature supplies no skills/script execution,
new tool capability, approval privilege, global library or native-provider profile configuration.

## Local validation

Focused tests live in `backend/tests/test_instructions.py`, Rust `core/tests/instructions.rs`,
the worker regression suites, `contract/test_instructions.py` (same checks on both backends),
and `scripts/verify-instructions-ui.mjs`. The Python suite executes local stubs through real
Claude and Copilot adapter launch/resume argv. Existing prompt goldens and workflow/flowctl
contracts cover backward compatibility and engine-enforced controls.
