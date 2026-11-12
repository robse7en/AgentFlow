// What the tape shows at a moment: nine lanes of bars, the pens on the now-line, the gutter beside them,
// the amber wait bands, failure lines and the hover tooltip (spec §6). A pure function of a run and the
// clock, so node can check it (see `check-tape.mts`) and the component only draws it.
//
// The paper is laid out in "paper" coordinates (x grows with time, 17px a second from an epoch) and the
// component slides it left with one long CSS animation, so nothing redraws at 60 fps. Pens, gutter and
// tooltip are in screen coordinates. The model works out both from one `tt`, the tape's own time: the
// clock while the run goes, the run's end once it is over.
import type { ActivityEntry, ActivitySpan, ActivityStation, ActivitySummary } from '@/api/types'
import { assignSlots, formatClock, formatTook } from './feed.ts'
import { STATIONS, stationInfo } from './stations.ts'

export const LANE = 29
export const TTOP = 24
export const AXIS = 24
/** Pixels of paper a second of time takes. */
export const PPS = 17
export const TAPE_H = TTOP + STATIONS.length * LANE + AXIS
/** The paper's epoch is moved every three hours (so its coordinates stay small); its one animation lasts four. */
export const EPOCH_MS = 3 * 3600 * 1000
export const SCROLL_S = 4 * 3600
/** A running bar is drawn this far past the now-line (and clipped there), so it still reaches the pen until the next redraw. */
export const OVERSHOOT_S = 5

export const STRIKE = 0.32
export const PEN_RING = 0.5
export const PEN_SHAKE = 0.9
export const PEN_DONE = 0.6
export const PEN_STAGGER = 0.07
export const BAR_FADE = 0.6
export const COOL = 1.2
export const FAIL_RIPPLE = 0.9
export const GRID_S = 5
/** Where, from the left edge, a wait's flag stops following its band. */
export const EDGE_FLAG_X = 4

export const gutterOf = (W: number) => (W < 400 ? 140 : 156)
export const epochOf = (now: number) => Math.floor(now / EPOCH_MS) * EPOCH_MS

const startOf = (entry: Pick<ActivityEntry, 'startedAt'>) => Date.parse(entry.startedAt)
const endOf = (entry: Pick<ActivityEntry, 'endedAt'>) => (entry.endedAt ? Date.parse(entry.endedAt) : null)
const clamp01 = (value: number) => Math.max(0, Math.min(1, value))
const r1 = (value: number) => Math.round(value * 10) / 10
const fit = (text: string, chars: number) => (text.length > chars ? `${text.slice(0, Math.max(1, chars - 1))}…` : text)
/** "0:05", "1:02": a time on the grid. */
export const mmss = (seconds: number) => `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, '0')}`

/** A time the agent waited on the person: ms since the epoch; `to` is null while the question is open. */
export interface Wait { from: number; to: number | null }

type Tone = 'live' | 'accent' | 'warn' | 'think'

export interface RunBounds {
  /** The first call's start, or null before any. */
  start: number | null
  /** When the run ended (its last write, or its last call's end), or null while it goes. */
  end: number | null
}

/**
 * Where a run began and ended. There is no end time on the wire: the run's last write, or its last call's
 * end, is the best there is.
 */
export function runBounds(
  entries: Pick<ActivityEntry, 'startedAt' | 'endedAt'>[], spans: Pick<ActivitySpan, 'startedAt' | 'endedAt'>[],
  summary: ActivitySummary | null, waiting: boolean,
): RunBounds {
  const all = spans.length >= entries.length ? spans : entries
  let start: number | null = null
  for (const item of all) {
    const at = startOf(item)
    if (Number.isFinite(at) && (start === null || at < start)) start = at
  }
  if (!summary?.ended || waiting) return { start, end: null }
  let end = summary.lastWriteAt ? Date.parse(summary.lastWriteAt) : 0
  for (const item of all) end = Math.max(end, endOf(item) ?? 0)
  return { start, end: end || null }
}

export interface TapeInput {
  W: number
  entries: ActivityEntry[]
  spans: ActivitySpan[]
  summary: ActivitySummary | null
  now: number
  /** Past and open waits for the person (an open one has `to: null`). */
  waits: Wait[]
  /** The node waits for the person now (the node's state, not the transcript). */
  waiting: boolean
  /** Reduced motion: every state stays, no effect is made. */
  still: boolean
  /** The pointer, in the tape's own coordinates; null when it is not over it. */
  hover: { x: number; y: number } | null
}

export interface TapeBar {
  id: string
  lane: number
  x: number; y: number; w: number; h: number; rx: number
  tone: Tone | 'red'
  sub: boolean
  running: boolean
  failed: boolean
  hit: boolean
  /** Its opacity: full while running, fading to idle over `BAR_FADE` after it ends, or a failure's own. */
  ink: 'run' | 'fading' | 'idle' | 'fail'
  endAt: number | null
  label: string | null
  labelX: number; labelY: number
  dots: { x1: number; x2: number; y: number } | null
  /** The red ✕ after a failed call, and the ripple that goes with it while it is new. */
  cross: { x: number; y: number } | null
  rippleAt: number | null
}

export interface TapeDiamond { id: string; x: number; y: number; tone: Tone; hit: boolean }
/** A subagent's stint: a pale window across every lane. `end` is false while it still runs. */
export interface TapeWindow { id: string; x: number; w: number; end: boolean }
export interface TapeWaitBand { id: string; x: number; w: number; open: boolean; flag: string | null }
/** The flag of a wait that began off the left edge: it stays at the edge, in screen coordinates, so a long wait stays labelled. */
export interface TapeEdgeFlag { id: string; x: number; text: string }
/** `flag` draws the red marker over the lane; `word` adds the word "failed" when there is room. */
export interface TapeFailLine { id: string; x: number; flag: boolean; word: boolean }
export interface TapeGridLine { k: number; x: number; label: string | null }

export interface TapePen {
  id: ActivityStation
  name: string
  lane: number
  cy: number
  tone: Tone
  active: boolean
  /** The pen's ring breathes instead of turning: the agent is held, waiting for the person. */
  held: boolean
  count: number
  errors: number
  /** What is running here, for the gutter; null when nothing is. */
  running: { head: string; label: string; timer: string } | null
  /** Share of the run spent at this kind, and the busy time to print beside it. */
  share: { frac: number; busy: string } | null
  strikeAt: number | null
  ringAt: number | null
  shakeAt: number | null
  /** When the last call ended, while the pen's outline is still cooling off. */
  coolAt: number | null
  /** The cooling outline is red (a failure). */
  coolRed: boolean
  doneAt: number | null
  /** The glyph's ink: the hot tone while a call runs, ash once used, dim before. */
  ink: 'hot' | 'ash' | 'dim'
  nameY: number
  textY: number
}

export interface TapeTip { x: number; y: number; w: number; l1: string; l2: string }

export interface TapeModel {
  W: number
  xNow: number
  right: number
  gx: number
  top: number
  bot: number
  /** The clock the model was made at, which the one-shot effects are timed against. */
  now: number
  /** The time the tape shows, ms: the clock while the run goes, its end once it is over. */
  tt: number
  epoch: number
  /** Where the paper sits at `tt`, in px: `xNow - (tt - epoch) / 1000 * PPS`. */
  shift: number
  started: boolean
  ended: boolean
  /** Paper to the left of the run's start is veiled: its x, in paper. */
  veil: number | null
  startFlag: number | null
  head: { word: 'NOW' | 'DONE' | 'IDLE'; clock: string; tone: 'accent' | 'ok' | 'strong' }
  windows: TapeWindow[]
  waitBands: TapeWaitBand[]
  edgeFlags: TapeEdgeFlag[]
  failLines: TapeFailLine[]
  bars: TapeBar[]
  diamonds: TapeDiamond[]
  grid: TapeGridLine[]
  pens: TapePen[]
  tip: TapeTip | null
}

export function deriveTape(input: TapeInput): TapeModel {
  const { W, entries, summary, now, still, hover } = input
  const G = gutterOf(W)
  const xNow = W - G
  const right = W - 16
  const gx = xNow + 19
  const N = STATIONS.length
  const top = TTOP
  const bot = TTOP + N * LANE
  const counted: Pick<ActivitySpan, 'station' | 'startedAt' | 'endedAt' | 'status' | 'bySubagent' | 'inbound'>[] =
    input.spans.length >= entries.length ? input.spans : entries
  const bounds = runBounds(entries, input.spans, summary, input.waiting)
  const started = !!summary?.available && bounds.start !== null
  const ended = started && bounds.end !== null
  const runStart = bounds.start ?? now
  const tt = ended ? Math.min(now, bounds.end!) : now
  const epoch = epochOf(tt)
  const shift = xNow - ((tt - epoch) / 1000) * PPS
  // A time, on screen and on the paper (the paper slides by `shift`).
  const S = (at: number) => xNow - ((tt - at) / 1000) * PPS
  const P = (at: number) => S(at) - shift
  const live = (entry: ActivityEntry) => entry.status === 'run' && !ended
  const past = (entry: ActivityEntry) => !live(entry) && endOf(entry) !== null
  const far = tt + OVERSHOOT_S * 1000
  const cy = (lane: number) => top + lane * LANE + LANE / 2
  const laneOf = new Map<string, number>(STATIONS.map((station, lane) => [station.id, lane]))
  const slots = assignSlots(entries, tt)
  const visible = started ? entries.filter((entry) => startOf(entry) <= tt) : []

  const bars: TapeBar[] = []
  const diamonds: TapeDiamond[] = []
  const windows: TapeWindow[] = []
  const failLines: TapeFailLine[] = []
  let tip: TapeTip | null = null
  const hoverable = hover !== null && hover.x <= xNow

  for (const entry of visible) {
    const t0 = startOf(entry)
    const t1 = endOf(entry)
    const running = live(entry)
    const end = running ? far : Math.min(t1 ?? tt, tt)
    const sx1 = S(Math.min(end, tt))
    if (sx1 < -8) continue
    const failed = entry.status === 'err' && !running
    const lane = laneOf.get(entry.station) ?? 0
    const tone = stationInfo(entry.station).tone
    const line = cy(lane)
    // A subagent's stint: a pale window across every lane, so its hollow calls read as one chapter.
    if (entry.station === 'agents' && !entry.bySubagent) {
      windows.push({ id: entry.id, x: P(t0), w: Math.max(P(end) - P(t0), 1), end: !running })
    }
    if (failed) failLines.push({ id: entry.id, x: P(end), flag: sx1 > 6, word: sx1 > 6 && sx1 < xNow - 60 })
    if (entry.inbound) {
      // Something arriving from the person: a hollow diamond where it landed.
      const hit = hoverable && Math.abs(hover!.x - S(t0)) < 8 && Math.abs(hover!.y - line) < 8
      diamonds.push({ id: entry.id, x: P(t0), y: line, tone, hit })
      if (hit) {
        tip = tipOf(entry, S(t0), line - 6, running, failed, tt, runStart, W)
      }
      continue
    }
    const slot = slots.get(entry.id) ?? { slot: 0, slots: 1 }
    const ih = LANE - 10
    const bh = (ih - (slot.slots - 1) * 2) / slot.slots
    const by = top + lane * LANE + 5 + slot.slot * (bh + 2)
    const w = Math.max(P(end) - P(t0), 2.4)
    const visibleW = Math.max(((Math.min(end, tt) - t0) / 1000) * PPS, 2.4)
    const rx = Math.min(3, bh / 2, visibleW / 2)
    const hit = hoverable && hover!.x >= S(t0) - 2 && hover!.x <= S(t0) + visibleW + 2 && hover!.y >= by - 2 && hover!.y <= by + bh + 2
    if (hit) {
      tip = tipOf(entry, S(t0) + visibleW / 2, by, running, failed, tt, runStart, W)
    }
    const age = t1 === null ? Infinity : (tt - t1) / 1000
    const ink = running ? 'run' : !entry.bySubagent && !still && age < BAR_FADE && !failed ? 'fading' : failed ? 'fail' : 'idle'
    // The label takes what room the bar has at this moment; it is fitted again on every redraw.
    const chars = Math.floor((visibleW - 10) / 5.45)
    let label: string | null = null
    let dotsFrom = P(t0) + 5
    if (!entry.bySubagent && entry.station !== 'model' && slot.slots === 1 && chars >= 5) {
      label = fit(entry.label, chars)
      dotsFrom = P(t0) + 6 + label.length * 5.45 + 7
    }
    bars.push({
      id: entry.id, lane, x: r1(P(t0)), y: r1(by), w: r1(w), h: r1(bh), rx: r1(rx),
      tone: failed ? 'red' : tone, sub: entry.bySubagent, running, failed, hit, ink,
      endAt: ink === 'fading' ? t1 : null,
      label, labelX: r1(P(t0) + 6), labelY: r1(by + bh / 2 + 3.2),
      dots: running && !entry.bySubagent && visibleW - (dotsFrom - P(t0)) > 12
        ? { x1: r1(dotsFrom), x2: r1(P(far) - 9), y: r1(by + bh / 2) } : null,
      cross: failed ? { x: r1(P(end) + 7), y: r1(by + bh / 2) } : null,
      rippleAt: failed && !still && age < FAIL_RIPPLE && t1 !== null ? t1 : null,
    })
  }

  // Waiting for the person: an amber band over everything, its stripes moving only while the wait lasts. Its
  // flag rides the band; once the band's start has gone off the left edge the flag stays at the edge.
  const waitBands: TapeWaitBand[] = []
  const edgeFlags: TapeEdgeFlag[] = []
  if (started) {
    input.waits.forEach((wait, index) => {
      const open = wait.to === null
      if (open && !input.waiting) return
      const from = wait.from
      const to = open ? far : Math.min(wait.to!, tt)
      const until = Math.min(to, tt)
      if (from > tt || S(until) < -8) return
      const x = P(from)
      const w = Math.max(P(to) - x, 1)
      const id = `wait-${index}-${from}`
      const edge = S(from) < EDGE_FLAG_X
      const room = S(until) - Math.max(S(from), EDGE_FLAG_X)
      const text = room > 120 ? `waiting for you · ${formatClock(until - from)}` : room > 30 ? 'waiting' : null
      waitBands.push({ id, x: r1(x), w: r1(w), open, flag: edge ? null : text })
      if (edge && text) edgeFlags.push({ id, x: EDGE_FLAG_X + 3, text })
    })
  }

  // The grid: a line every five seconds of the session, moving with the paper.
  const grid: TapeGridLine[] = []
  if (started) {
    const span = xNow / PPS
    let k = Math.max(0, Math.ceil((tt - span * 1000 - runStart) / (GRID_S * 1000)) * GRID_S)
    for (; runStart + k * 1000 <= far; k += GRID_S) {
      const x = S(runStart + k * 1000)
      grid.push({ k, x: r1(P(runStart + k * 1000)), label: x > 16 && x < xNow - 30 ? mmss(k) : null })
    }
  }

  // The nine pens on the now-line, and what stands beside each.
  const elapsedMs = Math.max(tt - runStart, 1)
  const waitStart = input.waits.filter((wait) => wait.to === null).map((wait) => wait.from).sort((a, b) => a - b)[0] ?? null
  const pens: TapePen[] = STATIONS.map((station, lane) => {
    const id = station.id
    const own = visible.filter((entry) => entry.station === id)
    const open = own.filter((entry) => live(entry) && !entry.inbound)
    const held = id === 'you' && input.waiting && started
    const active = open.length > 0 || held
    const latest = own[own.length - 1]
    const since = latest ? (now - startOf(latest)) / 1000 : Infinity
    let lastEnd = -Infinity
    let lastErr = -Infinity
    for (const entry of own) {
      const t1 = endOf(entry)
      if (t1 === null || t1 > tt || live(entry)) continue
      lastEnd = Math.max(lastEnd, t1)
      if (entry.status === 'err') lastErr = Math.max(lastErr, t1)
    }
    const errAge = (now - lastErr) / 1000
    const total = counted.filter((item) => item.station === id)
    const errors = total.filter((item) => item.status === 'err').length
    const count = total.length
    const cooling = !active && Number.isFinite(lastEnd) && (now - lastEnd) / 1000 < COOL
    const busy = summary?.busyMs[id] ?? total.reduce((sum, item) => {
      const t1 = endOf(item)
      return t1 === null || item.inbound ? sum : sum + Math.max(0, t1 - startOf(item))
    }, 0)
    let running: TapePen['running'] = null
    if (active) {
      const heldOnly = open.length === 0
      const since0 = heldOnly ? waitStart : Math.min(...open.map(startOf))
      const head = open.length > 1 ? `×${open.length} ` : ''
      running = {
        head, label: heldOnly ? 'waiting for you' : open[open.length - 1].label,
        timer: since0 === null ? '–' : formatClock(tt - since0),
      }
    }
    const two = active || count > 0
    const doneAge = ended ? (now - bounds.end!) / 1000 - lane * PEN_STAGGER : Infinity
    return {
      id, name: station.name, lane, cy: cy(lane), tone: station.tone, active, held, count, errors, running,
      share: !active && count > 0 ? { frac: clamp01(busy / elapsedMs), busy: formatClock(busy) } : null,
      strikeAt: !still && !ended && since < STRIKE ? startOf(latest) : null,
      ringAt: !still && !ended && since < PEN_RING ? startOf(latest) : null,
      shakeAt: !still && errAge < PEN_SHAKE ? lastErr : null,
      coolAt: cooling ? lastEnd : null,
      coolRed: cooling && (now - lastErr) / 1000 < 3,
      doneAt: !still && ended && doneAge < PEN_DONE ? bounds.end! + lane * PEN_STAGGER * 1000 : null,
      ink: active ? 'hot' : count > 0 ? 'ash' : 'dim',
      nameY: r1(two ? cy(lane) - 2.5 : cy(lane) + 4),
      textY: r1(cy(lane) + 9.5),
    }
  })

  const head: TapeModel['head'] = {
    word: ended ? 'DONE' : started ? 'NOW' : 'IDLE',
    tone: ended ? 'ok' : started ? 'accent' : 'strong',
    clock: mmss(Math.max(0, tt - runStart) / 1000),
  }
  const xStart = S(runStart)
  return {
    W, xNow, right, gx, top, bot, now, tt, epoch, shift, started, ended,
    veil: started ? r1(P(runStart)) : null,
    startFlag: started && xStart > 30 && xStart < xNow - 46 ? r1(P(runStart) + 4) : null,
    head, windows, waitBands, edgeFlags, failLines, bars, diamonds, grid, pens, tip,
  }
}

function tipOf(
  entry: ActivityEntry, x: number, y: number, running: boolean, failed: boolean, tt: number, runStart: number, W: number,
): TapeTip {
  const t0 = startOf(entry)
  const t1 = endOf(entry)
  const l1 = `${stationInfo(entry.station).name} · ${entry.label}`
  const took = formatTook(Math.max(0, (running || t1 === null ? tt : Math.min(t1, tt)) - t0))
  const l2 = `${running ? 'running' : failed ? 'failed' : 'done'} · ${took} · at ${mmss(Math.max(0, t0 - runStart) / 1000)}${
    entry.bySubagent ? ' · subagent' : entry.inbound ? ' · from you' : ''}`
  const w = Math.min(W - 8, Math.max(l1.length * 5.9, l2.length * 5.5) + 18)
  const tx = Math.max(4, Math.min(x - w / 2, W - w - 4))
  const ty = y - 42 < 2 ? y + 22 : y - 42
  return { x: r1(tx), y: r1(ty), w: r1(w), l1: fit(l1, Math.floor((w - 18) / 5.9)), l2: fit(l2, Math.floor((w - 18) / 5.5)) }
}
