<script setup lang="ts">
import { computed, ref } from 'vue'
import type { WorkflowNodeDto } from '@/api/types'
import RunAgentButton from '@/components/RunAgentButton.vue';
import AgentHeartbeat from '@/components/AgentHeartbeat.vue';
import { useTaskStore } from '@/stores/tasks'
import { hasUsage, tokenRuns, usageDetail, useUsdToNzdRate } from '@/usage'
import HarnessLogo from './HarnessLogo.vue'
import QuestionIndicator from './QuestionIndicator.vue'

const props = defineProps<{ node: WorkflowNodeDto; taskKey: string }>()
const store = useTaskStore()
const usdToNzdRate = useUsdToNzdRate()
const busy = ref(false)
const error = ref<string | null>(null)

const isPending = computed(() => props.node.state === 'PENDING')
const canRun = computed(() => ['READY', 'FAILED'].includes(props.node.state))
const showIteration = computed(() => props.node.role !== 'architect' && props.node.iteration > 0)
const usage = computed(() => (hasUsage(props.node.usage) ? props.node.usage : null))
const usageTitle = computed(() => (usage.value ? usageDetail(usage.value, usdToNzdRate.value) : ''))

async function act(fn: () => Promise<unknown>) {
  busy.value = true
  error.value = null
  try {
    await fn()
  } catch (e) {
    error.value = (e as Error).message
  } finally {
    busy.value = false
  }
}

const openZellij = () => act(() => store.openZellij(props.taskKey, props.node.key))
const configure = () => window.dispatchEvent(new CustomEvent('agentflow:configure', { detail: props.node.key }))
</script>

<template>
  <div class="head">
    <HarnessLogo :harness="node.effectiveConfig?.harness" :name="node.effectiveConfig?.harnessDisplayName" />
    <div class="titles">
      <span class="title">{{ node.label }}</span>
      <span class="subtitle">
        {{ node.effectiveConfig?.harnessDisplayName ?? 'No harness' }}
        <span v-if="node.effectiveConfig && !node.effectiveConfig.harnessAvailable" class="badge FAILED">unavailable</span>
      </span>
    </div>
    <span class="badge" :class="node.displayState">{{ node.displayState.replaceAll('_', ' ') }}</span>
  </div>
  <AgentHeartbeat :node="node" />
  <QuestionIndicator v-if="node.state === 'WAITING_FOR_USER'" :count="node.openQuestions" />
  <div class="chips">
    <span class="chip mono" title="Model">{{ node.effectiveConfig?.model ?? 'harness default' }}</span>
    <span v-if="node.effectiveConfig?.reasoning" class="chip" title="Reasoning">{{ node.effectiveConfig.reasoning }}</span>
    <span v-if="showIteration" class="chip">iter {{ node.iteration }}</span>
    <span v-if="node.checkpointSha" class="chip mono" title="Checkpoint SHA">{{ node.checkpointSha.slice(0, 10) }}</span>
    <span v-if="node.findingsCount" class="chip" :class="{ warn: node.blockingCount }">
      {{ node.findingsCount }} finding{{ node.findingsCount === 1 ? '' : 's' }}<template v-if="node.blockingCount"> · {{ node.blockingCount }} blocking</template>
    </span>
    <span v-if="node.lastRun?.exitCode != null && node.state === 'FAILED'" class="chip danger">exit {{ node.lastRun.exitCode }}</span>
    <span v-if="usage && tokenRuns(usage)" class="chip" :title="usageTitle">
      {{ usage.totalTokens.toLocaleString() }} tok<span v-if="tokenRuns(usage) < usage.runs">*</span>
    </span>
  </div>
  <div v-if="node.message && node.state !== 'WAITING_FOR_USER'" class="message" :title="node.message">{{ node.message }}</div>
  <p v-if="node.effectiveConfig?.routingReason" class="small muted">{{ node.effectiveConfig.routingReason }}</p>
  <div v-if="error" class="alert error small" style="margin-top: 0.4rem">{{ error }}</div>
  <div class="actions nodrag">
    <RunAgentButton v-if="canRun" :task-key="taskKey" :node="node" />
    <button class="sm" :disabled="busy" @click="configure">Configure</button>
    <button class="sm" :disabled="busy || isPending" @click="openZellij">View pane</button>
  </div>
</template>
