// Checks for the canvas's pure modules (batons and the card's mini strip), which the frontend has no test
// runner for:
//   node frontend/src/workflow/check-canvas.mts
// The replay section reads notes/agent-activity-map/replay.json from the main checkout (notes/ is not in the
// worktree); it is skipped with a note when that file is not found.
import assert from 'node:assert/strict'
import fs from 'node:fs'
import { fileURLToPath } from 'node:url'
import type { ActivityEntry, ActivitySpan, ActivitySummary, NodeState } from '../api/types.ts'
import { deriveStrips, STRIP_H } from '../activity/strips.ts'
import {
  BATON_S, batonText, chipWidth, edgeFlows, edgeId, handoffs, HAND, isLoopEdge, isStaleFirstPass, LINGER, newBatons,
  type BatonTask,
} from './batons.ts'

let checks = 0
const ok = (cond: unknown, what: string) => { checks++; assert.ok(cond, what) }
const eq = (a: unknown, b: unknown, what: string) => { checks++; assert.deepEqual(a, b, what) }

// ---- the graph, as the server sends it ------------------------------------------------------------------
const EDGES = [
  ['architect', 'requirements_gate'], ['requirements_gate', 'orchestrator'], ['orchestrator', 'implementer'],
  ['implementer', 'code_review'], ['implementer', 'security_review'], ['code_review', 'consolidate'],
  ['security_review', 'consolidate'], ['consolidate', 'implementer'], ['consolidate', 'final_architect'],
  ['final_architect', 'ready_for_pr'],
].map(([source, target]) => ({ source, target }))
const KEYS = ['architect', 'requirements_gate', 'orchestrator', 'implementer', 'code_review', 'security_review', 'consolidate', 'final_architect', 'ready_for_pr']

type NodeInit = Partial<{ state: NodeState; checkpointSha: string | null; blockingCount: number; findingsCount: number; worker: boolean }>
function task(states: Record<string, NodeInit | NodeState> = {}, extra: Partial<BatonTask> = {}, jiraKey = 'PROJ-1', workers = 0): BatonTask {
  const nodes = KEYS.map((key) => {
    const init = typeof states[key] === 'string' ? { state: states[key] as NodeState } : (states[key] as NodeInit | undefined) ?? {}
    return { key, displayState: init.state ?? 'PENDING', checkpointSha: init.checkpointSha ?? null, blockingCount: init.blockingCount ?? 0, findingsCount: init.findingsCount ?? 0, worker: null }
  })
  for (let i = 0; i < workers; i++) nodes.push({ key: `worker_${i}`, displayState: 'COMPLETED', checkpointSha: null, blockingCount: 0, findingsCount: 0, worker: { uid: String(i) } as never })
  return { jiraKey, nodes: nodes as never, edges: EDGES, assessment: null, ...extra }
}
const ids = (list: { source: string; target: string }[]) => list.map(edgeId)

// ---- edge predicates -------------------------------------------------------------------------------------
ok(isLoopEdge({ source: 'consolidate', target: 'implementer' }), 'loop edge')
ok(!isLoopEdge({ source: 'orchestrator', target: 'implementer' }), 'forward edge into the Implementer is not the loop')
{
  const first = task({ orchestrator: 'COMPLETED', implementer: 'RUNNING' })
  const looped = task({ orchestrator: 'COMPLETED', implementer: 'RUNNING', consolidate: 'COMPLETED' })
  ok(!isStaleFirstPass(first, { source: 'orchestrator', target: 'implementer' }), 'first pass is not stale')
  ok(isStaleFirstPass(looped, { source: 'orchestrator', target: 'implementer' }), 'after the loop the forward edge is stale')
  ok(!isStaleFirstPass(looped, { source: 'consolidate', target: 'implementer' }), 'the loop edge itself is never stale')
  ok(edgeFlows(first, { source: 'orchestrator', target: 'implementer' }), 'dashes flow into the live Implementer')
  ok(!edgeFlows(looped, { source: 'orchestrator', target: 'implementer' }), 'and not along the stale forward edge')
  ok(edgeFlows(looped, { source: 'consolidate', target: 'implementer' }), 'but along the loop')
  ok(edgeFlows(task({ architect: 'COMPLETED', requirements_gate: 'READY' }), EDGES[0]), 'dashes also flow into a READY node')
  ok(!edgeFlows(task({ architect: 'RUNNING', requirements_gate: 'RUNNING' }), EDGES[0]), 'not out of a source that has not finished')
  ok(!edgeFlows(task({ architect: 'COMPLETED' }), EDGES[0]), 'not into a PENDING node')
}

// ---- when a baton fires ------------------------------------------------------------------------------------
{
  const before = task({ architect: 'COMPLETED', requirements_gate: 'PENDING' })
  const after = task({ architect: 'COMPLETED', requirements_gate: 'WAITING_FOR_APPROVAL' })
  eq(ids(handoffs(before, after)), ['architect->requirements_gate'], 'the target going live fires one baton')
  eq(handoffs(null, after), [], 'the first snapshot (a reload mid-run) fires nothing')
  eq(handoffs(undefined, after), [], 'nor does a missing earlier one')
  eq(handoffs(task({}, {}, 'OTHER-9'), after), [], 'a different task fires nothing')
  eq(handoffs(after, after), [], 'the same snapshot twice fires nothing')
  eq(handoffs(task({ architect: 'COMPLETED', requirements_gate: 'READY' }), after).length, 1, 'READY to live is a handoff')
  eq(handoffs(task({ architect: 'COMPLETED', requirements_gate: 'READY' }), task({ architect: 'COMPLETED', requirements_gate: 'READY' })), [], 'READY alone is not (the dashes show it)')
  eq(handoffs(task({ architect: 'COMPLETED', requirements_gate: 'WAITING_FOR_APPROVAL' }), task({ architect: 'COMPLETED', requirements_gate: 'COMPLETED' })), [], 'a node finishing fires nothing')
  eq(handoffs(task({ architect: 'COMPLETED', requirements_gate: 'FAILED' }), task({ architect: 'COMPLETED', requirements_gate: 'RUNNING' })), [], 'a retry after a failure is not a handoff')
  eq(handoffs(task({ architect: 'COMPLETED', requirements_gate: 'INTERRUPTED' }), task({ architect: 'COMPLETED', requirements_gate: 'RUNNING' })), [], 'nor after an interruption')
  eq(handoffs(task({ architect: 'RUNNING', requirements_gate: 'PENDING' }), task({ architect: 'RUNNING', requirements_gate: 'RUNNING' })), [], 'a target live before its source is done fires nothing')
  eq(handoffs(task({ architect: 'COMPLETED', requirements_gate: 'RUNNING' }), task({ architect: 'COMPLETED', requirements_gate: 'WAITING_FOR_USER' })), [], 'live to live fires nothing')
  const both = handoffs(
    task({ architect: 'RUNNING', requirements_gate: 'PENDING' }),
    task({ architect: 'COMPLETED', requirements_gate: 'WAITING_FOR_APPROVAL' }),
  )
  eq(ids(both), ['architect->requirements_gate'], 'source and target changing in one snapshot fires once')
  // The loop: the Implementer runs again after Consolidate. Only the loop edge fires.
  const loopBefore = task({ orchestrator: 'COMPLETED', implementer: 'COMPLETED', consolidate: 'COMPLETED' })
  const loopAfter = task({ orchestrator: 'COMPLETED', implementer: 'RUNNING', consolidate: 'COMPLETED' })
  eq(ids(handoffs(loopBefore, loopAfter)), ['consolidate->implementer'], 'the loop fires the loop edge, not the stale forward one')
  eq(newBatons(loopBefore, loopAfter, 5)[0].loop, true, 'and the loop baton is flagged')
  eq(newBatons(before, after, 5)[0].loop, false, 'a forward baton is not')
  // Both reviews finishing before Consolidate starts: both edges fire together when it goes live.
  const reviews = handoffs(
    task({ code_review: 'COMPLETED', security_review: 'COMPLETED', consolidate: 'PENDING' }),
    task({ code_review: 'COMPLETED', security_review: 'COMPLETED', consolidate: 'RUNNING' }),
  )
  eq(ids(reviews).sort(), ['code_review->consolidate', 'security_review->consolidate'], 'two reviews, two batons')
  // The Implementer fans out to both reviews when each goes live.
  eq(ids(handoffs(task({ implementer: 'COMPLETED' }), task({ implementer: 'COMPLETED', code_review: 'RUNNING' }))), ['implementer->code_review'], 'one review goes live: one baton')
}

// ---- what a baton carries (spec §10) --------------------------------------------------------------------
{
  const t = (states: Record<string, NodeInit | NodeState>, extra: Partial<BatonTask> = {}, workers = 0) => task(states, extra, 'PROJ-1', workers)
  const text = (source: string, target: string, task: BatonTask) => batonText(task, { source, target })
  eq(text('architect', 'requirements_gate', t({})), 'requirements', 'Architect to Approval')
  eq(text('requirements_gate', 'orchestrator', t({})), 'approved', 'Approval to Planning')
  eq(text('orchestrator', 'implementer', t({}, {}, 3)), '3 subtasks', 'Planning to Implementer counts the workers')
  eq(text('orchestrator', 'implementer', t({}, {}, 1)), '1 subtask', 'one subtask is singular')
  eq(text('orchestrator', 'implementer', t({})), 'plan', 'with no workers in the snapshot it says what it is')
  eq(text('implementer', 'code_review', t({ implementer: { checkpointSha: '4f2a91c7d0e81b3a' } })), '4f2a91c', 'the checkpoint is seven characters')
  eq(text('implementer', 'security_review', t({ implementer: { checkpointSha: '4f2a91c7d0e81b3a' } })), '4f2a91c', 'to either review')
  eq(text('implementer', 'code_review', t({})), 'checkpoint', 'no SHA yet')
  eq(text('code_review', 'consolidate', t({ code_review: { blockingCount: 2, findingsCount: 3 } })), '2 blocking', 'blocking findings win')
  eq(text('security_review', 'consolidate', t({ security_review: { findingsCount: 1 } })), '1 minor', 'findings that block nothing are minor')
  eq(text('code_review', 'consolidate', t({})), 'clear', 'no findings is clear')
  eq(text('consolidate', 'implementer', t({ code_review: { blockingCount: 2 }, security_review: { blockingCount: 1 } })), 'fix 3 blocking', 'the loop sums both reviews')
  eq(text('consolidate', 'implementer', t({})), 'fix findings', 'the loop with no counts')
  eq(text('consolidate', 'final_architect', t({})), '0 blocking', 'Consolidate to Final')
  eq(text('final_architect', 'ready_for_pr', t({}, { assessment: { overallRisk: 'LOW' } })), 'low risk', 'the risk is the assessment\'s overallRisk')
  eq(text('final_architect', 'ready_for_pr', t({}, { assessment: { risk: 'High' } })), 'high risk', 'or risk')
  eq(text('final_architect', 'ready_for_pr', t({}, { assessment: { risk_level: 'medium' } })), 'medium risk', 'or risk_level')
  eq(text('final_architect', 'ready_for_pr', t({}, { assessment: {} })), 'done', 'no risk')
  eq(text('final_architect', 'ready_for_pr', t({})), 'done', 'no assessment')
  eq(text('x', 'y', t({})), 'handoff', 'an edge this build does not know')
  ok(chipWidth('4f2a91c') > chipWidth('clear'), 'chips grow with the text')
  // The text is fixed when the baton fires, whatever the snapshot does later.
  const fired = newBatons(task({ implementer: 'COMPLETED' }), task({ implementer: { state: 'COMPLETED', checkpointSha: 'aaaaaaa1' }, code_review: 'RUNNING' }), 1000)
  eq(fired.map((b) => [b.edge, b.text, b.key]), [['implementer->code_review', 'aaaaaaa', 'implementer->code_review:1000']], 'a fired baton carries its text and a key made of edge and time')
  ok(Math.abs(BATON_S - (HAND + LINGER)) < 1e-9 && HAND === 1.1 && LINGER === 0.7, 'run 1.1 s, linger 0.7 s')
}

// ---- the replay's node states: one baton per handoff ----------------------------------------------------
{
  const path = fileURLToPath(new URL('../../../../../../notes/agent-activity-map/replay.json', import.meta.url))
  const file = process.env.REPLAY_JSON ?? path
  if (!fs.existsSync(file)) {
    console.log(`note: ${file} not found, replay section skipped`)
  } else {
    const replay = JSON.parse(fs.readFileSync(file, 'utf8')) as { states: { at: number; nodeKey: string; state: NodeState }[] }
    const states: Record<string, NodeState> = {}
    let previous: BatonTask = task({})
    const fired: string[] = []
    for (const change of [...replay.states].sort((a, b) => a.at - b.at)) {
      states[change.nodeKey] = change.state
      const next = task({ ...states })
      for (const edge of handoffs(previous, next)) fired.push(`${change.at} ${edgeId(edge)}`)
      previous = next
    }
    eq(fired, [
      '22.5 architect->requirements_gate', '26.4 requirements_gate->orchestrator', '33 orchestrator->implementer',
      '56.8 implementer->code_review', '57.1 implementer->security_review',
      '68.2 code_review->consolidate', '68.2 security_review->consolidate', '70.5 consolidate->implementer',
      '81.6 implementer->code_review', '81.9 implementer->security_review',
      '89.4 code_review->consolidate', '89.4 security_review->consolidate', '91 consolidate->final_architect',
      '101.2 final_architect->ready_for_pr',
    ], 'the replay hands over fourteen times, once each, and the loop only along the loop edge')
  }
}

// ---- the mini strip ---------------------------------------------------------------------------------------
{
  const T0 = Date.parse('2026-11-12T10:00:00.000Z')
  const at = (s: number) => new Date(T0 + s * 1000).toISOString()
  const entry = (id: string, station: ActivityEntry['station'], s0: number, s1: number | null, extra: Partial<ActivityEntry> = {}): ActivityEntry =>
    ({ id, station, label: 'x', startedAt: at(s0), endedAt: s1 === null ? null : at(s1), status: s1 === null ? 'run' : 'ok', bySubagent: false, parentId: null, inbound: false, ...extra })
  const summary = (extra: Partial<ActivitySummary> = {}): ActivitySummary => ({
    available: true, counts: {}, errors: 0, ended: false, lastWriteAt: null, contextPercent: null, costUsd: null, tokens: 0, steps: 0, busyMs: 0, ...extra,
  } as ActivitySummary)
  const run = (entries: ActivityEntry[], extra: Partial<ActivitySummary> = {}, spans: ActivitySpan[] = []) => ({ entries, spans, summary: summary(extra) })
  const W = 226

  const empty = deriveStrips({ W, runs: [], now: T0, waiting: false })
  eq([empty.tracks.length, empty.bars.length, empty.dots.length, empty.H], [1, 0, 0, STRIP_H], 'no runs: one empty track')
  eq(empty.tracks[0], { y: 5.5, h: 5 }, 'a single track is 5px, centred in 16')
  const two = deriveStrips({ W, runs: [run([]), run([])], now: T0, waiting: false })
  eq(two.tracks.map((t) => t.h), [3.6, 3.6], 'two workers: thin tracks')
  ok(two.tracks[1].y - two.tracks[0].y > 3.6 && two.tracks[1].y + 3.6 <= STRIP_H, 'with a gap, inside the band')
  eq(deriveStrips({ W, runs: [run([])], now: T0 + 5000, waiting: false }).bars.length, 0, 'a run with no calls draws nothing')

  // One live run, ten seconds in: bars squeezed over the whole width, the dot at now.
  const live = run([
    entry('a', 'files', 0, 2), entry('b', 'shell', 3, 5), entry('m', 'model', 0, 10), entry('p', 'you', 1, 1, { inbound: true }),
    entry('s', 'files', 6, 7, { bySubagent: true }), entry('f', 'git', 7, 8, { status: 'err' }), entry('o', 'network', 9, null),
  ])
  const a = deriveStrips({ W, runs: [live], now: T0 + 10_000, waiting: false })
  eq(a.bars.length, 5, 'model steps and the person\'s words are not drawn')
  eq(a.bars[0], { key: '0:0', x: 0, y: 5.5, w: 45.2, h: 5, tone: 'live', sub: false }, 'a call is a bar of its share of the time')
  eq(a.bars.map((b) => b.tone), ['live', 'live', 'live', 'red', 'live'], 'a failure is red')
  eq(a.bars.map((b) => b.sub), [false, false, true, false, false], 'subagent calls are marked (drawn at 45%)')
  eq(a.dots.length, 1, 'a dot while it runs')
  ok(a.dots[0].x <= W - 2.5, 'the dot never leaves the strip')
  ok(a.bars[4].x + a.bars[4].w <= W + 0.1, 'the open call reaches now, no further')

  // Time is squeezed with a floor of ten seconds: a 2 s run fills a fifth.
  const brief = deriveStrips({ W, runs: [run([entry('a', 'files', 0, 2)])], now: T0 + 2000, waiting: false })
  ok(Math.abs(brief.bars[0].w - W * 0.2) < 0.2, 'the strip never spreads over less than ten seconds')
  ok(Math.abs(brief.dots[0].x - W * 0.2) < 0.2, 'and the dot sits where now is')

  // An ended run: no dot, the end fixes the span, a failure draws red; nothing drawn after "now".
  const done = run([entry('a', 'files', 0, 6), entry('f', 'shell', 6, 12, { status: 'err' })], { ended: true, lastWriteAt: at(12) })
  const d = deriveStrips({ W, runs: [done], now: T0 + 60_000, waiting: false })
  eq([d.dots.length, d.bars.length, d.bars[1].tone], [0, 2, 'red'], 'ended: no dot, both calls, the failure red')
  ok(Math.abs(d.bars[1].x + d.bars[1].w - W) < 0.2, 'the last call ends at the right edge')
  const waiting = deriveStrips({ W, runs: [done], now: T0 + 60_000, waiting: true })
  eq(waiting.dots.length, 1, 'a run waiting for the person has not ended, whatever the transcript says')
  const early = deriveStrips({ W, runs: [done], now: T0 + 9000, waiting: false })
  ok(early.bars.length === 2 && early.bars[1].w < d.bars[1].w + 100, 'a clock before the end clips the open part')

  // Workers: each on its own track, one shared time axis.
  const w1 = run([entry('a', 'files', 0, 10)], { ended: true, lastWriteAt: at(10) })
  const w2 = run([entry('b', 'shell', 5, 20)], { ended: true, lastWriteAt: at(20) })
  const ws = deriveStrips({ W, runs: [w1, w2], now: T0 + 30_000, waiting: false })
  eq(ws.bars.map((b) => b.y > 0), [true, true], 'bars on both tracks')
  ok(ws.bars[0].y !== ws.bars[1].y && ws.bars[1].x > ws.bars[0].x, 'one track each, the second starting later')
  ok(Math.abs(ws.bars[1].x + ws.bars[1].w - W) < 0.2, 'the shared axis ends with the last worker')

  // Spans carry the whole run; the entries are only the newest 80.
  const spans = Array.from({ length: 3 }, (_, i) => ({ station: 'files', startedAt: at(i * 3), endedAt: at(i * 3 + 2), status: 'ok', bySubagent: false, inbound: false })) as ActivitySpan[]
  eq(deriveStrips({ W, runs: [run([entry('a', 'shell', 7, 9)], {}, spans)], now: T0 + 12_000, waiting: false }).bars.length, 3, 'spans win when they hold at least as many calls as the entries')
}

console.log(`canvas checks: ${checks} passed`)
