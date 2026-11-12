<script setup lang="ts">
import { computed } from 'vue'
import type { ActivityContextStep, ActivityEntry, ActivitySpan, ActivitySummary } from '@/api/types'
import { deriveRun, RUN_H } from '@/activity/runStrip'
import type { Wait } from '@/activity/tapeModel'

// Run so far (spec §3, item 5): the whole session squeezed into one strip, one thin row per kind, with the
// context window's fill beneath. It reads the run's spans and context steps (the whole run, from the GET and
// kept up by the websocket), not just the newest calls.
const props = defineProps<{
  W: number
  entries: ActivityEntry[]
  spans: ActivitySpan[]
  contextSteps: ActivityContextStep[]
  summary: ActivitySummary | null
  waits: Wait[]
  waiting: boolean
  /** Mark the stretch the tape shows: on while the tape is the view. */
  window: boolean
  clock: () => number
  label: string
}>()

const model = computed(() => deriveRun({
  W: props.W, entries: props.entries, spans: props.spans, contextSteps: props.contextSteps, summary: props.summary,
  now: props.clock(), waits: props.waits, waiting: props.waiting, window: props.window,
}))
const TONE: Record<string, string> = {
  live: 'var(--act-live)', accent: 'var(--act-accent)', warn: 'var(--act-warn)', think: 'var(--act-think)', red: 'var(--act-red)',
}
</script>

<template>
  <svg class="run" :viewBox="`0 0 ${W} ${RUN_H}`" :width="W" :height="RUN_H" role="img" :aria-label="label">
    <rect class="r-ground" :width="W" :height="RUN_H" />
    <rect class="r-block" :x="model.block.x" :y="model.block.y" :width="model.block.w" :height="model.block.h" rx="5" />
    <line class="r-base" :x1="model.baseline.x1" :x2="model.baseline.x2" :y1="model.baseline.y" :y2="model.baseline.y" />
    <line class="r-guide" :x1="model.guide.x1" :x2="model.guide.x2" :y1="model.guide.y" :y2="model.guide.y" stroke-dasharray="2 4" />
    <text class="r-mono dim" :x="model.guide.x1" :y="model.guide.y - 3" font-size="8">70%</text>
    <template v-if="model.started">
      <rect v-for="wash in model.washes" :key="`w-${wash.key}`" class="r-wash" :x="wash.x" :y="model.block.y + 1" :width="wash.w" :height="model.block.h - 2" />
      <rect v-for="tick in model.ticks" :key="`t-${tick.key}`" class="r-tick" :x="tick.x" :y="model.block.y + 1" width="1.2" :height="model.block.h - 2" />
      <rect v-for="bar in model.bars" :key="bar.key" :x="bar.x" :y="bar.y" :width="bar.w" :height="model.rowH" rx="0.7"
        :fill="TONE[bar.tone]" :opacity="bar.sub ? 0.5 : 0.95" />
      <rect v-if="model.window" class="r-window" :x="model.window.x" :y="model.block.y" :width="model.window.w" :height="model.block.h" rx="5" />
      <g v-if="model.ctx" class="r-ctx" :class="model.ctx.level">
        <path class="r-area" :d="model.ctx.area" />
        <path class="r-line" :d="model.ctx.line" />
        <circle :cx="model.ctx.dot.x" :cy="model.ctx.dot.y" r="3.2" class="r-dot" />
        <text class="r-mono" :x="model.ctx.labelX" :y="model.ctx.labelY" :text-anchor="model.ctx.anchor" font-size="9.5">{{ model.ctx.label }}</text>
      </g>
      <line v-if="model.head" class="r-head" :x1="model.head.x" :x2="model.head.x" :y1="model.block.y" :y2="model.block.y + model.block.h" />
    </template>
    <text class="r-mono dim" :x="model.guide.x1" :y="RUN_H - 3" font-size="9">{{ model.axis.left }}</text>
    <text class="r-mono dim" :x="model.guide.x2" :y="RUN_H - 3" text-anchor="end" font-size="9">{{ model.axis.right }}</text>
  </svg>
</template>

<style scoped>
.run { display: block; overflow: hidden; font-family: var(--font); }
.r-ground { fill: var(--act-ground); }
.r-block { fill: var(--act-canvas); stroke: var(--act-line); }
.r-base { stroke: var(--act-line); }
.r-guide { stroke: var(--act-strong); }
.r-mono { font-family: var(--mono); fill: var(--act-ink); }
.r-mono.dim { fill: var(--act-dim); }
.r-wash { fill: var(--act-warn); opacity: 0.2; }
.r-tick { fill: var(--act-red); opacity: 0.8; }
.r-window { fill: var(--act-ink); fill-opacity: 0.04; stroke: var(--act-ash); stroke-width: 1.1; }
.r-ctx { --c: var(--act-ash); }
.r-ctx.warn { --c: var(--act-warn); }
.r-ctx.crit { --c: var(--act-red); }
.r-area { fill: var(--c); opacity: 0.12; }
.r-line { fill: none; stroke: var(--c); stroke-width: 1.6; stroke-linejoin: round; }
.r-dot { fill: var(--c); stroke: var(--act-ground); stroke-width: 1.5; }
.r-head { stroke: var(--act-accent); stroke-width: 1.2; }
</style>
