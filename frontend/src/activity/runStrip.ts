// Run so far: the whole session squeezed into one strip, one thin row per kind, with the context window's
// fill beneath (spec §3, item 5). A pure function of the run's spans and context steps, so node can check
// it (see `check-tape.mts`) and the component only draws it.
import type { ActivityContextStep, ActivityEntry, ActivitySpan, ActivitySummary } from '@/api/types'
import { gutterOf, mmss, PPS, runBounds, type Wait } from './tapeModel.ts'
import { ctxLevelOf } from './mapModel.ts'
import { STATIONS, stationInfo } from './stations.ts'

export const RUN_H = 94
const ROW_H = 3
const ROW_GAP = 1
const TOP = 6
const PAD_X = 18
const CTX_H = 28
/** Until the run is this long (seconds), the strip is stretched over this much, so a first call is not full width. */
const MIN_SPAN_S = 12

export interface RunStripInput {
  W: number
  entries: ActivityEntry[]
  spans: ActivitySpan[]
  contextSteps: ActivityContextStep[]
  summary: ActivitySummary | null
  now: number
  waits: Wait[]
  waiting: boolean
  /** Mark the stretch the tape is showing (only while the tape is on). */
  window: boolean
}

export interface RunStripBar {
  key: number
  x: number; y: number; w: number
  tone: 'live' | 'accent' | 'warn' | 'think' | 'red'
  sub: boolean
}

export interface RunStrip {
  W: number
  started: boolean
  ended: boolean
  /** The block of kind rows: its frame and height. */
  block: { x: number; y: number; w: number; h: number }
  rowH: number
  bars: RunStripBar[]
  /** Amber washes for waits (x, w), and red ticks where calls failed (x). */
  washes: { key: string; x: number; w: number }[]
  ticks: { key: number; x: number }[]
  /** The tape's window: the stretch of the run it shows. */
  window: { x: number; w: number } | null
  ctx: {
    /** The stepped line, and the same closed under itself for the fill. */
    line: string
    area: string
    level: 'ok' | 'warn' | 'crit'
    dot: { x: number; y: number }
    label: string
    labelX: number
    labelY: number
    anchor: 'start' | 'end'
  } | null
  guide: { y: number; x1: number; x2: number }
  baseline: { y: number; x1: number; x2: number }
  /** The now-line while the run goes. */
  head: { x: number } | null
  axis: { left: string; right: string }
}

const startOf = (item: Pick<ActivitySpan, 'startedAt'>) => Date.parse(item.startedAt)
const endOf = (item: Pick<ActivitySpan, 'endedAt'>) => (item.endedAt ? Date.parse(item.endedAt) : null)
const r1 = (value: number) => Math.round(value * 10) / 10

export function deriveRun(input: RunStripInput): RunStrip {
  const { W, now, summary } = input
  const all: Pick<ActivitySpan, 'station' | 'startedAt' | 'endedAt' | 'status' | 'bySubagent' | 'inbound'>[] =
    input.spans.length >= input.entries.length ? input.spans : input.entries
  const bounds = runBounds(input.entries, input.spans, summary, input.waiting)
  const started = !!summary?.available && bounds.start !== null
  const ended = started && bounds.end !== null
  const start = bounds.start ?? now
  const tt = ended ? Math.min(now, bounds.end!) : now
  const x0 = PAD_X
  const pw = W - 2 * PAD_X
  const d1 = ended ? bounds.end! : Math.max(tt, start + MIN_SPAN_S * 1000)
  const X = (at: number) => x0 + ((at - start) / (d1 - start)) * pw
  const n = STATIONS.length
  const blockH = n * (ROW_H + ROW_GAP) - ROW_GAP
  const cTop = TOP + blockH + 10
  const cBot = cTop + CTX_H
  const Y = (percent: number) => cBot - (Math.max(0, Math.min(100, percent)) / 100) * CTX_H
  const laneOf = new Map<string, number>(STATIONS.map((station, lane) => [station.id, lane]))

  const bars: RunStripBar[] = []
  const ticks: RunStrip['ticks'] = []
  if (started) {
    all.forEach((item, key) => {
      const t0 = startOf(item)
      if (t0 > tt) return
      const t1 = endOf(item)
      const open = item.status === 'run' && !ended
      const a = X(t0)
      const w = Math.max(X(open || t1 === null ? tt : Math.min(t1, tt)) - a, 1.2)
      const failed = item.status === 'err' && !open
      if (failed) ticks.push({ key, x: r1(a + w - 0.6) })
      bars.push({
        key, x: r1(a), w: r1(w), y: TOP + (laneOf.get(item.station) ?? 0) * (ROW_H + ROW_GAP),
        tone: failed ? 'red' : stationInfo(item.station).tone, sub: item.bySubagent,
      })
    })
  }

  const washes: RunStrip['washes'] = []
  if (started) {
    input.waits.forEach((wait, index) => {
      if (wait.to === null && !input.waiting) return
      const from = wait.from
      const to = Math.min(wait.to ?? tt, tt)
      if (from > tt) return
      washes.push({ key: `${index}-${from}`, x: r1(X(from)), w: r1(Math.max(X(to) - X(from), 1.2)) })
    })
  }

  // The stretch the tape above shows: what fits left of its gutter, ending now.
  let win: RunStrip['window'] = null
  if (started && input.window) {
    const shown = ((W - gutterOf(W)) / PPS) * 1000
    const a = X(Math.max(start, tt - shown))
    const b = X(tt)
    win = { x: r1(a - 1.5), w: r1(Math.max(b - a, 2) + 3) }
  }

  // Context: a stepped line, because it only grows when a result comes back.
  let ctx: RunStrip['ctx'] = null
  const steps = input.contextSteps
    .map((step) => ({ at: Date.parse(step.at), percent: step.percent }))
    .filter((step) => Number.isFinite(step.at))
    .sort((a, b) => a.at - b.at)
  const reported = summary?.contextPercent ?? null
  if (started && (steps.length || reported !== null)) {
    let value = steps.length ? steps[0].percent : reported!
    let line = `M${r1(X(start))} ${r1(Y(value))}`
    for (const step of steps) {
      if (step.at > tt) break
      line += `H${r1(X(step.at))}V${r1(Y(step.percent))}`
      value = step.percent
    }
    // The newest summary is at least as new as the newest step.
    if (reported !== null && !ended) value = reported
    const last = r1(X(tt))
    line += `H${last}`
    if (reported !== null && !ended && reported !== steps[steps.length - 1]?.percent) line += `V${r1(Y(value))}`
    const now100 = Math.round(value)
    const level = ctxLevelOf(now100)
    const lx = X(tt)
    const anchor = lx > x0 + 110 ? 'end' : 'start'
    ctx = {
      line, area: `${line}V${cBot}H${r1(X(start))}z`, level, dot: { x: last, y: r1(Y(value)) },
      label: `context ${now100}%`, labelX: r1(anchor === 'end' ? lx - 8 : lx + 8), labelY: r1(Math.max(cTop + 7, Y(value) - 6)), anchor,
    }
  }

  return {
    W, started, ended,
    block: { x: x0 - 4, y: TOP - 3, w: pw + 8, h: blockH + 6 }, rowH: ROW_H,
    bars, washes, ticks, window: win, ctx,
    guide: { y: r1(Y(70)), x1: x0, x2: x0 + pw },
    baseline: { y: cBot, x1: x0, x2: x0 + pw },
    head: started && !ended ? { x: r1(X(tt)) } : null,
    axis: { left: '0:00', right: `${mmss((d1 - start) / 1000)}${ended ? ' · finished' : ''}` },
  }
}
