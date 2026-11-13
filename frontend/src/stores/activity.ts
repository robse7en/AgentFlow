import { defineStore } from 'pinia'
import { ref } from 'vue'
import { api } from '@/api/client'
import type { ActivityMessage, QuestionDto, RunActivity } from '@/api/types'
import {
  applyMessage, currentCall, latestRound, liveRunCount, liveSpend, nowLine, pulseLine, runFromPayload, runsForNode, waitsFor,
  type NowLine,
} from '@/activity/runs'

/**
 * What the agents are doing right now, from their transcripts: one entry per run, kept in memory
 * only. The backend pushes small updates over the websocket and serves the whole picture (with
 * the spans and context steps) on request; a task view asks for it when it opens and after the
 * socket reconnects.
 */
export const useActivityStore = defineStore('activity', () => {
  /** Every run seen, by run id. */
  const runs = ref<Record<number, RunActivity>>({})

  /** Tasks being loaded, and the updates that arrived meanwhile (the answer may predate them). */
  const loading = new Map<string, { count: number; queue: ActivityMessage[] }>()

  /** One `activity` websocket message. */
  function apply(message: ActivityMessage) {
    loading.get(message.jiraKey)?.queue.push(message)
    runs.value[message.runId] = applyMessage(runs.value[message.runId], message)
  }

  /**
   * Loads everything held for a task, replacing what the store has of those runs. Updates that
   * arrive while the request is out are applied again on top of the answer; merging one twice
   * changes nothing.
   */
  async function load(jiraKey: string) {
    const flight = loading.get(jiraKey) ?? { count: 0, queue: [] }
    flight.count += 1
    loading.set(jiraKey, flight)
    try {
      const { runs: held } = await api.activity(jiraKey)
      // A run the backend no longer holds (it restarted) would stay live here for good.
      const kept = new Set(held.map((run) => run.runId))
      for (const [id, run] of Object.entries(runs.value)) {
        if (run.jiraKey === jiraKey && !kept.has(Number(id))) delete runs.value[Number(id)]
      }
      for (const run of held) runs.value[run.runId] = runFromPayload(run)
    } catch {
      /* Display-only: a backend without the route (or a task not found) just shows no activity. */
    } finally {
      flight.count -= 1
      if (flight.count === 0) {
        loading.delete(jiraKey)
        for (const message of flight.queue) apply(message)
      }
    }
  }

  /** Forgets a deleted task's runs. */
  function forgetTask(jiraKey: string) {
    for (const [id, run] of Object.entries(runs.value)) {
      if (run.jiraKey === jiraKey) delete runs.value[Number(id)]
    }
  }

  /** The runs a node's card shows (the Implementer's include its workers'), oldest first. */
  const runsFor = (jiraKey: string, nodeKey: string): RunActivity[] =>
    runsForNode(Object.values(runs.value).filter((run) => run.jiraKey === jiraKey), nodeKey)

  /** What a live node is doing now, or null with no activity data for it. */
  const currentCallFor = (jiraKey: string, nodeKey: string): NowLine | null =>
    currentCall(runsFor(jiraKey, nodeKey))

  /** The card's current-call line: the live call, or the totals once the node's runs are over. */
  const lineFor = (jiraKey: string, nodeKey: string, live: boolean, finished: boolean): NowLine | null =>
    nowLine(runsFor(jiraKey, nodeKey), live, finished)

  /** Spend so far by the node's live runs (null when none is live). */
  const liveCost = (jiraKey: string, nodeKey: string) => liveSpend(runsFor(jiraKey, nodeKey))

  /** The heartbeat line from activity, or null to keep the usual one. */
  const pulse = (jiraKey: string, nodeKey: string, now: number) => pulseLine(runsFor(jiraKey, nodeKey), now)

  /** The node's latest round of runs (a worker round for the Implementer). */
  const roundFor = (jiraKey: string, nodeKey: string): RunActivity[] => latestRound(runsFor(jiraKey, nodeKey))

  /** How many of the node's runs are going at once (several workers side by side). */
  const liveRuns = (jiraKey: string, nodeKey: string) => liveRunCount(runsFor(jiraKey, nodeKey))

  /** When a run waited on the person, from the task's questions. */
  const waits = (run: Pick<RunActivity, 'runId'>, questions: QuestionDto[]) => waitsFor(run, questions)

  return {
    runs, apply, load, forgetTask, runsFor, currentCall: currentCallFor, lineFor, liveCost, pulse, roundFor,
    waitsFor: waits, liveRuns,
  }
})
