// Checks for the activity panel's pure modules (geometry, feed, mapModel), which the frontend has no
// test runner for:
//   node frontend/src/activity/check-map.mts
import assert from 'node:assert/strict'
import type { ActivityEntry, ActivitySpan, ActivitySummary } from '../api/types.ts'
import {
  assignSlots, contextLevel, feedRows, formatClock, formatTook, nowParts, type NowInput,
} from './feed.ts'
import { MAP_H, mapGeom, mapWidth, podSlot, visitsOf } from './geometry.ts'
import {
  ALARM, BACK, deriveMap, GLOW, LAP, OUT, RIP, shortModel, wearOf, type MapInput,
} from './mapModel.ts'
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
const mapOf = (entries: ActivityEntry[], now: number, over: Partial<MapInput> = {}) =>
  deriveMap({
    W: 450, entries, spans: entries.map(spanOf), summary: summary(), now: T0 + now, waiting: false, still: false, ...over,
  })
const dock = (model: ReturnType<typeof deriveMap>, id: string) => model.docks.find((d) => d.id === id)!

// ---- stations ----
assert.deepEqual(STATIONS.filter((s) => s.side > 0).sort((a, b) => a.row - b.row).map((s) => s.id), ['files', 'shell', 'git', 'network'])
assert.deepEqual(STATIONS.filter((s) => s.side < 0).sort((a, b) => a.row - b.row).map((s) => s.id), ['model', 'agentflow', 'skills', 'agents'])
assert.equal(STATIONS.filter((s) => s.side === 0).map((s) => s.id).join(), 'you')

// ---- geometry ----
assert.equal(MAP_H, 300)
assert.equal(mapWidth(100), 250)
assert.equal(mapWidth(900), 560)
const g = mapGeom(450)
assert.equal(g.cx, 225)
assert.equal(g.half, 46)
assert.equal(mapGeom(380).half, 38, 'a narrow map has a smaller hull')
assert.equal(g.at.you.port.y, 43)
assert.equal(g.at.network.y, 276)
assert.deepEqual(podSlot(g, 0, 2), { x: 225 - 19, y: 268 })
assert.deepEqual(podSlot(g, 1, 2), { x: 225 + 19, y: 268 })
assert.equal(g.cable.you, `M225 128L225 43`, 'the You cable is straight')
// The trail: own calls only, model steps and subagent calls skipped, repeats merged, last six kept.
const visits = visitsOf([
  entry('a', 'files', 0), entry('b', 'files', 10), entry('c', 'model', 20), entry('d', 'shell', 30),
  entry('e', 'git', 40, { bySubagent: true }), entry('f', 'git', 50), entry('g', 'network', 60), entry('h', 'files', 70),
  entry('i', 'skills', 80), entry('j', 'shell', 90), entry('k', 'agents', 100),
])
assert.deepEqual(visits.map((v) => v.id), ['git', 'network', 'files', 'skills', 'shell', 'agents'])
assert.equal(visits[0].at, T0 + 50)

// ---- formats ----
assert.equal(formatClock(500), '0s')
assert.equal(formatClock(12_900), '12s')
assert.equal(formatClock(65_000), '1:05')
assert.equal(formatClock(3_725_000), '1:02:05')
assert.equal(formatTook(40), '100 ms')
assert.equal(formatTook(2450), '2.5 s')
assert.equal(formatTook(65_000), '1:05')
assert.deepEqual(contextLevel(12), { level: 'ok', word: 'of the window' })
assert.deepEqual(contextLevel(70), { level: 'warn', word: 'filling up' })
assert.deepEqual(contextLevel(90), { level: 'crit', word: 'nearly full' })
assert.deepEqual(contextLevel(null), { level: 'ok', word: 'of the window' })
assert.equal(shortModel('claude-opus-5-5'), 'opus-5-5')
assert.equal(shortModel('claude-haiku-4-5-20251001'), 'haiku-4-5')
assert.equal(shortModel(null), '')

// ---- the Now headline ----
const nowOf = (entries: ActivityEntry[], now: number, over: Partial<NowInput> = {}) =>
  nowParts({ entries, summary: summary({ steps: 3, counts: { shell: 2, model: 3 } }), now: T0 + now, waiting: false, waitingSince: null, tab: 'Round 2', ...over })
let now = nowOf([entry('m', 'model', 0), entry('s', 'shell', 1000, { label: 'cargo test' })], 5000)
assert.equal(now.kind, 'Shell')
assert.equal(now.l1, 'cargo test', 'a tool call over a model step')
assert.equal(now.l2, 'Also running: Model')
assert.equal(now.tm, '4s')
assert.equal(now.cls, 'is-run')
now = nowOf([entry('m', 'model', 0, { label: 'opus' })], 5000)
assert.equal(now.kind, 'Thinking')
assert.equal(now.cls, 'is-think')
assert.equal(now.l2, 'Step 3 · 2 calls so far')
now = nowOf([entry('s', 'shell', 0, { label: 'flowctl ask', station: 'agentflow' })], 1000)
assert.equal(now.cls, 'is-flow')
now = nowOf([entry('a', 'agents', 0), entry('f', 'files', 100, { bySubagent: true, parentId: 'a', label: 'x.ts' })], 2000)
assert.equal(now.kind, 'Agents', 'an own call outranks a subagent call')
assert.equal(now.l2, 'Also running: subagent Files')
now = nowOf([done('a', 'shell', 0, 500)], 3000)
assert.equal(now.kind, 'Between calls')
assert.equal(now.l2, 'Step 3 · 2 calls so far')
now = nowOf([done('a', 'shell', 0, 500, { status: 'err', label: 'cargo build' })], 3000)
assert.equal(now.l2, 'Last call failed: cargo build')
assert.equal(now.bad, true)
now = nowOf([done('a', 'shell', 0, 500, { status: 'err', label: 'cargo build' })], 9000)
assert.equal(now.l2, 'Step 3 · 2 calls so far', 'a failure older than six seconds is not news')
// Waiting for the person outranks everything.
now = nowOf([entry('s', 'shell', 0)], 8000, { waiting: true, waitingSince: T0 + 2000 })
assert.equal(now.kind, 'Waiting for you')
assert.equal(now.tm, '6s')
assert.equal(now.cap, 'waiting')
assert.equal(now.cls, 'is-wait')
assert.equal(nowOf([], 8000, { waiting: true }).tm, '–', 'no known start, no timer')
// A finished run.
const finished = nowParts({
  entries: [done('a', 'shell', 0, 500)], summary: summary({ ended: true, steps: 4, counts: { shell: 1 }, contextPercent: 41.6, lastWriteAt: at(125_000) }),
  now: T0 + 130_000, waiting: false, waitingSince: null, tab: 'Round 2',
})
assert.equal(finished.kind, 'Finished')
assert.equal(finished.l1, 'Round 2 is done')
assert.equal(finished.l2, '4 steps · 1 call · context 42%')
assert.equal(finished.tm, '2:05')
assert.equal(finished.cap, 'total')

// ---- Latest calls ----
let rows = feedRows([done('a', 'files', 0, 100), done('b', 'files', 200, 300), done('c', 'files', 400, 500), entry('d', 'shell', 600)], T0 + 1000)
assert.deepEqual(rows.map((r) => [r.entry.id, r.times, r.cls, r.mark]), [['d', 1, 'run', '●'], ['c', 3, 'ok', '·']])
assert.equal(rows[0].took, 'running')
assert.equal(rows[1].took, '100 ms')
rows = feedRows([done('a', 'files', 0, 100), done('b', 'files', 200, 300, { bySubagent: true }), done('c', 'files', 400, 500, { status: 'err', endedAt: at(500) })], T0 + 1000)
assert.deepEqual(rows.map((r) => [r.entry.id, r.times, r.cls, r.mark]), [['c', 1, 'err', '✕'], ['b', 1, 'ok', '·'], ['a', 1, 'ok', '·']],
  'a failure and a subagent call do not fold')
assert.equal(feedRows(Array.from({ length: 20 }, (_, i) => done(`e${i}`, i % 2 ? 'files' : 'shell', i * 10, i * 10 + 5)), T0 + 1000).length, 6)
assert.deepEqual(feedRows([], T0), [])

// ---- slots for parallel calls on the tape ----
const slots = assignSlots([
  done('a', 'shell', 0, 5000), done('b', 'shell', 1000, 2000), done('c', 'shell', 1500, 3000), done('d', 'shell', 9000, 9500),
  done('p', 'you', 0, 1, { inbound: true }),
], T0 + 10_000)
assert.deepEqual([...['a', 'b', 'c', 'd'].map((id) => slots.get(id))], [
  { slot: 0, slots: 3 }, { slot: 1, slots: 3 }, { slot: 2, slots: 3 }, { slot: 0, slots: 1 },
])
assert.equal(slots.has('p'), false, 'an inbound prompt takes no lane slot')

// ---- the map: a call's life ----
// Quiet, never started.
let map = deriveMap({ W: 450, entries: [], spans: [], summary: null, now: T0, waiting: false, still: false })
assert.equal(map.hull.started, false)
assert.equal(map.docks.length, 9)
assert.ok(map.docks.every((d) => !d.active && d.glow === 'none' && d.second === ''))
// A call starts: a packet goes out, then a ripple lands; the dock is lit with a timer.
let calls = [entry('c1', 'shell', 1000, { label: 'cargo test' })]
map = mapOf(calls, 1200)
assert.deepEqual(map.effects.map((e) => [e.kind, e.station, e.dur]), [['out', 'shell', OUT], ['land', 'shell', RIP]])
assert.equal(map.effects[1].at, T0 + 1000 + OUT * 1000, 'the ripple starts when the packet lands')
assert.ok(dock(map, 'shell').active)
assert.equal(dock(map, 'shell').glow, 'active')
assert.equal(dock(map, 'shell').second, '0s')
assert.equal(dock(map, 'shell').bumpAt, T0 + 1500)
assert.equal(map.hull.rim, 'run')
assert.ok(dock(map, 'shell').cableRunning)
assert.equal(dock(map, 'files').cableRunning, false)
// Later: only the timer. No effect from the past is listed.
map = mapOf(calls, 20_000)
assert.deepEqual(map.effects, [])
assert.equal(dock(map, 'shell').second, '19s')
assert.equal(dock(map, 'shell').bumpAt, null)
// It ends ok: a grey packet comes back, then a result ripple; the dock afterglows and then goes quiet.
calls = [done('c1', 'shell', 1000, 3000)]
map = mapOf(calls, 3100)
assert.deepEqual(map.effects.map((e) => e.kind), ['back', 'result'])
assert.equal(map.effects.every((e) => !e.bad), true)
assert.equal(dock(map, 'shell').glow, 'afterglow')
assert.ok(dock(map, 'shell').fade > 0.9)
assert.equal(dock(map, 'shell').second, '1 call')
assert.equal(map.hull.rim, 'idle')
map = mapOf(calls, 3000 + (BACK + RIP) * 1000 + 50)
assert.deepEqual(map.effects, [])
map = mapOf(calls, 3000 + GLOW * 1000 + 100)
assert.equal(dock(map, 'shell').glow, 'none')
assert.equal(dock(map, 'shell').fade, 0)
// It fails: red packet back, shake, alarm for six seconds and a count that stays.
calls = [done('c1', 'shell', 1000, 3000, { status: 'err' })]
map = mapOf(calls, 3100)
assert.ok(map.effects.filter((e) => e.kind === 'back' || e.kind === 'result').every((e) => e.bad))
assert.equal(dock(map, 'shell').glow, 'alarm')
assert.equal(dock(map, 'shell').shakeAt, T0 + 3000)
assert.equal(dock(map, 'shell').errors, 1)
map = mapOf(calls, 3000 + ALARM * 1000 + 100)
assert.equal(dock(map, 'shell').glow, 'none')
assert.equal(dock(map, 'shell').errors, 1, 'the badge stays')
assert.equal(dock(map, 'shell').shakeAt, null)
// Parallel calls: ×N and side by side packets.
calls = [entry('a', 'files', 1000), entry('b', 'files', 1100)]
map = mapOf(calls, 1200)
assert.equal(dock(map, 'files').running, 2)
assert.deepEqual(map.effects.filter((e) => e.kind === 'out').map((e) => e.shift), [-2.75, 2.75])
// Wear follows the call count; a cable with no calls is not worn.
assert.equal(dock(map, 'git').worn, false)
assert.equal(dock(map, 'files').worn, true)
assert.equal(wearOf(0), 0)
assert.ok(wearOf(15) > wearOf(3) && wearOf(100000) === 1)
assert.equal(dock(map, 'files').wear, wearOf(2))
// Counts come from spans when they cover the whole run (entries are only the newest 80).
map = mapOf([done('c', 'files', 0, 10)], 60_000, { spans: Array.from({ length: 30 }, () => spanOf(done('x', 'files', 0, 10))) })
assert.equal(dock(map, 'files').count, 30)
assert.equal(dock(map, 'files').second, '30 calls')
// Reduced motion: every state stays and no effect is made.
map = mapOf([entry('c1', 'shell', 1000), done('c2', 'files', 0, 1100, { status: 'err' })], 1200, { still: true })
assert.deepEqual(map.effects, [])
assert.equal(dock(map, 'shell').bumpAt, null)
assert.equal(dock(map, 'files').shakeAt, null)
assert.equal(dock(map, 'files').glow, 'alarm')
assert.equal(dock(map, 'files').errors, 1)
assert.ok(dock(map, 'shell').active)

// ---- the map: waiting for the person ----
map = mapOf([entry('c1', 'shell', 1000)], 9000, { waiting: true })
assert.equal(map.held, true)
assert.equal(map.hull.rim, 'wait')
assert.ok(dock(map, 'you').waiting)
assert.ok(dock(map, 'you').cableParked && dock(map, 'you').cableRunning)
assert.deepEqual(map.docks.filter((d) => d.dimmed).map((d) => d.id).sort(), STATIONS.map((s) => s.id).filter((id) => id !== 'you').sort())
assert.equal(dock(map, 'you').dimmed, false)
assert.equal(mapOf([], 0, { waiting: true, summary: null }).held, false, 'no run, nothing to wait on')

// ---- the map: an inbound prompt ----
map = mapOf([done('p', 'you', 1000, 1000, { inbound: true })], 1100)
assert.deepEqual(map.effects.map((e) => e.kind), ['in', 'inland'])
assert.equal(map.effects[0].shift, 0)
assert.ok(!map.effects.some((e) => e.kind === 'back'), 'a prompt has no result coming back')

// ---- the map: a subagent ----
const sub = [
  entry('ag', 'agents', 1000, { label: 'Map the auth module' }),
  done('s1', 'files', 2000, 2500, { bySubagent: true, parentId: 'ag', label: 'tokens.ts' }),
  entry('s2', 'shell', 3000, { bySubagent: true, parentId: 'ag' }),
  done('s0', 'model', 1500, 1800, { bySubagent: true, parentId: null }),
]
map = mapOf(sub, 3200, { summary: summary({ subagentsInFlight: 1 }) })
assert.equal(map.pods.length, 1)
assert.equal(map.pods[0].key, 'ag')
assert.deepEqual(map.pods[0].pos, { x: 225, y: 268 })
assert.deepEqual(map.pods[0].hops.map((h) => h.key), ['s1', 's2'], 'a hop needs its parent; one that ended long ago is gone')
assert.ok(!map.effects.some((e) => e.key.startsWith('s')), 'a subagent call sends no packet from the hull')
assert.ok(dock(map, 'shell').active, 'a subagent call lights its dock')
assert.equal(dock(map, 'shell').cableRunning, false, 'but not the hull cable')
assert.equal(dock(map, 'files').worn, false, 'and does not wear the cable')
assert.equal(map.trail.length, 0, 'and is not in the trail')
// The Agents call ends: the pod returns for 0.4s, then is gone.
const after = [done('ag', 'agents', 1000, 5000), ...sub.slice(1, 3).map((e) => ({ ...e, endedAt: at(4500), status: 'ok' as const })), sub[3]]
map = mapOf(after, 5200, { summary: summary({ subagentsInFlight: 0 }) })
assert.equal(map.pods.length, 1)
assert.equal(map.pods[0].endAt, T0 + 5000)
map = mapOf(after, 5500)
assert.equal(map.pods.length, 0)
// A transcript that gave no parent: the pod sits in the bay with no hops.
map = mapOf([entry('ag', 'agents', 1000), entry('s', 'files', 2000, { bySubagent: true })], 3000, { summary: summary({ subagentsInFlight: 1 }) })
assert.equal(map.pods.length, 1)
assert.equal(map.pods[0].hops.length, 0)
// Two subagents share the bay.
map = mapOf([entry('a1', 'agents', 1000), entry('a2', 'agents', 1100)], 3000, { summary: summary({ subagentsInFlight: 2 }) })
assert.deepEqual(map.pods.map((p) => p.pos.x), [206, 244])
// Subagents in flight whose Agents call has left the newest 80 still show as pods.
map = mapOf([], 3000, { summary: summary({ subagentsInFlight: 1 }) })
assert.equal(map.pods.length, 1)
assert.equal(map.pods[0].hops.length, 0)

// ---- the map: the trail ----
map = mapOf([done('a', 'files', 0, 100), done('b', 'shell', 200, 300), done('c', 'git', 400, 500)], 1000)
assert.equal(map.trail.length, 2)
assert.deepEqual(map.trail.map((l) => l.newest), [false, true])
assert.ok(map.trail[0].opacity < map.trail[1].opacity)
assert.equal(map.trail[1].opacity, 0.5)
assert.equal(map.trail[1].end, mapGeom(450).at.git.port)

// ---- the hull ----
map = mapOf([], 1000, { summary: summary({ contextPercent: 71.4 }) })
assert.equal(map.hull.ctx, 71)
assert.equal(map.hull.ctxLevel, 'warn')
assert.equal(mapOf([], 1000, { summary: summary({ contextPercent: 95 }) }).hull.ctxLevel, 'crit')
assert.equal(mapOf([], 1000).hull.ctx, null)
map = mapOf([done('a', 'shell', 0, 100)], 130_000, { summary: summary({ ended: true, lastWriteAt: at(110_000) }) })
assert.equal(map.hull.rim, 'done')
assert.equal(map.hull.doneRingAt, null, 'the ring plays only just after the end')
map = mapOf([done('a', 'shell', 0, 100)], 110_500, { summary: summary({ ended: true, lastWriteAt: at(110_000) }) })
assert.equal(map.hull.doneRingAt, T0 + 110_000)
map = mapOf([done('a', 'shell', 0, 100)], 110_500, { summary: summary({ ended: true, lastWriteAt: at(110_000) }), waiting: true })
assert.equal(map.hull.rim, 'wait', 'waiting on the person outranks done')
assert.equal(LAP, 60)

console.log('activity panel modules: all checks passed')
