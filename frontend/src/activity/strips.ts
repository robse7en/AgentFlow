// The mini strip on an agent's card (spec §7): one thin track per worker (one for a single run), each call a
// bar in its tone, the run's time squeezed to fit, and a dot where "now" is while it goes. A pure function of
// the node's latest round of runs and the clock, so node can check it (see `check-canvas.mts`).
import type { ActivitySpan, RunActivity } from '@/api/types'
import { runBounds } from './tapeModel.ts'
import { stationInfo } from './stations.ts'

/** The strip's height (CSS px) and the shortest stretch of time it spreads its bars over. */
export const STRIP_H = 16
export const STRIP_MIN_SPAN_MS = 10_000

export type StripTone = 'live' | 'accent' | 'warn' | 'think' | 'red'

export interface StripBar { key: string; x: number; y: number; w: number; h: number; tone: StripTone; sub: boolean }
export interface StripModel {
  W: number
  H: number
  /** The empty tracks, one per run. */
  tracks: { y: number; h: number }[]
  bars: StripBar[]
  /** "Now" on each run that has not ended. */
  dots: { x: number; y: number; r: number }[]
}

export interface StripInput {
  W: number
  runs: Pick<RunActivity, 'entries' | 'spans' | 'summary'>[]
  now: number
  /** The node waits for the person: its run has not ended even if the transcript says it stopped. */
  waiting: boolean
}

type Item = Pick<ActivitySpan, 'station' | 'startedAt' | 'endedAt' | 'status' | 'bySubagent' | 'inbound'>
const r1 = (value: number) => Math.round(value * 10) / 10
const itemsOf = (run: Pick<RunActivity, 'entries' | 'spans'>): Item[] =>
  run.spans.length >= run.entries.length ? run.spans : run.entries

export function deriveStrips(input: StripInput): StripModel {
  const { W, now } = input
  const runs = input.runs
  const rows = Math.max(1, runs.length)
  const rh = rows === 1 ? 5 : 3.6
  const gap = rows === 1 ? 0 : 2.4
  const y0 = (STRIP_H - (rows * rh + (rows - 1) * gap)) / 2
  const tracks = Array.from({ length: rows }, (_, row) => ({ y: r1(y0 + row * (rh + gap)), h: rh }))
  const model: StripModel = { W, H: STRIP_H, tracks, bars: [], dots: [] }

  const bounds = runs.map((run) => runBounds(run.entries, run.spans, run.summary, input.waiting))
  const starts = bounds.flatMap((bound) => (bound.start === null ? [] : [bound.start]))
  if (!starts.length) return model
  const d0 = Math.min(...starts)
  // A run that has not ended reaches to now; the strip never spreads over less than ten seconds.
  const d1 = Math.max(Math.max(...bounds.map((bound) => bound.end ?? now)), d0 + STRIP_MIN_SPAN_MS)
  const X = (at: number) => ((at - d0) / (d1 - d0)) * W

  runs.forEach((run, row) => {
    const { start, end } = bounds[row]
    if (start === null || start > now) return
    const ended = end !== null
    const tt = ended ? Math.min(now, end) : now
    const ry = tracks[row].y
    itemsOf(run).forEach((item, index) => {
      if (item.station === 'model' || item.inbound) return
      const t0 = Date.parse(item.startedAt)
      if (!(t0 <= tt)) return
      const t1 = item.endedAt ? Date.parse(item.endedAt) : tt
      const a = X(t0)
      const w = Math.max(X(Math.min(t1, tt)) - a, 1.4)
      const failed = item.status === 'err' && !!item.endedAt && t1 <= tt
      model.bars.push({
        key: `${row}:${index}`, x: r1(a), y: r1(ry), w: r1(w), h: rh,
        tone: failed ? 'red' : stationInfo(item.station).tone, sub: item.bySubagent,
      })
    })
    if (!ended) model.dots.push({ x: r1(Math.min(X(tt), W - 2.5)), y: r1(ry + rh / 2), r: r1(rh / 2 + 1.3) })
  })
  return model
}
