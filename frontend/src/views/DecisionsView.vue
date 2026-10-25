<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { RouterLink } from 'vue-router'
import DecisionCard from '@/components/DecisionCard.vue'
import { overlaysOpen } from '@/overlayStack'
import { ageLabel } from '@/decisions'
import { useConfigStore } from '@/stores/config'
import { useDecisionStore } from '@/stores/decisions'
import { useTaskStore } from '@/stores/tasks'
import type { AnswerBody, OpenQuestionDto, TaskSnapshot } from '@/api/types'

// Open agent questions across tasks, one card at a time, oldest first. "Next decision" moves the
// current one to the back of the queue without answering it.
const decisions = useDecisionStore()
const tasks = useTaskStore()
const config = useConfigStore()
onMounted(() => decisions.start())

const answeredHere = ref<string[]>([])
const skipped = ref<string[]>([])
const held = ref<OpenQuestionDto | null>(null)
const sent = ref<string | null>(null)

const queue = computed(() => {
  const open = decisions.items.filter((q) => !answeredHere.value.includes(q.uid))
  const rank = (q: OpenQuestionDto) => skipped.value.indexOf(q.uid)
  return [...open].sort((a, b) => rank(a) - rank(b))
})
const current = computed(() => held.value ?? queue.value[0] ?? null)
const waiting = computed(() => queue.value.length)
const total = computed(() => answeredHere.value.length + waiting.value + (held.value && !answeredHere.value.includes(held.value.uid) ? 1 : 0))
const done = computed(() => decisions.loaded && !current.value)
const dots = computed(() => {
  const order = [...answeredHere.value, ...queue.value.map((q) => q.uid)]
  if (held.value && !order.includes(held.value.uid)) order.splice(answeredHere.value.length, 0, held.value.uid)
  return order.map((uid) => ({
    uid,
    cls: uid === current.value?.uid ? (sent.value === uid ? 'on done' : 'on') : answeredHere.value.includes(uid) ? 'done' : '',
  }))
})
const position = computed(() => dots.value.findIndex((d) => d.uid === current.value?.uid) + 1)
const subtitle = computed(() =>
  done.value && answeredHere.value.length
    ? `${answeredHere.value.length} of ${answeredHere.value.length} decided`
    : `${waiting.value} waiting for you`,
)

const meta = (q: OpenQuestionDto) => {
  const harness = q.harness ? (config.harnessByKey(q.harness)?.displayName ?? q.harness) : null
  return [harness, q.model, ageLabel(q.createdAt)].filter(Boolean).join(' · ')
}

const card = ref<InstanceType<typeof DecisionCard> | null>(null)
watch(() => current.value?.uid, async (uid) => {
  if (!uid) return
  await nextTick()
  card.value?.focus()
})

const submit = async (body: AnswerBody): Promise<TaskSnapshot> => {
  const question = current.value
  if (!question) throw new Error('No decision is open')
  held.value = question
  try {
    return await tasks.answerQuestion(question.jiraKey, question.uid, body)
  } catch (cause) {
    held.value = null
    throw cause
  }
}

const onAnswered = () => {
  if (!held.value) return
  sent.value = held.value.uid
  answeredHere.value = [...answeredHere.value, held.value.uid]
}

const onDone = () => {
  held.value = null
  sent.value = null
  void decisions.load()
}

const next = () => {
  const uid = current.value?.uid
  if (!uid || held.value || waiting.value < 2) return
  skipped.value = [...skipped.value.filter((s) => s !== uid), uid]
}
</script>

<template>
  <section class="decisions" aria-labelledby="decisions-title">
    <header class="decisions__head">
      <h1 id="decisions-title">
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true" class="decisions__icon"><circle cx="5" cy="12" r="2.2" /><circle cx="18" cy="6" r="2.2" /><circle cx="18" cy="18" r="2.2" /><path d="M7 12h4c2 0 2-6 5-6M11 12c2 0 2 6 5 6" /></svg>
        Decisions <span :key="waiting" class="decisions__sub bump">{{ subtitle }}</span>
      </h1>
      <div v-if="!done && dots.length > 1" class="question-pager" aria-label="Decisions">
        <span class="question-dots" aria-hidden="true"><span v-for="d in dots" :key="d.uid" class="question-dot" :class="d.cls" /></span>
        <span>{{ position }} of {{ total }}</span>
      </div>
    </header>

    <p v-if="decisions.error" class="alert error small">{{ decisions.error }}</p>

    <div v-if="done" class="decisions__done" role="status">
      <div class="decisions__done-icon"><svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 2.5l2.4 1.8 3-.2.9 2.9 2.5 1.7-1 2.8 1 2.8-2.5 1.7-.9 2.9-3-.2L12 21.5l-2.4-1.8-3 .2-.9-2.9L3.2 15.3l1-2.8-1-2.8 2.5-1.7.9-2.9 3 .2z" /><path d="M8.5 12.2l2.4 2.3 4.6-4.8" /></svg></div>
      <h2>All decided</h2>
      <p>New questions from your agents show up here.</p>
      <RouterLink to="/">Back to tasks</RouterLink>
    </div>

    <div v-else-if="current" class="decisions__stage">
      <DecisionCard
        ref="card"
        :key="current.uid"
        :question="current"
        :node-label="current.nodeLabel"
        :meta="meta(current)"
        :submit="submit"
        :can-next="waiting > 1 && !held"
        :active="!overlaysOpen"
        @answered="onAnswered"
        @done="onDone"
        @next="next"
      >
        <template #task>
          <RouterLink :to="`/tasks/${encodeURIComponent(current.jiraKey)}`" class="decisions__task mono">
            {{ current.jiraKey }}<template v-if="current.jiraTitle"> · {{ current.jiraTitle }}</template>
          </RouterLink>
        </template>
        <template #footer>
          <RouterLink :to="`/tasks/${encodeURIComponent(current.jiraKey)}`" class="small">Open {{ current.jiraKey }}</RouterLink>
        </template>
      </DecisionCard>
    </div>

    <p v-else class="muted">Loading decisions…</p>
  </section>
</template>

<style scoped>
.decisions { display: flex; flex-direction: column; gap: 18px; min-width: 0; }
.decisions__head { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
.decisions__head h1 { margin: 0; font-size: 18px; font-weight: 600; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.decisions__icon { color: var(--accent); }
.decisions__sub { color: var(--muted); font-weight: 400; font-size: 14px; display: inline-block; }
.bump { animation: decisions-bump 0.45s var(--spring); }
@keyframes decisions-bump { 0% { transform: scale(1); } 35% { transform: scale(1.25); } 100% { transform: scale(1); } }
.decisions__stage {
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-lg);
  padding: 20px 26px 16px; box-shadow: 0 8px 24px var(--shadow-2); overflow-x: auto;
}
.decisions__stage :deep(.dc-foot) { border-top: 1px solid var(--border); padding-top: 12px; }
.decisions__task { font-size: 12.5px; }
.decisions__done { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; text-align: center; padding: 80px 0; }
.decisions__done h2 { margin: 8px 0 0; font-size: 18px; animation: decisions-rise 0.45s 0.15s var(--spring) both; }
.decisions__done p { margin: 0 0 12px; color: var(--muted); animation: decisions-rise 0.45s 0.25s var(--spring) both; }
.decisions__done a { animation: decisions-rise 0.45s 0.35s var(--spring) both; }
.decisions__done-icon {
  width: 60px; height: 60px; border-radius: 16px; border: 1px solid var(--ok-border); background: var(--panel);
  display: grid; place-items: center; color: var(--ok); animation: decisions-badge 0.6s var(--spring) both;
}
@keyframes decisions-badge {
  0% { opacity: 0; transform: scale(0.6) rotate(-12deg); box-shadow: 0 0 0 0 var(--ok-soft); }
  60% { opacity: 1; transform: scale(1.06) rotate(0); box-shadow: 0 0 0 10px var(--ok-soft); }
  100% { transform: scale(1); box-shadow: 0 0 0 5px var(--ok-soft); }
}
@keyframes decisions-rise { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: none; } }
@media (prefers-reduced-motion: reduce) {
  .bump, .decisions__done h2, .decisions__done p, .decisions__done a, .decisions__done-icon { animation: none; }
}
</style>
