<script setup lang="ts">
// Lessons agents proposed with `flowctl lesson`. Approving one appends it (as edited here) to the
// role's .agentflow-instructions file in the task's original repository checkout: saved, never
// committed, and seen by no agent until a human commits it. Lesson text is agent output, so it is
// only ever shown as plain text (never Markdown or HTML).
import { computed, reactive, ref } from 'vue'
import { api } from '@/api/client'
import type { Lesson } from '@/api/types'

const props = defineProps<{ lessons: Lesson[]; taskKey: string; repositoryRoot: string }>()

const MAX_CHARS = 300
const proposed = computed(() => props.lessons.filter((lesson) => lesson.status === 'proposed'))
const decided = computed(() => props.lessons.filter((lesson) => lesson.status !== 'proposed').reverse())
const drafts = reactive<Record<string, string>>({})
const draft = (lesson: Lesson): string => drafts[lesson.uid] ?? lesson.text
const busy = ref<string | null>(null)
const errors = reactive<Record<string, string>>({})
const saved = ref<{ path: string; dirtyPaths: string[] } | null>(null)
const roleFile = (role: string) => `.agentflow-instructions/${role.replaceAll('_', '-')}.md`

async function decide(lesson: Lesson, approve: boolean) {
  busy.value = lesson.uid
  delete errors[lesson.uid]
  try {
    const result = approve
      ? await api.approveLesson(props.taskKey, lesson.uid, draft(lesson))
      : await api.dismissLesson(props.taskKey, lesson.uid)
    if (approve && result.lesson.path) saved.value = { path: result.lesson.path, dirtyPaths: result.dirtyPaths }
    delete drafts[lesson.uid]
  } catch (cause) {
    errors[lesson.uid] = cause instanceof Error ? cause.message : String(cause)
  } finally {
    busy.value = null
  }
}
</script>

<template>
  <div class="lessons">
    <p class="muted small">
      Agents propose lessons with <span class="mono">flowctl lesson</span>. Approving one adds it to that role's
      guidance in <span class="mono">{{ repositoryRoot }}</span>, uncommitted. No agent sees it until you commit the
      file; new tasks then pick it up. Nothing here changes this task's runs.
    </p>
    <div v-if="saved" class="alert small">
      Saved to <span class="mono">{{ saved.path }}</span>, not committed. Review and commit it in that checkout.
      <template v-if="saved.dirtyPaths.length">
        Uncommitted there now: <span class="mono">{{ saved.dirtyPaths.join(', ') }}</span>
      </template>
    </div>
    <p v-if="!lessons.length" class="muted small">No lessons proposed yet.</p>

    <div v-for="lesson in proposed" :key="lesson.uid" class="lesson card">
      <div class="small muted">
        {{ lesson.nodeLabel }}, run #{{ lesson.runNumber }} → <span class="mono">{{ roleFile(lesson.role) }}</span>
      </div>
      <textarea :value="draft(lesson)" rows="3" :maxlength="MAX_CHARS * 2" :disabled="busy === lesson.uid"
        @input="drafts[lesson.uid] = ($event.target as HTMLTextAreaElement).value" />
      <div class="actions">
        <button class="sm primary" :disabled="busy === lesson.uid || !draft(lesson).trim()" @click="decide(lesson, true)">
          Approve into guidance
        </button>
        <button class="sm" :disabled="busy === lesson.uid" @click="decide(lesson, false)">Dismiss</button>
        <span class="hint small">One line, at most {{ MAX_CHARS }} characters; line breaks become spaces.</span>
      </div>
      <p v-if="errors[lesson.uid]" class="alert error small">{{ errors[lesson.uid] }}</p>
    </div>

    <details v-if="decided.length" class="decided">
      <summary class="small">Decided ({{ decided.length }})</summary>
      <div v-for="lesson in decided" :key="lesson.uid" class="small decided-item">
        <span class="badge" :class="lesson.status === 'approved' ? 'COMPLETED' : 'CANCELLED'">{{ lesson.status }}</span>
        {{ lesson.approvedText ?? lesson.text }}
        <span class="muted">({{ lesson.nodeLabel }}<template v-if="lesson.path">, {{ lesson.path }}</template>)</span>
      </div>
    </details>
  </div>
</template>

<style scoped>
.lesson { margin: 0.5rem 0; padding: 0.5rem; display: grid; gap: 0.35rem; }
.lesson textarea { width: 100%; resize: vertical; }
.actions { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
.decided { margin-top: 0.75rem; }
.decided-item { margin: 0.3rem 0; overflow-wrap: anywhere; }
</style>
