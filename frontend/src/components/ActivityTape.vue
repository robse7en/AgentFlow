<script setup lang="ts">
import { computed, ref, useId } from 'vue'
import type { ActivityEntry, ActivitySpan, ActivitySummary } from '@/api/types'
import { stationInfo } from '@/activity/stations'
import { deriveTape, LANE, PPS, SCROLL_S, TAPE_H, type Wait } from '@/activity/tapeModel'

// The activity tape (spec §6): nine lanes, one pen each on the now-line, and the paper sliding away from
// the pens at 17 px a second. The paper is one group moved by one long CSS animation (no frame loop); the
// model redraws on the panel's one-second tick and whenever the data changes, and every one-shot effect is
// an animation whose start is moved back by its age, kept for as long as the effect lives (as on the map).
const props = defineProps<{
  W: number
  entries: ActivityEntry[]
  spans: ActivitySpan[]
  summary: ActivitySummary | null
  waits: Wait[]
  waiting: boolean
  still: boolean
  /** The time, read afresh each time it is needed (it is what re-runs this on the panel's tick). */
  clock: () => number
  label: string
}>()

const uid = useId()
const hover = ref<{ x: number; y: number } | null>(null)
const svgEl = ref<SVGSVGElement | null>(null)
const delays = new Map<string, number>()
/**
 * The paper's scroll animation: its epoch, whether it is frozen, and the delay it was given when that animation began
 * (kept, so a redraw never moves it). A frozen paper has no animation, so when it thaws (a finished run hands over to a
 * live one, or reduced motion goes off) a new one starts and needs a delay worked out afresh.
 */
let scroll: { epoch: number; frozen: boolean; delay: number } | null = null

const model = computed(() => deriveTape({
  W: props.W, entries: props.entries, spans: props.spans, summary: props.summary, now: props.clock(),
  waits: props.waits, waiting: props.waiting, still: props.still, hover: hover.value,
}))

const TONE: Record<string, [string, string]> = {
  live: ['var(--act-live)', 'var(--act-live-hot)'],
  accent: ['var(--act-accent)', 'var(--act-accent-hot)'],
  warn: ['var(--act-warn)', 'var(--act-warn-hot)'],
  think: ['var(--act-think)', 'var(--act-think-hot)'],
  red: ['var(--act-red)', 'var(--act-red-hot)'],
}
const toneStyle = (tone: string) => `--main:${TONE[tone][0]};--hot:${TONE[tone][1]}`

const view = computed(() => {
  const m = model.value
  const used = new Set<string>()
  const now = m.now
  // The effect's delay: negative once it has begun, positive while it is still to come.
  const delay = (key: string, at: number) => {
    used.add(key)
    let value = delays.get(key)
    if (value === undefined) {
      value = (at - now) / 1000
      delays.set(key, value)
    }
    return `animation-delay:${value.toFixed(3)}s`
  }
  const frozen = m.ended || props.still
  if (!scroll || scroll.epoch !== m.epoch || scroll.frozen !== frozen) scroll = { epoch: m.epoch, frozen, delay: -(m.now - m.epoch) / 1000 }
  const paperStyle = `--xn:${m.xNow}px;--t0:${m.shift.toFixed(2)}px;--travel:${SCROLL_S * PPS}px;animation-delay:${scroll.delay.toFixed(3)}s`

  const bars = m.bars.map((bar) => ({
    ...bar,
    style: toneStyle(bar.tone),
    fadeStyle: bar.endAt !== null ? delay(`bar:${bar.id}:fade`, bar.endAt) : '',
    rippleStyle: bar.rippleAt !== null ? delay(`bar:${bar.id}:rip`, bar.rippleAt) : '',
    rippleY: bar.cross ? bar.cross.y : 0,
    rippleX: bar.x + bar.w,
  }))
  const pens = m.pens.map((pen) => {
    const key = `pen:${pen.id}`
    return {
      ...pen,
      style: toneStyle(pen.tone),
      strike: pen.strikeAt !== null ? delay(`${key}:strike:${pen.strikeAt}`, pen.strikeAt) : '',
      ring: pen.ringAt !== null ? delay(`${key}:ring:${pen.ringAt}`, pen.ringAt) : '',
      shake: pen.shakeAt !== null ? delay(`${key}:shake:${pen.shakeAt}`, pen.shakeAt) : '',
      cool: pen.coolAt !== null ? delay(`${key}:cool:${pen.coolAt}`, pen.coolAt) : '',
      done: pen.doneAt !== null ? delay(`${key}:done:${pen.doneAt}`, pen.doneAt) : '',
      glyph: stationInfo(pen.id).icon,
      errX: m.right - String(pen.count).length * 6.4 - 12,
    }
  })
  for (const key of delays.keys()) if (!used.has(key)) delays.delete(key)
  return { bars, pens, paperStyle, frozen, paperKey: `${m.epoch}:${frozen}` }
})

function onMove(event: PointerEvent) {
  if (event.pointerType !== 'mouse' || !svgEl.value) return
  const box = svgEl.value.getBoundingClientRect()
  if (!box.width) return
  const scale = props.W / box.width
  hover.value = { x: (event.clientX - box.left) * scale, y: (event.clientY - box.top) * scale }
}
const onLeave = () => { hover.value = null }

const lanes = computed(() => Array.from({ length: 10 }, (_, i) => ({ y: model.value.top + i * LANE, edge: i === 0 || i === 9 })))
const hatchId = (tone: string) => `${uid}-h-${tone}`
const patternOf = (bar: { tone: string }) => `url(#${hatchId(bar.tone)})`
</script>

<template>
  <svg ref="svgEl" class="tp" :viewBox="`0 0 ${W} ${TAPE_H}`" :width="W" :height="TAPE_H" role="img" :aria-label="label"
    @pointermove="onMove" @pointerleave="onLeave">
    <defs>
      <clipPath :id="`${uid}-clip`"><rect x="0" y="0" :width="model.xNow" :height="TAPE_H" /></clipPath>
      <linearGradient :id="`${uid}-fade`" x1="0" x2="1" y1="0" y2="0">
        <stop offset="0" class="tp-fade-a" /><stop offset="1" class="tp-fade-b" />
      </linearGradient>
      <pattern v-for="tone in ['live', 'think', 'warn', 'accent', 'red']" :id="hatchId(tone)" :key="tone" width="6" height="6"
        patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
        <rect width="2" height="6" :class="`tp-hatch ${tone}`" />
      </pattern>
      <pattern :id="`${uid}-h-wait`" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
        <rect width="2" height="6" class="tp-hatch wait" />
      </pattern>
    </defs>

    <rect class="tp-ground" :width="W" :height="TAPE_H" />
    <rect class="tp-canvas" x="0" :y="model.top" :width="model.xNow" :height="model.bot - model.top" />
    <line v-for="lane in lanes" :key="lane.y" class="tp-lane" :class="{ edge: lane.edge }" x1="0" :x2="W" :y1="lane.y" :y2="lane.y" />

    <g :clip-path="`url(#${uid}-clip)`">
      <g :key="view.paperKey" class="tp-paper" :class="{ frozen: view.frozen }" :style="view.paperStyle">
        <rect v-if="model.veil !== null" class="tp-veil" :x="model.veil - 4000" :y="model.top" width="4000" :height="model.bot - model.top" />
        <g class="tp-grid">
          <template v-for="line in model.grid" :key="line.k">
            <line :x1="line.x" :x2="line.x" :y1="model.top" :y2="model.bot + 4" :class="{ first: line.k === 0 }" :stroke-dasharray="line.k === 0 ? undefined : '2 4'" />
            <text v-if="line.label" :x="line.x" :y="model.bot + 16" text-anchor="middle">{{ line.label }}</text>
          </template>
        </g>

        <!-- Under the bars: a subagent's stint, waits for the person, failures. -->
        <g v-for="win in model.windows" :key="`w-${win.id}`" class="tp-window">
          <rect :x="win.x" :y="model.top" :width="win.w" :height="model.bot - model.top" />
          <line :x1="win.x" :x2="win.x" :y1="model.top" :y2="model.bot" stroke-dasharray="1 3" />
          <line v-if="win.end" :x1="win.x + win.w" :x2="win.x + win.w" :y1="model.top" :y2="model.bot" stroke-dasharray="1 3" />
        </g>
        <g v-for="band in model.waitBands" :key="band.id" class="tp-wait">
          <rect class="tp-wait-wash" :x="band.x" :y="model.top" :width="band.w" :height="model.bot - model.top" />
          <svg :x="band.x" :y="model.top" :width="band.w" :height="model.bot - model.top" overflow="hidden">
            <g :class="{ march: band.open }"><rect x="-12" y="0" :width="band.w + 24" :height="model.bot - model.top" :fill="`url(#${uid}-h-wait)`" /></g>
          </svg>
        </g>
        <line v-for="line in model.failLines" :key="`f-${line.id}`" class="tp-fail-line" :x1="line.x" :x2="line.x" :y1="model.top" :y2="model.bot" stroke-dasharray="2 3" />

        <g v-for="bar in view.bars" :key="bar.id" class="tp-bar" :class="{ sub: bar.sub, hit: bar.hit }" :style="bar.style" :data-id="bar.id">
          <template v-if="bar.sub">
            <rect class="tp-sub-ground" :x="bar.x" :y="bar.y" :width="bar.w" :height="bar.h" :rx="bar.rx" />
            <rect class="tp-sub" :x="bar.x" :y="bar.y" :width="bar.w" :height="bar.h" :rx="bar.rx" :fill="patternOf(bar)" />
          </template>
          <template v-else>
            <rect v-if="bar.running" class="tp-glow" :x="bar.x - 2.5" :y="bar.y - 2.5" :width="bar.w + 5" :height="bar.h + 5" :rx="bar.rx + 2.5" />
            <rect class="tp-body" :class="[bar.ink, { fresh: bar.endAt !== null }]" :style="bar.fadeStyle" :x="bar.x" :y="bar.y" :width="bar.w" :height="bar.h" :rx="bar.rx" />
            <text v-if="bar.label" class="tp-bar-label" :class="{ run: bar.running }" :x="bar.labelX" :y="bar.labelY">{{ bar.label }}</text>
            <line v-if="bar.dots" class="tp-dots" :x1="bar.dots.x1" :x2="bar.dots.x2" :y1="bar.dots.y" :y2="bar.dots.y" stroke-dasharray="1.6 7" />
          </template>
          <template v-if="bar.cross">
            <path class="tp-cross" :d="`M${bar.cross.x - 3} ${bar.cross.y - 3}l6 6M${bar.cross.x + 3} ${bar.cross.y - 3}l-6 6`" />
            <circle v-if="bar.rippleAt !== null" class="tp-fail-rip" :cx="bar.rippleX" :cy="bar.rippleY" :style="bar.rippleStyle" />
          </template>
        </g>
        <path v-for="d in model.diamonds" :key="d.id" class="tp-diamond" :class="{ hit: d.hit }" :style="toneStyle(d.tone)"
          :d="`M${d.x} ${d.y - 6}l6 6l-6 6l-6 -6z`" />

        <!-- Flags over the lanes. -->
        <g class="tp-flags">
          <text v-if="model.startFlag !== null" class="dim" :x="model.startFlag" :y="model.top - 8">start</text>
          <template v-for="band in model.waitBands" :key="`wf-${band.id}`">
            <text v-if="band.flag" class="wait" :x="band.x + 3" :y="model.top - 8">{{ band.flag }}</text>
          </template>
          <template v-for="line in model.failLines" :key="`ff-${line.id}`">
            <path v-if="line.flag" class="red" :d="`M${line.x - 4.5} ${model.top - 9}h9l-4.5 6.5z`" />
            <text v-if="line.word" class="red" :x="line.x + 8" :y="model.top - 5">failed</text>
          </template>
        </g>
      </g>
    </g>
    <g class="tp-flags"><text v-for="flag in model.edgeFlags" :key="flag.id" class="wait" :x="flag.x" :y="model.top - 8">{{ flag.text }}</text></g>
    <rect class="tp-edge-fade" x="0" :y="model.top - 1" width="44" :height="model.bot - model.top + 2" :fill="`url(#${uid}-fade)`" />
    <text v-if="!model.started" class="tp-empty" :x="model.xNow / 2" :y="(model.top + model.bot) / 2 + 4" text-anchor="middle">No calls yet</text>

    <!-- The playhead and its nine pens. -->
    <line class="tp-head" :class="model.head.tone" :x1="model.xNow" :x2="model.xNow" :y1="model.top - 5" :y2="model.bot + 5" />
    <text class="tp-cap" :x="model.xNow" :y="model.top - 10" text-anchor="middle">{{ model.head.word }}</text>
    <text class="tp-cap dim" :x="model.right" :y="model.top - 10" text-anchor="end">CALLS</text>
    <text class="tp-clock" :x="model.xNow" :y="model.bot + 16" text-anchor="middle">{{ model.head.clock }}</text>

    <g v-for="pen in view.pens" :key="pen.id" class="tp-pen" :class="{ active: pen.active, used: pen.count > 0 }" :style="pen.style" :data-station="pen.id"
      :data-state="pen.held ? 'held' : pen.active ? 'active' : 'idle'">
      <g class="tp-shake" :class="{ shaking: pen.shakeAt !== null }" :style="pen.shake">
        <template v-if="pen.active">
          <circle class="tp-halo" :cx="model.xNow" :cy="pen.cy" r="14.5" />
          <circle v-if="pen.held" class="tp-hold" :cx="model.xNow" :cy="pen.cy" r="13" />
          <circle v-else class="tp-turn" :cx="model.xNow" :cy="pen.cy" r="13.4" stroke-dasharray="4 6.5" />
        </template>
        <circle v-if="pen.ringAt !== null" class="tp-ring" :cx="model.xNow" :cy="pen.cy" :style="pen.ring" />
        <circle v-if="pen.doneAt !== null" class="tp-done" :cx="model.xNow" :cy="pen.cy" :style="pen.done" />
        <g class="tp-strike" :class="{ striking: pen.strikeAt !== null }" :style="pen.strike">
          <circle class="tp-disc" :cx="model.xNow" :cy="pen.cy" r="10" />
          <circle v-if="pen.active" class="tp-cool solid" :cx="model.xNow" :cy="pen.cy" r="10" />
          <circle v-else-if="pen.coolAt !== null" class="tp-cool fading" :class="{ red: pen.coolRed }" :cx="model.xNow" :cy="pen.cy" r="10" :style="pen.cool" />
          <path class="tp-glyph" :class="pen.ink" :d="pen.glyph" :transform="`translate(${model.xNow} ${pen.cy}) scale(0.62)`" />
        </g>
      </g>

      <!-- Beside the pen: the kind, how many calls, and what is running or its share of the run. -->
      <text class="tp-name" :x="model.gx" :y="pen.nameY">{{ pen.name }}</text>
      <template v-if="pen.count > 0">
        <text class="tp-count" :x="model.right" :y="pen.cy - 2.5" text-anchor="end">{{ pen.count }}</text>
        <g v-if="pen.errors > 0" class="tp-err">
          <circle :cx="pen.errX" :cy="pen.cy - 6" r="6.2" />
          <text :x="pen.errX" :y="pen.cy - 2.9" text-anchor="middle">{{ pen.errors }}</text>
        </g>
      </template>
      <template v-if="pen.running">
        <text class="tp-run" :x="model.gx" :y="pen.textY">{{ pen.running.head }}{{ pen.running.label }}</text>
        <text class="tp-timer" :x="model.right" :y="pen.textY" text-anchor="end">{{ pen.running.timer }}</text>
      </template>
      <template v-else-if="pen.share">
        <rect class="tp-share-bg" :x="model.gx" :y="pen.cy + 4.5" :width="model.right - model.gx - 38" height="3" rx="1.5" />
        <rect class="tp-share" :x="model.gx" :y="pen.cy + 4.5" :width="Math.max(1.5, (model.right - model.gx - 38) * pen.share.frac)" height="3" rx="1.5" />
        <text class="tp-timer dim" :x="model.right" :y="pen.textY" text-anchor="end">{{ pen.share.busy }}</text>
      </template>
    </g>

    <g v-if="model.tip" class="tp-tip" pointer-events="none">
      <rect :x="model.tip.x" :y="model.tip.y" :width="model.tip.w" height="36" rx="8" />
      <text class="l1" :x="model.tip.x + 9" :y="model.tip.y + 15">{{ model.tip.l1 }}</text>
      <text class="l2" :x="model.tip.x + 9" :y="model.tip.y + 28">{{ model.tip.l2 }}</text>
    </g>
  </svg>
</template>

<style scoped>
.tp { display: block; overflow: hidden; font-family: var(--font); }
.tp-ground { fill: var(--act-ground); }
.tp-canvas { fill: var(--act-canvas); }
.tp-lane { stroke: var(--act-line); opacity: 0.5; }
.tp-lane.edge { opacity: 0.9; }
.tp-fade-a { stop-color: var(--act-ground); }
.tp-fade-b { stop-color: var(--act-ground); stop-opacity: 0; }
.tp-hatch { opacity: 0.55; }
.tp-hatch.live { fill: var(--act-live); }
.tp-hatch.think { fill: var(--act-think); opacity: 0.6; }
.tp-hatch.warn { fill: var(--act-warn); }
.tp-hatch.accent { fill: var(--act-accent); }
.tp-hatch.red { fill: var(--act-red); opacity: 0.6; }
.tp-hatch.wait { fill: var(--act-warn); opacity: 0.22; }

/* The paper: one long linear slide, started so that it is where the clock says. Past its end (never in a real
   run) it holds; a finished run, or reduced motion, holds it where the last redraw put it. */
.tp-paper { transform: translateX(var(--t0)); animation: tp-scroll 14400s linear forwards; }
.tp-paper.frozen { animation: none; }
@keyframes tp-scroll {
  from { transform: translateX(var(--xn)); }
  to { transform: translateX(calc(var(--xn) - var(--travel))); }
}
.tp-veil { fill: var(--act-ground); opacity: 0.7; }
.tp-grid line { stroke: var(--act-line); opacity: 0.8; }
.tp-grid line.first { opacity: 1; }
.tp-grid text { fill: var(--act-dim); font-family: var(--mono); font-size: 9px; }

.tp-window rect { fill: var(--act-live); opacity: var(--act-wash); }
.tp-window line { stroke: var(--act-live); opacity: 0.7; }
.tp-wait-wash { fill: var(--act-warn); opacity: calc(var(--act-wash) + 0.03); }
.tp-wait .march { animation: tp-wait-march 1.7s linear infinite; }
.tp-fail-line { stroke: var(--act-red); opacity: 0.7; }

.tp-body { fill: var(--main); opacity: var(--act-bar-idle); }
.tp-body.run { fill: var(--hot); opacity: 1; }
.tp-body.fail { opacity: 0.9; }
.tp-body.fading { fill: var(--main); animation: tp-idle 0.6s linear both; }
.tp-bar.hit .tp-body { stroke: var(--act-ink); stroke-width: 1.4; }
.tp-glow { fill: var(--hot); opacity: 0.2; }
.tp-bar-label { fill: var(--act-ink); font-family: var(--mono); font-size: 9px; opacity: 0.92; }
.tp-bar-label.run { fill: var(--act-ground); opacity: 1; }
.tp-dots { stroke: var(--act-ground); stroke-width: 1.8; stroke-linecap: round; opacity: 0.6; animation: tp-march 0.478s linear infinite; }
.tp-sub-ground { fill: var(--act-canvas); }
.tp-sub { stroke: var(--main); stroke-width: 1.1; }
.tp-bar.hit .tp-sub { stroke: var(--act-ink); stroke-width: 1.4; }
.tp-cross { fill: none; stroke: var(--act-red); stroke-width: 1.8; stroke-linecap: round; }
.tp-fail-rip { fill: none; stroke: var(--act-red); stroke-width: 1.5; opacity: 0; animation: tp-fail-rip 0.9s linear both; }
.tp-diamond { fill: var(--act-canvas); stroke: var(--hot); stroke-width: 1.4; }
.tp-diamond.hit { stroke: var(--act-ink); }

.tp-flags text { font-size: 9.5px; font-weight: 600; }
.tp-flags text.dim { fill: var(--act-dim); font-weight: 400; }
.tp-flags text.wait { fill: var(--act-wait-ink); }
.tp-flags text.red, .tp-flags path.red { fill: var(--act-red); }
.tp-empty { fill: var(--act-dim); font-family: var(--mono); font-size: 11px; }

.tp-head { stroke-width: 1.3; opacity: 0.75; stroke: var(--act-strong); }
.tp-head.accent { stroke: var(--act-accent); }
.tp-head.ok { stroke: var(--act-ok); }
.tp-cap { fill: var(--act-ash); font-size: 9.5px; font-weight: 600; letter-spacing: 0.06em; }
.tp-cap.dim { fill: var(--act-dim); }
.tp-clock { fill: var(--act-ink); font-family: var(--mono); font-size: 9.5px; }

/* pens */
.tp-disc { fill: var(--act-raised); stroke: var(--act-line); stroke-width: 1.2; }
.tp-halo { fill: var(--hot); opacity: 0.16; }
.tp-turn { fill: none; stroke: var(--hot); stroke-width: 1.4; stroke-linecap: round; transform-box: fill-box; transform-origin: center; animation: tp-turn 3.27s linear infinite; }
.tp-hold { fill: none; stroke: var(--hot); stroke-width: 1.3; animation: tp-hold 1.2s ease-in-out infinite alternate; }
.tp-cool { fill: none; stroke: var(--main); stroke-width: 1.3; }
.tp-cool.solid { stroke: var(--main); stroke-width: 1.7; }
.tp-cool.red { stroke: var(--act-red); }
.tp-cool.fading { animation: tp-cool 1.2s linear forwards; }
.tp-glyph { fill: none; stroke-width: 1.9; stroke-linecap: round; stroke-linejoin: round; }
.tp-glyph.hot { stroke: var(--hot); }
.tp-glyph.ash { stroke: var(--act-ash); }
.tp-glyph.dim { stroke: var(--act-dim); }
.tp-ring { fill: none; stroke: var(--hot); stroke-width: 1.4; opacity: 0; animation: tp-ring-r 0.5s cubic-bezier(0.5, 1, 0.89, 1) both, tp-ring-o 0.5s linear both; }
.tp-done { fill: none; stroke: var(--act-ok); stroke-width: 1.4; opacity: 0; animation: tp-done-r 0.6s cubic-bezier(0.5, 1, 0.89, 1) both, tp-done-o 0.6s linear both; }
.tp-strike, .tp-shake { transform-box: fill-box; transform-origin: center; }
.tp-strike.striking { animation: tp-strike 0.32s linear both; }
.tp-shake.shaking { animation: tp-shake 0.9s linear both; }

/* the gutter */
.tp-name { fill: var(--act-dim); font-size: 11.5px; font-weight: 500; }
.tp-pen.used .tp-name { fill: var(--act-ash); }
.tp-pen.active .tp-name { fill: var(--act-ink); font-weight: 600; }
.tp-count { fill: var(--act-ash); font-family: var(--mono); font-size: 10.5px; }
.tp-pen.active .tp-count { fill: var(--act-ink); }
.tp-err circle { fill: var(--act-red); }
.tp-err text { fill: var(--act-ground); font-size: 8.5px; font-weight: 700; }
.tp-run { fill: var(--act-ink); font-family: var(--mono); font-size: 9px; }
.tp-timer { fill: var(--act-ash); font-family: var(--mono); font-size: 9px; }
.tp-timer.dim { fill: var(--act-dim); }
.tp-share-bg { fill: var(--act-line); }
.tp-share { fill: var(--main); }

.tp-tip rect { fill: var(--act-raised); stroke: var(--act-strong); }
.tp-tip .l1 { fill: var(--act-ink); font-size: 10.5px; font-weight: 600; }
.tp-tip .l2 { fill: var(--act-ash); font-family: var(--mono); font-size: 9px; }

@keyframes tp-march { to { stroke-dashoffset: -8.6; } }
@keyframes tp-wait-march { to { transform: translateX(8.485px); } }
@keyframes tp-turn { to { transform: rotate(360deg); } }
@keyframes tp-hold { from { r: 13px; opacity: 0.9; } to { r: 16px; opacity: 0.3; } }
@keyframes tp-idle { from { opacity: 1; } to { opacity: var(--act-bar-idle); } }
@keyframes tp-cool { from { opacity: 1; } to { opacity: 0; } }
@keyframes tp-ring-r { from { r: 10px; } to { r: 23px; } }
@keyframes tp-ring-o { from { opacity: 0.85; } to { opacity: 0; } }
@keyframes tp-done-r { from { r: 10px; } to { r: 19px; } }
@keyframes tp-done-o { from { opacity: 0.8; } to { opacity: 0; } }
@keyframes tp-fail-rip { from { r: 6px; opacity: 0.8; } to { r: 28px; opacity: 0; } }
@keyframes tp-strike { 0%, 100% { transform: scale(1); } 25%, 75% { transform: scale(1.212); } 50% { transform: scale(1.3); } }
@keyframes tp-shake {
  0% { transform: translateX(0); } 2% { transform: translateX(2.17px); } 4% { transform: translateX(2.71px); } 6% { transform: translateX(1.53px); }
  8% { transform: translateX(-0.39px); } 10% { transform: translateX(-1.79px); } 12% { transform: translateX(-1.9px); } 14% { transform: translateX(-0.85px); }
  17% { transform: translateX(1.1px); } 20% { transform: translateX(1.3px); } 23% { transform: translateX(-0.12px); } 26% { transform: translateX(-1.09px); }
  30% { transform: translateX(-0.14px); } 35% { transform: translateX(0.73px); } 40% { transform: translateX(-0.48px); } 46% { transform: translateX(0.1px); }
  60% { transform: translateX(-0.08px); } 80% { transform: translateX(0.12px); } 100% { transform: translateX(0); }
}

/* Reduced motion: no strike, ring, ripple, marching or slide. Every state stays; the paper steps once a second. */
@media (prefers-reduced-motion: reduce) {
  .tp-paper { animation: none; }
  .tp-wait .march, .tp-dots, .tp-turn, .tp-hold { animation: none; }
  .tp-ring, .tp-done, .tp-fail-rip { display: none; }
  .tp-strike.striking, .tp-shake.shaking { animation: none; }
  .tp-body.fading { animation: none; opacity: var(--act-bar-idle); }
  .tp-cool.fading { animation-timing-function: steps(4, end); }
}
</style>
