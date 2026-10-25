<script setup lang="ts">
import { computed } from 'vue'
import type { EventDto } from '@/api/types'

const props = defineProps<{ events: EventDto[] }>()
const newestFirst = computed(() => [...props.events].reverse())
const time = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
</script>

<template>
  <div class="event-log">
    <div v-for="e in newestFirst" :key="e.id" class="ev" :class="e.level">
      <time :title="e.timestamp">{{ time(e.timestamp) }}</time>
      <span>{{ e.message }}</span>
    </div>
    <p v-if="!events.length" class="muted small">No events yet.</p>
  </div>
</template>
