<script setup lang="ts">
import { computed } from 'vue'
import type { WorkflowNodeDto } from '@/api/types'
import { useActivityStore } from '@/stores/activity'
import ActivityGlyph from './ActivityGlyph.vue'

// The card's current-call line: the call the agent is on now (or "Between calls"), and once the
// node's runs are over, what they came to. It shows nothing when there is no activity data for
// the node (an unsupported harness, an older backend, a task that predates the map).
const props = defineProps<{ node: WorkflowNodeDto; taskKey: string }>()
const activity = useActivityStore()

const live = computed(() => ['RUNNING', 'WAITING_FOR_USER'].includes(props.node.state))
const finished = computed(() => ['COMPLETED', 'FAILED', 'INTERRUPTED'].includes(props.node.state))
const line = computed(() => activity.lineFor(props.taskKey, props.node.key, live.value, finished.value))
</script>

<template>
  <div v-if="line" class="wf-now" :class="{ dim: line.kind === 'text' && line.dim }" :title="line.kind === 'call' ? 'Current call' : undefined">
    <template v-if="line.kind === 'call'">
      <ActivityGlyph :station="line.station" />
      <span class="lb">{{ line.label }}</span>
    </template>
    <span v-else class="lb">{{ line.text }}</span>
  </div>
</template>
