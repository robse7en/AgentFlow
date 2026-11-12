<script setup lang="ts">
import { computed } from 'vue'
import { hullPath, mapGeom, MAP_H, r1, type DockGeom, type Pt } from '@/activity/geometry'
import { ALARM, BACK, DRAW, GLOW, LAP, OUT, POD_OUT, type MapModel } from '@/activity/mapModel'
import { stationInfo } from '@/activity/stations'

// The hull-and-docks map (spec §4, §5). The model says what is true now; this draws it. One-shot
// effects (packets, ripples, shakes) are CSS animations whose start is moved back by the effect's
// age, and that delay is kept for as long as the effect lives, so a re-render in mid-flight carries
// on instead of restarting or jumping. Loops are infinite animations. Nothing here runs a frame loop.
const props = defineProps<{
  model: MapModel
  /** The agent's name for the hull, upper case. */
  name: string
  /** The model, under the name. */
  modelName: string
  label: string
}>()

const g = computed(() => mapGeom(props.model.W))
const ringScale = computed(() => (g.value.half + 6 + 34) / (g.value.half + 6))

// Each one-shot effect's delay, fixed when it is first drawn (see above). Pruned as effects expire.
const delays = new Map<string, number>()
const TONE: Record<string, [string, string]> = {
  live: ['var(--act-live)', 'var(--act-live-hot)'],
  accent: ['var(--act-accent)', 'var(--act-accent-hot)'],
  warn: ['var(--act-warn)', 'var(--act-warn-hot)'],
  think: ['var(--act-think)', 'var(--act-think-hot)'],
}
const toneStyle = (station: string) => {
  const [main, hot] = TONE[stationInfo(station).tone]
  return `--main:${main};--hot:${hot}`
}

interface Packet { key: string; d: string; cls: string; dash: string; style: string; hollow: boolean; shift: string }
interface Ripple { key: string; style: string }
interface DockView {
  id: string
  geom: DockGeom
  name: string
  cls: Record<string, boolean>
  style: string
  plate: { x: number; y: number; w: number }
  glyphD: string
  second: string
  nameX: number; nameY: number; nameAnchor: string; secondX: number; secondY: number
  fs: number
  glowStyle: string
  shakeStyle: string
  bumpStyle: string
  trace: number | null
  lapRing: boolean
  rings: { key: string; style: string }[]
  errors: number
  running: number
  model: MapModel['docks'][number]
}

const view = computed(() => {
  const m = props.model
  const geom = g.value
  const used = new Set<string>()
  // The effect's delay in seconds: negative once it has begun, positive while it is still to come.
  const delay = (key: string, at: number) => {
    used.add(key)
    let value = delays.get(key)
    if (value === undefined) {
      value = (at - m.now) / 1000
      delays.set(key, value)
    }
    return `${value.toFixed(3)}s`
  }
  const px = (v: number) => `${r1(v)}px`

  const packets: Packet[] = []
  const ripples: Ripple[] = []
  for (const e of m.effects) {
    const at = geom.at[e.station]
    const hot = toneStyle(e.station)
    const ad = `animation-delay:${delay(e.key, e.at)}`
    const shift = e.shift ? `translate(${at.side === 0 ? r1(e.shift) : 0} ${at.side === 0 ? 0 : r1(e.shift)})` : ''
    if (e.kind === 'out') packets.push({ key: e.key, d: geom.cable[e.station], cls: 'out', dash: '26 200', style: `${hot};--sw:5.2;${ad}`, hollow: false, shift })
    else if (e.kind === 'in') packets.push({ key: e.key, d: geom.cable[e.station], cls: 'in', dash: '26 200', style: `${hot};--sw:5.4;${ad}`, hollow: true, shift })
    else if (e.kind === 'back') {
      packets.push({ key: e.key, d: geom.cable[e.station], cls: 'back', dash: '22 200', shift,
        style: `${hot};--c:${e.bad ? 'var(--act-red)' : 'var(--act-ash)'};--sw:3.4;${ad}`, hollow: false })
    } else {
      // A square ripple: at the dock when a call lands, round the hull when a prompt lands, and where the
      // cable meets the hull when a result does.
      const box = (cx: number, cy: number, h0: number, r0: number, h1: number, r1_: number, o: number, color: string, sw: number) =>
        `--x0:${px(cx - h0)};--y0:${px(cy - h0)};--s0:${px(2 * h0)};--r0:${px(r0)};--x1:${px(cx - h1)};--y1:${px(cy - h1)};--s1:${px(2 * h1)};--r1:${px(r1_)};--o:${o};--c:${color};--sw:${sw};${ad}`
      if (e.kind === 'land') ripples.push({ key: e.key, style: box(at.sx, at.sy, 15, 9, 27, 16, 0.75, 'var(--hot)', 1.3) + ';' + hot })
      else if (e.kind === 'inland') ripples.push({ key: e.key, style: box(geom.cx, geom.cy, geom.reach, 26, geom.reach + 12, 32, 0.75, 'var(--hot)', 1.4) + ';' + hot })
      else ripples.push({ key: e.key, style: box(at.hub.x, at.hub.y, 3, 2.5, 13, 6.5, 0.85, e.bad ? 'var(--act-red)' : 'var(--main)', 1.4) + ';' + hot })
    }
  }

  const docks: DockView[] = m.docks.map((d) => {
    const A = geom.at[d.id]
    const info = stationInfo(d.id)
    const fs = geom.small ? 10 : 11
    const left = A.side < 0
    const tx = left ? A.x + 9 : A.sx + 20
    const you = A.side === 0
    const glowKey = `${d.id}:glow:${d.glowAt}`
    const glowDur = d.glow === 'alarm' ? ALARM : GLOW
    const fade = d.glow === 'alarm' || d.glow === 'afterglow'
    const rings = !d.waiting ? [] : [0, 0.5].map((lag) => ({
      key: `${d.id}:ring${lag}`,
      style: `--x0:${px(A.x)};--y0:${px(A.y - 20)};--w0:${px(A.w)};--h0:${px(40)};--r0:${px(13)};` +
        `--x1:${px(A.x - 22)};--y1:${px(A.y - 42)};--w1:${px(A.w + 44)};--h1:${px(84)};--r1:${px(35)};animation-delay:${-lag * 2}s`,
    }))
    return {
      id: d.id, geom: A, name: info.name,
      cls: {
        active: d.active, waiting: d.waiting, dimmed: d.dimmed, used: d.count > 0, you,
        alarm: d.glow === 'alarm', lit: d.glow !== 'none',
      },
      style: toneStyle(d.id),
      plate: { x: A.x, y: A.y - 20, w: A.w },
      glyphD: info.icon,
      second: d.second,
      nameX: you ? geom.cx - 23 : tx, nameY: you ? A.sy + 4 : d.second ? A.sy - 2.5 : A.sy + 4,
      nameAnchor: you ? 'end' : 'start', secondX: you ? geom.cx + 23 : tx, secondY: you ? A.sy + 3.5 : A.sy + 10.5, fs,
      glowStyle: fade ? `--dur:${glowDur}s;animation-delay:${delay(glowKey, d.glowAt)}` : '',
      shakeStyle: d.shakeAt !== null ? `animation-delay:${delay(`${d.id}:shake:${d.shakeAt}`, d.shakeAt)}` : '',
      bumpStyle: d.bumpAt !== null ? `animation-delay:${delay(`${d.id}:bump:${d.bumpAt}`, d.bumpAt)}` : '',
      trace: d.active ? (d.elapsedMs / 1000 % LAP) / LAP : null,
      lapRing: d.active && d.elapsedMs / 1000 >= LAP,
      rings,
      errors: d.errors, running: d.running, model: d,
    }
  })

  const legs = m.trail.map((leg) => ({
    ...leg,
    style: leg.newest ? `animation-delay:${delay(`trail:${leg.key}`, leg.at)}` : '',
    dotStyle: `animation-delay:${delay(`trail:${leg.key}:dot`, leg.at + DRAW * 900)}`,
  }))

  const pods = m.pods.map((pod) => {
    const home = geom.at.agents
    const dx = home.sx - pod.pos.x
    const dy = home.sy - pod.pos.y
    const outStyle = pod.startAt ? `animation-delay:${delay(`${pod.key}:out`, pod.startAt)}` : 'animation-delay:-5s'
    const backStyle = pod.endAt !== null ? `animation-delay:${delay(`${pod.key}:back`, pod.endAt)}` : ''
    const hops = pod.hops.map((hop) => {
      const age = (m.now - hop.outAt) / 1000
      const back = hop.endAt === null ? -1 : (m.now - hop.endAt) / 1000
      return {
        ...hop,
        style: toneStyle(hop.station),
        fadeStyle: hop.endAt === null ? '' : `animation-delay:${delay(`${hop.key}:hf`, hop.endAt)}`,
        out: age < OUT ? `${toneStyle(hop.station)};animation-delay:${delay(`${hop.key}:hout`, hop.outAt)}` : '',
        back: back >= 0 && back < BACK ? `animation-delay:${delay(`${hop.key}:hback`, hop.endAt!)}` : '',
      }
    })
    return {
      ...pod, dx: r1(dx), dy: r1(dy), outStyle, backStyle, hops, returning: pod.endAt !== null,
      style: `--dx:${px(dx)};--dy:${px(dy)}`,
      origin: `${r1(home.sx)}px ${r1(home.sy)}px`,
      labelStyle: pod.startAt ? `animation-delay:${delay(`${pod.key}:cap`, pod.startAt + POD_OUT * 1000)}` : '',
    }
  })

  const doneStyle = m.hull.doneRingAt !== null ? `--grow:${ringScale.value};animation-delay:${delay('hull:done', m.hull.doneRingAt)}` : ''
  for (const key of delays.keys()) if (!used.has(key)) delays.delete(key)
  return { packets, ripples, docks, legs, pods, doneStyle }
})

// Hull: sized to its name.
const hullSize = computed(() => {
  const len = Math.max(1, props.name.length)
  return Math.min(14, (g.value.hull - 18) / (0.66 * len))
})
const breathe = computed(() => {
  const h = props.model.hull
  const working = h.anyRun && !props.model.held
  return {
    on: h.started && h.rim !== 'done',
    period: working ? 1.6 : 4.8,
    lo: 0.04,
    hi: 0.04 + (h.anyRun ? 0.13 : 0.05),
    scale: (g.value.half + 11) / (g.value.half + 4),
  }
})
const stillRing = computed(() => {
  // Reduced motion: one still ring on the You plate while waiting.
  const you = g.value.at.you
  const u = 0.35
  const k = 22 * u
  return { x: you.x - k, y: you.y - 20 - k, w: you.w + 2 * k, h: 40 + 2 * k, r: 13 + k, o: 0.7 * (1 - u) }
})
const sqr = (x: number, y: number, half: number) => ({ x: r1(x - half), y: r1(y - half), s: r1(2 * half) })
const gaugeD = computed(() => hullPath(g.value.cx, g.value.cy, g.value.half + 6, 26))
const checkD = computed(() => `M${g.value.cx - 5} ${g.value.cy + 31}l3.4 3.4l6.4 -7`)
const podPos = (p: Pt) => `translate(${r1(p.x)} ${r1(p.y)})`
</script>

<template>
  <svg class="am" :viewBox="`0 0 ${model.W} ${MAP_H}`" :width="model.W" :height="MAP_H" role="img" :aria-label="label">
    <!-- Cables: dotted until used, worn in by use, a current while a call is out on them. -->
    <g class="am-under">
      <g v-for="dock in view.docks" :key="`cable-${dock.id}`" :class="{ dimmed: dock.cls.dimmed }" :style="dock.style">
        <path :d="g.cable[dock.geom.id]" class="am-cable" :class="{ used: dock.model.worn }" />
        <path v-if="dock.model.worn" :d="g.cable[dock.geom.id]" class="am-wear"
          :style="{ strokeWidth: 1.2 + 3.2 * dock.model.wear, opacity: 0.2 + 0.45 * dock.model.wear }" />
        <path v-if="dock.model.cableRunning" :d="g.cable[dock.geom.id]" class="am-current"
          :class="{ parked: dock.model.cableParked }" :stroke-dasharray="dock.model.cableParked ? '2 7' : '3 9'" />
      </g>
      <!-- The trail: the last docks the agent itself visited, joined in order. -->
      <g class="am-trail" :class="{ dimmed: model.held }">
        <template v-for="leg in view.legs" :key="leg.key">
          <path :d="leg.d" class="am-leg" :class="{ newest: leg.newest }" :style="leg.style" :opacity="leg.opacity" pathLength="100" />
          <circle v-if="leg.newest" :cx="r1(leg.end.x)" :cy="r1(leg.end.y)" r="2.3" class="am-dot" :style="leg.dotStyle" />
        </template>
      </g>
    </g>

    <!-- One-shot effects, under the docks: packets along the cables and ripples where they land. -->
    <g class="am-mid" aria-hidden="true">
      <template v-for="p in view.packets" :key="p.key">
        <path :d="p.d" class="am-pk" :class="p.cls" :style="p.style" :stroke-dasharray="p.dash" :transform="p.shift || undefined" pathLength="100" />
        <path v-if="p.hollow" :d="p.d" class="am-pk am-pk-core in" :style="p.style" :stroke-dasharray="p.dash" pathLength="100" />
      </template>
      <rect v-for="r in view.ripples" :key="r.key" class="am-rip" :style="r.style" />
    </g>

    <g class="am-docks">
      <g v-for="dock in view.docks" :key="dock.id" class="am-dock" :class="dock.cls" :style="dock.style" :data-station="dock.id"
        :data-glow="dock.model.glow">
        <template v-if="dock.cls.waiting">
          <rect v-for="ring in dock.rings" :key="ring.key" class="am-ring" :style="ring.style" />
          <rect class="am-ring still" :x="r1(stillRing.x)" :y="r1(stillRing.y)" :width="r1(stillRing.w)" :height="r1(stillRing.h)"
            :rx="r1(stillRing.r)" :opacity="stillRing.o" />
        </template>
        <rect class="am-plate" :x="dock.plate.x" :y="dock.plate.y" :width="dock.plate.w" height="40" rx="13" />
        <rect v-if="dock.model.glow !== 'none'" class="am-tint" :class="{ fading: dock.model.glow !== 'active' }" :x="dock.plate.x"
          :y="dock.plate.y" :width="dock.plate.w" height="40" rx="13" :style="dock.glowStyle" />
        <g class="am-shake" :class="{ shaking: dock.model.shakeAt !== null }" :style="dock.shakeStyle">
          <g class="am-bump" :class="{ bumping: dock.model.bumpAt !== null }" :style="dock.bumpStyle">
            <rect class="am-squircle" :x="sqr(dock.geom.sx, dock.geom.sy, 14).x" :y="sqr(dock.geom.sx, dock.geom.sy, 14).y" width="28" height="28" rx="9" />
            <rect v-if="dock.model.glow !== 'none'" class="am-sq-ring" :class="{ fading: dock.model.glow !== 'active' }"
              :x="sqr(dock.geom.sx, dock.geom.sy, 14).x" :y="sqr(dock.geom.sx, dock.geom.sy, 14).y" width="28" height="28" rx="9" :style="dock.glowStyle" />
            <path class="am-glyph" :d="dock.glyphD" :transform="`translate(${r1(dock.geom.sx)} ${dock.geom.sy}) scale(0.74)`" />
            <path v-if="dock.model.glow !== 'none'" class="am-glyph hot" :class="{ fading: dock.model.glow !== 'active' }" :d="dock.glyphD"
              :transform="`translate(${r1(dock.geom.sx)} ${dock.geom.sy}) scale(0.74)`" :style="dock.glowStyle" />
          </g>
          <template v-if="dock.trace !== null">
            <rect v-if="dock.lapRing" class="am-lap faint" :x="r1(dock.geom.sx - 17.5)" :y="r1(dock.geom.sy - 17.5)" width="35" height="35" rx="11.5" />
            <rect class="am-lap" :x="r1(dock.geom.sx - 17.5)" :y="r1(dock.geom.sy - 17.5)" width="35" height="35" rx="11.5" pathLength="1"
              :stroke-dasharray="`${dock.trace.toFixed(3)} 1`" />
          </template>
          <g v-if="dock.running > 1" class="am-badge par">
            <circle :cx="r1(dock.geom.sx + 13)" :cy="dock.geom.sy - 14" r="7.6" />
            <text :x="r1(dock.geom.sx + 13)" :y="dock.geom.sy - 11" text-anchor="middle">×{{ dock.running }}</text>
          </g>
          <g v-if="dock.errors > 0" class="am-badge err">
            <circle :cx="r1(dock.geom.sx - 13)" :cy="dock.geom.sy - 14" r="6.8" />
            <text :x="r1(dock.geom.sx - 13)" :y="dock.geom.sy - 10.9" text-anchor="middle">{{ dock.errors }}</text>
          </g>
        </g>
        <text class="am-name" :x="r1(dock.nameX)" :y="dock.nameY" :text-anchor="dock.nameAnchor" :font-size="dock.cls.you ? 12 : dock.fs">{{ dock.name }}</text>
        <text v-if="dock.second" class="am-second" :x="r1(dock.secondX)" :y="dock.secondY"
          :font-size="dock.cls.active ? (dock.cls.you ? 10.5 : 10) : (dock.cls.you ? 9.5 : 9)">{{ dock.second }}</text>
      </g>
    </g>

    <!-- The hull, with the context window as a gauge round its rim. -->
    <g class="am-hull" :class="`rim-${model.hull.rim}`">
      <rect v-if="breathe.on" class="am-halo" :class="{ held: model.held }" :x="g.cx - g.half - 4" :y="g.cy - g.half - 4"
        :width="2 * (g.half + 4)" :height="2 * (g.half + 4)" rx="24"
        :style="{ '--period': breathe.period + 's', '--lo': breathe.lo, '--hi': breathe.hi, '--mid': (breathe.lo + breathe.hi) / 2, '--grow': breathe.scale }" />
      <rect v-else class="am-halo quiet" :x="g.cx - g.half - 4" :y="g.cy - g.half - 4" :width="2 * (g.half + 4)" :height="2 * (g.half + 4)" rx="24" />
      <rect class="am-body" :x="g.cx - g.half" :y="g.cy - g.half" :width="2 * g.half" :height="2 * g.half" rx="22" />
      <path class="am-gauge-bg" :d="gaugeD" />
      <path v-if="model.hull.ctx !== null" class="am-gauge" :class="model.hull.ctxLevel" :d="gaugeD" pathLength="100"
        :stroke-dasharray="`${model.hull.ctx} 100`" />
      <text v-if="model.hull.ctx !== null" class="am-ctx" :x="g.cx" :y="g.cy - g.half + 17" text-anchor="middle">{{ model.hull.ctx }}% ctx</text>
      <text v-else-if="!model.hull.started" class="am-ctx dim" :x="g.cx" :y="g.cy - g.half + 17" text-anchor="middle">not started</text>
      <text class="am-core" :class="{ on: model.hull.started }" :x="g.cx" :y="g.cy + 5" text-anchor="middle" :font-size="r1(hullSize)"
        :letter-spacing="r1(hullSize * 0.1)">{{ name }}</text>
      <text v-if="model.held" class="am-sub wait" :x="g.cx" :y="g.cy + 20" text-anchor="middle">{{ g.small ? 'waiting' : 'waiting for you' }}</text>
      <text v-else class="am-sub" :x="g.cx" :y="g.cy + 20" text-anchor="middle">{{ modelName }}</text>
      <rect v-if="model.hull.doneRingAt !== null" class="am-done-ring" :x="g.cx - g.half - 6" :y="g.cy - g.half - 6"
        :width="2 * (g.half + 6)" :height="2 * (g.half + 6)" rx="26" :style="view.doneStyle" />
      <path v-if="model.hull.rim === 'done'" class="am-check" :d="checkD" />
    </g>

    <!-- Subagents: a pod undocks from Agents into the bay and makes its own hops. -->
    <g class="am-pods">
      <g v-for="pod in view.pods" :key="pod.key" class="am-pod" :class="{ returning: pod.returning }">
        <path v-for="hop in pod.hops" :key="`hop-${hop.key}`" :d="hop.d" class="am-hop" :class="{ ending: hop.endAt !== null }" :style="`${hop.style};${hop.fadeStyle}`" />
        <template v-for="hop in pod.hops" :key="`hp-${hop.key}`">
          <path v-if="hop.out" :d="hop.d" class="am-pk hop out" :style="hop.out" stroke-dasharray="20 200" pathLength="100" />
          <path v-if="hop.back" :d="hop.d" class="am-pk hop back" :style="`${hop.style};${hop.back}`" stroke-dasharray="18 200" pathLength="100" />
        </template>
        <line class="am-tether" :x1="g.at.agents.sx" :y1="g.at.agents.sy" :x2="r1(pod.pos.x)" :y2="r1(pod.pos.y)"
          :class="{ out: !pod.returning, back: pod.returning }" :style="`transform-origin:${pod.origin};${pod.returning ? pod.backStyle : pod.outStyle}`" />
        <g class="am-pod-move" :class="{ out: !pod.returning, back: pod.returning }" :style="`${pod.style};${pod.returning ? pod.backStyle : pod.outStyle}`">
          <g class="am-pod-bob">
            <rect class="am-pod-body" :x="r1(pod.pos.x - 12)" :y="r1(pod.pos.y - 12)" width="24" height="24" rx="8" />
            <path class="am-pod-glyph" :d="stationInfo('agents').icon" :transform="`${podPos(pod.pos)} scale(0.5)`" />
          </g>
        </g>
        <text class="am-pod-cap" :x="r1(pod.pos.x)" :y="r1(g.bay.y + 25)" text-anchor="middle" :style="pod.labelStyle">subagent</text>
      </g>
    </g>
  </svg>
</template>

<style scoped>
.am { display: block; overflow: visible; }
.am-under, .am-mid, .am-docks, .am-hull, .am-pods { fill: none; }
.dimmed { opacity: 0.4; }
.am-dock.dimmed { opacity: 0.45; }

/* cables */
.am-cable { stroke: var(--act-strong); stroke-width: 1.1; stroke-linecap: round; stroke-dasharray: 1.5 4.5; }
.am-cable.used { stroke-dasharray: none; opacity: 0.5; }
.am-wear { stroke: var(--main); stroke-linecap: round; }
.am-current { stroke: var(--hot); stroke-width: 1.9; stroke-linecap: round; animation: am-current 0.4s linear infinite; }
.am-current.parked { animation: none; }
.am-leg { stroke: var(--act-trail); stroke-width: 1.1; stroke-linecap: round; }
.am-leg.newest { stroke-width: 1.4; stroke-dasharray: 100 100; animation: am-draw 0.5s linear both; }
.am-dot { fill: var(--act-trail); opacity: 0; animation: am-appear 0.01s linear forwards; }

/* packets: a window of the cable, slid along it with the dash offset. */
.am-pk { fill: none; stroke: var(--c, var(--hot)); stroke-width: var(--sw, 4); stroke-linecap: round; opacity: 0; }
.am-pk.out { animation: am-out 0.5s cubic-bezier(0.5, 1, 0.89, 1) forwards; }
.am-pk.in { animation: am-back 0.5s cubic-bezier(0.5, 1, 0.89, 1) forwards; }
.am-pk-core { stroke: var(--act-ground); stroke-width: 2.4; }
.am-pk.back { animation: am-back 0.45s cubic-bezier(0.45, 0, 0.55, 1) forwards; }
.am-pk.hop.out { --sw: 3.6; stroke: var(--hot); animation-duration: 0.5s; }
.am-pk.hop.back { --sw: 2.6; --c: var(--act-ash); animation-duration: 0.45s; }
.am-rip { fill: none; stroke: var(--c); stroke-width: var(--sw); vector-effect: non-scaling-stroke; opacity: 0;
  animation: am-ripple 0.5s linear forwards; }
.am-ring { fill: none; stroke: var(--hot); stroke-width: 1.4; opacity: 0; animation: am-grow 2s linear infinite; }
.am-ring.still { display: none; opacity: 1; animation: none; }

/* docks */
.am-plate { fill: var(--act-raised); stroke: var(--act-line); stroke-width: 1; }
.am-dock.active .am-plate { stroke: var(--main); stroke-width: 1.4; }
.am-dock.alarm:not(.active) .am-plate { stroke: var(--act-line); }
.am-tint { fill: var(--main); opacity: 0.13; }
.am-dock.waiting .am-tint { fill: var(--hot); opacity: 0.2; }
.am-dock.alarm:not(.active) .am-tint { fill: var(--act-red); }
.am-tint.fading, .am-sq-ring.fading, .am-glyph.fading { animation: am-fade var(--dur, 2.6s) linear forwards; }
.am-tint.fading { --from: 0.13; }
.am-squircle { fill: var(--act-ground); stroke: var(--act-strong); stroke-width: 1.2; }
.am-sq-ring { fill: none; stroke: var(--main); stroke-width: 1.6; }
.am-dock.active .am-sq-ring { stroke: var(--hot); }
.am-dock.alarm:not(.active) .am-sq-ring { stroke: var(--act-red); }
.am-glyph { fill: none; stroke: var(--act-dim); stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
.am-dock.used .am-glyph { stroke: var(--act-ash); }
.am-glyph.hot { stroke: var(--hot); }
.am-dock.alarm:not(.active) .am-glyph.hot { stroke: var(--act-red); }
.am-lap { fill: none; stroke: var(--hot); stroke-width: 2.2; stroke-linecap: round; }
.am-lap.faint { stroke-width: 2; opacity: 0.35; }
.am-name { fill: var(--act-dim); font-weight: 500; }
.am-dock.used .am-name { fill: var(--act-ash); }
.am-dock.active .am-name { fill: var(--act-ink); font-weight: 600; }
.am-second { fill: var(--act-dim); font-family: var(--mono); }
.am-dock.active .am-second { fill: var(--act-ink); }
.am-badge.par circle { fill: var(--act-raised); stroke: var(--hot); stroke-width: 1.1; }
.am-badge.par text { fill: var(--act-ink); font-family: var(--mono); font-size: 8.5px; }
.am-badge.err circle { fill: var(--act-red); }
.am-badge.err text { fill: var(--act-ground); font-size: 9px; font-weight: 700; }
.am-shake, .am-bump { transform-box: fill-box; transform-origin: center; }
.am-shake.shaking { animation: am-shake 0.7s linear forwards; }
.am-bump.bumping { animation: am-bump 0.3s linear both; }

/* hull */
.am-halo { fill: var(--act-accent); transform-box: fill-box; transform-origin: center; opacity: 0.03; }
.am-halo.held { fill: var(--act-warn); }
.am-halo:not(.quiet) { opacity: var(--lo); animation: am-breathe calc(var(--period) / 2) ease-in-out infinite alternate; }
.am-body { fill: var(--act-raised); stroke: var(--act-line); stroke-width: 1.4; }
.rim-run .am-body { stroke: var(--act-accent); }
.rim-done .am-body { stroke: var(--act-ok); }
.rim-wait .am-body { stroke: var(--act-warn); stroke-dasharray: 4 4; }
.am-gauge-bg { fill: none; stroke: var(--act-line); stroke-width: 2.4; }
.am-gauge { fill: none; stroke: var(--act-ash); stroke-width: 2.4; stroke-linecap: round; }
.am-gauge.warn { stroke: var(--act-warn); }
.am-gauge.crit { stroke: var(--act-red); }
.am-ctx { fill: var(--act-ash); font-family: var(--mono); font-size: 8.5px; }
.am-ctx.dim { fill: var(--act-dim); }
.am-core { fill: var(--act-ash); font-family: var(--mono); font-weight: 500; }
.am-core.on { fill: var(--act-ink); }
.am-sub { fill: var(--act-ash); font-family: var(--mono); font-size: 8.5px; }
.am-sub.wait { fill: var(--act-wait-ink); font-family: var(--font); font-size: 9px; font-weight: 600; }
.am-done-ring { fill: none; stroke: var(--act-ok); stroke-width: 1.6; vector-effect: non-scaling-stroke; transform-box: fill-box;
  transform-origin: center; opacity: 0; animation: am-done 1.1s cubic-bezier(0.5, 1, 0.89, 1) forwards; }
.am-check { fill: none; stroke: var(--act-ok); stroke-width: 1.9; stroke-linecap: round; stroke-linejoin: round; }

/* pods */
.am-hop { fill: none; stroke: var(--main); stroke-width: 1.1; stroke-dasharray: 2 4; opacity: 0.8; }
.am-hop.ending { animation: am-hopfade 0.9s linear forwards; }
.am-tether { stroke: var(--act-live); stroke-width: 1; stroke-dasharray: 1.5 3.5; opacity: 0.7; vector-effect: non-scaling-stroke; }
.am-tether.out { animation: am-tether-out 0.45s cubic-bezier(0.5, 1, 0.89, 1) both; }
.am-tether.back { animation: am-tether-back 0.4s cubic-bezier(0.45, 0, 0.55, 1) forwards; }
.am-pod-move.out { animation: am-pod-out 0.45s cubic-bezier(0.5, 1, 0.89, 1) both; }
.am-pod-move.back { animation: am-pod-back 0.4s cubic-bezier(0.45, 0, 0.55, 1) forwards; }
.am-pod-bob { animation: am-bob 2.4s ease-in-out infinite; }
.am-pod-body { fill: var(--act-raised); stroke: var(--act-live-hot); stroke-width: 1.5; }
.am-pod-glyph { fill: none; stroke: var(--act-live-hot); stroke-width: 2.2; stroke-linecap: round; stroke-linejoin: round; }
.am-pod-cap { fill: var(--act-ash); font-family: var(--mono); font-size: 8.5px; opacity: 0; animation: am-appear 0.01s linear forwards; }

@keyframes am-current { to { stroke-dashoffset: -12; } }
@keyframes am-out {
  from { opacity: 1; stroke-dashoffset: 26; }
  99.9% { opacity: 1; stroke-dashoffset: -74; }
  to { opacity: 0; stroke-dashoffset: -74; }
}
@keyframes am-back {
  from { opacity: 1; stroke-dashoffset: -100; }
  99.9% { opacity: 1; stroke-dashoffset: 0; }
  to { opacity: 0; stroke-dashoffset: 0; }
}
@keyframes am-ripple {
  from { x: var(--x0); y: var(--y0); width: var(--s0); height: var(--s0); rx: var(--r0); opacity: var(--o); }
  to { x: var(--x1); y: var(--y1); width: var(--s1); height: var(--s1); rx: var(--r1); opacity: 0; }
}
@keyframes am-grow {
  from { x: var(--x0); y: var(--y0); width: var(--w0); height: var(--h0); rx: var(--r0); opacity: 0.7; }
  to { x: var(--x1); y: var(--y1); width: var(--w1); height: var(--h1); rx: var(--r1); opacity: 0; }
}
@keyframes am-fade { from { opacity: var(--from, 1); } to { opacity: 0; } }
@keyframes am-bump { 0%, 100% { transform: scale(1); } 25%, 75% { transform: scale(1.113); } 50% { transform: scale(1.16); } }
@keyframes am-shake {
  0% { transform: translateX(0); } 2.86% { transform: translateX(2.22px); } 5.71% { transform: translateX(2.31px); }
  8.57% { transform: translateX(0.57px); } 11.43% { transform: translateX(-1.29px); } 14.29% { transform: translateX(-1.81px); }
  17.14% { transform: translateX(-0.82px); } 20% { transform: translateX(0.63px); } 22.86% { transform: translateX(1.33px); }
  25.71% { transform: translateX(0.86px); } 28.57% { transform: translateX(-0.19px); } 31.43% { transform: translateX(-0.91px); }
  34.29% { transform: translateX(-0.78px); } 40% { transform: translateX(0.57px); } 45.71% { transform: translateX(0.21px); }
  51.43% { transform: translateX(-0.5px); } 57.14% { transform: translateX(0.14px); } 65.71% { transform: translateX(-0.03px); }
  100% { transform: translateX(0); }
}
@keyframes am-breathe { from { transform: scale(1); opacity: var(--lo); } to { transform: scale(var(--grow)); opacity: var(--hi); } }
@keyframes am-done {
  from { transform: scale(1); opacity: 0.7; }
  to { transform: scale(var(--grow)); opacity: 0; }
}
@keyframes am-draw { from { stroke-dashoffset: 100; } to { stroke-dashoffset: 0; } }
@keyframes am-appear { to { opacity: 0.85; } }
@keyframes am-hopfade { from { opacity: 0.8; } to { opacity: 0; } }
@keyframes am-pod-out { from { transform: translate(var(--dx), var(--dy)); } to { transform: translate(0, 0); } }
@keyframes am-pod-back {
  from { transform: translate(0, 0); opacity: 1; }
  99% { transform: translate(var(--dx), var(--dy)); opacity: 1; }
  to { transform: translate(var(--dx), var(--dy)); opacity: 0; }
}
@keyframes am-tether-out { from { transform: scale(0); } to { transform: scale(1); } }
@keyframes am-tether-back { from { transform: scale(1); } 99% { transform: scale(0.03); } to { transform: scale(0); opacity: 0; } }
@keyframes am-bob { 0%, 100% { transform: translateY(-1.5px); } 50% { transform: translateY(1.5px); } }

/* Reduced motion: every state stays, the movement goes. */
@media (prefers-reduced-motion: reduce) {
  .am-current, .am-ring, .am-halo, .am-halo:not(.quiet), .am-pod-bob { animation: none; }
  .am-halo:not(.quiet) { opacity: var(--mid); }
  .am-ring { display: none; }
  .am-ring.still { display: inline; opacity: 0.45; }
  .am-tint.fading, .am-sq-ring.fading, .am-glyph.fading { animation-timing-function: steps(4, end); }
  .am-pk, .am-rip, .am-done-ring { display: none; }
  .am-leg.newest { animation: none; }
  .am-dot { animation: none; opacity: 0.85; }
  .am-pod-move, .am-tether, .am-pod-cap { animation: none; }
  .am-pod-cap { opacity: 1; }
  .am-bump.bumping, .am-shake.shaking { animation: none; }
}
</style>
