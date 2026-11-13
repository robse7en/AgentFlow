// Batons: the small capsule that runs along an edge when work is handed to the next stage, carrying what is
// handed over (spec §7 and §10). Pure functions of two task snapshots, so node can check them (see
// `check-canvas.mts`); the canvas decides when to look and the edge component draws what this says.
import type { NodeState, TaskSnapshot } from '@/api/types'

/** The capsule's run along the edge, then how long its chip lingers at the target (seconds). */
export const HAND = 1.1
export const LINGER = 0.7
export const BATON_S = HAND + LINGER

/** The part of a task snapshot batons read. */
export type BatonTask = Pick<TaskSnapshot, 'jiraKey' | 'nodes' | 'edges' | 'assessment'>
export interface BatonEdge { source: string; target: string }

/** A baton in flight: which edge, when it was fired, and what it carries (fixed at the moment of the handoff). */
export interface Baton {
  /** `<edge id>:<fired at>`: a baton is the same element for as long as it lasts. */
  key: string
  edge: string
  at: number
  text: string
  loop: boolean
}

export const edgeId = (edge: BatonEdge) => `${edge.source}->${edge.target}`

/** The fix loop: Consolidate sends the work back to the Implementer. */
export const isLoopEdge = (edge: BatonEdge) => edge.source === 'consolidate' && edge.target === 'implementer'

/** States in which a node is the one the workflow is waiting on. */
export const LIVE_STATES: readonly NodeState[] = ['RUNNING', 'WAITING_FOR_USER', 'WAITING_FOR_APPROVAL']
/** Where an edge's dashes flow: the live states, and READY (a node that waits for a click to start). */
export const FEEDING_STATES: readonly NodeState[] = [...LIVE_STATES, 'READY']

const stateOf = (task: Pick<BatonTask, 'nodes'>, key: string): NodeState | undefined =>
  task.nodes.find((node) => node.key === key)?.displayState

/**
 * The forward edge into the Implementer (orchestrator->implementer) only drives its very first run. Once the
 * fix loop has fired, consolidate->implementer is the edge that re-triggers it, while the forward source stays
 * COMPLETED, so it must not count again.
 */
export function isStaleFirstPass(task: Pick<BatonTask, 'nodes'>, edge: BatonEdge): boolean {
  return edge.target === 'implementer' && edge.source !== 'consolidate' && stateOf(task, 'consolidate') === 'COMPLETED'
}

/** Whether work is flowing along an edge: its source is done and its target is the node asking for attention. */
export function edgeFlows(task: Pick<BatonTask, 'nodes'>, edge: BatonEdge): boolean {
  const target = stateOf(task, edge.target)
  return !isStaleFirstPass(task, edge) && stateOf(task, edge.source) === 'COMPLETED'
    && target !== undefined && FEEDING_STATES.includes(target)
}

/** Whether the target of an edge has gone live on the strength of its source finishing. */
function handedOver(task: Pick<BatonTask, 'nodes'>, edge: BatonEdge): boolean {
  const target = stateOf(task, edge.target)
  return !isStaleFirstPass(task, edge) && stateOf(task, edge.source) === 'COMPLETED'
    && target !== undefined && LIVE_STATES.includes(target)
}

/**
 * The edges a handoff has just happened along: the target went live (one of `LIVE_STATES`) while the source is
 * done, and it was not before. A node going live again after a failure is a retry, not a handoff. Nothing fires
 * without an earlier snapshot of the same task, so a reload mid-run starts quiet.
 */
export function handoffs(previous: BatonTask | null | undefined, next: BatonTask): BatonEdge[] {
  if (!previous || previous.jiraKey !== next.jiraKey) return []
  return next.edges.filter((edge) => {
    if (!handedOver(next, edge) || handedOver(previous, edge)) return false
    const was = stateOf(previous, edge.target)
    return was !== 'FAILED' && was !== 'INTERRUPTED'
  })
}

const nodeOf = (task: Pick<BatonTask, 'nodes'>, key: string) => task.nodes.find((node) => node.key === key)

/** What a review hands to Consolidate: "2 blocking", "1 minor" or "clear". */
function findingsText(task: Pick<BatonTask, 'nodes'>, key: string): string {
  const review = nodeOf(task, key)
  if (review?.blockingCount) return `${review.blockingCount} blocking`
  if (review?.findingsCount) return `${review.findingsCount} minor`
  return 'clear'
}

/** The final assessment's risk as a phrase ("low risk"); the field is `overallRisk` on the wire. */
function riskText(assessment: BatonTask['assessment']): string {
  const risk = assessment?.overallRisk ?? assessment?.risk ?? assessment?.risk_level
  return typeof risk === 'string' && risk.trim() ? `${risk.trim().toLowerCase()} risk` : 'done'
}

/**
 * What goes along an edge (spec §10), read from the snapshot the handoff arrived in. Where the field is
 * missing the text says what the handover is in a word instead.
 */
export function batonText(task: BatonTask, edge: BatonEdge): string {
  const { source, target } = edge
  if (source === 'architect') return 'requirements'
  if (source === 'requirements_gate') return 'approved'
  if (source === 'orchestrator') {
    const subtasks = task.nodes.filter((node) => node.worker).length
    return subtasks ? `${subtasks} subtask${subtasks === 1 ? '' : 's'}` : 'plan'
  }
  if (source === 'implementer') return nodeOf(task, 'implementer')?.checkpointSha?.slice(0, 7) || 'checkpoint'
  if (source === 'code_review' || source === 'security_review') return findingsText(task, source)
  if (source === 'consolidate' && target === 'implementer') {
    const blocking = ['code_review', 'security_review'].reduce((sum, key) => sum + (nodeOf(task, key)?.blockingCount ?? 0), 0)
    return blocking ? `fix ${blocking} blocking` : 'fix findings'
  }
  if (source === 'consolidate') return '0 blocking'
  if (source === 'final_architect') return riskText(task.assessment)
  return 'handoff'
}

/** The batons a new snapshot starts, given the one before it. */
export function newBatons(previous: BatonTask | null | undefined, next: BatonTask, at: number): Baton[] {
  return handoffs(previous, next).map((edge) => {
    const id = edgeId(edge)
    return { key: `${id}:${at}`, edge: id, at, text: batonText(next, edge), loop: isLoopEdge(edge) }
  })
}

/** How wide a payload chip is drawn (mono 10.5px text with padding). */
export const chipWidth = (text: string) => Math.round(text.length * 6.4 + 16)
