// Checks for the tape and Run so far models, which the frontend has no test runner for:
//   node frontend/src/activity/check-tape.mts
import assert from 'node:assert/strict'
import type { ActivityContextStep, ActivityEntry, ActivitySpan, ActivitySummary } from '../api/types.ts'
import { deriveRun, RUN_H } from './runStrip.ts'
import {
  BAR_FADE, deriveTape, EPOCH_MS, epochOf, gutterOf, LANE, mmss, OVERSHOOT_S, PPS, runBounds, SCROLL_S, STRIKE, TAPE_H, TTOP,
  type TapeInput, type Wait,
} from './tapeModel.ts'
import { STATIONS } from './stations.ts'

const T0 = Date.parse('2026-11-12T10:00:00.000Z')
const at = (ms: number) => new Date(T0 + ms).toISOString()
const summary = (over: Partial<ActivitySummary> = {}): ActivitySummary => ({
  counts: {}, busyMs: {}, errors: 0, subagentsInFlight: 0, model: 'claude-opus-5-5', steps: 0, contextPercent: null,
  tokens: 0, costUsd: 0, lastWriteAt: at(0), available: true, ended: false, ...over,
})
const entry = (id: string, station: ActivityEntry['station'], ms: number, over: Partial<ActivityEntry> = {}): ActivityEntry => ({
  id, station, label: id, startedAt: at(ms), endedAt: null, status: 'run', bySubagent: false, parentId: null,
  inbound: false, ...over,
})
const done = (id: string, station: ActivityEntry['station'], from: number, to: number, over: Partial<ActivityEntry> = {}) =>
  entry(id, station, from, { endedAt: at(to), status: 'ok', ...over })
const spanOf = (e: ActivityEntry): ActivitySpan => ({
  station: e.station, startedAt: e.startedAt, endedAt: e.endedAt, status: e.status, bySubagent: e.bySubagent, inbound: e.inbound,
})
const tapeOf = (entries: ActivityEntry[], now: number, over: Partial<TapeInput> = {}) =>
  deriveTape({
    W: 450, entries, spans: entries.map(spanOf), summary: summary(), now: T0 + now, waits: [], waiting: false, still: false,
    hover: null, ...over,
  })
const bar = (model: ReturnType<typeof deriveTape>, id: string) => model.bars.find((b) => b.id === id)!
const pen = (model: ReturnType<typeof deriveTape>, id: string) => model.pens.find((p) => p.id === id)!
const wait = (from: number, to: number | null): Wait => ({ from: T0 + from, to: to === null ? null : T0 + to })

// ---- constants and small helpers ----
assert.equal(TAPE_H, TTOP + 9 * LANE + 24)
assert.equal(gutterOf(399), 140)
assert.equal(gutterOf(400), 156)
assert.equal(mmss(65), '1:05')
assert.equal(mmss(4.9), '0:04')
assert.equal(epochOf(T0) % EPOCH_MS, 0)
assert.ok(SCROLL_S * 1000 > EPOCH_MS, 'the one scroll animation outlasts an epoch')

// ---- geometry: the paper and the screen agree ----
{
  const e = [done('a', 'shell', 1000, 3000), entry('b', 'files', 9000)]
  const m = tapeOf(e, 10_000)
  assert.equal(m.xNow, 450 - 156)
  assert.equal(m.top, 24)
  // A call that ended 7s ago sits 7s × 17 px/s left of the now-line, on screen (paper + shift).
  const a = bar(m, 'a')
  assert.ok(Math.abs(a.x + m.shift - (m.xNow - 9 * PPS)) < 0.11, `bar a starts 9s back, got ${a.x + m.shift}`)
  assert.ok(Math.abs(a.w - 2 * PPS) < 0.11, 'and is 2s wide')
  assert.equal(a.lane, 4)
  assert.equal(a.y, 24 + 4 * LANE + 5)
  assert.equal(a.h, LANE - 10)
  // A running bar is drawn past the pen, to be clipped there, but its label room is what is visible.
  const b = bar(m, 'b')
  assert.ok(b.running && b.ink === 'run')
  assert.ok(b.x + m.shift < m.xNow && b.x + b.w + m.shift > m.xNow + OVERSHOOT_S * PPS - 1)
  // The paper's shift is what slides it: at another instant the same bar is 17 px further left in screen terms.
  const later = tapeOf(e, 11_000)
  assert.ok(Math.abs((a.x + m.shift) - (bar(later, 'a').x + later.shift) - PPS) < 0.2)
  // ...and the paper itself does not move (its coordinates are fixed to time) while the shift does.
  assert.equal(bar(later, 'a').x, a.x)
  assert.ok(Math.abs(m.shift - later.shift - PPS) < 0.01)
}

// ---- lanes follow the station order; parallel calls split a lane ----
{
  const e = [entry('p', 'shell', 1000), entry('q', 'shell', 1500), done('r', 'files', 1000, 2000)]
  const m = tapeOf(e, 4000)
  assert.equal(bar(m, 'p').lane, STATIONS.findIndex((s) => s.id === 'shell'))
  assert.ok(bar(m, 'p').h < LANE - 10 && bar(m, 'q').h === bar(m, 'p').h, 'two calls at one station share its height')
  assert.notEqual(bar(m, 'p').y, bar(m, 'q').y)
  assert.equal(bar(m, 'r').h, LANE - 10, 'a lone call has the whole lane')
  assert.equal(bar(m, 'p').label, null, 'a bar in a shared lane carries no label')
}

// ---- labels only when the bar is wide enough, and truncated ----
{
  const e = [done('long', 'shell', 0, 20_000, { label: 'cargo test --workspace --all-targets' }), done('tiny', 'files', 20_000, 20_200)]
  const m = tapeOf(e, 21_000)
  assert.ok(bar(m, 'long').label && bar(m, 'long').label!.length <= Math.floor((20 * PPS - 10) / 5.45))
  assert.match(bar(m, 'long').label!, /…$|^cargo test/)
  assert.equal(bar(m, 'tiny').label, null)
  assert.ok(bar(m, 'tiny').w >= 2.4)
}

// ---- running bars: dots, fade after the end, failures ----
{
  const m = tapeOf([entry('r', 'shell', 0, { label: 'cargo build' })], 12_000)
  assert.ok(bar(m, 'r').dots, 'a long running bar has marching dots')
  assert.ok(bar(m, 'r').dots!.x2 > m.xNow - m.shift, 'running into the pen')
  const fresh = tapeOf([done('f', 'files', 0, 5000)], 5000 + BAR_FADE * 500)
  assert.equal(bar(fresh, 'f').ink, 'fading')
  assert.equal(bar(fresh, 'f').endAt, T0 + 5000)
  const old = tapeOf([done('f', 'files', 0, 5000)], 9000)
  assert.equal(bar(old, 'f').ink, 'idle')
  const bad = tapeOf([done('x', 'shell', 0, 5000, { status: 'err' })], 5300)
  assert.equal(bar(bad, 'x').ink, 'fail')
  assert.ok(bar(bad, 'x').cross && bar(bad, 'x').rippleAt === T0 + 5000, 'a failure gets a cross and a ripple while new')
  assert.equal(bar(bad, 'x').tone, 'red')
  assert.equal(bad.failLines.length, 1)
  assert.ok(bad.failLines[0].flag)
  const stale = tapeOf([done('x', 'shell', 0, 5000, { status: 'err' })], 20_000)
  assert.equal(bar(stale, 'x').rippleAt, null)
  assert.equal(pen(stale, 'shell').errors, 1)
}

// ---- subagents: hollow bars and a window across every lane; inbound diamonds ----
{
  const e = [
    entry('ag', 'agents', 1000), done('s1', 'files', 2000, 3000, { bySubagent: true, parentId: 'ag' }),
    done('in', 'you', 500, 500, { inbound: true, label: 'Prompt' }),
  ]
  const m = tapeOf(e, 5000)
  assert.ok(bar(m, 's1').sub && bar(m, 's1').label === null, 'a subagent bar is hollow, with no label')
  assert.equal(m.windows.length, 1, 'only the Agents call itself opens a window')
  assert.equal(m.windows[0].end, false, 'running: no closing line')
  assert.equal(m.diamonds.length, 1)
  assert.equal(m.bars.some((b) => b.id === 'in'), false, 'an inbound prompt is a diamond, not a bar')
  const after = tapeOf([done('ag', 'agents', 1000, 4000)], 5000)
  assert.equal(after.windows[0].end, true)
}

// ---- waiting: a band over every lane, stripes only while open ----
{
  const closed = tapeOf([done('a', 'shell', 0, 1000)], 20_000, { waits: [wait(4000, 12_000)] })
  assert.equal(closed.waitBands.length, 1)
  assert.equal(closed.waitBands[0].open, false)
  assert.ok(Math.abs(closed.waitBands[0].w - 8 * PPS) < 0.2)
  assert.match(closed.waitBands[0].flag!, /^waiting for you · 8s$/)
  assert.equal(closed.edgeFlags.length, 0)
  const open = tapeOf([done('a', 'shell', 0, 1000)], 30_000, { waits: [wait(25_000, null)], waiting: true })
  assert.equal(open.waitBands[0].open, true)
  assert.ok(open.pens[0].held && open.pens[0].active, 'the You pen holds while the node waits')
  assert.equal(open.pens[0].running!.label, 'waiting for you')
  assert.equal(open.pens[0].running!.timer, '5s')
  const stale = tapeOf([done('a', 'shell', 0, 1000)], 30_000, { waits: [wait(25_000, null)], waiting: false })
  assert.equal(stale.waitBands.length, 0, 'an open question the node is no longer waiting on draws nothing')
  const short = tapeOf([done('a', 'shell', 0, 1000)], 30_000, { waits: [wait(28_000, null)], waiting: true })
  assert.equal(short.waitBands[0].flag, 'waiting', 'a narrow band says only "waiting"')
  // A wait that began off the left edge keeps its flag at the edge, with the whole wait's duration.
  const long = tapeOf([done('a', 'shell', 0, 1000)], 600_000, { waits: [wait(5000, null)], waiting: true })
  assert.equal(long.waitBands[0].flag, null)
  assert.deepEqual(long.edgeFlags.map((f) => [f.x, f.text]), [[7, 'waiting for you · 9:55']])
  // A band that fits between 30 and 120 px says only "waiting", not the clock.
  assert.equal(tapeOf([done('a', 'shell', 0, 1000)], 20_000, { waits: [wait(14_000, 20_000)] }).waitBands[0].flag, 'waiting')
}

// ---- pens ----
{
  const m = tapeOf([entry('c', 'shell', 9800, { label: 'cargo test' }), done('g', 'git', 1000, 2000)], 10_000)
  const shell = pen(m, 'shell')
  assert.ok(shell.active && shell.ink === 'hot')
  assert.equal(shell.strikeAt, T0 + 9800, 'a call that began 0.2s ago strikes its pen')
  assert.equal(shell.running!.label, 'cargo test')
  assert.equal(shell.running!.timer, '0s')
  assert.equal(shell.share, null, 'a running pen shows the call, not the share')
  const git = pen(m, 'git')
  assert.equal(git.active, false)
  assert.equal(git.count, 1)
  assert.ok(git.share && git.share.busy === '1s' && git.share.frac > 0 && git.share.frac < 1)
  assert.equal(git.coolAt, null, 'it ended long ago')
  assert.equal(pen(m, 'network').ink, 'dim')
  assert.equal(pen(m, 'network').share, null)
  // Past the strike, no strike.
  assert.equal(pen(tapeOf([entry('c', 'shell', 9800)], 10_000 + STRIKE * 1000), 'shell').strikeAt, null)
  // Cooling after an end, red after a failure.
  const cool = tapeOf([done('c', 'shell', 0, 5000, { status: 'err' })], 5400)
  assert.equal(pen(cool, 'shell').coolAt, T0 + 5000)
  assert.ok(pen(cool, 'shell').coolRed)
  assert.equal(pen(cool, 'shell').shakeAt, T0 + 5000)
  // ×N when parallel.
  const par = tapeOf([entry('a', 'files', 5000), entry('b', 'files', 5100)], 8000)
  assert.equal(pen(par, 'files').running!.head, '×2 ')
  // Counts come from the spans (the whole run) when they cover more than the entries.
  const spans = Array.from({ length: 200 }, (_, i) => spanOf(done(`s${i}`, 'shell', i * 10, i * 10 + 5)))
  const many = tapeOf([done('only', 'shell', 1999, 2000)], 3000, { spans })
  assert.equal(pen(many, 'shell').count, 200)
  // A model step is not labelled with the model's name.
  assert.equal(bar(tapeOf([done('m', 'model', 0, 20_000, { label: 'claude-opus-5-5' })], 21_000), 'm').label, null)
}

// ---- still mode: states stay, effects go ----
{
  const e = [entry('c', 'shell', 9800), done('x', 'git', 9000, 9500, { status: 'err' })]
  const m = tapeOf(e, 10_000, { still: true })
  assert.ok(m.pens.every((p) => p.strikeAt === null && p.ringAt === null && p.shakeAt === null && p.doneAt === null))
  assert.equal(bar(m, 'x').rippleAt, null)
  assert.ok(pen(m, 'shell').active && pen(m, 'git').errors === 1, 'but the states are there')
  assert.equal(bar(tapeOf([done('f', 'files', 0, 5000)], 5100, { still: true }), 'f').ink, 'idle')
}

// ---- the run's end: the tape freezes and the pens ring one after another ----
{
  const e = [done('a', 'shell', 0, 5000)]
  const over = summary({ ended: true, lastWriteAt: at(5000) })
  const justNow = tapeOf(e, 5100, { summary: over })
  assert.equal(justNow.head.word, 'DONE')
  assert.equal(justNow.ended, true)
  assert.equal(pen(justNow, 'you').doneAt, T0 + 5000, 'the first pen rings at the end')
  assert.equal(pen(justNow, 'skills').doneAt, T0 + 5000 + 8 * 70, 'the last follows 8 steps later')
  const later = tapeOf(e, 3_600_000, { summary: over })
  assert.equal(later.tt, T0 + 5000, 'an old run stays at its end')
  assert.ok(later.pens.every((p) => p.doneAt === null))
  const m2 = tapeOf(e, 3_600_500, { summary: over })
  assert.equal(m2.shift, later.shift, 'a finished tape does not move')
  // A run that ended but is waiting again is not over.
  assert.equal(tapeOf(e, 6000, { summary: over, waiting: true }).ended, false)
}

// ---- before anything happens ----
{
  const m = tapeOf([], 5000)
  assert.equal(m.started, false)
  assert.equal(m.head.word, 'IDLE')
  assert.equal(m.bars.length, 0)
  assert.equal(m.veil, null)
  assert.equal(tapeOf([done('a', 'shell', 0, 100)], 5000, { summary: summary({ available: false }) }).started, false)
}

// ---- the grid ----
{
  const m = tapeOf([done('a', 'shell', 0, 100)], 12_000)
  assert.deepEqual(m.grid.filter((g) => g.k <= 10).map((g) => g.k), [0, 5, 10])
  assert.ok(m.grid[0].label === '0:00' || m.grid[0].label === null)
  assert.ok(m.grid.every((g) => g.k % 5 === 0))
  assert.ok(m.startFlag === null || m.startFlag > 0)
}

// ---- hover ----
{
  const e = [done('a', 'shell', 0, 4000, { label: 'cargo test' }), done('in', 'you', 1000, 1000, { inbound: true, label: 'Prompt' })]
  const m0 = tapeOf(e, 6000)
  const a = bar(m0, 'a')
  const hover = { x: a.x + m0.shift + 10, y: a.y + 4 }
  const m = tapeOf(e, 6000, { hover })
  assert.ok(bar(m, 'a').hit)
  assert.equal(m.tip!.l1, 'Shell · cargo test')
  assert.equal(m.tip!.l2, 'done · 4.0 s · at 0:00')
  assert.ok(m.tip!.x >= 4 && m.tip!.x + m.tip!.w <= 450 - 4)
  const d = tapeOf(e, 6000, { hover: { x: m0.xNow - 5 * PPS, y: 24 + LANE / 2 } })
  assert.ok(d.diamonds[0].hit)
  assert.match(d.tip!.l2, /from you$/)
  assert.equal(tapeOf(e, 6000, { hover: { x: m0.xNow + 30, y: a.y } }).tip, null, 'nothing is hit in the gutter')
  assert.equal(tapeOf(e, 6000, { hover: { x: 3, y: 3 } }).tip, null)
}

// ---- runBounds ----
{
  const e = [done('a', 'shell', 1000, 2000), done('b', 'shell', 3000, 9000)]
  assert.deepEqual(runBounds(e, e.map(spanOf), summary(), false), { start: T0 + 1000, end: null })
  assert.equal(runBounds(e, e.map(spanOf), summary({ ended: true, lastWriteAt: at(9500) }), false).end, T0 + 9500)
  assert.equal(runBounds(e, e.map(spanOf), summary({ ended: true, lastWriteAt: at(9500) }), true).end, null)
  assert.deepEqual(runBounds([], [], summary(), false), { start: null, end: null })
}

// ---- Run so far ----
const stepOf = (ms: number, percent: number): ActivityContextStep => ({ at: at(ms), percent })
const runOf = (entries: ActivityEntry[], now: number, over: Record<string, unknown> = {}) =>
  deriveRun({
    W: 450, entries, spans: entries.map(spanOf), contextSteps: [], summary: summary(), now: T0 + now, waits: [], waiting: false,
    window: false, ...over,
  } as Parameters<typeof deriveRun>[0])
{
  assert.equal(RUN_H, 94)
  const e = [done('a', 'shell', 0, 4000), done('b', 'files', 4000, 8000, { status: 'err' }), done('s', 'git', 8000, 9000, { bySubagent: true })]
  const m = runOf(e, 20_000)
  assert.equal(m.bars.length, 3)
  const [a, b, s] = m.bars
  assert.equal(a.y, 6 + 4 * 4)
  assert.equal(b.tone, 'red')
  assert.equal(m.ticks.length, 1)
  assert.ok(s.sub)
  assert.ok(a.x === 18 && b.x > a.x, 'time runs left to right from the left edge')
  assert.ok(m.head, 'a live run has its now-line')
  assert.equal(m.axis.left, '0:00')
  assert.equal(m.axis.right, '0:20')
  // Fewer than 12 seconds in, the strip is still stretched over 12.
  const early = runOf([entry('a', 'shell', 0)], 3000)
  assert.ok(early.bars[0].w < 450 * 0.4, 'the first call does not fill the strip')
  // Ended: the strip is the whole run, to its end.
  const fin = runOf(e, 3_600_000, { summary: summary({ ended: true, lastWriteAt: at(9000) }) })
  assert.equal(fin.ended, true)
  assert.equal(fin.head, null)
  assert.equal(fin.axis.right, '0:09 · finished')
  const lastEnd = fin.bars[2]
  assert.ok(Math.abs(lastEnd.x + lastEnd.w - (450 - 18)) < 0.5, 'the last call ends at the right edge')
  // The tape window.
  assert.equal(m.window, null)
  const w = runOf(e, 20_000, { window: true })
  assert.ok(w.window && w.window.w > 2)
  // Waits wash the block.
  const washed = runOf(e, 20_000, { waits: [wait(10_000, 14_000)] })
  assert.equal(washed.washes.length, 1)
  assert.equal(runOf(e, 20_000, { waits: [wait(10_000, null)], waiting: false }).washes.length, 0)
  assert.equal(runOf(e, 20_000, { waits: [wait(10_000, null)], waiting: true }).washes.length, 1)
}

// ---- the context line ----
{
  const e = [done('a', 'shell', 0, 4000)]
  const none = runOf(e, 20_000)
  assert.equal(none.ctx, null, 'no data, no line')
  const steps = runOf(e, 20_000, {
    contextSteps: [stepOf(1000, 20), stepOf(6000, 45), stepOf(12_000, 72)], summary: summary({ contextPercent: 72 }),
  })
  assert.ok(steps.ctx)
  assert.equal(steps.ctx!.level, 'warn')
  assert.equal(steps.ctx!.label, 'context 72%')
  assert.equal((steps.ctx!.line.match(/V/g) ?? []).length, 3, 'one riser per step: it only grows when a result comes back')
  assert.match(steps.ctx!.line, /^M18/)
  assert.ok(steps.ctx!.area.endsWith('z'))
  assert.equal(runOf(e, 20_000, { contextSteps: [stepOf(1000, 95)], summary: summary({ contextPercent: 95 }) }).ctx!.level, 'crit')
  assert.equal(runOf(e, 20_000, { contextSteps: [stepOf(1000, 30)], summary: summary({ contextPercent: 30 }) }).ctx!.level, 'ok')
  // No steps yet but a known fill: a flat line.
  const flat = runOf(e, 20_000, { summary: summary({ contextPercent: 33 }) })
  assert.equal(flat.ctx!.label, 'context 33%')
  // Steps in the future of a frozen (ended) run are not drawn.
  const fin = runOf(e, 3_600_000, { summary: summary({ ended: true, lastWriteAt: at(4000), contextPercent: 40 }), contextSteps: [stepOf(1000, 40), stepOf(90_000, 99)] })
  assert.equal(fin.ctx!.label, 'context 40%')
  // The 70% guide is where 70% is.
  assert.ok(Math.abs(steps.guide.y - (steps.baseline.y - 0.7 * 28)) < 0.1)
}

// ---- Run so far uses the whole run's spans, not just the newest 80 calls ----
{
  const spans = Array.from({ length: 300 }, (_, i) => spanOf(done(`s${i}`, i % 2 ? 'shell' : 'files', i * 1000, i * 1000 + 800)))
  const newest = spans.slice(-80).map((span, i) => entry(`e${i}`, span.station, 220_000 + i * 1000, { endedAt: at(220_000 + i * 1000 + 800), status: 'ok' }))
  const m = deriveRun({ W: 450, entries: newest, spans, contextSteps: [], summary: summary(), now: T0 + 400_000, waits: [], waiting: false, window: false })
  assert.equal(m.bars.length, 300, 'a strip made from 300 spans has 300 bars although only 80 calls are held in full')
  assert.equal(m.axis.right, '6:40')
}

console.log('check-tape: all checks passed')
