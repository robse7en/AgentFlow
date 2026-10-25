<script setup lang="ts">
import { computed } from 'vue'
import type { NodeState, WorkflowNodeDto } from '@/api/types'
import { formatNzd } from '@/currency'
import { formatElapsed, parseTimestamp, useNow } from '@/now'
import { hasUsage, tokenRuns, usageDetail, useUsdToNzdRate } from '@/usage'

// The card's footer: how long the current (or last) run has taken, ticking while it runs, and
// what the node's runs have cost. Usage is only collected once a run exits, so while one is
// live the cost is the settled total of the earlier runs plus a "+ run N…" marker.
const LIVE_RUN_STATES: NodeState[] = ['RUNNING', 'WAITING_FOR_USER', 'WAITING_FOR_APPROVAL']

const props = defineProps<{ node: WorkflowNodeDto }>()
const usdToNzdRate = useUsdToNzdRate()

const run = computed(() => props.node.lastRun)
const live = computed(() => !!run.value && LIVE_RUN_STATES.includes(run.value.state))
const startedAt = computed(() => parseTimestamp(run.value?.startedAt))
const now = useNow(() => live.value && startedAt.value !== null)

const elapsed = computed(() => {
  if (startedAt.value === null) return null
  const end = live.value ? now.value : parseTimestamp(run.value?.completedAt)
  return end === null ? null : formatElapsed(end - startedAt.value)
})
const elapsedTitle = computed(() => {
  if (startedAt.value === null) return ''
  const started = `Run ${run.value!.runNumber} started ${new Date(startedAt.value).toLocaleString()}`
  return live.value ? started : `${started}, ${run.value!.state.toLowerCase()}`
})
// No run of its own yet: a coordinator shown as RUNNING (its workers are) isn't "not started".
const idleLabel = computed(() => (props.node.displayState === 'PENDING' ? 'not started' : '—'))

const usage = computed(() => (hasUsage(props.node.usage) ? props.node.usage : null))
const cost = computed(() => {
  const u = usage.value
  if (!u) return null
  if (u.costUsd !== null) return formatNzd(u.costUsd, usdToNzdRate.value) + (u.runsWithUsage < u.runs ? '*' : '')
  return tokenRuns(u) ? `${u.totalTokens.toLocaleString()} tok` : 'cost unknown'
})
const costTitle = computed(() => (usage.value ? usageDetail(usage.value, usdToNzdRate.value) : ''))
</script>

<template>
  <div class="wf-foot">
    <span v-if="elapsed !== null" class="elapsed" :title="elapsedTitle">
      <span v-if="live" class="live-dot" />
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" />
      </svg>
      {{ elapsed }}<span v-if="run!.runNumber > 1" class="dim"> · run {{ run!.runNumber }}</span>
    </span>
    <span v-else class="dim">{{ idleLabel }}</span>
    <span class="cost" :title="costTitle">
      <template v-if="cost">{{ cost }}</template>
      <span v-if="live" class="dim">{{ cost ? ` + run ${run!.runNumber}…` : 'cost after run' }}</span>
      <span v-else-if="!cost" class="dim">—</span>
    </span>
  </div>
</template>
