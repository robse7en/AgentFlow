<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { TaskSnapshot } from '@/api/types'
import { useTaskStore } from '@/stores/tasks'
import MarkdownView from '@/components/MarkdownView.vue'
import Modal from '@/components/Modal.vue'

const props = defineProps<{ task: TaskSnapshot }>()
const store = useTaskStore()
const busy = ref(false)
const error = ref<string | null>(null)
const showFeedback = ref(false)
const feedback = ref('')

const sections: { key: string; label: string }[] = [
  { key: 'requirements', label: 'Requirements' },
  { key: 'acceptanceCriteria', label: 'Acceptance criteria' },
  { key: 'assumptions', label: 'Assumptions' },
  { key: 'implementationPlan', label: 'Proposed implementation' },
  { key: 'risks', label: 'Known risks' },
]

const openSection = ref<string | null>(null)
const activeSection = computed(() => sections.find((s) => s.key === openSection.value) ?? null)

// Close any open section modal if the underlying task changes out from under it (e.g. navigating
// between tasks reuses this component instance), so it never keeps showing another task's content.
watch(() => props.task.jiraKey, () => { openSection.value = null })

const gate = computed(() => props.task.nodes.find((n) => n.key === 'requirements_gate'))
const architectNode = computed(() => props.task.nodes.find((n) => n.key === 'architect'))
const revisionsLeft = computed(() => props.task.maxRequirementsRevisions - (architectNode.value?.iteration ?? 0))
const awaitingApproval = computed(() => gate.value?.state === 'WAITING_FOR_APPROVAL'
    || (gate.value?.state === 'COMPLETED' && !props.task.requirementsDigest));
// Unlike awaitingApproval, this excludes the legacy (pre-snapshot) COMPLETED case: there is no
// draft waiting on the backend for it to send back, so request-changes would just 409.
const canRequestChanges = computed(() => gate.value?.state === 'WAITING_FOR_APPROVAL')
const openQuestions = computed(() => props.task.questions.filter((q) => q.actionable))

// Close the feedback panel if the gate leaves WAITING_FOR_APPROVAL out from under it (e.g. the
// user approves instead, or another client approves it over the websocket while this is open).
watch(canRequestChanges, (can) => {
  if (!can) {
    showFeedback.value = false
    feedback.value = ''
  }
})

async function act(fn: () => Promise<unknown>) {
  busy.value = true
  error.value = null
  try {
    await fn()
  } catch (e) {
    error.value = (e as Error).message
  } finally {
    busy.value = false
  }
}
const approve = () => act(() => store.approveNode(props.task.jiraKey, 'requirements_gate'))
const openArchitect = () => act(() => store.openZellij(props.task.jiraKey, 'architect'))
const requestChanges = () => act(async () => {
  const result = await store.requestRequirementsChanges(props.task.jiraKey, feedback.value)
  feedback.value = ''
  showFeedback.value = false
  return result
})
</script>

<template>
  <div>
    <div v-if="task.requirementsDigest && task.requirementsDigest !== task.requirementsDraftDigest"
        class="alert error">
        These documents differ from the approved scope. Restore the approved version before continuing.
    </div>
    <details v-if="task.approvedRequirements">
        <summary>Approved requirements snapshot</summary>
        <MarkdownView v-for="section in sections" :key="section.key"
            :source="task.approvedRequirements[section.key]" />
    </details>
    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="awaitingApproval" class="alert warn">
      Review the Architect's output. Its AgentFlow pane is read-only; attach manually for freeform discussion if it needs
      more work. Implementation cannot start before approval.
    </div>
    <div class="row" style="margin-bottom: 0.75rem">
      <button class="success" v-if="awaitingApproval" :disabled="busy" @click="approve">Approve requirements</button>
      <button v-if="canRequestChanges && !showFeedback" :disabled="busy || revisionsLeft <= 0"
          @click="showFeedback = true">Request changes</button>
      <button :disabled="busy" @click="openArchitect">View Architect pane</button>
    </div>
    <p v-if="canRequestChanges && revisionsLeft <= 0" class="muted small">
      Maximum requirement revisions reached ({{ task.maxRequirementsRevisions }}). Approve as-is or edit the
      documents directly.
    </p>
    <div v-if="showFeedback" class="field" style="margin-bottom: 0.75rem">
      <label>
        <span>What should the Architect change? ({{ revisionsLeft }} revision(s) left)</span>
        <textarea v-model="feedback" maxlength="4000" required
            placeholder="e.g. The acceptance criteria miss the mobile case; also check the shared auth library." />
      </label>
      <div class="row">
        <button class="success" :disabled="busy || !canRequestChanges || !feedback.trim()"
            @click="requestChanges">Send back to Architect</button>
        <button :disabled="busy" @click="showFeedback = false; feedback = ''">Cancel</button>
      </div>
    </div>
    <details v-if="task.requirementsFeedback.length" style="margin-bottom: 0.75rem">
      <summary>Revision history ({{ task.requirementsFeedback.length }})</summary>
      <ul class="small">
        <li v-for="entry in task.requirementsFeedback" :key="entry.iteration">
          <b>Round {{ entry.iteration }}</b>
          <span class="muted"> — {{ new Date(entry.at).toLocaleString() }}</span>
          <div>{{ entry.feedback }}</div>
        </li>
      </ul>
    </details>
    <template v-if="openQuestions.length">
      <h4>Outstanding questions</h4>
      <ul class="small"><li v-for="q in openQuestions" :key="q.uid">{{ q.message }}</li></ul>
    </template>
    <ul class="section-list">
      <li v-for="s in sections" :key="s.key">
        <button type="button" class="section-list__item" :disabled="!task.requirements[s.key]?.trim()"
            @click="openSection = s.key">
          <b>{{ s.label }}</b>
          <span class="muted small" v-if="!task.requirements[s.key]?.trim()">(not written yet)</span>
          <span class="muted small" v-else>View &rarr;</span>
        </button>
      </li>
    </ul>
    <Modal v-if="activeSection" :title="activeSection.label" @close="openSection = null">
      <MarkdownView :source="task.requirements[activeSection.key]" />
    </Modal>
  </div>
</template>

<style scoped>
.section-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
}

.section-list__item {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    text-align: left;
    background: var(--panel);
}

.section-list__item:hover:not(:disabled) {
    background: var(--hover);
}
</style>
