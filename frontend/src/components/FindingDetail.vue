<script setup lang="ts">
// One review finding in full: its code with the reviewer's comment on it, the recommendation, the
// implementer's answer, the reviewer's verdict, and the rounds that raised it before. Pages
// through the Findings list with the header buttons or the arrow keys.
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { Finding } from '@/api/types'
import Modal from '@/components/Modal.vue'
import { location, shortSha } from '@/findingCode'
import { blocksNow, hasCode, statusLabel, typeLabel } from '@/findings'

// Loaded with the first code it draws, so highlight.js stays out of the main bundle.
const FindingCode = defineAsyncComponent(() => import('@/components/FindingCode.vue'))

const props = defineProps<{
  finding: Finding
  /** The earlier rows with this finding's id, oldest first. */
  earlier: Finding[]
  taskKey: string
  currentSha: string | null
  /** Where the finding sits in the list as filtered (-1 when the filters now hide it). */
  index: number
  total: number
}>()
const emit = defineEmits<{ close: []; step: [delta: number] }>()

const modal = ref<InstanceType<typeof Modal> | null>(null)
const body = ref<HTMLElement | null>(null)
const f = computed(() => props.finding)
const canPrev = computed(() => props.index > 0)
const canNext = computed(() => props.index < props.total - 1)

const statusTone = (s: Finding['status']) => (s === 'open' ? 'WAITING_FOR_USER' : s === 'resolved' ? 'COMPLETED' : '')

function onKeydown(e: KeyboardEvent) {
  if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return
  if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || !modal.value?.isTop) return
  // Arrows keep their meaning in a field and in the code, which scrolls sideways.
  const target = e.target as HTMLElement | null
  if (target?.closest('input, textarea, select, [contenteditable="true"], .code-scroll')) return
  const delta = e.key === 'ArrowLeft' ? -1 : 1
  if (delta < 0 ? !canPrev.value : !canNext.value) return
  e.preventDefault()
  emit('step', delta)
}
onMounted(() => window.addEventListener('keydown', onKeydown))
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))

// A new finding starts at its top, not wherever the last one was scrolled to.
watch(() => f.value.id, () => {
  void nextTick(() => body.value?.closest('.modal-dialog__body')?.scrollTo({ top: 0 }))
})
</script>

<template>
  <Modal ref="modal" :title="`${f.id} · ${f.title}`" @close="emit('close')">
    <template #actions>
      <span v-if="index >= 0" class="muted small finding-pos">{{ index + 1 }} of {{ total }}</span>
      <button class="sm" :disabled="!canPrev" title="Previous finding (←)" aria-label="Previous finding" @click="emit('step', -1)">‹</button>
      <button class="sm" :disabled="!canNext" title="Next finding (→)" aria-label="Next finding" @click="emit('step', 1)">›</button>
    </template>

    <div ref="body" class="finding-detail">
      <div class="finding-meta">
        <span class="badge" :class="f.severity">{{ f.severity }}</span>
        <span v-if="blocksNow(f)" class="badge FAILED">blocking</span>
        <span class="badge" :class="statusTone(f.status)">{{ statusLabel(f) }}</span>
        <span v-if="f.occurrence > 1" class="badge WAITING_FOR_USER" title="Review rounds that raised it">raised {{ f.occurrence }}×</span>
        <span class="muted small">
          {{ typeLabel(f.type) }} · iteration {{ f.iteration }}<template v-if="f.reviewedSha"> · reviewed @ <span class="mono">{{ shortSha(f.reviewedSha) }}</span></template>
          <template v-if="f.blocking && !blocksNow(f)"> · was blocking</template>
        </span>
      </div>

      <FindingCode v-if="hasCode(f)" :key="f.dbId" wide :task-key="taskKey" :finding="f" :current-sha="currentSha">
        <div class="finding-text">{{ f.description || f.title }}</div>
      </FindingCode>
      <template v-else>
        <div v-if="f.file" class="mono muted small">{{ location(f.file, f.line, f.endLine) }}</div>
        <div v-if="f.description" class="finding-text finding-lead">{{ f.description }}</div>
      </template>

      <section v-if="f.recommendation" class="finding-block">
        <h4>Recommendation</h4>
        <div class="finding-text">{{ f.recommendation }}</div>
      </section>
      <section v-if="f.response" class="finding-block">
        <h4>Implementer · {{ f.response }}</h4>
        <div class="finding-text">{{ f.responseNote || 'No note.' }}</div>
      </section>
      <section v-if="f.resolution" class="finding-block">
        <h4>Reviewer evidence</h4>
        <div class="finding-text">{{ f.resolution }}</div>
      </section>

      <section v-if="earlier.length" class="finding-block">
        <h4>Earlier rounds</h4>
        <ol class="finding-history">
          <li v-for="r in earlier" :key="r.dbId">
            <div class="finding-history__head">
              <b>Iteration {{ r.iteration }}</b>
              <span class="badge" :class="r.severity">{{ r.severity }}</span>
              <span v-if="r.blocking" class="muted small">blocking</span>
              <span class="badge" :class="statusTone(r.status)">{{ r.status }}</span>
              <span v-if="r.reviewedSha" class="muted small mono">@ {{ shortSha(r.reviewedSha) }}</span>
            </div>
            <div v-if="r.title !== f.title" class="finding-text">{{ r.title }}</div>
            <div v-if="r.response" class="finding-text"><b>Implementer {{ r.response }}:</b> {{ r.responseNote }}</div>
            <div v-if="r.resolution" class="finding-text"><b>Reviewer:</b> {{ r.resolution }}</div>
          </li>
        </ol>
      </section>
    </div>
  </Modal>
</template>

<style scoped>
.finding-pos { margin-right: 0.25rem; white-space: nowrap; }
.finding-detail { display: flex; flex-direction: column; gap: 0.9rem; }
.finding-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem; }
.finding-text { white-space: pre-wrap; font-size: 13px; line-height: 1.55; }
.finding-lead { font-size: 13.5px; }
.finding-block h4 { margin: 0 0 0.3rem; }
.finding-block { border-top: 1px solid var(--border); padding-top: 0.75rem; }
.finding-detail :deep(.code-view) { margin-top: 0; }
.finding-history { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.6rem; }
.finding-history li { border-left: 2px solid var(--border); padding-left: 0.75rem; display: flex; flex-direction: column; gap: 0.25rem; }
.finding-history__head { display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem; font-size: 13px; }
</style>
