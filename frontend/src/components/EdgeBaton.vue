<script setup lang="ts">
import { computed } from 'vue'
import { BATON_S, chipWidth, HAND, LINGER, type Baton } from '@/workflow/batons'

// One baton on an edge (spec §7): a capsule (a fifth of the path) with a head runs along it carrying a payload
// chip, then the chip lingers at the target, rising and fading. Every part is a CSS animation with a negative
// delay equal to the baton's age, so a redraw in mid-flight carries on instead of starting over (spec §9). The
// age is read once per baton: changing a delay on a running animation would move it. Under reduced motion
// there is no movement, just the chip still at the edge's midpoint for the same time.
const props = defineProps<{ path: string; baton: Baton | null }>()

const ages = new Map<string, number>()
const view = computed(() => {
  const baton = props.baton
  if (!baton) return null
  if (!ages.has(baton.key)) ages.set(baton.key, Math.max(0, (Date.now() - baton.at) / 1000))
  for (const key of ages.keys()) if (key !== baton.key) ages.delete(key)
  const age = ages.get(baton.key)!
  if (age > BATON_S) return null
  const w = chipWidth(baton.text)
  return {
    key: baton.key, text: baton.text, w, loop: baton.loop,
    style: { '--bd': `${(-age).toFixed(3)}s`, '--hand': `${HAND}s`, '--linger': `${LINGER}s` },
    along: { 'offset-path': `path('${props.path}')` },
  }
})
</script>

<template>
  <g v-if="view" :key="view.key" class="bt" :class="{ loop: view.loop }" :style="view.style" aria-hidden="true">
    <path class="bt-seg" :d="path" pathLength="1" />
    <g class="bt-head" :style="view.along">
      <circle class="bt-dot" r="5.2" />
      <g class="bt-in">
        <rect class="bt-chip" :x="-view.w / 2" y="-29" :width="view.w" height="19" rx="9.5" />
        <text class="bt-text" x="0" y="-15.6" text-anchor="middle">{{ view.text }}</text>
      </g>
    </g>
    <g class="bt-linger" :style="{ ...view.along, 'offset-distance': '100%' }">
      <g class="bt-rise">
        <rect class="bt-chip" :x="-view.w / 2" y="-29" :width="view.w" height="19" rx="9.5" />
        <text class="bt-text" x="0" y="-15.6" text-anchor="middle">{{ view.text }}</text>
      </g>
    </g>
    <g class="bt-still" :style="{ ...view.along, 'offset-distance': '50%' }">
      <rect class="bt-chip" :x="-view.w / 2" y="-29" :width="view.w" height="19" rx="9.5" />
      <text class="bt-text" x="0" y="-15.6" text-anchor="middle">{{ view.text }}</text>
    </g>
  </g>
</template>

<style scoped>
.bt { --bt: var(--baton-hot); }
.bt.loop { --bt: var(--baton-warm); }
.bt-seg { fill: none; stroke: var(--bt); stroke-width: 4.6; stroke-linecap: round; stroke-dasharray: 0.2 2; stroke-dashoffset: 0.2;
  animation: bt-seg var(--hand) cubic-bezier(0.455, 0.03, 0.515, 0.955) var(--bd) both, bt-gone var(--hand) steps(1, end) var(--bd) both; }
.bt-head { offset-rotate: 0deg; offset-distance: 0%;
  animation: bt-move var(--hand) cubic-bezier(0.455, 0.03, 0.515, 0.955) var(--bd) both, bt-gone var(--hand) steps(1, end) var(--bd) both; }
.bt-dot { fill: var(--bt); stroke: var(--baton-ring); stroke-width: 1.6; }
.bt-in { animation: bt-in calc(var(--hand) * 0.15) linear var(--bd) both; }
.bt-chip { fill: var(--baton-card); stroke: var(--bt); stroke-width: 1.3; }
.bt-text { fill: var(--baton-ink); font-family: var(--mono); font-size: 10.5px; }
/* The chip left at the target starts when the capsule arrives; before that it is not there. */
.bt-linger { offset-rotate: 0deg; opacity: 0; animation: bt-keep var(--linger) linear calc(var(--hand) + var(--bd)) forwards; }
.bt-rise { animation: bt-rise var(--linger) cubic-bezier(0.25, 0.46, 0.45, 0.94) calc(var(--hand) + var(--bd)) forwards; }
.bt-still { display: none; offset-rotate: 0deg; }

@keyframes bt-seg { from { stroke-dashoffset: 0.2; } to { stroke-dashoffset: -0.8; } }
@keyframes bt-move { from { offset-distance: 0%; } to { offset-distance: 100%; } }
@keyframes bt-gone { from { opacity: 1; } to { opacity: 0; } }
@keyframes bt-in { from { opacity: 0; } to { opacity: 1; } }
@keyframes bt-keep { from { opacity: 1; } to { opacity: 1; } }
@keyframes bt-rise { from { transform: translateY(0); opacity: 1; } to { transform: translateY(-8px); opacity: 0; } }

@media (prefers-reduced-motion: reduce) {
  .bt-seg, .bt-head, .bt-linger { display: none; animation: none; }
  .bt-in, .bt-rise { animation: none; }
  .bt-still { display: block; }
}
</style>
