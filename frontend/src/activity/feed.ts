// The words around the map: the Now headline, the vitals tiles, the Latest calls list, and the
// lane slots parallel calls share on the tape. Pure functions of a run and the time, so they can be
// run with node (see `check.mts`).
import type { ActivityEntry, ActivityStation, ActivitySummary } from '@/api/types'
import { callCount } from './runs.ts'
import { stationInfo } from './stations.ts'

/** "12s", "3:05", "1:02:03": how long a call has run, to the second (the clock ticks once a second). */
export function formatClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  if (total < 60) return `${total}s`
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = String(total % 60).padStart(2, '0')
  return h ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`
}

/** How long a finished call took: "300 ms", "2.4 s", "1:05". */
export function formatTook(ms: number): string {
  if (ms < 1000) return `${Math.max(100, Math.round(ms / 100) * 100)} ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`
  return formatClock(ms)
}

const startOf = (entry: Pick<ActivityEntry, 'startedAt'>) => Date.parse(entry.startedAt)
const endOf = (entry: Pick<ActivityEntry, 'endedAt'>) => (entry.endedAt ? Date.parse(entry.endedAt) : null)

/** How the Now headline is tinted. */
export type NowClass = 'is-idle' | 'is-run' | 'is-flow' | 'is-think' | 'is-wait' | 'is-done'

export interface NowParts {
  cls: NowClass
  icon: ActivityStation
  kind: string
  l1: string
  l2: string
  /** The second line reports a failure. */
  bad: boolean
  /** The timer, and what it times ("this call", "waiting", "total"). */
  tm: string
  cap: string
}

export interface NowInput {
  entries: ActivityEntry[]
  summary: ActivitySummary
  now: number
  /** The person is being waited on (the node's state, not the transcript). */
  waiting: boolean
  /** When the open question was asked, if known. */
  waitingSince: number | null
  /** The run's name, for "Round 2 is done". */
  tab: string
}

const FAILED_RECENTLY_MS = 6000

/** What the agent is doing right now (spec §3 step 2). Waiting for the person outranks everything. */
export function nowParts(run: NowInput): NowParts {
  const { entries, summary, now } = run
  const calls = callCount(summary)
  const tally = `Step ${summary.steps} · ${calls} call${calls === 1 ? '' : 's'} so far`
  if (summary.ended && !run.waiting) {
    const first = entries.length ? startOf(entries[0]) : null
    const last = summary.lastWriteAt ? Date.parse(summary.lastWriteAt) : null
    const context = summary.contextPercent === null ? '' : ` · context ${Math.round(summary.contextPercent)}%`
    return {
      cls: 'is-done', icon: 'agentflow', kind: 'Finished', l1: `${run.tab} is done`, bad: false,
      l2: `${summary.steps} step${summary.steps === 1 ? '' : 's'} · ${calls} call${calls === 1 ? '' : 's'}${context}`,
      tm: first !== null && last !== null && last >= first ? formatClock(last - first) : '–', cap: 'total',
    }
  }
  const open = entries.filter((entry) => entry.status === 'run')
  const failed = [...entries].reverse().find((entry) => {
    const ended = endOf(entry)
    return entry.status === 'err' && ended !== null && now - ended >= 0 && now - ended < FAILED_RECENTLY_MS
  })
  if (run.waiting) {
    const asked = open.filter((entry) => entry.station === 'you' && !entry.inbound).pop()
    const since = run.waitingSince ?? (asked ? startOf(asked) : null)
    return {
      cls: 'is-wait', icon: 'you', kind: 'Waiting for you', l1: asked?.label ?? 'A question for you', bad: false,
      l2: 'The agent cannot go on until you answer.', tm: since === null ? '–' : formatClock(now - since), cap: 'waiting',
    }
  }
  const top = open.filter((entry) => entry.station !== 'model' && !entry.bySubagent).pop()
    ?? open.filter((entry) => entry.station !== 'model').pop()
    ?? open[open.length - 1]
  if (!top) {
    return {
      cls: 'is-idle', icon: 'model', kind: 'Between calls', l1: 'Nothing is running', bad: !!failed, tm: '–', cap: '',
      l2: failed ? `Last call failed: ${failed.label}` : tally,
    }
  }
  const others = open.filter((entry) => entry !== top)
  const also = others.length
    ? `Also running: ${[...new Set(others.map((entry) => (entry.bySubagent ? 'subagent ' : '') + stationInfo(entry.station).name))].join(', ')}`
    : ''
  const cls: NowClass = top.station === 'model' ? 'is-think' : top.station === 'agentflow' ? 'is-flow'
    : top.station === 'you' ? 'is-wait' : 'is-run'
  return {
    cls, icon: top.station, bad: !!failed,
    kind: top.station === 'model' ? 'Thinking' : `${stationInfo(top.station).name}${top.bySubagent ? ' · subagent' : ''}`,
    l1: top.station === 'model' ? (top.label || 'Thinking') : top.label,
    l2: failed ? `Last call failed: ${failed.label}` : also || tally,
    tm: formatClock(now - startOf(top)), cap: 'this call',
  }
}

export type ContextLevel = 'ok' | 'warn' | 'crit'

/** The context tile: its level (warn from 70%, critical from 90%) and the words under the number. */
export function contextLevel(percent: number | null): { level: ContextLevel; word: string } {
  if (percent !== null && percent >= 90) return { level: 'crit', word: 'nearly full' }
  if (percent !== null && percent >= 70) return { level: 'warn', word: 'filling up' }
  return { level: 'ok', word: 'of the window' }
}

export interface FeedRow {
  entry: ActivityEntry
  /** How many consecutive finished ok calls at this station this row stands for (1 = just itself). */
  times: number
  cls: 'run' | 'err' | 'ok'
  mark: '●' | '✕' | '·'
  /** "running", or how long the call took. */
  took: string
}

/**
 * The newest `max` calls, newest first. Finished ok calls in a row at the same station (and from the
 * same side, own or subagent) fold into one row with a count.
 */
export function feedRows(entries: ActivityEntry[], now: number, max = 6): FeedRow[] {
  const plain = (entry: ActivityEntry) => entry.status === 'ok'
  const rows: { entry: ActivityEntry; times: number }[] = []
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i]
    const last = rows[rows.length - 1]
    if (last && plain(entry) && plain(last.entry) && last.entry.station === entry.station
      && last.entry.bySubagent === entry.bySubagent && last.entry.inbound === entry.inbound) {
      last.times++
      continue
    }
    if (rows.length === max) break
    rows.push({ entry, times: 1 })
  }
  return rows.map(({ entry, times }) => {
    const running = entry.status === 'run'
    const ended = endOf(entry)
    return {
      entry, times,
      cls: running ? 'run' : entry.status === 'err' ? 'err' : 'ok',
      mark: running ? '●' : entry.status === 'err' ? '✕' : '·',
      took: running ? 'running' : ended === null ? '' : formatTook(ended - startOf(entry)),
    }
  })
}

/**
 * The lane slot of each call, for a tape that lets overlapping calls at one station share its height
 * the way overlapping events share a calendar column. Keyed by entry id; inbound prompts take no slot.
 */
export function assignSlots(entries: ActivityEntry[], now: number): Map<string, { slot: number; slots: number }> {
  const out = new Map<string, { slot: number; slots: number }>()
  const stations = new Set(entries.map((entry) => entry.station))
  for (const station of stations) {
    const own = entries.filter((entry) => entry.station === station && !entry.inbound)
      .sort((a, b) => startOf(a) - startOf(b))
    let group: { id: string; slot: number; end: number }[] = []
    let groupEnd = -Infinity
    const close = () => {
      const slots = Math.max(...group.map((member) => member.slot)) + 1
      for (const member of group) out.set(member.id, { slot: member.slot, slots })
      group = []
    }
    for (const entry of own) {
      const start = startOf(entry)
      const end = endOf(entry) ?? Math.max(now, start)
      if (group.length && start >= groupEnd) close()
      const taken = new Set(group.filter((member) => member.end > start).map((member) => member.slot))
      let slot = 0
      while (taken.has(slot)) slot++
      group.push({ id: entry.id, slot, end })
      groupEnd = Math.max(groupEnd, end)
    }
    if (group.length) close()
  }
  return out
}
