<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import type { AnswerBody, DecisionChoiceDto, QuestionDto, TaskSnapshot } from '@/api/types'
import {
  RISK_LABELS, answerPath, branchState, decisionView, drop, elbow, hint, initialState, keyAction, keyAllowed,
  optionState, otherState, sentStatus, sourceHref, type Box, type Chosen, type ItemState, type SentStatus,
} from '@/decisions'

// One agent question as a decision tree: the question card, its numbered branches, each branch's
// follow-up options (or consequences), Other…, and the sent state. Layout is measured: three grid
// columns joined by SVG connectors, or one stacked column without connectors when narrow. The
// parent supplies `submit` (the answer call) and moves on when `done` fires.
const props = withDefaults(defineProps<{
  question: QuestionDto
  nodeLabel: string
  /** e.g. "Claude Code · opus · 4m ago". */
  meta?: string
  submit: (body: AnswerBody) => Promise<TaskSnapshot>
  /** The inbox has another decision to show (→). */
  canNext?: boolean
  /** Keys are handled only while true (e.g. no other overlay is on top). */
  active?: boolean
}>(), { meta: '', canNext: false, active: true })
const emit = defineEmits<{ done: [status: SentStatus | null], next: [], answered: [snapshot: TaskSnapshot] }>()

const view = computed(() => decisionView(props.question))
const s = reactive(initialState())
const otherText = ref('')
const error = ref('')
const status = ref<SentStatus | null>(null)
const reduced = typeof window !== 'undefined' && typeof window.matchMedia === 'function'
  && window.matchMedia('(prefers-reduced-motion: reduce)').matches
const freeTextOnly = computed(() => view.value.branches.length === 0)

const timers: ReturnType<typeof setTimeout>[] = []
const later = (ms: number, fn: () => void) => { timers.push(setTimeout(fn, ms)) }

const root = ref<HTMLElement | null>(null)
const stage = ref<HTMLElement | null>(null)
const content = ref<HTMLElement | null>(null)
const cardEl = ref<HTMLElement | null>(null)
const otherEl = ref<HTMLElement | null>(null)
const otherInput = ref<HTMLTextAreaElement | null>(null)
const branchEls: Record<number, HTMLElement> = {}
const pillEls: Record<number, HTMLElement> = {}
const leafEls: Record<number, HTMLElement> = {}
const optionEls: Record<string, HTMLElement> = {}
const keep = (store: Record<string | number, HTMLElement>, key: string | number) => (el: unknown) => {
  if (el instanceof HTMLElement) store[key] = el
  else delete store[key]
}

// ------------------------------------------------------------------------------ states

const branchCls = (i: number): ItemState => branchState(view.value, s, i)
const optionCls = (i: number, j: number): ItemState => optionState(view.value, s, i, j)
const otherCls = computed(() => otherState(view.value, s))
const pre = computed(() => (s.phase === 'pre' ? 'pre' : ''))
/** Transition delays only while entering, so later hovers react at once. */
const delay = (ms: number) => (s.phase === 'entering' ? `${ms}ms` : '0ms')
const ready = () => s.phase === 'idle' || s.phase === 'entering'
const hasLeaf = (b: DecisionChoiceDto) => !b.followUp && !!(b.detail || b.warning)
const numbered = (i: number) => s.picked === i || s.chosen?.branch === i
const footHint = computed(() => hint(view.value, s))

function settle() {
  if (s.phase === 'entering') s.phase = 'idle'
}

function pickBranch(i: number) {
  if (!ready()) return
  settle()
  const branch = view.value.branches[i]
  if (branch?.followUp) {
    Object.assign(s, { picked: i, hover: null, hoverOption: null, otherOpen: false })
  } else {
    void choose({ branch: i, option: null, other: null })
  }
}

function pickOption(i: number, j: number) {
  if (!ready()) return
  settle()
  void choose({ branch: i, option: j, other: null })
}

function openOther() {
  if (!ready()) return
  settle()
  Object.assign(s, { otherOpen: true, picked: null, hover: null, hoverOption: null })
  void nextTick(() => otherInput.value?.focus())
}

function back() {
  if (s.picked !== null) {
    Object.assign(s, { picked: null, hoverOption: null })
  } else if (s.otherOpen && !freeTextOnly.value) {
    s.otherOpen = false
    otherText.value = ''
    void nextTick(() => root.value?.focus({ preventScroll: true }))
  }
}

function sendOther() {
  const text = otherText.value.trim()
  if (text && ready()) void choose({ branch: null, option: null, other: text })
}

async function choose(chosen: Chosen) {
  Object.assign(s, { chosen, phase: 'sending', hover: null, hoverOption: null, otherOpen: false })
  error.value = ''
  const path = answerPath(chosen)
  const started = Date.now()
  try {
    const snapshot = await props.submit(path ? { path } : { answer: chosen.other ?? '' })
    emit('answered', snapshot)
    const result = sentStatus(snapshot, props.question, props.nodeLabel)
    // The typing dots stay long enough to read, however fast the reply was delivered.
    later(Math.max(0, (reduced ? 150 : 600) - (Date.now() - started)), () => {
      status.value = result
      s.phase = 'sent'
      later(1600, () => {
        s.phase = 'leaving'
        later(reduced ? 0 : 320, () => emit('done', result))
      })
    })
  } catch (cause) {
    error.value = (cause as Error).message
    Object.assign(s, { chosen: null, phase: 'idle', otherOpen: chosen.other !== null || freeTextOnly.value })
  }
}

function hoverBranch(i: number | null) {
  if (!ready() || s.chosen || s.picked !== null || s.otherOpen) return
  s.hover = i
}

function hoverOption(i: number, j: number | null) {
  if (!ready() || s.chosen) return
  if (j === null) {
    s.hoverOption = null
    return
  }
  s.hoverOption = { branch: i, option: j }
  if (s.picked === null) s.hover = i
}

// ------------------------------------------------------------------------------ keyboard

function onKey(event: KeyboardEvent) {
  if (!props.active) return
  const target = event.target instanceof HTMLElement ? event.target : null
  const allowed = keyAllowed({
    onBody: !target || target === document.body || target === document.documentElement,
    inCard: !!target && !!root.value?.contains(target),
    inTerminal: !!target?.closest('.terminal-overlay, .terminal-drawer, .xterm'),
    editable: !!target && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)),
    modifier: event.ctrlKey || event.metaKey || event.altKey,
  })
  if (!allowed) return
  const action = keyAction(event.key, view.value, s, props.canNext)
  if (!action) return
  event.preventDefault()
  if (action.kind === 'branch') pickBranch(action.index)
  else if (action.kind === 'option' && s.picked !== null) pickOption(s.picked, action.index)
  else if (action.kind === 'other') openOther()
  else if (action.kind === 'back') back()
  else if (action.kind === 'next') emit('next')
}

function onOtherKey(event: KeyboardEvent) {
  event.stopPropagation()
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault()
    sendOther()
  } else if (event.key === 'Escape' && !freeTextOnly.value) {
    event.preventDefault()
    back()
  }
}

// ------------------------------------------------------------------------------ layout

const narrow = ref(false)
interface Boxes {
  card: Box
  branches: Record<number, Box>
  pills: Record<number, Box>
  leaves: Record<number, Box>
  options: Record<string, Box>
  other: Box | null
  contentHeight: number
}
const boxes = ref<Boxes | null>(null)

/** An element's layout box relative to the stage, ignoring transforms (entry slides, hover lifts). */
function box(el: HTMLElement | undefined | null): Box | null {
  if (!el || !stage.value) return null
  let left = 0
  let top = 0
  let node: HTMLElement | null = el
  while (node && node !== stage.value) {
    left += node.offsetLeft
    top += node.offsetTop
    node = node.offsetParent as HTMLElement | null
  }
  if (node !== stage.value) return null
  return { left, top, right: left + el.offsetWidth, bottom: top + el.offsetHeight }
}

function measure() {
  if (!stage.value || !content.value || !root.value) return
  narrow.value = root.value.clientWidth < 760
  if (narrow.value) {
    boxes.value = null
    return
  }
  const card = box(cardEl.value)
  if (!card) return
  const collect = (els: Record<string | number, HTMLElement>) =>
    Object.fromEntries(Object.entries(els).map(([k, el]) => [k, box(el)]).filter(([, b]) => b)) as Record<string, Box>
  boxes.value = {
    card,
    branches: collect(branchEls), pills: collect(pillEls), leaves: collect(leafEls), options: collect(optionEls),
    other: box(otherEl.value), contentHeight: content.value.offsetHeight,
  }
}

const mid = (b: Box) => (b.top + b.bottom) / 2

interface Wire { key: string, d: string, len: number, cls: string, delay: string }

/** Where the status chip goes, and the connector into it, once a choice is sent. */
const chip = computed(() => {
  const b = boxes.value
  const chosen = s.chosen
  if (!b || !chosen) return null
  const column = Object.values({ ...b.pills, ...b.leaves })[0]?.left
    ?? ((b.branches[0]?.right ?? b.other?.right ?? b.card.right) + 40)
  const beside = (anchor: Box) => ({ left: column, top: mid(anchor) - 32, wire: elbow(anchor.right, mid(anchor), anchor.right + (column - anchor.right) / 2, column, mid(anchor)) })
  const below = (anchor: Box) => ({ left: anchor.left + 52, top: anchor.bottom + 14, wire: drop(anchor.left + 26, anchor.bottom, anchor.bottom + 46, anchor.left + 52) })
  if (chosen.other !== null) return b.other ? beside(b.other) : null
  if (chosen.branch === null) return null
  if (chosen.option !== null) {
    const option = b.options[`${chosen.branch}.${chosen.option}`]
    return option ? below(option) : null
  }
  const leaf = b.leaves[chosen.branch]
  if (leaf) return below(leaf)
  const branch = b.branches[chosen.branch]
  return branch ? beside(branch) : null
})

const wires = computed<Wire[]>(() => {
  const b = boxes.value
  if (!b) return []
  const out: Wire[] = []
  const tag = pre.value ? ' pre' : ''
  const cx = b.card.right
  const cy = mid(b.card)
  const firstLeft = b.branches[0]?.left ?? b.other?.left ?? cx + 48
  const trunk = cx + (firstLeft - cx) / 2
  const n = view.value.branches.length
  view.value.branches.forEach((branch, i) => {
    const bx = b.branches[i]
    if (!bx) return
    const state = branchCls(i)
    out.push({ key: `b${i}`, ...elbow(cx, cy, trunk, bx.left, mid(bx)), cls: state + tag, delay: delay(120 + i * 70) })
    const pill = b.pills[i]
    if (pill && branch.followUp) {
      out.push({ key: `p${i}`, ...elbow(bx.right, mid(bx), bx.right + (pill.left - bx.right) / 2, pill.left, mid(pill)), cls: state + tag, delay: delay(340 + i * 70) })
      branch.followUp.choices.forEach((_, j) => {
        const option = b.options[`${i}.${j}`]
        if (!option) return
        const os = optionCls(i, j)
        out.push({ key: `o${i}.${j}`, ...drop(pill.left + 20, pill.bottom, mid(option), option.left), cls: (os === 'lift' ? 'on' : os) + tag, delay: delay(400 + i * 70 + j * 50) })
      })
    }
    const leaf = b.leaves[i]
    if (leaf) {
      out.push({ key: `l${i}`, ...elbow(bx.right, mid(bx), bx.right + (leaf.left - bx.right) / 2, leaf.left, mid(leaf)), cls: state + tag, delay: delay(340 + i * 70) })
    }
  })
  if (b.other && n) {
    out.push({ key: 'other', ...elbow(cx, cy, trunk, b.other.left, mid(b.other)), cls: otherCls.value + tag, delay: delay(160 + n * 70) })
  }
  if (chip.value && s.phase !== 'pre') out.push({ key: 'chip', ...chip.value.wire, cls: 'on draw', delay: '0ms' })
  return out
})

const stagePad = computed(() => (chip.value && boxes.value ? Math.max(0, chip.value.top + 76 - boxes.value.contentHeight) : 0))

/** The "Agent's note" popover for the hovered or focused choice. */
const note = computed(() => {
  if (s.chosen || narrow.value || !boxes.value) return null
  if (s.hoverOption) {
    const { branch, option } = s.hoverOption
    const choice = view.value.branches[branch]?.followUp?.choices[option]
    const anchor = boxes.value.options[`${branch}.${option}`]
    return choice?.note && anchor ? { choice, left: anchor.left + 16, top: anchor.bottom + 6 } : null
  }
  if (s.hover !== null && s.picked === null) {
    const choice = view.value.branches[s.hover]
    const anchor = boxes.value.branches[s.hover]
    return choice?.note && anchor ? { choice, left: anchor.left + 16, top: anchor.bottom + 6 } : null
  }
  return null
})

let observer: ResizeObserver | null = null
onMounted(() => {
  window.addEventListener('keydown', onKey)
  if (freeTextOnly.value) s.otherOpen = true
  observer = new ResizeObserver(() => measure())
  if (root.value) observer.observe(root.value)
  if (content.value) observer.observe(content.value)
  void nextTick(measure)
  void document.fonts?.ready.then(measure)
  if (reduced) {
    s.phase = 'idle'
    return
  }
  later(40, () => { s.phase = 'entering' })
  later(1000, settle)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKey)
  observer?.disconnect()
  timers.forEach(clearTimeout)
})
// Opening Other… grows its row; picking a branch changes nothing in size but keeps it honest.
watch(() => [s.otherOpen, s.picked, s.chosen], () => { void nextTick(measure); later(340, measure) })

defineExpose({
  /** Focus the card (or its reply box for a free-text question) unless something else has focus. */
  focus() {
    const active = document.activeElement
    if (active && active !== document.body) return
    if (s.otherOpen) otherInput.value?.focus()
    else root.value?.focus({ preventScroll: true })
  },
})

const mono = computed(() => (props.nodeLabel.trim()[0] ?? '?').toUpperCase())
const riskLabel = computed(() => (view.value.risk ? RISK_LABELS[view.value.risk] : null))
</script>

<template>
  <section
    ref="root"
    class="dc"
    :class="[`phase-${s.phase}`, { narrow, reduced }]"
    tabindex="-1"
    :aria-label="`${nodeLabel} needs a decision`"
  >
    <div ref="stage" class="dc-stage" :style="{ paddingBottom: `${stagePad}px` }">
      <svg v-if="!narrow" class="dc-wires" aria-hidden="true">
        <path
          v-for="w in wires"
          :key="w.key"
          :d="w.d"
          class="dc-wire"
          :class="w.cls"
          :style="{ strokeDasharray: w.len, strokeDashoffset: s.phase === 'pre' ? w.len : 0, '--len': w.len, transitionDelay: w.delay }"
        />
      </svg>

      <div ref="content" class="dc-content" :style="{ '--rows': view.branches.length + 1 }">
        <article ref="cardEl" class="dc-card it" :class="pre">
          <span v-if="riskLabel" class="dc-risk" :class="`risk-${view.risk}`">{{ riskLabel }}</span>
          <div class="dc-who">
            <span class="dc-mono" aria-hidden="true">{{ mono }}</span>
            <div>
              <div class="dc-role">{{ nodeLabel }}</div>
              <div v-if="meta" class="dc-meta">{{ meta }}</div>
            </div>
          </div>
          <p class="dc-message">{{ question.message }}</p>
          <p v-if="view.context" class="dc-context">{{ view.context }}</p>
          <slot name="task" />
        </article>

        <template v-for="(branch, i) in view.branches" :key="`b${i}`">
          <button
            :ref="keep(branchEls, i)"
            type="button"
            class="dc-branch it"
            :class="[branchCls(i), pre]"
            :style="{ '--row': i + 1, transitionDelay: delay(160 + i * 70) }"
            :tabindex="branchCls(i) === 'gone' ? -1 : 0"
            :aria-pressed="s.picked === i"
            @click="pickBranch(i)"
            @mouseenter="hoverBranch(i)"
            @mouseleave="hoverBranch(null)"
            @focus="hoverBranch(i)"
            @blur="hoverBranch(null)"
          >
            <span class="dc-kbd">{{ i + 1 }}</span>
            <span class="dc-label">
              <span class="dc-label-top">
                <span>{{ branch.label }}</span>
                <span v-if="branch.note" class="dc-notemark" title="Agent's note"><svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true"><path d="M12 7v6M12 17.5v.01" /></svg><span class="sr-only">Has an agent's note</span></span>
              </span>
              <span v-if="branch.recommended" class="dc-pick">Agent's pick</span>
              <span v-if="branch.followUp && branch.detail" class="dc-detail">{{ branch.detail }}</span>
              <span v-if="branch.followUp && branch.warning" class="dc-warn">{{ branch.warning }}</span>
            </span>
          </button>

          <div
            v-if="branch.followUp"
            v-show="!narrow || numbered(i)"
            class="dc-follow"
            :style="{ '--row': i + 1 }"
          >
            <div
              :ref="keep(pillEls, i)"
              class="dc-pill it"
              :class="[branchCls(i), pre]"
              :style="{ transitionDelay: delay(360 + i * 70) }"
            >{{ branch.followUp.message }}</div>
            <div class="dc-options">
              <template v-for="(option, j) in branch.followUp.choices" :key="`o${i}.${j}`">
                <button
                  :ref="keep(optionEls, `${i}.${j}`)"
                  type="button"
                  class="dc-option it"
                  :class="[optionCls(i, j), pre, { numbered: numbered(i) }]"
                  :style="{ transitionDelay: delay(420 + i * 70 + j * 50) }"
                  :tabindex="optionCls(i, j) === 'gone' ? -1 : 0"
                  @click="pickOption(i, j)"
                  @mouseenter="hoverOption(i, j)"
                  @mouseleave="hoverOption(i, null)"
                  @focus="hoverOption(i, j)"
                  @blur="hoverOption(i, null)"
                >
                  <span class="dc-onum" aria-hidden="true">{{ j + 1 }}</span>
                  <span class="dc-otext">
                    <span class="dc-label-top">
                      <span>{{ option.label }}</span>
                      <span v-if="option.recommended" class="dc-pick">Agent's pick</span>
                      <span v-if="option.note" class="dc-notemark" title="Agent's note"><svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true"><path d="M12 7v6M12 17.5v.01" /></svg><span class="sr-only">Has an agent's note</span></span>
                    </span>
                    <span v-if="option.detail" class="dc-detail">{{ option.detail }}</span>
                    <span v-if="option.warning" class="dc-warn">{{ option.warning }}</span>
                    <span v-if="narrow && option.note && numbered(i)" class="dc-inline-note">
                      <strong>Agent's note:</strong> {{ option.note }}
                      <a v-if="sourceHref(option.source)" :href="sourceHref(option.source)!" target="_blank" rel="noopener noreferrer" class="dc-src">{{ option.source }}</a>
                      <span v-else-if="option.source" class="dc-src">{{ option.source }}</span>
                    </span>
                  </span>
                </button>
              </template>
            </div>
          </div>

          <div
            v-else-if="hasLeaf(branch) || (narrow && branch.note)"
            :ref="keep(leafEls, i)"
            class="dc-leaf it"
            :class="[branchCls(i), pre]"
            :style="{ '--row': i + 1, transitionDelay: delay(360 + i * 70) }"
          >
            <span v-if="branch.warning" class="dc-warn">{{ branch.warning }}</span>
            <span v-if="branch.detail" class="dc-detail">{{ branch.detail }}</span>
            <span v-if="narrow && branch.note" class="dc-inline-note">
              <strong>Agent's note:</strong> {{ branch.note }}
              <a v-if="sourceHref(branch.source)" :href="sourceHref(branch.source)!" target="_blank" rel="noopener noreferrer" class="dc-src">{{ branch.source }}</a>
              <span v-else-if="branch.source" class="dc-src">{{ branch.source }}</span>
            </span>
          </div>
        </template>

        <div
          ref="otherEl"
          class="dc-other it"
          :class="[freeTextOnly ? '' : otherCls, pre, { open: s.otherOpen, solo: freeTextOnly }]"
          :style="{ '--row': view.branches.length + 1, transitionDelay: delay(200 + view.branches.length * 70) }"
        >
          <button
            v-if="!s.otherOpen && s.chosen?.other == null"
            type="button"
            class="dc-branch ghost"
            :tabindex="otherCls === 'gone' ? -1 : 0"
            @click="openOther"
          >
            <span class="dc-kbd">{{ view.branches.length + 1 }}</span><span>Other…</span>
          </button>
          <div v-else-if="s.chosen?.other != null" class="dc-other-done">{{ s.chosen.other }}</div>
          <div v-else class="dc-other-input">
            <label class="sr-only" :for="`dc-other-${question.uid}`">{{ freeTextOnly ? 'Your reply' : 'Tell the agent what to do instead' }}</label>
            <textarea
              :id="`dc-other-${question.uid}`"
              ref="otherInput"
              v-model="otherText"
              :maxlength="view.otherLimit"
              :placeholder="freeTextOnly ? 'Type your reply…' : 'Tell the agent what to do instead…'"
              :disabled="!ready()"
              @keydown="onOtherKey"
            />
            <button type="button" class="sm primary" :disabled="!otherText.trim() || !ready()" @click="sendOther">Send</button>
          </div>
        </div>

        <div v-if="narrow && s.chosen" class="dc-chip static" :class="{ sending: s.phase === 'sending' }" role="status">
          <template v-if="s.phase === 'sending'">
            <div class="dc-chip-title"><span class="dc-typing" aria-hidden="true"><span /><span /><span /></span><span class="dc-meta">Sending to {{ nodeLabel }}…</span></div>
          </template>
          <template v-else-if="status">
            <div class="dc-chip-title"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" aria-hidden="true" class="dc-ok"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>{{ status.title }}</div>
            <div class="dc-line mono">{{ status.line }}</div>
          </template>
        </div>
      </div>

      <div
        v-if="chip && !narrow"
        class="dc-chip"
        :class="{ sending: s.phase === 'sending' }"
        :style="{ left: `${chip.left}px`, top: `${chip.top}px` }"
        role="status"
      >
        <template v-if="s.phase === 'sending'">
          <div class="dc-chip-title"><span class="dc-typing" aria-hidden="true"><span /><span /><span /></span><span class="dc-meta">Sending to {{ nodeLabel }}…</span></div>
        </template>
        <template v-else-if="status">
          <div class="dc-chip-title"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" aria-hidden="true" class="dc-ok"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>{{ status.title }}</div>
          <div class="dc-line mono">{{ status.line }}</div>
        </template>
      </div>

      <div v-if="note" class="dc-note" :style="{ left: `${note.left}px`, top: `${note.top}px` }" role="note">
        <span class="dc-note-head"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M12 3l9.5 17h-19z" /><path d="M12 10v4M12 17.5v.01" /></svg>Agent's note</span>
        <span>{{ note.choice.note }}</span>
        <a v-if="sourceHref(note.choice.source)" :href="sourceHref(note.choice.source)!" target="_blank" rel="noopener noreferrer" class="dc-src">{{ note.choice.source }}</a>
        <span v-else-if="note.choice.source" class="dc-src">{{ note.choice.source }}</span>
      </div>
    </div>

    <p v-if="error" class="alert error small dc-error">{{ error }}</p>

    <footer class="dc-foot">
      <div v-if="footHint.kind === 'pick'" class="dc-hints">
        <span>Pick a branch</span><span class="dc-kbd">1</span><span>–</span><span class="dc-kbd">{{ footHint.last }}</span><span>·</span><span class="dc-kbd">O</span><span>other</span>
        <template v-if="canNext"><span>·</span><button type="button" class="link-button" @click="emit('next')">Next decision</button><span class="dc-kbd">→</span></template>
      </div>
      <div v-else-if="footHint.kind === 'then'" class="dc-hints">
        <span>Then</span><span class="dc-kbd">1</span><span>–</span><span class="dc-kbd">{{ footHint.last }}</span><span>·</span><button type="button" class="link-button" @click="back">Change branch</button><span class="dc-kbd">Esc</span>
      </div>
      <div v-else-if="footHint.kind === 'other'" class="dc-hints">
        <span class="dc-kbd">Enter</span><span>send</span><template v-if="!freeTextOnly"><span>·</span><span class="dc-kbd">Esc</span><span>cancel</span></template>
      </div>
      <span v-else />
      <slot name="footer" />
    </footer>
  </section>
</template>

<style scoped>
/* Focused only so its keys work (it is not itself a control), so no focus ring of its own. */
.dc { display: flex; flex-direction: column; gap: 0.75rem; outline: none; min-width: 0; }
.dc-stage { position: relative; transition: opacity 0.3s ease, transform 0.3s ease; }
.phase-leaving .dc-stage { opacity: 0; transform: translateY(-8px); }

.dc-wires { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
.dc-wire {
  fill: none; stroke: var(--border-strong); stroke-width: 2; stroke-linecap: round;
  transition: stroke 0.25s, opacity 0.28s, stroke-dashoffset 0.55s ease;
}
.dc-wire.on { stroke: var(--accent); }
.dc-wire.dim { opacity: 0.35; }
.dc-wire.gone { opacity: 0; }
.dc-wire.pre { transition: none; }
.dc-wire.draw { animation: dc-draw 0.45s ease both; }
@keyframes dc-draw { from { stroke-dashoffset: var(--len); } to { stroke-dashoffset: 0; } }

/* Three columns: the question card (spanning every row), the branches, and each branch's
   follow-up or consequences. Rows are one per branch plus Other…. */
.dc-content {
  display: grid;
  grid-template-columns: minmax(220px, 290px) minmax(180px, 240px) minmax(220px, 340px);
  column-gap: 48px;
  row-gap: 16px;
  align-items: center;
  justify-content: start;
}
.dc-card { grid-column: 1; grid-row: 1 / span var(--rows); align-self: center; }
.dc-branch, .dc-other { grid-column: 2; grid-row: var(--row); }
.dc-follow, .dc-leaf { grid-column: 3; grid-row: var(--row); }
.dc-other.solo { grid-column: 2 / span 2; }

.it { transition: opacity 0.28s ease, transform 0.35s var(--spring), border-color 0.2s, box-shadow 0.25s, background 0.2s; }
.it.pre { opacity: 0; transform: translateX(-10px); transition: none; }
.it.dim { opacity: 0.35; }
.it.gone { opacity: 0; pointer-events: none; }

.dc-card {
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-md);
  padding: 16px 18px; box-shadow: 0 8px 24px var(--shadow-2); display: flex; flex-direction: column; gap: 11px;
}
.dc-risk { align-self: flex-start; font-size: 12.5px; font-weight: 600; padding: 3px 9px; border-radius: 8px; }
.risk-high { background: var(--danger-soft); color: var(--danger); }
.risk-medium { background: var(--warn-soft); color: var(--warn); }
.risk-low { background: var(--ok-soft); color: var(--ok); }
.dc-who { display: flex; align-items: center; gap: 11px; }
.dc-mono {
  width: 36px; height: 36px; border-radius: 10px; border: 1px solid var(--accent-border); display: grid; place-items: center;
  font-weight: 700; color: var(--accent); background: var(--accent-soft); flex: none;
}
.dc-role { font-weight: 600; }
.dc-meta { color: var(--muted); font-size: 12.5px; }
.dc-message { margin: 0; font-size: 16px; line-height: 1.4; font-weight: 600; white-space: pre-wrap; overflow-wrap: anywhere; }
.dc-context { margin: 0; color: var(--muted); font-size: 13px; line-height: 1.5; overflow-wrap: anywhere; }

.dc-branch {
  min-height: 56px; width: 100%; font: inherit; text-align: left; color: var(--text); background: var(--panel);
  border: 1px solid var(--border); border-radius: 14px; padding: 8px 14px; display: flex; align-items: center; gap: 12px;
  cursor: pointer;
}
.dc-branch:hover { border-color: var(--border-strong); }
.dc-branch.on { border-color: var(--accent-border); box-shadow: 0 8px 22px var(--shadow-2); transform: translateY(-1px); }
.dc-branch.ghost { color: var(--muted); }
.dc-kbd {
  flex: none; min-width: 22px; height: 22px; padding: 0 4px; box-sizing: border-box; border-radius: 6px;
  background: var(--info-soft); border: 1px solid var(--border); display: inline-grid; place-items: center;
  font-size: 12px; font-family: var(--mono); color: var(--muted); transition: background 0.2s, color 0.2s, border-color 0.2s;
}
.dc-branch.on .dc-kbd { background: var(--accent-soft); border-color: var(--accent-border); color: var(--accent); }
.dc-label, .dc-otext { display: flex; flex-direction: column; gap: 3px; font-weight: 500; min-width: 0; overflow-wrap: anywhere; }
.dc-label-top { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
.dc-pick { color: var(--accent); font-size: 12.5px; font-weight: 600; }

.dc-follow { display: flex; flex-direction: column; gap: 8px; min-width: 0; }
.dc-pill {
  align-self: flex-start; min-height: 34px; box-sizing: border-box; background: var(--panel); border: 1px solid var(--border);
  border-radius: 10px; padding: 6px 12px; display: flex; align-items: center; font-weight: 500; max-width: 100%; overflow-wrap: anywhere;
}
.dc-pill.on { border-color: var(--accent-border); }
.dc-options { display: flex; flex-direction: column; gap: 8px; margin-left: 44px; }
.dc-option {
  min-height: 58px; font: inherit; color: var(--text); text-align: left; background: var(--panel); border: 1px solid var(--border);
  border-radius: 12px; padding: 8px 13px; display: flex; align-items: center; cursor: pointer;
}
.dc-option.lift { border-color: var(--accent-border); box-shadow: 0 8px 20px var(--shadow-2); transform: translateY(-1px); }
.dc-option.on { border-color: var(--accent-border); }
.dc-onum {
  display: inline-grid; place-items: center; overflow: hidden; max-width: 0; opacity: 0; margin-right: 0; flex: none; height: 22px;
  border-radius: 6px; background: var(--info-soft); border: 0 solid var(--border); font-size: 12px; font-family: var(--mono); color: var(--muted);
  transition: max-width 0.3s var(--spring), opacity 0.25s, margin-right 0.3s var(--spring);
}
.dc-option.numbered .dc-onum { max-width: 24px; min-width: 22px; opacity: 1; margin-right: 11px; border-width: 1px; }
.dc-detail { color: var(--muted); font-size: 12.5px; font-weight: 400; }
.dc-warn {
  align-self: flex-start; background: var(--warn-soft); color: var(--warn); border: 1px solid var(--warn-border);
  font-size: 12px; font-weight: 500; padding: 1px 7px; border-radius: 6px;
}
.dc-leaf {
  min-height: 64px; box-sizing: border-box; background: var(--panel); border: 1px dashed var(--border-strong); border-radius: 12px;
  padding: 8px 13px; display: flex; flex-direction: column; justify-content: center; gap: 5px; max-width: 320px;
}
.dc-leaf.on { border-style: solid; border-color: var(--accent-border); }
.dc-notemark {
  display: inline-grid; place-items: center; width: 16px; height: 16px; border-radius: 5px; background: var(--warn-soft);
  color: var(--warn); flex: none; align-self: center;
}

.dc-other-input { display: flex; flex-direction: column; gap: 6px; align-items: flex-end; }
.dc-other-input textarea {
  width: 100%; height: 84px; box-sizing: border-box; font: inherit; color: var(--text); background: var(--input-bg);
  border: 1px solid var(--accent); box-shadow: 0 0 0 4px var(--accent-soft); border-radius: 14px; padding: 10px 12px;
  resize: none; outline: none; animation: dc-grow 0.3s var(--spring) both;
}
@keyframes dc-grow { from { height: 56px; opacity: 0.6; } to { height: 84px; opacity: 1; } }
.dc-other-done {
  min-height: 56px; box-sizing: border-box; border: 1px solid var(--accent-border); border-radius: 14px; background: var(--panel);
  display: flex; align-items: center; padding: 8px 14px; font-weight: 500; box-shadow: 0 8px 22px var(--shadow-2); overflow-wrap: anywhere;
}

.dc-chip {
  position: absolute; max-width: 360px; z-index: 4; background: var(--panel); border: 1px solid var(--ok-border); border-radius: 14px;
  padding: 9px 14px; box-shadow: 0 10px 26px var(--shadow-1); display: flex; flex-direction: column; justify-content: center; gap: 5px;
  min-height: 64px; box-sizing: border-box; animation: dc-chip-in 0.32s 0.3s var(--spring) both;
}
.dc-chip.static { position: static; max-width: none; grid-column: 1 / -1; animation-delay: 0s; }
.dc-chip.sending { border-color: var(--border); }
@keyframes dc-chip-in { from { opacity: 0; transform: translateX(-6px); } to { opacity: 1; transform: none; } }
.dc-chip-title { display: flex; align-items: center; gap: 8px; font-weight: 600; animation: dc-fade 0.25s both; }
.dc-ok { color: var(--ok); flex: none; }
.dc-typing { display: flex; gap: 4px; align-items: center; height: 16px; }
.dc-typing span { width: 6px; height: 6px; border-radius: 3px; background: var(--muted); animation: dc-blink 1s infinite both; }
.dc-typing span:nth-child(2) { animation-delay: 0.15s; }
.dc-typing span:nth-child(3) { animation-delay: 0.3s; }
@keyframes dc-blink { 0%, 80%, 100% { opacity: 0.25; transform: translateY(0); } 40% { opacity: 1; transform: translateY(-2px); } }
.dc-line { font-size: 12px; color: var(--muted); overflow-wrap: anywhere; animation: dc-fade 0.3s 0.1s both; }
@keyframes dc-fade { from { opacity: 0; } to { opacity: 1; } }

.dc-note {
  position: absolute; width: 300px; max-width: calc(100% - 16px); z-index: 5; background: var(--panel); border: 1px solid var(--border);
  border-radius: 12px; box-shadow: var(--elev); padding: 11px 13px; display: flex; flex-direction: column; gap: 6px;
  animation: dc-pop 0.2s var(--spring) both; pointer-events: none;
}
.dc-note a { pointer-events: auto; }
@keyframes dc-pop { from { opacity: 0; transform: translateY(-5px) scale(0.98); } to { opacity: 1; transform: none; } }
.dc-note-head { display: flex; align-items: center; gap: 6px; color: var(--warn); font-size: 11px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; }
.dc-src {
  align-self: flex-start; font-size: 11.5px; font-family: var(--mono); background: var(--info-soft); color: var(--muted);
  border-radius: 5px; padding: 1px 6px; overflow-wrap: anywhere;
}
a.dc-src { color: var(--accent); }
.dc-inline-note { display: flex; flex-direction: column; gap: 4px; margin-top: 4px; font-size: 12.5px; font-weight: 400; color: var(--text); }

.dc-error { margin: 0; }
.dc-foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; color: var(--muted); font-size: 13px; min-height: 30px; }
.dc-hints { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; animation: dc-fade 0.25s both; }
.link-button { border: 0; background: none; color: var(--accent); padding: 4px 6px; min-height: 0; font: inherit; cursor: pointer; }
.link-button:hover { text-decoration: underline; background: none; }
.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }

/* One column without connectors: the card, then each branch with its follow-up (once picked) or
   consequences under it, then Other…. Notes show inline instead of as a popover. */
.narrow .dc-content { grid-template-columns: minmax(0, 1fr); row-gap: 10px; }
.narrow .dc-card, .narrow .dc-branch, .narrow .dc-other, .narrow .dc-follow, .narrow .dc-leaf { grid-column: 1; grid-row: auto; }
.narrow .dc-other.solo { grid-column: 1; }
.narrow .dc-follow, .narrow .dc-leaf { margin-left: 24px; }
.narrow .dc-options { margin-left: 0; }
.narrow .dc-leaf { max-width: none; min-height: 0; }
/* Stacked, a faded-out branch would only leave a gap: it goes away (the wide stage keeps it, so
   nothing reflows there). */
.narrow .it.gone, .narrow .dc-option.gone { display: none; }

@media (prefers-reduced-motion: reduce) {
  .dc *, .dc *::before, .dc *::after, .dc-stage { transition: none !important; animation: none !important; }
  .it.pre { transform: none; }
  .phase-leaving .dc-stage { transform: none; }
}
</style>
