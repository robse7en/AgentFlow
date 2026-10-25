<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import {
  VueFlow,
  useVueFlow,
  getRectOfNodes,
  MarkerType,
  Panel,
  type Edge,
  type GraphNode,
  type Node,
  type ViewportTransform,
} from '@vue-flow/core'
import { Background } from '@vue-flow/background'
import { MiniMap } from '@vue-flow/minimap'
import type { NodeState, TaskSnapshot } from '@/api/types'
import { EDGE_LABELS, NODE_POSITIONS } from '@/workflow/layout'
import WorkflowNode from './WorkflowNode.vue'
import LoopbackEdge from './LoopbackEdge.vue'

// `locked` makes the graph inert (no focus, no keyboard/pointer interaction) while an overlay
// rendered through the default slot — e.g. the question modal — demands an answer first.
// `focusKey` is a deep-linked node (`?node=`), framed on arrival instead of the live node(s).
const props = defineProps<{ task: TaskSnapshot; selected: string | null; locked?: boolean; focusKey?: string | null }>()
const emit = defineEmits<{ (e: 'select', key: string | null): void }>()

// Custom layouts persist across tasks/sessions since every task renders the same fixed
// workflow graph — one saved arrangement applies everywhere, not per-task.
const POSITIONS_STORAGE_KEY = 'agentflow:node-positions'

const loadStoredPositions = (): Record<string, { x: number; y: number }> => {
  try {
    const raw = localStorage.getItem(POSITIONS_STORAGE_KEY)
    return raw ? JSON.parse(raw) : {}
  } catch {
    return {}
  }
}

const positionOverrides = ref<Record<string, { x: number; y: number }>>(loadStoredPositions())
const hasCustomLayout = computed(() => Object.keys(positionOverrides.value).length > 0)

const persistPositions = (): void => {
  try {
    localStorage.setItem(POSITIONS_STORAGE_KEY, JSON.stringify(positionOverrides.value))
  } catch {
    // Storage unavailable (e.g. private browsing quota) — layout still works, just won't persist.
  }
}

const resetLayout = (): void => {
  positionOverrides.value = {}
  try {
    localStorage.removeItem(POSITIONS_STORAGE_KEY)
  } catch {
    /* ignore */
  }
  setTimeout(focusActive, 0)
}

const nodes = computed<Node[]>(() =>
  props.task.nodes.filter((node) => !node.worker).map((node) => ({
    id: node.key,
    type: 'agentflow',
    position: positionOverrides.value[node.key] ?? NODE_POSITIONS[node.key] ?? { x: 0, y: 0 },
    data: { node, taskKey: props.task.jiraKey, selected: props.selected === node.key },
    draggable: true,
    connectable: false,
  })),
)

const stateOf = (key: string) => props.task.nodes.find((n) => n.key === key)?.displayState

// The forward edge into the Implementer (orchestrator->implementer) only ever drives its very
// first run. Once the fix loop has fired at least once (consolidate->implementer, the "loopback"
// edge below), consolidate is the edge actually re-triggering the Implementer on every later
// iteration — but the forward source stays COMPLETED forever, so it would otherwise keep
// animating too.
const loopedBack = computed(() => stateOf('consolidate') === 'COMPLETED')

const edges = computed<Edge[]>(() =>
  props.task.edges.map(({ source, target }) => {
    const id = `${source}->${target}`
    const active = stateOf(source) === 'COMPLETED' && stateOf(target) !== 'PENDING'
    const isStaleFirstPass = target === 'implementer' && source !== 'consolidate' && loopedBack.value
    // Only the edge actually feeding the live node animates — a convergent edge whose own
    // source hasn't completed (e.g. an unused loop-back path), or whose path has been
    // superseded by a later re-entry into the same target, is not part of the live flow.
    // "Live" covers every state where the target node is the one currently demanding attention:
    // READY (needs a click to start), RUNNING, or WAITING_FOR_USER/APPROVAL.
    const targetState = stateOf(target)
    const running =
      !isStaleFirstPass &&
      stateOf(source) === 'COMPLETED' &&
      (targetState === 'READY' ||
        targetState === 'RUNNING' ||
        targetState === 'WAITING_FOR_USER' ||
        targetState === 'WAITING_FOR_APPROVAL')
    return {
      id,
      source,
      target,
      label: EDGE_LABELS[id],
      type: source === 'consolidate' && target === 'implementer' ? 'loopback' : 'default',
      animated: false,
      markerEnd: MarkerType.ArrowClosed,
      // Running edges get a "wf-edge-flow" class (see styles.css) instead of vue-flow's default
      // marching-ants: dashed segments travel *along the rendered path* via stroke-dashoffset,
      // which is correct for any edge shape (straight spine, diagonal branch into/out of the
      // review nodes, or the curved loop-back) — a color-only gradient keyed to a fixed x-axis
      // (the earlier approach) only ever lined up on perfectly horizontal edges. Every path is
      // drawn starting at its source handle and ending at its target handle, and the default
      // (non-reversed) animation already travels that same source->target direction — including
      // consolidate->implementer, whose path starts at consolidate's handle (the arc's right
      // end) and ends at implementer's (the arc's left end), so it already reads as flowing back
      // into Implementer without needing a reversed class.
      class: running ? 'wf-edge-flow' : '',
      style: running
        ? { stroke: 'var(--edge-flow-light)', strokeWidth: 2.5 }
        : { stroke: active ? 'var(--ok)' : 'var(--border-strong)', strokeWidth: active ? 2 : 1.5 },
      labelStyle: { fill: 'var(--muted)', fontSize: 11 },
      labelBgStyle: { fill: 'var(--panel)' },
    }
  }),
)

const { fitView, onNodeClick, onPaneClick, onMoveStart, onNodesInitialized, setViewport, onNodeDragStop, findNode, dimensions } =
  useVueFlow()
onNodeClick(({ node }) => emit('select', node.id))
onPaneClick(() => emit('select', null))

onNodeDragStop(({ node }) => {
  positionOverrides.value = { ...positionOverrides.value, [node.id]: { x: node.position.x, y: node.position.y } }
  persistPositions()
})

// Camera framing ("spotlight"). Fit-all clamps at MIN_ZOOM on windows narrower than ~1600px,
// leaving node text around 5px, so instead the view opens at FOCUS_ZOOM on the node(s) that need
// attention: their left edge sits a short lead in from the left of the canvas (the steps that
// follow stay in view to the right), vertically centred. Neither axis pans past the ends of the
// graph, and an axis the whole graph fits along is centred instead.
//
// The camera follows the workflow (re-frames when the focused nodes change or the canvas
// resizes) until the user pans or zooms; "Focus active", a task switch or a new deep link hands
// control back to it. "Fit all" counts as a user move.
const FOCUS_ZOOM = 1
// A focused group taller than the canvas (e.g. a node showing the CLI sign-in steps) shrinks to
// fit only down to this zoom, then shows from its top edge instead, so text stays legible.
const MIN_FOCUS_ZOOM = 0.75
const MIN_ZOOM = 0.4
const MAX_ZOOM = 1.5
const FIT_ALL_PADDING = 0.15
const EDGE_PAD = 24
const MIN_LEAD = 48
const LEAD_FRACTION = 0.08
const FRAME_ANIMATION_MS = 300

// Highest priority first; every node in the first non-empty tier is framed together, so the two
// parallel reviews stay in view side by side.
const FOCUS_TIERS: NodeState[][] = [
  ['WAITING_FOR_USER', 'WAITING_FOR_APPROVAL'],
  ['FAILED', 'INTERRUPTED'],
  ['RUNNING'],
  ['READY'],
]

const liveKeys = computed<string[]>(() => {
  const graphNodes = props.task.nodes.filter((node) => !node.worker)
  for (const tier of FOCUS_TIERS) {
    const keys = graphNodes.filter((node) => tier.includes(node.displayState)).map((node) => node.key)
    if (keys.length) return keys
  }
  // Nothing live (finished, cancelled, merged): frame how far the task got, else its start.
  const completed = graphNodes.filter((node) => node.displayState === 'COMPLETED')
  const last = completed[completed.length - 1] ?? graphNodes[0]
  return last ? [last.key] : []
})

// A deep link (e.g. a home-page attention item) is framed on arrival, then the camera returns to
// the live nodes once they change, so approving a linked gate moves on to the next step.
// (Watch order matters: on a task switch that also carries a link, the link must win.)
const pinnedLink = ref<string | null>(null)
watch(
  () => liveKeys.value.join(','),
  () => (pinnedLink.value = null),
)
watch(
  () => props.focusKey,
  (key) => (pinnedLink.value = key && props.task.nodes.some((node) => node.key === key && !node.worker) ? key : null),
  { immediate: true },
)
const focusKeys = computed<string[]>(() => (pinnedLink.value ? [pinnedLink.value] : liveKeys.value))

const clamp = (value: number, min: number, max: number): number => Math.min(Math.max(value, min), max)

const rectOf = (keys: string[]) => {
  const graphNodes = keys.map((key) => findNode(key)).filter((node): node is GraphNode => !!node)
  return graphNodes.length ? getRectOfNodes(graphNodes) : null
}

const spotlightViewport = (width: number, height: number): ViewportTransform | null => {
  const all = rectOf(nodes.value.map((node) => node.id))
  const group = rectOf(focusKeys.value)
  if (!all || !group) return null
  const zoom = clamp(
    Math.min(FOCUS_ZOOM, (width - 2 * EDGE_PAD) / group.width, (height - 2 * EDGE_PAD) / group.height),
    MIN_FOCUS_ZOOM,
    MAX_ZOOM,
  )
  const groupFits = group.height * zoom + 2 * EDGE_PAD <= height
  const groupY = groupFits ? height / 2 - (group.y + group.height / 2) * zoom : EDGE_PAD - group.y * zoom
  const lead = Math.max(MIN_LEAD, width * LEAD_FRACTION)
  const x =
    all.width * zoom + 2 * EDGE_PAD <= width
      ? (width - all.width * zoom) / 2 - all.x * zoom
      : clamp(lead - group.x * zoom, width - EDGE_PAD - (all.x + all.width) * zoom, EDGE_PAD - all.x * zoom)
  const y =
    all.height * zoom + 2 * EDGE_PAD <= height
      ? (height - all.height * zoom) / 2 - all.y * zoom
      : clamp(groupY, height - EDGE_PAD - (all.y + all.height) * zoom, EDGE_PAD - all.y * zoom)
  return { x, y, zoom }
}

const prefersReducedMotion = (): boolean => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false

// The graph stays hidden until the first frame lands (`revealed`), so it never flashes at
// vue-flow's default viewport first. `framed` is whether the camera has been placed at all.
const revealed = ref(false)
const framed = ref(false)
let nodesReady = false
let userMoved = false
// Set while our own setViewport runs, so its move events aren't taken for the user's. A user
// gesture that interrupts the animation still counts (it carries a DOM source event). The minimap
// pans programmatically (no source event), which is why the flag is needed at all.
let autoMoving = false
let autoMovingTimer: ReturnType<typeof setTimeout> | undefined

const frame = async (animate: boolean): Promise<void> => {
  const { width, height } = dimensions.value
  if (!nodesReady || !width || !height) return
  const viewport = spotlightViewport(width, height)
  if (!viewport) return
  const duration = animate && framed.value && !prefersReducedMotion() ? FRAME_ANIMATION_MS : 0
  autoMoving = true
  clearTimeout(autoMovingTimer)
  // An interrupted transition never resolves its promise, so a timer clears the flag as well.
  autoMovingTimer = setTimeout(() => (autoMoving = false), duration + 50)
  framed.value = true
  revealed.value = true
  await setViewport(viewport, { duration })
}

const focusActive = (): void => {
  userMoved = false
  void frame(true)
}

const fitAll = (): void => {
  userMoved = true
  void fitView({ padding: FIT_ALL_PADDING, duration: prefersReducedMotion() ? 0 : FRAME_ANIMATION_MS })
}

onMoveStart(({ event }) => {
  if (!framed.value) return
  const fromUser = !('sourceEvent' in event) || event.sourceEvent != null
  if (!autoMoving || fromUser) userMoved = true
})

onNodesInitialized(() => {
  nodesReady = true
  if (!framed.value) void frame(false)
})

watch(
  () => [dimensions.value.width, dimensions.value.height],
  () => {
    if (!userMoved) void frame(false)
  },
)

// A focused node changing size (its CLI sign-in steps dismissed, an error appearing) re-frames it.
watch(
  () => focusKeys.value.map((key) => findNode(key)?.dimensions.height ?? 0).join(','),
  () => {
    if (!userMoved && framed.value) void frame(true)
  },
)

watch(
  [() => props.task.jiraKey, () => focusKeys.value.join(','), () => props.focusKey],
  ([jiraKey, , link], [prevJiraKey, , prevLink]) => {
    const switched = jiraKey !== prevJiraKey
    if (switched || (link && link !== prevLink)) userMoved = false
    if (userMoved) return
    // On a task switch the new nodes render first, then the camera jumps without animating.
    if (switched) void nextTick().then(() => frame(false))
    else void frame(true)
  },
)

const isEditable = (target: EventTarget | null): boolean =>
  target instanceof HTMLElement &&
  (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))

const onKeydown = (event: KeyboardEvent): void => {
  if (event.key.toLowerCase() !== 'f' || event.repeat || event.ctrlKey || event.metaKey || event.altKey) return
  if (event.defaultPrevented || props.locked || isEditable(event.target)) return
  focusActive()
}

// Safety net: never leave the graph hidden if the first frame can't be computed.
let revealTimer: ReturnType<typeof setTimeout> | undefined
onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  revealTimer = setTimeout(() => (revealed.value = true), 1500)
})
onUnmounted(() => {
  window.removeEventListener('keydown', onKeydown)
  clearTimeout(revealTimer)
  clearTimeout(autoMovingTimer)
})

const minimapNodeClass = (node: GraphNode): string => `wf-minimap-node ${node.data?.node?.displayState ?? ''}`
</script>

<template>
  <div class="canvas-wrap theme-dark" :class="{ 'canvas-wrap--unframed': !revealed }">
    <VueFlow
      :nodes="nodes"
      :edges="edges"
      :nodes-draggable="true"
      :zoom-on-double-click="false"
      :min-zoom="MIN_ZOOM"
      :max-zoom="MAX_ZOOM"
      :inert="locked"
    >
      <Background pattern-color="var(--border-strong)" :gap="18" :size="2.5" />
      <Panel position="top-right" class="canvas-actions">
        <button class="sm" title="Zoom to the node that needs attention (F)" @click="focusActive">Focus active</button>
        <button class="sm" title="Zoom out to show the whole workflow" @click="fitAll">Fit all</button>
        <button class="sm" :disabled="!hasCustomLayout" @click="resetLayout">Reset layout</button>
      </Panel>
      <MiniMap
        class="wf-minimap"
        position="bottom-right"
        pannable
        zoomable
        :width="180"
        :height="110"
        :node-class-name="minimapNodeClass"
        :node-border-radius="12"
        aria-label="Workflow overview"
      />
      <template #node-agentflow="nodeProps">
        <WorkflowNode :data="nodeProps.data" />
      </template>
      <template #edge-loopback="edgeProps">
        <LoopbackEdge v-bind="edgeProps" />
      </template>
    </VueFlow>
    <!-- Sibling of VueFlow (not inside it) so wheel/drag over an overlay never reaches d3-zoom. -->
    <slot />
  </div>
</template>
