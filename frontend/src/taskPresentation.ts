import type { TaskStatus, TaskSummary } from '@/api/types'

// The workflow's stages in display order. Labels match the node labels the backend puts in
// `TaskSummary.phase` ("<node label>: <node state>") while a task is active.
const STAGE_LABELS = [
  'Architect',
  'Requirements Approval',
  'Orchestrator',
  'Implementer',
  'Code Review',
  'Security Review',
  'Consolidate Findings',
  'Final Architect Verification',
  'Ready for PR',
] as const

// New Parallel tasks give the planning stage to the Architect, under its own label.
const STAGE_ALIASES: Partial<Record<string, string>> = { 'Architect Planning': 'Orchestrator' }

export interface TaskProgress {
  stage: string
  percent: number
}

export type StatusTone = 'running' | 'attention' | 'done' | 'failed' | 'idle'

const FINISHED_PROGRESS: Partial<Record<TaskStatus, TaskProgress>> = {
  READY_FOR_PR: { stage: 'Ready for PR', percent: 100 },
  PR_CREATED: { stage: 'PR created', percent: 100 },
  MERGED: { stage: 'Merged', percent: 100 },
}

const STATUS_TONES: Record<TaskStatus, StatusTone> = {
  ACTIVE: 'running',
  NEEDS_HUMAN_REVIEW: 'attention',
  READY_FOR_PR: 'done',
  PR_CREATED: 'done',
  MERGED: 'done',
  FAILED: 'failed',
  CANCELLED: 'idle',
}

export const statusTone = (status: TaskStatus): StatusTone => STATUS_TONES[status]

export const STATUS_LABELS: Record<TaskStatus, string> = {
  ACTIVE: 'In progress',
  NEEDS_HUMAN_REVIEW: 'Needs Human Review',
  READY_FOR_PR: 'Ready for PR',
  PR_CREATED: 'PR Created',
  MERGED: 'Merged',
  FAILED: 'Failed',
  CANCELLED: 'Cancelled',
}

// How an active task's current node state reads after its label ("Implementer running").
const NODE_STATE_WORDS: Partial<Record<string, string>> = {
  READY: 'ready to start',
  RUNNING: 'running',
  WAITING_FOR_USER: 'has a question',
  WAITING_FOR_APPROVAL: 'waiting on you',
  FAILED: 'failed',
  INTERRUPTED: 'interrupted',
}

/** The task's status in words: the current stage while active, else the status label. */
export function phaseLabel(task: Pick<TaskSummary, 'status' | 'phase'>): string {
  if (task.status !== 'ACTIVE') return STATUS_LABELS[task.status]
  const [stage, state] = task.phase.split(':').map((part) => part.trim())
  const words = state ? NODE_STATE_WORDS[state] : undefined
  return words ? `${stage} ${words}` : STATUS_LABELS.ACTIVE
}

/**
 * What a Merged task still has on disk that cleanup would remove ("its worktree and 2 worker
 * worktrees"), or null when nothing is known to remain. Only positive evidence counts: an unknown
 * (null) check isn't reported, unlike the fail-open rule that decides whether cleanup is offered.
 */
export function leftoverWorktree(
  task: Pick<TaskSummary, 'status' | 'worktreeExists' | 'workerWorktreeCount'>,
): string | null {
  if (task.status !== 'MERGED') return null
  const workers = task.workerWorktreeCount ?? 0
  const parts: string[] = []
  if (task.worktreeExists === true) parts.push('its worktree')
  if (workers > 0) parts.push(`${workers} worker worktree${workers === 1 ? '' : 's'}`)
  return parts.length ? parts.join(' and ') : null
}

/** The last segment of a repository path, which is how rows and headers name it. */
export const shortPath = (path: string): string =>
  path.replaceAll('\\', '/').split('/').filter(Boolean).pop() ?? path

export function taskProgress(task: Pick<TaskSummary, 'status' | 'phase'>): TaskProgress {
  const finished = FINISHED_PROGRESS[task.status]
  if (finished) return finished

  const stage = task.phase.split(':')[0].trim()
  const index = STAGE_LABELS.findIndex((label) => label === (STAGE_ALIASES[stage] ?? stage))
  if (index < 0) return { stage: task.status === 'ACTIVE' ? 'Starting' : stage, percent: 0 }
  return { stage, percent: Math.round(((index + 0.5) / STAGE_LABELS.length) * 100) }
}
