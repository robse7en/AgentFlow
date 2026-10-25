<script setup lang="ts">
import { computed, ref } from 'vue'
import type { WorkflowNodeDto } from '@/api/types'
import { useTaskStore } from '@/stores/tasks'
import { DRIFT_DISCARDED_NOTICE, useWorktreeDrift } from '@/drift'
import DriftDiscard from './DriftDiscard.vue'

const props = defineProps<{ node: WorkflowNodeDto; taskKey: string }>()
const store = useTaskStore()
const busy = ref(false)
const error = ref<string | null>(null)
const reason = ref('');
const blockers = computed(() => props.node.key === 'ready_for_pr'
    ? store.byKey(props.taskKey)?.blockers ?? [] : []);
const drift = useWorktreeDrift(() => props.taskKey, busy, error)
const needsReason = computed(() => blockers.value.length > 0 || drift.worktreeDrift.value)

async function approve() {
  busy.value = true
  drift.beginAction()
  try {
    await store.approveNode(props.taskKey, props.node.key, reason.value)
    drift.clearDrift()
  } catch (e) {
    drift.noteFailure(e)
  } finally {
    busy.value = false
  }
}
const review = () => window.dispatchEvent(new CustomEvent('agentflow:configure', { detail: props.node.key }))
</script>

<template>
  <div class="head">
    <span class="harness-logo gate" :title="node.kind === 'system' ? 'System step' : 'Human approval'">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path v-if="node.kind === 'system'" d="M4 6h16M4 12h16M4 18h10" />
        <template v-else><path d="M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6z" /><path d="M9 12l2 2 4-4" /></template>
      </svg>
    </span>
    <div class="titles">
      <span class="title">{{ node.label }}</span>
      <span class="subtitle">{{ node.kind === 'system' ? 'System step' : 'Human approval' }}</span>
    </div>
    <span class="badge" :class="node.state">{{ node.state.replaceAll('_', ' ') }}</span>
  </div>
  <div class="chips" v-if="node.kind === 'system' && node.iteration">
    <span class="chip">iter {{ node.iteration }}</span>
  </div>
  <div v-if="node.message" class="message" :title="node.message">{{ node.message }}</div>
  <p v-if="drift.discarded.value" class="muted small">{{ DRIFT_DISCARDED_NOTICE }} Click Approve to continue.</p>
  <div v-if="error" class="alert error small" style="margin-top: 0.4rem">
    {{ error }}
    <DriftDiscard v-if="drift.worktreeDrift.value" v-model:confirming="drift.confirmingDiscard.value" :busy="busy" @discard="drift.discardDrift" />
  </div>
  <div class="actions nodrag" v-if="node.state === 'WAITING_FOR_APPROVAL'">
    <label v-if="needsReason" class="field nodrag">
      <span>Reason for overriding {{ blockers.length ? `${blockers.length} blocker(s)` : 'worktree drift' }}</span>
      <textarea v-model="reason" maxlength="4000" required />
    </label>
    <button class="sm success" :disabled="busy || (needsReason && !reason.trim())" @click="approve">
      {{ needsReason ? 'Override & approve' : 'Approve' }}
    </button>
    <button class="sm" @click="review">Review</button>
  </div>
</template>
