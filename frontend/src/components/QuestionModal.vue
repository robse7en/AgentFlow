<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { RouterLink } from 'vue-router'
import DecisionCard from '@/components/DecisionCard.vue'
import { overlaysOpen } from '@/overlayStack'
import { useTaskStore } from '@/stores/tasks'
import { ageLabel } from '@/decisions'
import type { AnswerBody, QuestionDto, TaskSnapshot } from '@/api/types'

const props = defineProps<{ task: TaskSnapshot }>()
const emit = defineEmits<{ answered: [question: QuestionDto] }>()

const store = useTaskStore()

const openQuestions = computed(() =>
  props.task.questions
    .filter((question) => question.actionable)
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt)),
)

// Answered questions drop out of openQuestions, so "n of total" is tracked per batch: every uid
// seen since the list was last empty. A question answered elsewhere (e.g. in the agent's
// terminal or Slack) simply leaves the list and the dialog advances to the next one.
const batch = ref<string[]>([])
// The question just answered here: its card keeps showing the sent state (and the next card
// waits) although the snapshot the answer returned no longer lists it as open.
const held = ref<QuestionDto | null>(null)
// Its answer was delivered: the dot turns green while the card shows the sent state.
const sent = ref<string | null>(null)
watch([openQuestions, held], ([open, holding]) => {
  if (!open.length && !holding) {
    batch.value = []
    return
  }
  const fresh = open.map((question) => question.uid).filter((uid) => !batch.value.includes(uid))
  if (fresh.length) batch.value = [...batch.value, ...fresh]
}, { immediate: true })

const current = computed(() => held.value ?? openQuestions.value[0] ?? null)
const position = computed(() => (current.value ? batch.value.indexOf(current.value.uid) + 1 : 0))
const dots = computed(() => batch.value.map((uid) => {
  if (uid === current.value?.uid) return sent.value === uid ? 'on done' : 'on'
  return openQuestions.value.some((q) => q.uid === uid) ? '' : 'done'
}))

const nodeFor = (question: QuestionDto) => {
  const key = question.nodeKey ?? question.role
  return props.task.nodes.find((node) => node.key === key)
}
const nodeLabel = (question: QuestionDto): string =>
  nodeFor(question)?.label ?? (question.nodeKey ?? question.role).replaceAll('_', ' ')
const meta = (question: QuestionDto): string => {
  const node = nodeFor(question)
  const harness = node?.effectiveConfig?.harnessDisplayName ?? node?.lastRun?.harness
  const model = node?.lastRun?.model ?? node?.effectiveConfig?.model
  return [harness, model, ageLabel(question.createdAt)].filter(Boolean).join(' · ')
}

const card = ref<InstanceType<typeof DecisionCard> | null>(null)
// Questions arrive live, so only take focus when nothing else has it — otherwise a new question
// would steal keystrokes from e.g. the terminal drawer.
watch(() => current.value?.uid, async (uid) => {
  if (!uid) return
  await nextTick()
  card.value?.focus()
}, { immediate: true })

const submit = async (body: AnswerBody): Promise<TaskSnapshot> => {
  const question = current.value
  if (!question) throw new Error('No question is open')
  held.value = question
  try {
    return await store.answerQuestion(props.task.jiraKey, question.uid, body)
  } catch (cause) {
    held.value = null
    throw cause
  }
}

const done = () => {
  const question = held.value
  held.value = null
  sent.value = null
  if (question) emit('answered', question)
}
</script>

<template>
  <Transition name="question-overlay">
    <div v-if="current" class="question-overlay">
      <article class="question-dialog" role="dialog" aria-labelledby="question-dialog-title">
        <header class="question-dialog__head">
          <h3 id="question-dialog-title">{{ nodeLabel(current) }} needs a decision</h3>
          <div v-if="batch.length > 1" class="question-pager" aria-label="Questions in this batch">
            <span class="question-dots" aria-hidden="true"><span v-for="(cls, i) in dots" :key="batch[i]" class="question-dot" :class="cls" /></span>
            <span>{{ position }} of {{ batch.length }}</span>
          </div>
          <span v-else class="badge WAITING_FOR_USER">Input required</span>
        </header>
        <DecisionCard
          ref="card"
          :key="current.uid"
          :question="current"
          :node-label="nodeLabel(current)"
          :meta="meta(current)"
          :submit="submit"
          :active="!overlaysOpen"
          @answered="sent = current.uid"
          @done="done"
        >
          <template #footer>
            <RouterLink to="/decisions" class="small">Open in Decisions</RouterLink>
          </template>
        </DecisionCard>
      </article>
    </div>
  </Transition>
</template>
