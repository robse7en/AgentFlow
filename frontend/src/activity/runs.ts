// What the activity store does with the messages and what the cards show, as pure functions so they
// can be run with node (see `activity/check.mts`). Nothing here touches Vue or the network.
import type {
  ActivityEntry, ActivityMessage, ActivitySpan, ActivitySpanEnd, ActivityStation, ActivitySummary,
  QuestionDto, RunActivity, RunActivityPayload,
} from '@/api/types'
import { stationInfo } from './stations.ts'

/** The backend keeps the newest 80 calls, 2000 spans and 500 context steps; so does the client. */
export const MAX_ENTRIES = 80
export const MAX_SPANS = 2000
export const MAX_CONTEXT_STEPS = 500

const spanOf = (entry: ActivityEntry): ActivitySpan => ({
  station: entry.station, startedAt: entry.startedAt, endedAt: entry.endedAt, status: entry.status,
  bySubagent: entry.bySubagent, inbound: entry.inbound,
})

/**
 * Updates the span `before` made, found from the end (a call ends soon after it starts). Calls that
 * began together look alike, so the span taken is one that is in the state `before` was in: the
 * first of two to end must not leave the other's span running for good.
 */
function updateSpan(spans: ActivitySpan[], before: ActivityEntry, after: ActivityEntry) {
  for (let i = spans.length - 1; i >= 0; i--) {
    const span = spans[i]
    if (
      span.startedAt === before.startedAt && span.station === before.station &&
      span.bySubagent === before.bySubagent && span.endedAt === before.endedAt
    ) {
      spans[i] = spanOf(after)
      return
    }
  }
}

/** Ends the running span that a call made, after the call had left the newest entries. */
function endSpan(spans: ActivitySpan[], end: ActivitySpanEnd) {
  for (let i = spans.length - 1; i >= 0; i--) {
    const span = spans[i]
    if (
      span.endedAt === null && span.startedAt === end.startedAt && span.station === end.station &&
      span.bySubagent === end.bySubagent
    ) {
      spans[i] = { ...span, endedAt: end.endedAt, status: end.status }
      return
    }
  }
}

/** A run from the GET, with the lists it leaves out while they are empty filled in. */
export function runFromPayload(run: RunActivityPayload): RunActivity {
  return { ...run, spans: run.spans ?? [], contextSteps: run.contextSteps ?? [] }
}

/**
 * One run after an `activity` message: its entries merged in (new ones appended, changed ones
 * replaced), spans and context steps appended from what the message shows, and the new summary.
 * Never changes `previous`.
 */
export function applyMessage(previous: RunActivity | undefined, message: ActivityMessage): RunActivity {
  const entries = previous ? previous.entries.slice() : []
  const spans = previous ? previous.spans.slice() : []
  const contextSteps = previous ? previous.contextSteps.slice() : []
  for (const entry of message.entries) {
    const index = entries.findIndex((held) => held.id === entry.id)
    if (index >= 0) {
      updateSpan(spans, entries[index], entry)
      entries[index] = entry
    } else {
      entries.push(entry)
      spans.push(spanOf(entry))
    }
  }
  for (const end of message.spanEnds ?? []) endSpan(spans, end)
  if (entries.length > MAX_ENTRIES) entries.splice(0, entries.length - MAX_ENTRIES)
  if (spans.length > MAX_SPANS) spans.splice(0, spans.length - MAX_SPANS)
  const summary = message.summary
  if (previous && summary.steps > previous.summary.steps && summary.contextPercent !== null) {
    contextSteps.push({ at: summary.lastWriteAt ?? new Date().toISOString(), percent: summary.contextPercent })
    if (contextSteps.length > MAX_CONTEXT_STEPS) contextSteps.splice(0, contextSteps.length - MAX_CONTEXT_STEPS)
  }
  return {
    jiraKey: message.jiraKey, nodeKey: message.nodeKey, runId: message.runId, sessionId: message.sessionId,
    runNumber: message.runNumber, harness: message.harness, entries, summary, spans, contextSteps,
  }
}

/** Whether a run is one the node's card shows: the node's own, or (the Implementer's) a worker's. */
export function belongsToNode(run: RunActivity, nodeKey: string): boolean {
  return run.nodeKey === nodeKey || (nodeKey === 'implementer' && run.nodeKey.startsWith('worker_'))
}

/** The runs a node shows, oldest first. */
export function runsForNode(runs: Iterable<RunActivity>, nodeKey: string): RunActivity[] {
  return [...runs].filter((run) => belongsToNode(run, nodeKey)).sort((a, b) => a.runId - b.runId)
}

/**
 * The runs of a node's latest round: its newest run, and, for the Implementer, every worker run
 * since its own last run (the workers of one planning round run side by side).
 */
export function latestRound(runs: RunActivity[]): RunActivity[] {
  const last = runs[runs.length - 1]
  if (!last) return []
  if (!last.nodeKey.startsWith('worker_')) return [last]
  let since = runs.length - 1
  while (since > 0 && runs[since - 1].nodeKey.startsWith('worker_')) since--
  return runs.slice(since)
}

const isLiveRun = (run: RunActivity) => run.summary.available && !run.summary.ended

/** How many runs of the node's latest round are going at once: the card's "×N workers" chip shows at two or more. */
export function liveRunCount(runs: RunActivity[]): number {
  return latestRound(runs).filter(isLiveRun).length
}

/** What a card's current-call line shows. */
export type NowLine =
  | { kind: 'call'; station: ActivityStation; label: string; entry: ActivityEntry }
  | { kind: 'text'; text: string; dim: boolean }

/** The run's own calls still open, newest first (a subagent's are not the card's current call). */
export function openCalls(runs: RunActivity[]): ActivityEntry[] {
  return runs
    .filter(isLiveRun)
    .flatMap((run) => run.entries.filter((entry) => entry.status === 'run' && !entry.bySubagent))
    .sort((a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt))
}

/**
 * The newest call of a live node: a tool call over a model step, "Thinking" for a model step,
 * "Between calls" when none is open. Only runs that have not ended count (an earlier run of the
 * node, or one on a harness with no data, says nothing about now); null when there is none.
 */
export function currentCall(runs: RunActivity[]): NowLine | null {
  const live = runs.filter(isLiveRun)
  if (!live.length) return null
  const open = openCalls(live)
  const top = open.find((entry) => entry.station !== 'model') ?? open[0]
  if (top) {
    return top.station === 'model'
      ? { kind: 'call', station: 'model', label: 'Thinking', entry: top }
      : { kind: 'call', station: top.station, label: top.label, entry: top }
  }
  if (live.every((run) => run.entries.length === 0)) return { kind: 'text', text: 'No calls yet', dim: true }
  return { kind: 'text', text: 'Between calls', dim: true }
}

/** Calls made, as the card counts them: tool calls, not the model's steps or the person's words. */
export function callCount(summary: ActivitySummary): number {
  return Object.entries(summary.counts).reduce(
    (sum, [station, count]) => (station === 'model' || station === 'you' ? sum : sum + (count ?? 0)), 0)
}

/** "3 workers · 41 calls · 1 failed", for a node whose runs are over. */
export function roundSummary(round: RunActivity[]): NowLine | null {
  const counted = round.filter((run) => run.summary.available)
  if (!counted.length) return null
  const calls = counted.reduce((sum, run) => sum + callCount(run.summary), 0)
  const failed = counted.reduce((sum, run) => sum + run.summary.errors, 0)
  const workers = counted.length > 1 ? `${counted.length} workers · ` : ''
  return { kind: 'text', text: `${workers}${calls} call${calls === 1 ? '' : 's'}${failed ? ` · ${failed} failed` : ''}`, dim: true }
}

/** The card's current-call line, or null to show nothing (no activity data for the node). */
export function nowLine(runs: RunActivity[], live: boolean, finished: boolean): NowLine | null {
  if (live) return currentCall(runs)
  return finished ? roundSummary(latestRound(runs)) : null
}

/** What a live node has spent so far, summed over its live runs. Cost is null when any is unknown. */
export function liveSpend(runs: RunActivity[]): { costUsd: number | null; tokens: number } | null {
  const live = runs.filter(isLiveRun)
  if (!live.length) return null
  const tokens = live.reduce((sum, run) => sum + run.summary.tokens, 0)
  const known = live.every((run) => run.summary.costUsd !== null)
  return { costUsd: known ? live.reduce((sum, run) => sum + (run.summary.costUsd ?? 0), 0) : null, tokens }
}

/** "140k tok", "950 tok". */
export function formatTokens(tokens: number): string {
  if (tokens < 1000) return `${Math.round(tokens)} tok`
  return `${tokens < 10_000 ? (tokens / 1000).toFixed(1) : Math.round(tokens / 1000)}k tok`
}

/** A short age: "42s", "6m", "2h 5m". */
export function formatSpan(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000))
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m`
  const hours = Math.floor(minutes / 60)
  return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`
}

/** How long a live run may go without writing its transcript before the heartbeat says so. */
export const SILENCE_AFTER_MS = 60_000

/**
 * The heartbeat line from activity: "Shell · cargo test · 6m" while a call runs, "No transcript
 * writes for 6m" when the transcript has gone quiet with no call running (`silentMs` is how long;
 * it is 0 while a call runs, which is not silence). Null leaves the
 * heartbeat as it was (a model step in progress with recent writes, or no activity data).
 */
export function pulseLine(runs: RunActivity[], now: number): { text: string; silentMs: number } | null {
  const live = runs.filter(isLiveRun)
  if (!live.length) return null
  const call = openCalls(live).find((entry) => entry.station !== 'model')
  if (call) {
    const since = Date.parse(call.startedAt)
    const age = Number.isFinite(since) ? ` · ${formatSpan(now - since)}` : ''
    return { text: `${stationInfo(call.station).name} · ${call.label}${age}`, silentMs: 0 }
  }
  const written = Math.max(...live.map((run) => Date.parse(run.summary.lastWriteAt ?? '') || 0))
  if (written > 0 && now - written >= SILENCE_AFTER_MS) {
    return { text: `No transcript writes for ${formatSpan(now - written)}`, silentMs: now - written }
  }
  return null
}

/** When an agent was waiting on the person: each question of this run, answered or not. */
export function waitsFor(run: Pick<RunActivity, 'runId'>, questions: Pick<QuestionDto, 'runId' | 'createdAt' | 'answeredAt'>[]) {
  return questions
    .filter((question) => question.runId === run.runId)
    .map((question) => ({ from: question.createdAt, to: question.answeredAt }))
    .sort((a, b) => Date.parse(a.from) - Date.parse(b.from))
}
