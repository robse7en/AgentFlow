<script setup lang="ts">
import { computed } from 'vue'
import type { ActivityStation } from '@/api/types'
import { stationInfo } from '@/activity/stations'

// A station's glyph in its hot colour, as the activity map draws it.
const props = defineProps<{ station: ActivityStation }>()
const info = computed(() => stationInfo(props.station))
</script>

<template>
  <svg class="act-glyph" viewBox="-10 -10 20 20" aria-hidden="true">
    <path :d="info.icon" fill="none" :stroke="`var(--act-${info.tone}-hot)`" stroke-width="1.7"
      stroke-linecap="round" stroke-linejoin="round" />
  </svg>
</template>
