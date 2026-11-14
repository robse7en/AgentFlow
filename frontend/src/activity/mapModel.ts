// What the hull-and-docks map shows at a moment: each dock's state, the one-shot effects in flight
// (packets and ripples), the subagent pods, the trail and the hull. A pure function of a run and the
// clock, so the component only draws it and node can check it (see `check.mts`). Spec §4 and §5.
import type { ActivityEntry, ActivitySpan, ActivityStation, ActivitySummary } from '@/api/types'
import { formatClock } from './feed.ts'
import { hopLeg, mapGeom, podHop, podSlot, visitsOf, type Pt } from './geometry.ts'
import { STATIONS } from './stations.ts'

/** Timings, in seconds (spec §5). `LAP` is a minute here; the mockup's 10s suits its short demo calls. */
export const OUT = 0.5
export const BACK = 0.45
export const RIP = 0.5
export const GLOW = 2.6
export const ALARM = 6
export const LAP = 60
export const SHAKE = 0.7
export const BUMP = 0.3
export const POD_OUT = 0.45
export const POD_BACK = 0.4
export const HOP_FADE = 0.9
export const DONE_RING = 1.1
export const DRAW = 0.5
/** Calls that start within this many milliseconds of each other fly side by side. */
export const TOGETHER_MS = 250
export const SIDE_BY_SIDE = 5.5

export interface MapInput {
  W: number
  entries: ActivityEntry[]
  spans: ActivitySpan[]
  summary: ActivitySummary | null
  now: number
  /** The person is being waited on: the node's state is WAITING_FOR_USER for this run. */
  waiting: boolean
  /** Reduced motion: every state stays, no effect is made. */
  still: boolean
}

export type Glow = 'none' | 'afterglow' | 'alarm' | 'active'

export interface DockModel {
  id: ActivityStation
  /** Calls at this dock, from spans (the whole run) when they cover the entries. */
  count: number
  /** Finished failed calls. */
  errors: number
  /** Calls running now, subagents' included, as the mockup counts them. */
  running: number
  active: boolean
  /** The You dock while the node waits for the person. */
  waiting: boolean
  elapsedMs: number
  glow: Glow
  /** When the glow's clock started (the last end, or the last failure), for keying its fade. */
  glowAt: number
  /** Seconds since `glowAt`. */
  glowAge: number
  /** Fade left at `now`, 0 to 1. */
  fade: number
  /** When the newest call lands here, while it is still bumping. */
  bumpAt: number | null
  /** When the newest failure happened, while the dock is still shaking. */
  shakeAt: number | null
  /** The text under the name: the call's timer, "N calls", or nothing. */
  second: string
  dimmed: boolean
  /** Cable wear, 0 to 1, from the dock's own calls. */
  wear: number
  worn: boolean
  cableRunning: boolean
  cableParked: boolean
}

export type EffectKind = 'out' | 'land' | 'back' | 'result' | 'in' | 'inland'

/** A one-shot effect: it starts at `at` (it may be a moment away) and lasts `dur` seconds. */
export interface Effect {
  key: string
  kind: EffectKind
  station: ActivityStation
  at: number
  dur: number
  /** Sideways offset so calls that start together fly side by side. */
  shift: number
  bad: boolean
}

export interface HopModel {
  key: string
  station: ActivityStation
  d: string
  outAt: number
  /** When the call ended, so its packet comes back and the line fades. */
  endAt: number | null
}

export interface PodModel {
  key: string
  pos: Pt
  startAt: number
  endAt: number | null
  hops: HopModel[]
}

export interface TrailLeg {
  key: string
  d: string
  opacity: number
  newest: boolean
  at: number
  end: Pt
}

export type HullRim = 'idle' | 'run' | 'wait' | 'done'

export interface HullModel {
  started: boolean
  rim: HullRim
  anyRun: boolean
  /** Context window fill, whole percent; null while unknown. */
  ctx: number | null
  ctxLevel: 'ok' | 'warn' | 'crit'
  /** When the run ended, while its ring is still expanding. */
  doneRingAt: number | null
}

export interface MapModel {
  W: number
  /** The clock the model was made at, which the one-shot effects are timed against. */
  now: number
  docks: DockModel[]
  effects: Effect[]
  pods: PodModel[]
  trail: TrailLeg[]
  hull: HullModel
  held: boolean
}

const startOf = (entry: ActivityEntry) => Date.parse(entry.startedAt)
const endOf = (entry: ActivityEntry) => (entry.endedAt ? Date.parse(entry.endedAt) : null)
const clamp01 = (value: number) => Math.max(0, Math.min(1, value))

/** "claude-opus-5-5" → "opus-5-5": the model as the hull's second line. */
export function shortModel(model: string | null | undefined): string {
  if (!model) return ''
  return model.replace(/^claude-/, '').replace(/-\d{8}$/, '')
}

export function ctxLevelOf(percent: number): 'ok' | 'warn' | 'crit' {
  return percent >= 90 ? 'crit' : percent >= 70 ? 'warn' : 'ok'
}

/** Cable wear: more calls wear it in, with diminishing returns (spec §5, Wear). */
export const wearOf = (calls: number) => Math.min(1, Math.log2(1 + calls) / 4.5)

export function deriveMap(input: MapInput): MapModel {
  const { entries, summary, now, still } = input
  const g = mapGeom(input.W)
  const started = !!summary && summary.available
  const live = (entry: ActivityEntry) => entry.status === 'run' && !(summary?.ended ?? false)
  const held = input.waiting && started
  const own = entries.filter((entry) => !entry.bySubagent)
  // Spans cover the whole run (the entries only the newest 80); use them for counts when they do.
  const counted: Pick<ActivitySpan, 'station' | 'status' | 'bySubagent' | 'inbound'>[] =
    input.spans.length >= entries.length ? input.spans : entries
  const anyRun = entries.some(live)

  const docks: DockModel[] = STATIONS.map((station) => {
    const id = station.id
    const all = entries.filter((entry) => entry.station === id)
    const running = all.filter(live)
    const active = running.length > 0
    let lastEnd = -Infinity
    for (const entry of all) {
      const ended = endOf(entry)
      if (ended !== null && ended <= now) lastEnd = Math.max(lastEnd, ended)
    }
    const since = active ? 0 : (now - lastEnd) / 1000
    let lastErr = -Infinity
    for (const entry of all) {
      const ended = endOf(entry)
      if (entry.status === 'err' && ended !== null && ended <= now) lastErr = Math.max(lastErr, ended)
    }
    const errAge = (now - lastErr) / 1000
    const alarm = errAge < ALARM
    const lit = active || since < GLOW
    const glow: Glow = active ? 'active' : alarm ? 'alarm' : lit ? 'afterglow' : 'none'
    const fade = active ? 1 : alarm ? clamp01(1 - errAge / ALARM) : lit ? clamp01(1 - since / GLOW) : 0
    const latest = all[all.length - 1]
    const land = latest ? (now - startOf(latest)) / 1000 - OUT : -1
    const calls = counted.filter((entry) => entry.station === id)
    const ownCalls = calls.filter((entry) => !entry.bySubagent).length
    const waiting = id === 'you' && held
    const elapsedMs = active ? now - Math.min(...running.map(startOf)) : 0
    const count = calls.length
    const parked = waiting
    const cableRunning = waiting || own.some((entry) => entry.station === id && live(entry))
    return {
      id, count, errors: calls.filter((entry) => entry.status === 'err').length,
      running: running.length, active, waiting, elapsedMs, glow,
      glowAt: glow === 'alarm' ? lastErr : lastEnd, glowAge: glow === 'alarm' ? errAge : since, fade,
      bumpAt: !still && latest && land > -OUT && land < BUMP ? startOf(latest) + OUT * 1000 : null,
      shakeAt: !still && errAge < SHAKE ? lastErr : null,
      second: active ? formatClock(elapsedMs) : count ? `${count} call${count === 1 ? '' : 's'}` : '',
      dimmed: held && id !== 'you',
      wear: wearOf(ownCalls), worn: ownCalls > 0, cableRunning, cableParked: parked,
    }
  })

  // One-shot effects: a packet out when a call starts, a ripple where it lands, a packet back with
  // the result when it ends. Only those still in flight (or about to start) are listed.
  const effects: Effect[] = []
  if (!still && started) {
    for (const entry of own) {
      const t0 = startOf(entry)
      const t1 = endOf(entry)
      const peers = own.filter((other) => other.station === entry.station && Math.abs(startOf(other) - t0) < TOGETHER_MS)
      const shift = (peers.indexOf(entry) - (peers.length - 1) / 2) * SIDE_BY_SIDE
      const age = (now - t0) / 1000
      if (age < OUT + RIP) {
        const base = { station: entry.station, shift: entry.inbound ? 0 : shift, bad: false }
        if (age < OUT) effects.push({ ...base, key: `${entry.id}:out`, kind: entry.inbound ? 'in' : 'out', at: t0, dur: OUT })
        effects.push({ ...base, key: `${entry.id}:land`, kind: entry.inbound ? 'inland' : 'land', at: t0 + OUT * 1000, dur: RIP })
      }
      if (entry.inbound || t1 === null || entry.status === 'run') continue
      const back = (now - t1) / 1000
      if (back < BACK + RIP) {
        const bad = entry.status === 'err'
        const base = { station: entry.station, shift, bad }
        if (back < BACK) effects.push({ ...base, key: `${entry.id}:back`, kind: 'back', at: t1, dur: BACK })
        effects.push({ ...base, key: `${entry.id}:result`, kind: 'result', at: t1 + BACK * 1000, dur: RIP })
      }
    }
  }

  // Subagents: a pod per running Agents call (and for 0.4s after it ends, while the pod returns), with a
  // hop to each dock a subagent's call uses. Hops need `parentId`; without it the pod just sits in the bay.
  const crew = own.filter((entry) => entry.station === 'agents'
    && (live(entry) || (!still && endOf(entry) !== null && (now - endOf(entry)!) / 1000 < POD_BACK)))
  const ghosts = started && summary ? Math.max(0, summary.subagentsInFlight - crew.filter(live).length) : 0
  const pods: PodModel[] = []
  const count = crew.length + ghosts
  crew.forEach((entry, k) => {
    const pos = podSlot(g, k, count)
    const hops: HopModel[] = []
    for (const sub of entries) {
      if (!sub.bySubagent || sub.parentId !== entry.id) continue
      const ended = endOf(sub)
      if (ended !== null && (now - ended) / 1000 > HOP_FADE) continue
      hops.push({ key: sub.id, station: sub.station, d: podHop(g, pos, sub.station), outAt: startOf(sub), endAt: ended })
    }
    pods.push({ key: entry.id, pos, startAt: startOf(entry), endAt: live(entry) ? null : endOf(entry), hops })
  })
  for (let i = 0; i < ghosts; i++) pods.push({ key: `ghost-${i}`, pos: podSlot(g, crew.length + i, count), startAt: 0, endAt: null, hops: [] })

  // The trail: the last few docks the agent itself visited, joined in order.
  const visits = visitsOf(entries)
  const trail: TrailLeg[] = []
  for (let k = 1; k < visits.length; k++) {
    trail.push({
      key: `${visits[k - 1].id}>${visits[k].id}@${visits[k].at}`, d: hopLeg(g, visits[k - 1].id, visits[k].id),
      opacity: 0.08 + (0.42 * k) / (visits.length - 1), newest: k === visits.length - 1, at: visits[k].at,
      end: g.at[visits[k].id].port,
    })
  }

  const ctx = started && summary!.contextPercent !== null ? Math.round(summary!.contextPercent!) : null
  const ended = started && summary!.ended && !held
  // There is no end time on the wire: the run's last write, or its last call's end, is the best there is.
  const finished = ended
    ? Math.max(summary!.lastWriteAt ? Date.parse(summary!.lastWriteAt) : 0, ...entries.map((entry) => endOf(entry) ?? 0)) || null
    : null
  const hull: HullModel = {
    started, anyRun, ctx, ctxLevel: ctx === null ? 'ok' : ctxLevelOf(ctx),
    rim: held ? 'wait' : anyRun ? 'run' : ended ? 'done' : 'idle',
    doneRingAt: !still && finished !== null && (now - finished) / 1000 < DONE_RING ? finished : null,
  }
  return { W: input.W, now, docks, effects, pods, trail, hull, held }
}

