<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { RunActivity, TaskSnapshot, WorkflowNodeDto } from '@/api/types'
import { contextLevel, feedRows, nowParts } from '@/activity/feed'
import { mapWidth } from '@/activity/geometry'
import { deriveMap, shortModel } from '@/activity/mapModel'
import { callCount, formatTokens } from '@/activity/runs'
import { stationInfo } from '@/activity/stations'
import type { Wait } from '@/activity/tapeModel'
import { formatNzd } from '@/currency'
import { parseTimestamp, useNow } from '@/now'
import { useActivityStore } from '@/stores/activity'
import { useUsdToNzdRate } from '@/usage'
import ActivityMap from './ActivityMap.vue'
import ActivityRun from './ActivityRun.vue'
import ActivityTape from './ActivityTape.vue'
import HarnessLogo from './HarnessLogo.vue'

// The activity panel beside the canvas: one agent's header, what it is doing now, its vitals, the
// hull-and-docks map or the tape, the whole run squeezed into one strip, and its latest calls (spec §3).
// With no transcript data it says so and keeps the header.
const props = defineProps<{ task: TaskSnapshot; selected: string | null }>()
const activity = useActivityStore()
const usdToNzdRate = useUsdToNzdRate()

// ---- which agent and which run ----
// Follow (on by default) shows the run that started most recently and is still going. Picking a
// canvas node or a tab turns it off and pins that node (and run).
const follow = ref(true)
const pinnedNode = ref<string | null>(null)
const pinnedRun = ref<number | null>(null)

const agentNodes = computed(() => props.task.nodes.filter((node) => node.kind === 'agent' && !node.worker))
const nodeByKey = (key: string): WorkflowNodeDto | undefined => props.task.nodes.find((node) => node.key === key)
/** The canvas card a run belongs to: a worker's run belongs to the Implementer. */
const cardOf = (run: RunActivity) => (run.nodeKey.startsWith('worker_') ? 'implementer' : run.nodeKey)
const isLive = (run: RunActivity) => run.summary.available && !run.summary.ended

const taskRuns = computed(() => Object.values(activity.runs)
  .filter((run) => run.jiraKey === props.task.jiraKey)
  .sort((a, b) => a.runId - b.runId))

const followedRun = computed(() => taskRuns.value.filter(isLive).at(-1) ?? taskRuns.value.filter((run) => run.summary.available).at(-1) ?? null)

const LIVE_STATES = ['RUNNING', 'WAITING_FOR_USER']
const shownNode = computed<WorkflowNodeDto | null>(() => {
  const agents = agentNodes.value
  if (!follow.value && pinnedNode.value) {
    const pinned = agents.find((node) => node.key === pinnedNode.value)
    if (pinned) return pinned
  }
  const run = followedRun.value
  return (run && agents.find((node) => node.key === cardOf(run)))
    ?? agents.find((node) => LIVE_STATES.includes(node.state)) ?? agents[0] ?? null
})
const nodeRuns = computed(() => (shownNode.value ? activity.runsFor(props.task.jiraKey, shownNode.value.key) : []))
const run = computed<RunActivity | null>(() => {
  const node = shownNode.value
  if (!node) return null
  if (follow.value) {
    const followed = followedRun.value
    if (followed && cardOf(followed) === node.key) return followed
  }
  const runs = nodeRuns.value
  return runs.find((candidate) => candidate.runId === pinnedRun.value) ?? runs.at(-1) ?? null
})
const hasData = computed(() => !!run.value && run.value.summary.available)

/** The canvas calls this when a node is clicked: only an agent card pins the panel. */
function pick(key: string | null) {
  if (!key || !agentNodes.value.some((node) => node.key === key)) return
  follow.value = false
  pinnedNode.value = key
  pinnedRun.value = null
}
function pickRun(runId: number) {
  follow.value = false
  pinnedNode.value = shownNode.value?.key ?? null
  pinnedRun.value = runId
}
defineExpose({ pick })
watch(follow, (on) => {
  if (on) {
    pinnedNode.value = null
    pinnedRun.value = null
  }
})
// A task change starts afresh.
watch(() => props.task.jiraKey, () => {
  follow.value = true
  pinnedNode.value = null
  pinnedRun.value = null
})

// ---- the words ----
const workerRuns = computed(() => nodeRuns.value.filter((candidate) => candidate.nodeKey.startsWith('worker_')))
const tabLabel = (candidate: RunActivity): string => {
  if (candidate.nodeKey.startsWith('worker_')) {
    const title = nodeByKey(candidate.nodeKey)?.label
    return `Worker ${workerRuns.value.indexOf(candidate) + 1}${title ? ` · ${title}` : ''}`
  }
  return `Round ${candidate.runNumber}`
}
const runName = computed(() => {
  const current = run.value
  if (!current) return shownNode.value?.label ?? 'Activity'
  return nodeRuns.value.length > 1 ? tabLabel(current) : shownNode.value?.label ?? 'Run'
})
const harnessName = computed(() => shownNode.value?.effectiveConfig?.harnessDisplayName ?? run.value?.harness ?? '')
const subtitle = computed(() => {
  if (!shownNode.value) return ''
  const bits: string[] = []
  if (run.value && run.value.nodeKey.startsWith('worker_') && nodeRuns.value.length <= 1) bits.push(tabLabel(run.value))
  if (harnessName.value) bits.push(harnessName.value)
  return bits.join(' · ')
})
// As on the card: the configured model, else the one the run reports.
const modelChip = computed(() => shownNode.value?.effectiveConfig?.model ?? run.value?.summary.model ?? null)
const runNode = computed(() => (run.value ? nodeByKey(run.value.nodeKey) ?? shownNode.value : shownNode.value))
const waiting = computed(() => !!run.value && runNode.value?.state === 'WAITING_FOR_USER'
  && nodeRuns.value.filter((candidate) => candidate.nodeKey === run.value!.nodeKey).at(-1)?.runId === run.value.runId)
const waitingSince = computed(() => {
  const open = props.task.questions.filter((question) => !question.answered && question.runId === run.value?.runId)
  const times = open.map((question) => parseTimestamp(question.createdAt)).filter((time): time is number => time !== null)
  return times.length ? Math.min(...times) : null
})

// The clock ticks once a second while anything is moving; the model is a function of it.
const ticking = computed(() => !!run.value && (isLive(run.value) || waiting.value))
const tick = useNow(() => ticking.value)
const reduced = ref(false)
let reducedQuery: MediaQueryList | null = null
const onReduced = () => { reduced.value = !!reducedQuery?.matches }
onMounted(() => {
  reducedQuery = window.matchMedia('(prefers-reduced-motion: reduce)')
  onReduced()
  reducedQuery.addEventListener('change', onReduced)
})
onBeforeUnmount(() => reducedQuery?.removeEventListener('change', onReduced))

const vizEl = ref<HTMLElement | null>(null)
const width = ref(450)
/** The panel's own width: the tape and Run so far fill it (the map stops growing at 560). */
const fullWidth = ref(450)
// The map is drawn at the panel's width, so follow the container (it comes and goes with the data).
let observer: ResizeObserver | null = null
const measure = () => {
  if (!vizEl.value) return
  width.value = mapWidth(vizEl.value.clientWidth)
  fullWidth.value = Math.max(250, Math.round(vizEl.value.clientWidth) || 450)
}
watch(vizEl, (el) => {
  observer?.disconnect()
  observer = null
  if (!el) return
  measure()
  observer = new ResizeObserver(measure)
  observer.observe(el)
}, { flush: 'post' })
onBeforeUnmount(() => observer?.disconnect())

// The time, read afresh each time it is needed: a socket update has to see its own moment, not the last
// tick's (a cached clock launches a new call's packet late and drops a just-ended dock's glow until the
// next tick). Reading `tick` is what re-runs the callers once a second.
const clock = (): number => { void tick.value; return Date.now() }

const headline = computed(() => {
  const current = run.value
  if (!current) return null
  return nowParts({
    entries: current.entries, summary: current.summary, now: clock(), waiting: waiting.value,
    waitingSince: waitingSince.value, tab: runName.value,
  })
})
const vitals = computed(() => {
  const summary = run.value?.summary
  if (!summary) return null
  const context = summary.contextPercent === null ? null : Math.round(summary.contextPercent)
  const calls = callCount(summary)
  return {
    context, ...contextLevel(context),
    spend: summary.costUsd !== null ? formatNzd(summary.costUsd, usdToNzdRate.value) : formatTokens(summary.tokens),
    spendNote: summary.costUsd !== null ? formatTokens(summary.tokens) : 'cost unknown',
    steps: summary.steps, calls, failed: summary.errors,
  }
})
const model = computed(() => {
  const current = run.value
  return deriveMap({
    W: width.value, entries: current?.entries ?? [], spans: current?.spans ?? [], summary: current?.summary ?? null,
    now: clock(), waiting: waiting.value, still: reduced.value,
  })
})
// The hull's name has to fit its box: the mockup's own short names, else the label or its first long word.
const HULL_NAMES: Record<string, string> = {
  orchestrator: 'PLANNING', code_review: 'REVIEW', security_review: 'SECURITY', final_architect: 'VERIFY',
}
const hullName = computed(() => {
  const current = run.value
  const key = current?.nodeKey ?? shownNode.value?.key ?? ''
  if (HULL_NAMES[key]) return HULL_NAMES[key]
  const label = (current && key.startsWith('worker_') ? `Worker ${workerRuns.value.indexOf(current) + 1}` : shownNode.value?.label ?? '').toUpperCase()
  if (label.length <= 11) return label
  const words = label.split(/\s+/)
  return words[0].length > 5 ? words[0] : words[1] ?? words[0]
})
const feed = computed(() => (run.value ? feedRows(run.value.entries, clock()) : []))
const mapLabel = computed(() => `Activity map for ${runName.value}`)

// The headline is also read out, but at most every five seconds and only when what the agent is doing
// changes (not for its timer), so a screen reader is not talking over a fast agent.
const announced = ref('')
let announcedAt = 0
let announceTimer: ReturnType<typeof setTimeout> | undefined
const spoken = computed(() => (headline.value ? `${headline.value.kind}: ${headline.value.l1}` : ''))
watch(spoken, (text) => {
  clearTimeout(announceTimer)
  const wait = Math.max(0, announcedAt + 5000 - Date.now())
  announceTimer = setTimeout(() => {
    announced.value = text
    announcedAt = Date.now()
  }, wait)
}, { immediate: true })
onBeforeUnmount(() => clearTimeout(announceTimer))

// The Map | Tape switch is remembered for this viewer.
const VIEW_KEY = 'agentflow.activityView'
const readView = (): 'map' | 'tape' => {
  try {
    return localStorage.getItem(VIEW_KEY) === 'tape' ? 'tape' : 'map'
  } catch {
    return 'map'
  }
}
const view = ref<'map' | 'tape'>(readView())
watch(view, (value) => {
  try {
    localStorage.setItem(VIEW_KEY, value)
  } catch { /* A private window or blocked storage: the choice just is not remembered. */ }
})

// When this run waited on the person: each of its questions, to the minute it was answered (open ones run on).
const waits = computed<Wait[]>(() => {
  const current = run.value
  if (!current) return []
  return activity.waitsFor(current, props.task.questions).flatMap((wait) => {
    const from = parseTimestamp(wait.from)
    if (from === null) return []
    return [{ from, to: wait.to === null ? null : parseTimestamp(wait.to) }]
  })
})
const runLabel = computed(() => `Whole run so far for ${runName.value}`)
const tapeLabel = computed(() => `Activity tape for ${runName.value}`)
const glyph = (id: string) => stationInfo(id).icon
</script>

<template>
  <section class="act" aria-label="Agent activity">
    <header class="act-head">
      <HarnessLogo v-if="shownNode" class="act-logo" :harness="shownNode.effectiveConfig?.harness" :name="shownNode.effectiveConfig?.harnessDisplayName" />
      <div class="titles">
        <div class="trow">
          <span class="dtitle">{{ shownNode?.label ?? 'Activity' }}</span>
          <span v-if="shownNode" class="dchips">
            <span class="mono" title="Model">{{ modelChip ?? 'harness default' }}</span>
            <span v-if="shownNode.effectiveConfig?.reasoning" title="Reasoning">{{ shownNode.effectiveConfig.reasoning }}</span>
          </span>
        </div>
        <span v-if="subtitle && nodeRuns.length <= 1" class="dsub">{{ subtitle }}</span>
        <div v-if="nodeRuns.length > 1" class="act-tabs" role="group" aria-label="Runs of this agent">
          <button v-for="candidate in nodeRuns" :key="candidate.runId" type="button" :aria-pressed="candidate.runId === run?.runId"
            @click="pickRun(candidate.runId)">{{ tabLabel(candidate) }}</button>
        </div>
      </div>
      <label class="follow" title="Show whichever agent is running"><input v-model="follow" type="checkbox"> Follow</label>
    </header>

    <template v-if="hasData && headline && vitals && run">
      <div class="nowline" :class="headline.cls">
        <span class="g" aria-hidden="true">
          <svg viewBox="-10 -10 20 20"><path :d="glyph(headline.icon)" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </span>
        <div class="w">
          <span class="kind">{{ headline.kind }}</span>
          <span class="l1">{{ headline.l1 }}</span>
          <span class="l2" :class="{ bad: headline.bad }">{{ headline.l2 }}</span>
        </div>
        <div class="tm">{{ headline.tm }}<small>{{ headline.cap }}</small></div>
      </div>
      <p class="sr-only" aria-live="polite">{{ announced }}</p>

      <div class="vitals">
        <div class="vit">
          <span class="k">Context</span>
          <span class="v">{{ vitals.context === null ? '–' : `${vitals.context}%` }}</span>
          <template v-if="vitals.level !== 'ok'"><span class="s">{{ vitals.word }}</span></template>
          <span class="ctxm" :class="{ inline: vitals.level === 'ok' }">
            <i :class="vitals.level" :style="{ width: `${vitals.context ?? 0}%` }" /><em style="left: 70%" /><em style="left: 90%" />
          </span>
        </div>
        <div class="vit"><span class="k">Spend</span><span class="v">{{ vitals.spend }}</span><span class="s">{{ vitals.spendNote }}</span></div>
        <div class="vit">
          <span class="k">Steps</span><span class="v">{{ vitals.steps }}</span>
          <span class="s">{{ vitals.calls }} call{{ vitals.calls === 1 ? '' : 's' }}<template v-if="vitals.failed"> · <b>{{ vitals.failed }} failed</b></template></span>
        </div>
      </div>

      <div class="cap">
        <span>Activity</span>
        <span v-if="view === 'tape'" class="hintline">Hover a bar for the call</span>
        <div class="seg" role="group" aria-label="Activity view">
          <button type="button" :aria-pressed="view === 'map'" @click="view = 'map'">Map</button>
          <button type="button" :aria-pressed="view === 'tape'" @click="view = 'tape'">Tape</button>
        </div>
      </div>
      <div ref="vizEl" class="viz">
        <ActivityMap v-if="view === 'map'" :model="model" :name="hullName" :model-name="shortModel(modelChip)" :label="mapLabel" />
        <ActivityTape v-else :W="fullWidth" :entries="run.entries" :spans="run.spans" :summary="run.summary" :waits="waits"
          :waiting="waiting" :still="reduced" :clock="clock" :label="tapeLabel" />
      </div>

      <div class="cap"><span>Run so far</span><span class="sub">every call by kind, context % beneath</span></div>
      <div class="viz">
        <ActivityRun :W="fullWidth" :entries="run.entries" :spans="run.spans" :context-steps="run.contextSteps" :summary="run.summary"
          :waits="waits" :waiting="waiting" :window="view === 'tape'" :clock="clock" :label="runLabel" />
      </div>

      <div class="cap"><span>Latest calls</span></div>
      <ul class="feed" aria-label="Latest tool calls">
        <li v-for="row in feed" :key="row.entry.id" :class="row.cls">
          <span>{{ row.mark }}</span>
          <span>{{ stationInfo(row.entry.station).name }}</span>
          <span class="lb">{{ row.entry.label }}<template v-if="row.times > 1">  +{{ row.times - 1 }}</template></span>
          <span class="tk">{{ row.took }}</span>
        </li>
        <li v-if="!feed.length" class="empty">No tool calls yet.</li>
      </ul>
    </template>
    <div v-else class="act-empty">
      <p class="act-empty__title">No live activity for this run</p>
      <p class="muted small">
        What an agent is doing shows here while it works: the calls it makes, its context and its spend.
        <template v-if="shownNode && LIVE_STATES.includes(shownNode.state)">This agent's transcript is not available.</template>
        <template v-else>Nothing is running right now.</template>
      </p>
    </div>
  </section>
</template>

<style scoped>
.act { --pad: 1.15rem; padding: 0.8rem var(--pad) 1rem; display: flex; flex-direction: column; gap: 0.45rem; }
.act > * { flex: none; }
.act-head { display: flex; align-items: center; gap: 0.6rem; min-width: 0; }
.act-head .titles { display: flex; flex-direction: column; min-width: 0; flex: 1; }
.act-head .trow { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; }
:root:not([data-theme="dark"]) .act-logo { box-shadow: inset 0 0 0 1px var(--border); }
.dtitle { font-weight: 600; font-size: 15px; }
.dsub { color: var(--muted); font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dchips { display: inline-flex; flex-wrap: wrap; gap: 4px; }
.dchips span { font-size: 11px; color: var(--muted); border: 1px solid var(--border); background: var(--raised); border-radius: 999px; padding: 1px 8px; }
.dchips .mono { font-family: var(--mono); }
.follow { display: inline-flex; align-items: center; gap: 0.35rem; color: var(--muted); font-size: 12.5px; flex: none; cursor: pointer; border-radius: 6px; }
.follow input { accent-color: var(--accent); margin: 0; }
.act-tabs { display: flex; gap: 0.3rem; overflow-x: auto; scrollbar-width: none; margin-top: 0.25rem; }
.act-tabs::-webkit-scrollbar { display: none; }
.act-tabs button { flex: none; padding: 0.12rem 0.6rem; font-size: 11.5px; color: var(--muted); white-space: nowrap; }
.act-tabs button[aria-pressed="true"] { border-color: var(--accent-border); color: var(--accent-strong); background: var(--accent-soft); }

.nowline { --tone: var(--border-strong); display: grid; grid-template-columns: 34px minmax(0, 1fr) auto; gap: 0.7rem; align-items: center; padding: 0.4rem 0.8rem 0.4rem 0.5rem; border-radius: var(--radius-md); background: var(--raised); border: 1px solid var(--border); min-height: 50px; }
.nowline .g { width: 34px; height: 34px; border-radius: 11px; display: grid; place-items: center; border: 1.5px solid var(--tone); color: var(--tone); background: var(--panel); }
.nowline .g svg { width: 19px; height: 19px; display: block; }
.nowline .w { min-width: 0; display: flex; flex-direction: column; }
.nowline .kind { font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); font-weight: 600; }
.nowline .l1 { font-size: 13.5px; font-weight: 600; line-height: 1.25; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.nowline .l2 { font-size: 11.5px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.nowline .l2.bad { color: var(--danger); }
.nowline .tm { font-family: var(--mono); font-size: 19px; font-variant-numeric: tabular-nums; text-align: right; line-height: 1.1; white-space: nowrap; }
.nowline .tm small { display: block; font-family: var(--font); font-size: 10px; color: var(--muted); margin-top: 2px; }
.nowline.is-run { --tone: var(--live); }
.nowline.is-flow { --tone: var(--accent); }
.nowline.is-think { --tone: var(--muted); }
.nowline.is-wait { --tone: var(--warn-hot); border-color: var(--warn-border); background: var(--warn-soft); }
.nowline.is-done { --tone: var(--ok); }

.vitals { display: grid; grid-template-columns: 1.15fr 1fr 1fr; }
.vit { min-width: 0; padding: 0 0.7rem; border-left: 1px solid var(--border); display: grid; grid-template-columns: auto minmax(0, 1fr); column-gap: 0.5rem; align-items: baseline; }
.vit:first-child { padding-left: 0; border-left: 0; }
.vit .k { grid-column: 1 / -1; font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); font-weight: 600; }
.vit .v { font-family: var(--mono); font-size: 14px; font-variant-numeric: tabular-nums; }
.vit .s { font-family: var(--mono); font-size: 10.5px; color: var(--muted); font-variant-numeric: tabular-nums; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vit .s b { color: var(--danger); font-weight: 500; }
.vit .ctxm { grid-column: 1 / -1; position: relative; height: 4px; border-radius: 3px; background: var(--border); margin: 3px 0 0; }
.vit .ctxm.inline { grid-column: 2; align-self: center; margin: 0; }
.ctxm i { position: absolute; left: 0; top: 0; bottom: 0; border-radius: 3px; background: var(--muted); }
.ctxm i.warn { background: var(--warn); }
.ctxm i.crit { background: var(--danger); }
.ctxm em { position: absolute; top: -2px; bottom: -2px; width: 1px; background: var(--border-strong); }

.cap { display: flex; justify-content: space-between; align-items: center; gap: 0.75rem; font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); font-weight: 600; min-height: 24px; }
.seg { display: inline-flex; border: 1px solid var(--border); border-radius: var(--radius-pill); overflow: hidden; flex: none; }
.seg button { border: 0; border-radius: 0; padding: 0.12rem 0.7rem; font-size: 11.5px; letter-spacing: 0; text-transform: none; background: transparent; color: var(--muted); }
.seg button:hover:not(:disabled) { transform: none; }
.seg button[aria-pressed="true"] { background: var(--accent-soft); color: var(--accent-strong); font-weight: 600; }
.viz { margin-inline: calc(var(--pad) * -1); min-width: 0; overflow: hidden; }
.viz :deep(svg) { display: block; margin: 0 auto; }
.cap .hintline, .cap .sub { letter-spacing: 0; text-transform: none; font-weight: 400; font-size: 11.5px; color: var(--dim); }
.cap .hintline { margin-right: auto; }

.feed { list-style: none; margin: 0; padding: 0; font-family: var(--mono); font-size: 12px; display: flex; flex-direction: column; gap: 3px; }
.feed li { display: grid; grid-template-columns: 1.4ch 10ch minmax(0, 1fr) auto; gap: 8px; color: var(--dim); }
.feed .lb { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); }
.feed .tk { font-variant-numeric: tabular-nums; }
.feed li.run .lb { color: var(--text); }
.feed li.run span:first-child { color: var(--live); }
.feed li.err, .feed li.err .lb { color: var(--danger); }
.feed li.empty { display: block; }

.act-empty { margin-top: 0.4rem; padding: 0.9rem 1rem; border-radius: var(--radius-md); background: var(--raised); }
.act-empty__title { margin: 0 0 0.3rem; font-weight: 600; }
.act-empty p.small { margin: 0; line-height: 1.5; }
</style>
