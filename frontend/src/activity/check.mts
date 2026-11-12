// Checks for activity/runs.ts, which the frontend has no test runner for:
//   node frontend/src/activity/check.mts
import assert from 'node:assert/strict'
import type { ActivityEntry, ActivityMessage, ActivitySummary, RunActivity } from '../api/types.ts'
import {
  applyMessage, callCount, currentCall, formatSpan, formatTokens, latestRound, liveSpend, MAX_ENTRIES, nowLine,
  pulseLine, roundSummary, runFromPayload, runsForNode, waitsFor,
} from './runs.ts'

const T0 = Date.parse('2026-11-12T10:00:00.000Z')
const at = (ms: number) => new Date(T0 + ms).toISOString()

const summary = (over: Partial<ActivitySummary> = {}): ActivitySummary => ({
  counts: {}, busyMs: {}, errors: 0, subagentsInFlight: 0, model: 'claude-test', steps: 0, contextPercent: null,
  tokens: 0, costUsd: 0, lastWriteAt: at(0), available: true, ended: false, ...over,
})
const entry = (id: string, station: ActivityEntry['station'], ms: number, over: Partial<ActivityEntry> = {}): ActivityEntry => ({
  id, station, label: id, startedAt: at(ms), endedAt: null, status: 'run', bySubagent: false, parentId: null,
  inbound: false, ...over,
})
const message = (runId: number, nodeKey: string, entries: ActivityEntry[], over: Partial<ActivitySummary> = {}): ActivityMessage => ({
  type: 'activity', jiraKey: 'T-1', nodeKey, runId, sessionId: runId, runNumber: 1, harness: 'claude', entries,
  summary: summary(over),
})
const run = (runId: number, nodeKey: string, entries: ActivityEntry[], over: Partial<ActivitySummary> = {}): RunActivity =>
  applyMessage(undefined, message(runId, nodeKey, entries, over))

// ---- merging ----
let held = applyMessage(undefined, message(1, 'architect', [entry('a', 'shell', 0), entry('b', 'files', 100)]))
assert.deepEqual(held.entries.map((e) => e.id), ['a', 'b'])
assert.equal(held.spans.length, 2)
// A changed entry replaces its own, and its span follows; the others stay.
const ended = entry('a', 'shell', 0, { endedAt: at(500), status: 'ok' })
const before = held
held = applyMessage(held, message(1, 'architect', [ended]))
assert.equal(held.entries[0].status, 'ok')
assert.equal(held.spans[0].status, 'ok')
assert.equal(held.spans[0].endedAt, at(500))
assert.equal(held.spans[1].status, 'run')
assert.equal(before.entries[0].status, 'run', 'the earlier run is never changed')
// A new entry is appended, with its span.
held = applyMessage(held, message(1, 'architect', [entry('c', 'git', 900)]))
assert.deepEqual(held.entries.map((e) => e.id), ['a', 'b', 'c'])
assert.equal(held.spans.length, 3)
// Context steps are appended when a step is counted and the window is known.
held = applyMessage(held, message(1, 'architect', [], { steps: 1, contextPercent: 12.5, lastWriteAt: at(1000) }))
assert.deepEqual(held.contextSteps, [{ at: at(1000), percent: 12.5 }])
held = applyMessage(held, message(1, 'architect', [], { steps: 1, contextPercent: 12.5 }))
assert.equal(held.contextSteps.length, 1, 'no new step, no new point')
held = applyMessage(held, message(1, 'architect', [], { steps: 2, contextPercent: null }))
assert.equal(held.contextSteps.length, 1, 'an unknown window adds nothing')
// Only the newest 80 entries are kept.
let big: RunActivity | undefined
for (let i = 0; i < 100; i++) big = applyMessage(big, message(2, 'architect', [entry(`e${i}`, 'files', i)]))
assert.equal(big!.entries.length, MAX_ENTRIES)
assert.equal(big!.entries[0].id, 'e20')
assert.equal(big!.spans.length, 100)

// ---- a run from the GET, which leaves out empty lists ----
const sent = { ...run(9, 'architect', []) } as Partial<RunActivity> & Omit<RunActivity, 'spans' | 'contextSteps'>
delete sent.spans
delete sent.contextSteps
const loaded = runFromPayload(sent)
assert.deepEqual([loaded.spans, loaded.contextSteps], [[], []])
const afterLoad = applyMessage(loaded, message(9, 'architect', [entry('n', 'git', 5)], { steps: 1, contextPercent: 3 }))
assert.equal(afterLoad.spans.length, 1, 'a message after a GET that sent no spans still merges')
assert.equal(afterLoad.contextSteps.length, 1, 'and so does the context point of a new step')
const withLists = runFromPayload({ ...sent, spans: [{ station: 'git', startedAt: at(0), endedAt: null, status: 'run', bySubagent: false, inbound: false }] })
assert.equal(withLists.spans.length, 1)

// ---- which runs a node shows ----
const runs = [
  run(10, 'architect', []), run(11, 'implementer', []), run(12, 'worker_a', []), run(13, 'worker_b', []),
  run(14, 'implementer', []), run(15, 'worker_a', []), run(16, 'worker_b', []), run(17, 'code_review', []),
]
assert.deepEqual(runsForNode(runs, 'architect').map((r) => r.runId), [10])
assert.deepEqual(runsForNode(runs, 'implementer').map((r) => r.runId), [11, 12, 13, 14, 15, 16])
assert.deepEqual(runsForNode(runs, 'worker_a').map((r) => r.runId), [12, 15], 'a worker row shows only its own runs')
assert.deepEqual(latestRound(runsForNode(runs, 'implementer')).map((r) => r.runId), [15, 16], 'the workers since the last own run')
assert.deepEqual(latestRound(runsForNode(runs.slice(0, 5), 'implementer')).map((r) => r.runId), [14])
assert.deepEqual(latestRound(runsForNode(runs.slice(0, 4), 'implementer')).map((r) => r.runId), [12, 13])
assert.deepEqual(latestRound([]), [])

// ---- the current call ----
assert.equal(currentCall([]), null, 'no activity, no line')
assert.equal(currentCall([run(1, 'x', [], { available: false })]), null)
assert.deepEqual(currentCall([run(1, 'x', [])]), { kind: 'text', text: 'No calls yet', dim: true })
const quiet = run(1, 'x', [entry('a', 'shell', 0, { endedAt: at(10), status: 'ok' })])
assert.deepEqual(currentCall([quiet]), { kind: 'text', text: 'Between calls', dim: true })
const thinking = run(1, 'x', [entry('think-a', 'model', 20)])
const now = currentCall([thinking])
assert.ok(now?.kind === 'call' && now.label === 'Thinking' && now.station === 'model')
// A tool call beats a model step, the newest of the tool calls wins, and a subagent's call does not count.
const busy = run(1, 'x', [
  entry('think-b', 'model', 30), entry('old', 'files', 40), entry('new', 'shell', 50, { label: 'cargo' }),
  entry('sub', 'git', 60, { bySubagent: true, parentId: 'p' }),
])
const top = currentCall([busy])
assert.ok(top?.kind === 'call' && top.label === 'cargo' && top.station === 'shell')
// Across the workers of one round the newest wins; an ended run is ignored.
const other = run(2, 'worker_b', [entry('w', 'network', 70, { label: 'Jira · T-1' })])
const done = run(3, 'worker_c', [entry('z', 'git', 90)], { ended: true })
const across = currentCall([busy, other, done])
assert.ok(across?.kind === 'call' && across.label === 'Jira · T-1')
assert.equal(currentCall([done]), null, 'an ended run says nothing about now')
// Earlier runs do not decide it: a new run with no calls yet is "No calls yet", and a run with no
// data (another harness) after an ended Claude one shows nothing.
assert.deepEqual(currentCall([done, run(4, 'x', [])]), { kind: 'text', text: 'No calls yet', dim: true })
assert.equal(currentCall([done, run(5, 'x', [], { available: false })]), null)
assert.equal(currentCall([quiet, run(6, 'x', [], { available: false })])?.kind, 'text', 'a live run with data still counts')

// ---- after the run ----
const counted = (id: number, node: string, counts: ActivitySummary['counts'], errors = 0) =>
  run(id, node, [entry('a', 'shell', 0)], { counts, errors, ended: true })
assert.equal(callCount(summary({ counts: { model: 40, you: 2, shell: 30, files: 10, git: 1 } })), 41)
const w1 = counted(1, 'worker_a', { shell: 20, model: 9 }), w2 = counted(2, 'worker_b', { files: 21, you: 1 }, 1)
assert.deepEqual(roundSummary([w1, w2]), { kind: 'text', text: '2 workers · 41 calls · 1 failed', dim: true })
assert.deepEqual(roundSummary([counted(3, 'architect', { git: 1 })]), { kind: 'text', text: '1 call', dim: true })
assert.equal(roundSummary([run(4, 'architect', [], { available: false })]), null)
assert.equal(nowLine([w1, w2], false, false), null, 'not started, nothing to say')
assert.deepEqual(nowLine([w1], false, true), roundSummary([w1]))
assert.equal(nowLine([], true, false), null)

// ---- live spend ----
assert.equal(liveSpend([run(1, 'x', [], { ended: true })]), null)
assert.deepEqual(liveSpend([run(1, 'x', [], { tokens: 1500, costUsd: 0.25 }), run(2, 'y', [], { tokens: 500, costUsd: 0.5 })]),
  { costUsd: 0.75, tokens: 2000 })
assert.deepEqual(liveSpend([run(1, 'x', [], { tokens: 1500, costUsd: 0.25 }), run(2, 'y', [], { tokens: 500, costUsd: null })]),
  { costUsd: null, tokens: 2000 }, 'one unknown price makes the cost unknown')
assert.equal(formatTokens(950), '950 tok')
assert.equal(formatTokens(1500), '1.5k tok')
assert.equal(formatTokens(140_000), '140k tok')

// ---- the heartbeat ----
assert.equal(formatSpan(42_000), '42s')
assert.equal(formatSpan(6 * 60_000 + 5000), '6m')
assert.equal(formatSpan(125 * 60_000), '2h 5m')
const T = (ms: number) => T0 + ms
assert.equal(pulseLine([], T(0)), null)
assert.deepEqual(pulseLine([run(1, 'x', [entry('c', 'shell', 0, { label: 'cargo test' })])], T(6 * 60_000)),
  { text: 'Shell · cargo test · 6m', silentMs: 0 }, 'a long call is not silence, whatever the transcript says')
const silent = run(1, 'x', [entry('a', 'shell', 0, { endedAt: at(10), status: 'ok' })], { lastWriteAt: at(0) })
assert.deepEqual(pulseLine([silent], T(6 * 60_000)), { text: 'No transcript writes for 6m', silentMs: 6 * 60_000 })
assert.equal(pulseLine([silent], T(30_000)), null, 'recent writes: keep the usual heartbeat')
assert.equal(pulseLine([run(1, 'x', [entry('t', 'model', 0)], { lastWriteAt: at(0) })], T(30_000)), null, 'thinking is not a call')
assert.equal(pulseLine([run(1, 'x', [entry('c', 'shell', 0)], { ended: true })], T(1000)), null)

// ---- waiting for you ----
const questions = [
  { runId: 5, createdAt: at(2000), answeredAt: at(3000) }, { runId: 6, createdAt: at(1000), answeredAt: null },
  { runId: 5, createdAt: at(500), answeredAt: null },
]
assert.deepEqual(waitsFor({ runId: 5 }, questions), [
  { from: at(500), to: null }, { from: at(2000), to: at(3000) },
])
assert.deepEqual(waitsFor({ runId: 7 }, questions), [])

console.log('activity/runs.ts: all checks passed')
