// Mirrors the JSON produced by backend/agentflow/workflow/engine.py snapshot().

export type NodeState =
  | 'PENDING'
  | 'READY'
  | 'RUNNING'
  | 'WAITING_FOR_USER'
  | 'WAITING_FOR_APPROVAL'
  | 'COMPLETED'
  | 'FAILED'
  | 'SKIPPED'
  | 'CANCELLED'
  | 'INTERRUPTED'

export type TaskStatus = 'ACTIVE' | 'NEEDS_HUMAN_REVIEW' | 'READY_FOR_PR' | 'PR_CREATED' | 'MERGED' | 'FAILED' | 'CANCELLED'

export interface AgentOverride {
  harness?: string | null
  model?: string | null
  reasoning?: string | null
}

export interface EffectiveConfig {
    billingMode?: 'metered' | 'subscription' | 'unknown';
    routingReason?: string;
    requiresMeteredApproval?: boolean;
  harness: string
  model: string | null
  reasoning: string | null
  harnessDisplayName: string
  harnessAvailable: boolean
}

export interface UsageSummary {
    runsWithCost?: number;
    meteredRuns?: number;
    meteredRunsWithCost?: number;
    subscriptionRuns?: number;
    unknownBillingRuns?: number;
    apiCostUsd?: number | null;
    runsWithBilledCost?: number;
    billedCostUsd?: number | null;
    runsWithEstimatedCost?: number;
    estimatedCostUsd?: number | null;
  runs: number
  runsWithUsage: number
  /** Runs whose usage includes token counts; Copilot reports cost only. Absent in older payloads. */
  runsWithTokens?: number
  inputTokens: number
  outputTokens: number
  cachedInputTokens: number
  totalTokens: number
  costUsd: number | null
}

export interface HeartbeatSummary {
    observedAt: string;
    warningSince: string;
    receivedAt: string | null;
    phase: string | null;
    warningSeconds: number;
}

export interface NodeRunSummary {
    heartbeat?: HeartbeatSummary;
  id: number
  runNumber: number
  state: NodeState
  harness: string | null
  model: string | null
  reasoning: string | null
  runDir: string | null
  promptPath: string | null
  resultPath: string | null
  exitCode: number | null
  error: string | null
  startedAt: string | null
  completedAt: string | null
  result: Record<string, unknown> | null
}

export interface WorkflowNodeDto {
  worker?: {
    uid: string;
    key: string;
    ownedPaths: string[];
    acceptanceCriteria: string[];
    dependencies: string[];
    worktreePath: string;
    branchName: string;
  } | null;
  key: string
  kind: 'agent' | 'approval' | 'system' | 'terminal'
  label: string
  role: string | null
  state: NodeState
  displayState: NodeState
  executionMode: string
  iteration: number
  checkpointSha: string | null
  message: string | null
  error: string | null
  configOverride: AgentOverride
  effectiveConfig: EffectiveConfig | null
  openQuestions: number
  findingsCount: number
  blockingCount: number
  runCount: number
  lastRun: NodeRunSummary | null
  usage: UsageSummary | null
  /** What one run of this stage usually costs, from past runs (display only); absent from older backends. */
  costEstimate?: CostEstimate | null
  description: string
}

/** A stage's cost from its last (up to 20) finished runs with the same harness and model: USD, or
 *  tokens on a subscription harness. `typical` is the median, `high` the 90th percentile; both are
 *  null until `enough` (3+) runs exist. */
export interface CostEstimate {
  unit: 'usd' | 'tokens'
  runs: number
  enough: boolean
  typical: number | null
  high: number | null
  /** USD only: provider-billed, list-price estimates, or a mix of both. */
  costKind: 'billed' | 'estimated' | 'mixed' | null
}

export interface EstimateTotals {
  usd: { typical: number; high: number } | null
  tokens: { typical: number; high: number } | null
}

/** POST /api/tasks/cost-estimate: what a new task would cost on a route. */
export interface RouteEstimate {
  implementationMode: 'direct' | 'parallel'
  stages: { key: string; label: string; harness: string; model: string | null; billingMode: string; estimate: CostEstimate }[]
  onePass: EstimateTotals
  perWorker: EstimateTotals | null
  perFixRound: EstimateTotals
  /** Stages without enough history; left out of the totals. */
  missing: string[]
}

export interface Finding {
  id: string
  dbId: number
  type: string
  iteration: number
  reviewedSha: string | null
  severity: string
  blocking: boolean
  file: string | null
  line: number | null
  /** The last line of a finding that spans several lines (only set when after `line`). */
  endLine: number | null
  title: string
  description: string | null
  recommendation: string | null
  status: 'open' | 'resolved' | 'withdrawn' | 'accepted' | 'superseded'
  /** How many review rounds raised this finding (a re-raised finding keeps its id). */
  occurrence: number
  /** The reviewer's evidence for its verdict on this finding in the following round. */
  resolution: string | null
  response: 'fixed' | 'disputed' | 'deferred' | null
  responseNote: string | null
  createdAt: string
  resolvedAt: string | null
}

/** One line of a diff hunk, numbered on the side(s) it belongs to. */
export interface DiffRow {
  kind: 'context' | 'add' | 'del'
  old: number | null
  new: number | null
  text: string
}

export interface DiffHunk {
  header: string
  oldStart: number
  oldCount: number
  newStart: number
  newCount: number
  rows: DiffRow[]
}

/** `GET /api/tasks/{key}/findings/{id}/code`: the code a review finding points at. */
export interface FindingCode {
  file: string | null
  line: number | null
  endLine: number | null
  reviewedSha: string | null
  /** The file at the reviewed commit. */
  snapshot: { sha: string; lines: string[] } | null
  /** The task's change to the file: where the branch left its base → the reviewed commit. */
  diff: { from: string; to: string; hunks: DiffHunk[] } | null
  /** The file at the task's current commit, the finding's lines mapped there, and what changed since the review. */
  now: { sha: string; lines: string[]; line: number | null; endLine: number | null; changed: boolean; hunks: DiffHunk[] } | null
  /** Why a view is null, by view. */
  unavailable: Partial<Record<'snapshot' | 'diff' | 'now', string>>
}

export interface AgentSessionDto {
  nodeKey?: string | null;
  id: number
  role: string
  harness: string
  model: string | null
  reasoning: string | null
  tabName: string
  tabId: number | null
  paneId: string | null
  harnessSessionId: string | null
  status: string
  activity: string
  screenState: string | null
  screenStateAt: string | null
  startedAt: string
  endedAt: string | null
}

/** One choice of a decision tree, as the backend validated it (workflow/decisions.py). */
export interface DecisionChoiceDto {
  label: string
  recommended?: boolean
  detail?: string
  warning?: string
  /** The agent's evidence for this choice, with where it came from (a repo path or https URL). */
  note?: string
  source?: string
  /** Only on top-level choices: one more question whose choices are leaves. */
  followUp?: { message: string, choices: DecisionChoiceDto[] }
}

export type DecisionRisk = 'low' | 'medium' | 'high'

export interface DecisionDto {
  context: string | null
  risk: DecisionRisk | null
  choices: DecisionChoiceDto[]
}

export interface QuestionDto {
  nodeKey?: string | null;
  uid: string
  role: string
  message: string
  choices: string[]
  /** A decision tree; null for a flat question (`--message`/`--choice`, or asked before trees). */
  decision: DecisionDto | null
  answered: boolean
  answer: string | null
  createdAt: string
  answeredAt: string | null
  runId: number | null
  actionable: boolean
}

/** `GET /api/questions?open=true`: an open question with the task and agent it belongs to. */
export interface OpenQuestionDto extends QuestionDto {
  jiraKey: string
  jiraTitle: string | null
  nodeLabel: string
  harness: string | null
  model: string | null
}

/** How the human answered: free text, or 0-based choice indexes (branch, then follow-up option). */
export type AnswerBody = { answer: string } | { path: number[] }

export interface EventDto {
  id: number
  timestamp: string
  level: string
  nodeKey: string | null
  message: string
  data: Record<string, unknown> | null
}

export interface TaskSummary {
    taskSource?: 'local' | 'jira';
    attention?: { nodeKey: string; label: string; state: NodeState; since: string | null;
        questions: number; action: string }[];
    totalUsage?: UsageSummary | null;
  jiraKey: string
  jiraTitle: string | null
  status: TaskStatus
  statusMessage: string | null
  phase: string
  repositoryRoot: string
  projectRelativePath: string | null
  baseBranch: string
  branchName: string
  worktreePath: string
  zellijSession: string
  reviewIteration: number
  maxReviewIterations: number
  maxRequirementsRevisions: number
  currentSha: string | null
  prUrl: string | null
  prCreatedAt: string | null
  mergedAt: string | null
  /** The branch the recorded PR merges into, as GitHub last reported it (null: not looked up). */
  prBaseBranch: string | null
  prSyncPaused: boolean
  archivedAt: string | null
  /** The task's project (a Project id), if it has one. */
  projectId: number | null
  /** Filesystem checks, only made once the task is Merged (null before that). */
  worktreeExists: boolean | null
  workerWorktreeCount: number | null
  createdAt: string
  updatedAt: string
}

export type TaskListFilter = 'exclude' | 'include' | 'only'

export interface DeleteTaskPreview {
    worktree: { path: string; exists: boolean; dirtyPaths: string[]; error: string | null }
    branch: {
        name: string; exists: boolean | null; sha: string | null; unmergedCommits?: number | null
        upstream?: string | null; unpushedCommits?: number | null; error?: string
    }
    workerWorktrees: string[]
    workerBranches: string[]
    taskFolder: { path: string; exists: boolean }
    sessionWillBeClosed: boolean
    unknownCostRunsToday: number
}

export interface DeleteTaskOptions {
    removeWorktree: boolean
    removeBranch: boolean
    removeTaskFolder: boolean
    expectedSha: string | null
}

export interface WorktreeCleanupPreview {
    dirtyPaths: string[]
    openAgentPanes: string[]
    workerWorktrees: string[]
    sessionWillBeClosed: boolean
    otherOpenTabs: string[]
}

export interface BudgetGroup {
    knownUsd: number;
    reservedUsd: number;
    unknownRuns: number;
    limitUsd: number | null;
    warning: boolean;
}

export interface MeteredApproval {
    harness: string;
    model: string | null;
    reason: string;
}

export interface WorktreeSetupStatus {
  /** `task`, or `worker-<uid>` for a worker's worktree. */
  name: string
  worktree: string
  digest: string
  state: 'running' | 'succeeded' | 'failed'
  startedAt: string
  completedAt: string | null
  error: string | null
  logPath: string
}

export interface TaskSnapshot extends TaskSummary {
    budget?: {
        task: BudgetGroup;
        daily: BudgetGroup;
        launchReserveUsd: number;
        warningPercent: number;
        dayUtc: string;
    };
    requirementsDigest?: string | null;
    requirementsDraftDigest?: string;
    requirementsFeedback: { iteration: number; feedback: string; at: string }[];
    approvedRequirements?: Record<string, string> | null;
    totalUsage?: UsageSummary | null;
    executionEvidence?: {
        runId: number;
        nodeKey: string;
        requestedModel: string | null;
        confirmedModel: string | null;
        durationSeconds: number;
        iteration: number;
          usage: { totalTokens: number | null; costUsd: number | null; costKind?: 'billed' | 'estimated' | null; source?: string; provenance: string } | null;
          billingMode?: 'metered' | 'subscription' | 'unknown';
        verification: { sha: string; passed: boolean; commands?: {
            command: string[]; exitCode: number; output: string;
        }[]; error?: string } | null;
    }[];
  blockers: string[];
  additionalContext: string | null
  taskDir: string
  harnessCwd: string
  /** Rust only, and only when the repository has `workspace.worktreeSetup` configured. */
  worktreeSetup?: WorktreeSetupStatus[]
  agentOverrides: Record<string, AgentOverride>
  zellij: { session: string; attachCommand: string; webUrl: string | null }
  nodes: WorkflowNodeDto[]
  edges: { source: string; target: string }[]
  sessions: AgentSessionDto[]
  findings: Finding[]
  questions: QuestionDto[]
  events: EventDto[]
  requirements: Record<string, string>
  assessment: Record<string, any> | null
  assessmentMarkdown: string
}

export interface HarnessInfo {
    billingMode?: 'metered' | 'subscription' | 'unknown';
    automaticTrustWorkspaces?: string[];
  key: string
  displayName: string
  type: string
  enabled: boolean
  available: boolean
  command: string
  resolvedPath: string | null
  version: string | null
  message: string | null
  supportsModel: boolean
  supportsReasoning: boolean
  supportsResume: boolean
  defaultModel: string | null
  availableModels: string[]
}

export interface SystemConfig {
  configPath: string
  dataDirectory: string
  databasePath: string
  worktreeDirectory: string
  taskDirectory: string
  defaultRepositoryDirectory: string | null
  jiraEnabled: boolean
  jiraBaseUrl: string | null
  github: { prSync: boolean; pollIntervalSeconds: number; command: string; hosts: string[] }
  platform: string
  python: string
  system: {
    git: { available: boolean; version: string | null }
    /** Where agents run: terminals built into the app, or zellij (which must be installed). */
    terminal: { backend: 'embedded' | 'zellij'; available: boolean; version: string | null }
    zellij: { available: boolean; version: string | null; command: string }
  }
  zellij: Record<string, unknown>
  workflow: Record<string, unknown>
  agents: Record<string, { harness: string; model: string | null; reasoning: string | null }>
  harnesses: HarnessInfo[]
}

export interface DirectoryListing {
  path: string | null
  parent: string | null
  roots?: string[]
  home?: string
  root?: string
  relative?: string
  isGitRepo?: boolean
  entries: { name: string; path: string }[]
}

export interface JiraSearchResult {
  key: string
  summary: string
  iconUrl?: string | null
  section?: string | null
}

export interface RepositoryValidation {
  ok: boolean
  root?: string
  currentBranch?: string | null
  branches: string[]
  checks: { ok: boolean; message: string }[]
}

export interface CreateTaskPayload {
    taskSource?: 'local' | 'jira';
    prompt?: string;
    implementationMode?: 'direct' | 'parallel';
  jiraKey?: string
  jiraTitle: string | null
  repositoryPath: string
  projectRelativePath: string | null
  baseBranch: string
  additionalContext: string | null
  agentOverrides: Record<string, AgentOverride>
  projectId?: number | null
}

export const PROJECT_COLORS = ['blue', 'purple', 'orange', 'teal', 'green', 'red', 'pink', 'slate'] as const
export type ProjectColor = (typeof PROJECT_COLORS)[number]

/** A named group of tasks, with defaults the New Task form copies when the project is picked. */
export interface Project {
  id: number
  /** The URL token (`/?project=<slug>`); set at creation and kept through renames. */
  slug: string
  name: string
  color: ProjectColor
  description: string | null
  defaultRepositoryPath: string | null
  defaultProjectRelativePath: string | null
  defaultBaseBranch: string | null
  agentOverrides: Record<string, AgentOverride>
  archivedAt: string | null
  createdAt: string
  updatedAt: string
}

/** Creating a project, or editing one: an edit changes only the fields it sends, and null clears one. */
export type ProjectPayload = Partial<Pick<Project, 'name' | 'color' | 'description' | 'defaultRepositoryPath'
  | 'defaultProjectRelativePath' | 'defaultBaseBranch' | 'agentOverrides'>>
