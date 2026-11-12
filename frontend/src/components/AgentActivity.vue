<script setup lang="ts">
import { computed } from 'vue'
import type { TaskSnapshot } from '@/api/types'
import { useActivityStore } from '@/stores/activity'
import ActivityGlyph from './ActivityGlyph.vue'

// The activity panel beside the canvas. This is its shell: the agent being shown and what it is
// doing right now, or "No live activity". The map, tape and Run so far come in later slices.
const props = defineProps<{ task: TaskSnapshot; selected: string | null }>()
const activity = useActivityStore()

const LIVE = ['RUNNING', 'WAITING_FOR_USER']
// The agent the person picked on the canvas, else the first one working.
const shown = computed(() => {
  const agents = props.task.nodes.filter((node) => node.kind === 'agent' && !node.worker)
  return agents.find((node) => node.key === props.selected) ?? agents.find((node) => LIVE.includes(node.state)) ?? null
})
const line = computed(() => {
  const node = shown.value
  if (!node) return null
  return activity.lineFor(props.task.jiraKey, node.key, LIVE.includes(node.state),
    ['COMPLETED', 'FAILED', 'INTERRUPTED'].includes(node.state))
})
</script>

<template>
  <section class="act-panel" aria-label="Agent activity">
    <header class="act-head">
      <h3>{{ shown?.label ?? 'Activity' }}</h3>
      <span v-if="shown" class="badge" :class="shown.displayState">{{ shown.displayState.replaceAll('_', ' ') }}</span>
    </header>
    <p v-if="line" class="act-now" :class="{ dim: line.kind === 'text' && line.dim }">
      <template v-if="line.kind === 'call'"><ActivityGlyph :station="line.station" /> {{ line.label }}</template>
      <template v-else>{{ line.text }}</template>
    </p>
    <div v-else class="act-empty">
      <p class="act-empty__title">No live activity</p>
      <p class="muted small">
        What an agent is doing shows here while it works: the calls it makes, its context and its spend.
        Nothing is running right now.
      </p>
    </div>
  </section>
</template>

<style scoped>
.act-panel { padding: 0.8rem 1.15rem 1rem; display: flex; flex-direction: column; gap: 0.6rem; }
.act-head { display: flex; align-items: center; gap: 0.6rem; }
.act-head h3 { margin: 0; font-size: 15px; font-weight: 600; }
.act-now { margin: 0; display: flex; align-items: center; gap: 0.4rem; font-family: var(--mono); font-size: 12px; }
.act-now.dim { color: var(--muted); }
.act-now :deep(.act-glyph) { flex: none; width: 15px; height: 15px; }
.act-empty { margin-top: 0.4rem; padding: 0.9rem 1rem; border-radius: var(--radius-md); background: var(--raised); }
.act-empty__title { margin: 0 0 0.3rem; font-weight: 600; }
.act-empty p.small { margin: 0; line-height: 1.5; }
</style>
