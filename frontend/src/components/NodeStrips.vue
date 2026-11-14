<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import type { WorkflowNodeDto } from '@/api/types'
import { deriveStrips, STRIP_H } from '@/activity/strips'
import { useNow } from '@/now'
import { useActivityStore } from '@/stores/activity'

// The card's mini strip: the node's calls, one thin track per worker, squeezed to fit, with a dot for "now"
// while it runs (spec §7). Like the current-call line it draws nothing when there is no activity data for
// the node. It has its own clock, so only the strip redraws each second.
const props = defineProps<{ node: WorkflowNodeDto; taskKey: string }>()
const activity = useActivityStore()

/**
 * The strip is drawn at the width it is shown at, so the dot stays round. A card's content is a little
 * narrower while it runs (its border is thicker) than after, so it is measured, not assumed.
 */
const el = ref<HTMLElement | null>(null)
const W = ref(230)
let watcher: ResizeObserver | null = null
watch(el, (box) => {
  watcher?.disconnect()
  if (!box) return
  W.value = Math.round(box.clientWidth) || W.value
  watcher = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => { W.value = Math.round(box.clientWidth) || W.value })
  watcher?.observe(box)
})
onBeforeUnmount(() => watcher?.disconnect())
const TONE: Record<string, string> = {
  live: 'var(--act-live)', accent: 'var(--act-accent)', warn: 'var(--act-warn)', think: 'var(--act-think)', red: 'var(--act-red)',
}

const round = computed(() => activity.roundFor(props.taskKey, props.node.key).filter((run) => run.summary.available))
const going = computed(() => round.value.some((run) => !run.summary.ended) || props.node.state === 'WAITING_FOR_USER')
const now = useNow(() => going.value)
const model = computed(() => deriveStrips({
  W: W.value, runs: round.value, now: going.value ? now.value : Date.now(), waiting: props.node.state === 'WAITING_FOR_USER',
}))
</script>

<template>
  <div v-if="round.length" ref="el" class="wf-strips" aria-hidden="true">
    <svg :viewBox="`0 0 ${W} ${STRIP_H}`" preserveAspectRatio="none">
      <rect v-for="(track, row) in model.tracks" :key="`t${row}`" class="st-track" x="0" :y="track.y" :width="W" :height="track.h" :rx="track.h / 2" />
      <rect v-for="bar in model.bars" :key="bar.key" :x="bar.x" :y="bar.y" :width="bar.w" :height="bar.h" rx="0.8"
        :fill="TONE[bar.tone]" :opacity="bar.sub ? 0.45 : 0.95" />
      <circle v-for="(dot, i) in model.dots" :key="`d${i}`" class="st-dot" :cx="dot.x" :cy="dot.y" :r="dot.r" />
    </svg>
  </div>
</template>
