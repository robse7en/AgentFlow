// What the dashboard and the Projects page share about projects: colours, the URL filter, and
// per-project counts taken from the live task list.
import type { Project, ProjectColor, TaskSummary } from './api/types'

export const projectColor = (color: ProjectColor): string => `var(--project-${color})`

/** The `?project=` value of the "No project" filter (the backend never gives a project this slug). */
export const NO_PROJECT = 'none'

/**
 * Whether `task` belongs in the dashboard's `?project=` filter. An unknown slug (a project still
 * loading, or deleted since the link was made) filters nothing rather than everything.
 */
export function inProjectFilter(task: TaskSummary, slug: string | null, bySlug: Map<string, Project>): boolean {
  if (!slug) return true
  if (slug === NO_PROJECT) return task.projectId == null
  const project = bySlug.get(slug)
  return !project || task.projectId === project.id
}

export interface ProjectCounts {
  total: number
  active: number
  attention: number
  ready: number
  merged: number
  costUsd: number | null
  /** The newest updatedAt among its tasks. */
  lastActivity: string | null
}

/** Counts per project id (`null` for tasks without one), over the tasks given. */
export function countByProject(tasks: TaskSummary[]): Map<number | null, ProjectCounts> {
  const counts = new Map<number | null, ProjectCounts>()
  for (const task of tasks) {
    const key = task.projectId ?? null
    let c = counts.get(key)
    if (!c) {
      c = { total: 0, active: 0, attention: 0, ready: 0, merged: 0, costUsd: null, lastActivity: null }
      counts.set(key, c)
    }
    c.total++
    if (task.status === 'ACTIVE') c.active++
    if (task.attention?.length) c.attention++
    if (task.status === 'READY_FOR_PR') c.ready++
    if (task.status === 'MERGED') c.merged++
    const cost = task.totalUsage?.costUsd
    if (cost != null) c.costUsd = (c.costUsd ?? 0) + cost
    if (!c.lastActivity || task.updatedAt > c.lastActivity) c.lastActivity = task.updatedAt
  }
  return counts
}

/** Paths compared the way Windows treats them: either slash, any case, no trailing slash. */
export const samePath = (a: string | null | undefined, b: string | null | undefined): boolean =>
  !!a && !!b && a.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase() === b.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase()

/** "just now", "3m ago", "5h ago", "2d ago". */
export function timeAgo(ms: number, now = Date.now()): string {
  const minutes = Math.max(0, Math.floor((now - ms) / 60000))
  if (minutes < 1) return 'just now'
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  return hours < 24 ? `${hours}h ago` : `${Math.floor(hours / 24)}d ago`
}
