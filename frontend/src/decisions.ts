/* The decision card's logic, kept free of Vue and the DOM so scripts/verify-decision-ui.mjs can
   check it directly: what a question offers, how each branch and option looks in each state,
   which key does what, and what the card says once the answer is sent. The server builds the
   line typed into the agent's pane (workflow/decisions.py); the card only sends a path. */
import type {
  DecisionChoiceDto, DecisionRisk, OpenQuestionDto, QuestionDto, TaskSnapshot,
} from '@/api/types'

const OTHER_PREFIX = 'other: '
const MAX_ANSWER = 4000

export const RISK_LABELS: Record<DecisionRisk, string> = { low: 'Low risk', medium: 'Medium risk', high: 'High risk' }

/** What a question offers. A flat question is a tree with plain branches and no pick. */
export interface DecisionView {
  tree: boolean
  context: string | null
  risk: DecisionRisk | null
  branches: DecisionChoiceDto[]
  /** The agent's pick at the top level. */
  pick: number | null
  /** Characters the free-text answer may have: the server adds `other: ` on a tree. */
  otherLimit: number
}

export function decisionView(question: Pick<QuestionDto, 'choices' | 'decision'>): DecisionView {
  const decision = question.decision
  const branches: DecisionChoiceDto[] = decision ? decision.choices : question.choices.map((label) => ({ label }))
  const pick = branches.findIndex((branch) => branch.recommended)
  return {
    tree: !!decision,
    context: decision?.context ?? null,
    risk: decision?.risk ?? null,
    branches,
    pick: pick >= 0 ? pick : null,
    otherLimit: decision ? MAX_ANSWER - OTHER_PREFIX.length : MAX_ANSWER,
  }
}

export type Phase = 'pre' | 'entering' | 'idle' | 'sending' | 'sent' | 'leaving'

export interface Chosen {
  branch: number | null
  option: number | null
  /** Free text from Other…, or null for a choice. */
  other: string | null
}

export interface CardState {
  phase: Phase
  hover: number | null
  hoverOption: { branch: number, option: number } | null
  /** A branch with a follow-up the human picked; its options are next. */
  picked: number | null
  chosen: Chosen | null
  otherOpen: boolean
}

export const initialState = (): CardState => ({
  phase: 'pre', hover: null, hoverOption: null, picked: null, chosen: null, otherOpen: false,
})

/** '' plain · 'on' on the highlighted path · 'dim' faded · 'gone' faded out in place · 'lift' hovered. */
export type ItemState = '' | 'on' | 'dim' | 'gone' | 'lift'

export function branchState(view: DecisionView, s: CardState, index: number): ItemState {
  if (s.chosen) return s.chosen.other === null && s.chosen.branch === index ? 'on' : 'gone'
  if (s.picked !== null) return index === s.picked ? 'on' : 'gone'
  if (s.otherOpen) return 'dim'
  const focus = s.hover ?? view.pick
  if (focus === null) return ''
  return index === focus ? 'on' : 'dim'
}

export function optionState(view: DecisionView, s: CardState, branch: number, option: number): ItemState {
  if (s.chosen) return s.chosen.branch === branch && s.chosen.option === option ? 'on' : 'gone'
  const state = branchState(view, s, branch)
  if (state === 'gone' || state === 'dim') return state
  if (s.hoverOption?.branch === branch) return s.hoverOption.option === option ? 'lift' : 'dim'
  return ''
}

export function otherState(view: DecisionView, s: CardState): ItemState {
  if (s.chosen) return s.chosen.other !== null ? 'on' : 'gone'
  if (s.picked !== null) return 'gone'
  if (s.otherOpen) return 'on'
  return s.hover !== null || view.pick !== null ? 'dim' : ''
}

/** The path the server expects for a choice: 0-based branch, then follow-up option. */
export function answerPath(chosen: Chosen): number[] | null {
  if (chosen.other !== null || chosen.branch === null) return null
  return chosen.option === null ? [chosen.branch] : [chosen.branch, chosen.option]
}

export type KeyAction =
  | { kind: 'branch', index: number }
  | { kind: 'option', index: number }
  | { kind: 'other' }
  | { kind: 'back' }
  | { kind: 'next' }
  | null

/** 1–N picks a branch (then a follow-up option), N+1 or O opens Other…, Esc goes back a level,
 *  → shows the next decision where there is one. Only while the card waits for a choice. */
export function keyAction(key: string, view: DecisionView, s: CardState, canNext = false): KeyAction {
  if (s.phase !== 'idle' || s.chosen) return null
  if (/^[1-9]$/.test(key)) {
    const index = Number(key) - 1
    if (s.picked !== null) {
      const options = view.branches[s.picked]?.followUp?.choices.length ?? 0
      return index < options ? { kind: 'option', index } : null
    }
    if (index < view.branches.length) return { kind: 'branch', index }
    return index === view.branches.length ? { kind: 'other' } : null
  }
  if ((key === 'o' || key === 'O') && s.picked === null && !s.otherOpen) return { kind: 'other' }
  if (key === 'Escape' && (s.picked !== null || s.otherOpen)) return { kind: 'back' }
  if (key === 'ArrowRight' && canNext && s.picked === null && !s.otherOpen) return { kind: 'next' }
  return null
}

/** Where a key press came from. The card reacts only to presses on the page itself or inside the
 *  card, never to typing in a field or in the terminal drawer, and never to shortcuts. */
export interface KeyOrigin {
  onBody: boolean
  inCard: boolean
  inTerminal: boolean
  editable: boolean
  modifier: boolean
}

export const keyAllowed = (origin: KeyOrigin): boolean =>
  !origin.modifier && !origin.inTerminal && !origin.editable && (origin.onBody || origin.inCard)

export type Hint = { kind: 'pick', last: number } | { kind: 'then', last: number } | { kind: 'other' } | { kind: 'none' }

/** The footer's key hint for the current step. */
export function hint(view: DecisionView, s: CardState): Hint {
  if (s.chosen || !['idle', 'entering', 'pre'].includes(s.phase)) return { kind: 'none' }
  if (s.otherOpen) return { kind: 'other' }
  if (s.picked !== null) return { kind: 'then', last: view.branches[s.picked]?.followUp?.choices.length ?? 0 }
  return { kind: 'pick', last: view.branches.length + 1 }
}

export interface SentStatus {
  title: string
  /** The node's state after the reply, from the server. */
  state: string | null
  /** The exact line typed into the agent's pane. */
  line: string
  /** Questions the same agent still waits on. */
  remaining: number
}

/** "Sent to {node}", the node's state after the reply and the line that was typed. */
export function sentStatus(snapshot: TaskSnapshot, question: QuestionDto, nodeLabel: string): SentStatus {
  const nodeKey = question.nodeKey ?? question.role
  const state = snapshot.nodes.find((node) => node.key === nodeKey)?.state ?? null
  const line = snapshot.questions.find((q) => q.uid === question.uid)?.answer ?? ''
  const remaining = snapshot.questions.filter((q) => q.actionable && (q.nodeKey ?? q.role) === nodeKey).length
  let title = `Sent to ${nodeLabel}`
  if (state === 'RUNNING') title += ' · running again'
  else if (state === 'WAITING_FOR_USER' && remaining) title += ` · waiting for ${remaining} more answer${remaining === 1 ? '' : 's'}`
  else if (state) title += ` · ${state.toLowerCase().replaceAll('_', ' ')}`
  return { title, state, line, remaining }
}

/** Oldest first across tasks; ties keep a stable order by task. */
export function inboxOrder(items: OpenQuestionDto[]): OpenQuestionDto[] {
  return [...items].sort((a, b) => a.createdAt.localeCompare(b.createdAt) || a.jiraKey.localeCompare(b.jiraKey))
}

/** "just now", "4m ago", "3h ago", "2d ago". */
export function ageLabel(createdAt: string, now = Date.now()): string {
  const seconds = Math.max(0, (now - Date.parse(createdAt)) / 1000)
  if (!Number.isFinite(seconds) || seconds < 60) return 'just now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`
  return `${Math.floor(seconds / 86400)}d ago`
}

/** A note's source as the card shows it: only an https URL becomes a link; a path stays text. */
export const sourceHref = (source: string | undefined): string | null =>
  source && /^https:\/\/\S+$/.test(source) ? source : null

// ------------------------------------------------------------------------------ connectors

export interface Box { left: number, top: number, right: number, bottom: number }

/** An elbow from (x0, y0) right to x1 at y1, turning at xm with rounded corners. */
export function elbow(x0: number, y0: number, xm: number, x1: number, y1: number): { d: string, len: number } {
  const dy = y1 - y0
  if (Math.abs(dy) < 1) return { d: `M ${x0} ${y0} H ${x1}`, len: Math.ceil(x1 - x0) + 2 }
  const r = Math.min(10, Math.abs(dy) / 2, Math.abs(xm - x0), Math.abs(x1 - xm))
  const sg = dy > 0 ? 1 : -1
  const d = `M ${x0} ${y0} H ${xm - r} Q ${xm} ${y0} ${xm} ${y0 + sg * r} V ${y1 - sg * r} Q ${xm} ${y1} ${xm + r} ${y1} H ${x1}`
  return { d, len: Math.ceil(Math.abs(x1 - x0) + Math.abs(dy)) + 2 }
}

/** Straight down from (x, y0) and right into x1 at y1. */
export function drop(x: number, y0: number, y1: number, x1: number): { d: string, len: number } {
  const r = Math.min(8, Math.abs(y1 - y0), Math.abs(x1 - x))
  return { d: `M ${x} ${y0} V ${y1 - r} Q ${x} ${y1} ${x + r} ${y1} H ${x1}`, len: Math.ceil(y1 - y0 + x1 - x) + 2 }
}
