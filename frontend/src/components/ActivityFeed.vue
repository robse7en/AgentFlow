<script setup lang="ts">
import { computed } from 'vue'
import type { EventDto } from '@/api/types'

const props = defineProps<{ events: EventDto[] }>()
// The latest few events; the Events tab keeps the full log.
const LIMIT = 6
const latest = computed(() => props.events.slice(-LIMIT).reverse())
const time = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
</script>

<template>
  <section v-if="events.length" class="panel activity" aria-label="Live activity">
    <h2>Live activity</h2>
    <div v-for="e in latest" :key="e.id" class="activity-row">
      <span class="dot" :class="e.level" aria-hidden="true" />
      <span class="text">{{ e.message }}</span>
      <time :datetime="e.timestamp" :title="e.timestamp">{{ time(e.timestamp) }}</time>
    </div>
  </section>
</template>

<style scoped>
.activity { margin-top: 1rem; }
.activity h2 { margin: 0 0 0.5rem 0.85rem; font-size: 15px; }
.activity-row { display: flex; align-items: baseline; gap: 0.75rem; padding: 0.65rem 0.85rem; border-radius: var(--radius-md); transition: background 0.25s; }
.activity-row:hover { background: var(--hover); }
.dot { flex: none; width: 10px; height: 10px; margin: 0; border-radius: 50%; background: var(--accent); transform: translateY(1px); }
.dot.warning { background: var(--warn); }
.dot.error { background: var(--danger); }
.text { flex: 1; min-width: 0; font-size: 14px; line-height: 1.5; overflow-wrap: anywhere; }
time { flex: none; font-family: var(--mono); font-size: 12px; color: var(--muted); }
</style>
