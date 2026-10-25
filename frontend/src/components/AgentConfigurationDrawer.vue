<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import AgentReadiness from '@/components/AgentReadiness.vue';
import type { WorkflowNodeDto } from '@/api/types'
import { useConfigStore } from '@/stores/config'
import { useTaskStore } from '@/stores/tasks'

const props = defineProps<{ node: WorkflowNodeDto; taskKey: string }>()
const configStore = useConfigStore()
const tasks = useTaskStore()

const form = reactive({ harness: '', model: '', reasoning: '' })
const busy = ref(false)
const error = ref<string | null>(null)
const saved = ref(false)
const approvalReason = ref('')
const readinessConfirmed = ref(false);
const needsReadiness = computed(() => !props.node.lastRun
    || (form.harness && form.harness !== props.node.effectiveConfig?.harness)
    || !selectedHarness.value?.available);
const launchReady = computed(() => !needsReadiness.value || readinessConfirmed.value);

const recover = async (retry: boolean) => {
    busy.value = true;
    error.value = null;
    try {
        await tasks.recoverNode(props.taskKey, props.node.key, retry);
    } catch (cause) {
        error.value = (cause as Error).message;
    } finally {
        busy.value = false;
    }
};

watch(() => props.node, (node) => {
  form.harness = node.configOverride.harness ?? ''
  form.model = node.configOverride.model ?? ''
  form.reasoning = node.configOverride.reasoning ?? ''
  saved.value = false
}, { immediate: true })

const canRun = computed(() => ['READY', 'FAILED'].includes(props.node.state))
const selectedHarness = computed(() => configStore.harnessByKey(form.harness || props.node.effectiveConfig?.harness || ''))
const finalHarness = computed(() => form.harness || props.node.effectiveConfig?.harness || '')
const finalModel = computed(() => form.model || (finalHarness.value === props.node.effectiveConfig?.harness
  ? props.node.effectiveConfig?.model : selectedHarness.value?.defaultModel) || null)
const requiresApproval = computed(() => {
  if (selectedHarness.value?.billingMode === 'subscription') return false
  const controls = configStore.config?.workflow.costControls as {
    requireMeteredApproval?: boolean;
    copilotRoles?: string[];
  } | undefined
  return controls?.requireMeteredApproval === true
    || (props.node.role != null && controls?.copilotRoles?.includes(props.node.role) === true)
})

function payload() {
  return { harness: form.harness || null, model: form.model || null, reasoning: form.reasoning || null }
}

async function save() {
  busy.value = true
  error.value = null
  try {
    await tasks.setNodeConfig(props.taskKey, props.node.key, payload())
    saved.value = true
  } catch (e) {
    error.value = (e as Error).message
  } finally {
    busy.value = false
  }
}

async function saveAndRun() {
  if (!launchReady.value) return;
  busy.value = true
  error.value = null
  try {
    const approval = requiresApproval.value
      ? { harness: finalHarness.value, model: finalModel.value, reason: approvalReason.value.trim() }
      : undefined
    const snapshot = await tasks.runNode(props.taskKey, props.node.key, payload(), approval)
    const updated = snapshot.nodes.find((n) => n.key === props.node.key)
    if (updated && updated.state !== 'RUNNING') {
      error.value = updated.message ?? 'Run did not start; no error was reported.'
    } else {
      approvalReason.value = ''
    }
  } catch (e) {
    error.value = (e as Error).message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div>
    <p class="muted small">{{ node.description }}</p>
    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="saved" class="alert ok">Override saved for this node. Defaults come from config.json; this applies only to {{ node.label }} in {{ taskKey }}.</div>

    <label class="field">
      <span>Harness</span>
      <select v-model="form.harness">
        <option value="">Default ({{ node.effectiveConfig?.harnessDisplayName }})</option>
        <option v-for="h in configStore.harnesses" :key="h.key" :value="h.key" :disabled="!h.available || !h.enabled">
          {{ h.displayName }}{{ h.available && h.enabled ? '' : ' — unavailable' }}
        </option>
      </select>
    </label>
    <label class="field">
      <span>Model <span class="hint">leave empty for the harness default</span></span>
      <select v-if="selectedHarness && selectedHarness.availableModels.length" v-model="form.model" :disabled="selectedHarness && !selectedHarness.supportsModel">
        <option value="">Default ({{ node.effectiveConfig?.model ?? selectedHarness.defaultModel ?? 'harness default' }})</option>
        <option v-for="m in selectedHarness.availableModels" :key="m" :value="m">{{ m }}</option>
      </select>
      <input v-else type="text" v-model="form.model" :placeholder="node.effectiveConfig?.model ?? 'harness default'" :disabled="selectedHarness && !selectedHarness.supportsModel" />
    </label>
    <label class="field">
      <span>Reasoning</span>
      <select v-model="form.reasoning" :disabled="selectedHarness && !selectedHarness.supportsReasoning">
        <option value="">Default ({{ node.effectiveConfig?.reasoning ?? '—' }})</option>
        <option value="low">Low</option>
        <option value="medium">Medium</option>
        <option value="high">High</option>
      </select>
    </label>
    <AgentReadiness v-if="canRun && needsReadiness" :harness="finalHarness"
        :task-key="taskKey" @ready="readinessConfirmed = $event" />
    <label v-if="requiresApproval && canRun" class="field">
      <span>Approve API launch: {{ finalHarness }} / {{ finalModel ?? 'default model' }}. Enter a reason.</span>
      <input v-model="approvalReason" maxlength="1000" :disabled="busy" />
    </label>
    <div class="row">
      <template v-if="node.state === 'INTERRUPTED'">
        <button :disabled="busy" @click="recover(false)">Resume existing agent</button>
        <button :disabled="busy" @click="recover(true)">Stop &amp; retry</button>
      </template>
      <button :disabled="busy" @click="save">Save override</button>
      <button class="primary" :disabled="busy || !canRun || !launchReady || (requiresApproval && !approvalReason.trim())" @click="saveAndRun">Save &amp; {{ node.state === 'FAILED' ? 'Retry' : 'Run' }}</button>
    </div>

    <template v-if="node.lastRun">
      <h4 style="margin-top: 1.2rem">Last run #{{ node.lastRun.runNumber }}</h4>
      <dl class="kv small">
        <dt>State</dt><dd><span class="badge" :class="node.lastRun.state">{{ node.lastRun.state }}</span></dd>
        <dt>Harness</dt><dd>{{ node.lastRun.harness }} <span class="muted">/ {{ node.lastRun.model ?? 'default model' }} / {{ node.lastRun.reasoning ?? '—' }}</span></dd>
        <dt>Started</dt><dd>{{ node.lastRun.startedAt ? new Date(node.lastRun.startedAt).toLocaleString() : '—' }}</dd>
        <dt>Completed</dt><dd>{{ node.lastRun.completedAt ? new Date(node.lastRun.completedAt).toLocaleString() : '—' }}</dd>
        <dt v-if="node.lastRun.exitCode != null">Exit code</dt><dd v-if="node.lastRun.exitCode != null">{{ node.lastRun.exitCode }}</dd>
        <dt>Run dir</dt><dd class="mono">{{ node.lastRun.runDir }}</dd>
      </dl>
      <div v-if="node.lastRun.error" class="alert error small" style="white-space: pre-wrap">{{ node.lastRun.error }}</div>
      <details v-if="node.lastRun.result" style="margin-top: 0.5rem">
        <summary class="small">Result JSON</summary>
        <pre>{{ JSON.stringify(node.lastRun.result, null, 2) }}</pre>
      </details>
    </template>
  </div>
</template>
