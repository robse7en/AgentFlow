<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useConfigStore } from '@/stores/config'
import HarnessStatus from '@/components/HarnessStatus.vue'
import DirectoryPicker from '@/components/DirectoryPicker.vue'
import CostSettings from '@/components/CostSettings.vue';

const store = useConfigStore()
const message = ref<string | null>(null)

onMounted(() => { if (!store.config) store.load() })

const isDesktop = (window as Window & { __AGENTFLOW_DESKTOP__?: boolean })
    .__AGENTFLOW_DESKTOP__ === true
// The desktop asks for confirmation, then restarts into setup to choose another config.json.
const changeInstance = () => window.location.assign('/__desktop/change-instance')
// The desktop checks its update server and offers to install a newer version.
const checkForUpdates = () => window.location.assign('/__desktop/check-updates')

async function reload() {
  message.value = null
  try {
    await store.reload()
    message.value = 'Configuration reloaded.'
  } catch (e) {
    message.value = null
  }
}

const defaultRepositoryPath = ref('')
const showDefaultRepoPicker = ref(false)
const savingDefaultRepo = ref(false)
const defaultRepoError = ref<string | null>(null)

watch(() => store.config?.defaultRepositoryDirectory, (value) => { defaultRepositoryPath.value = value ?? '' }, { immediate: true })

const defaultRepoDirty = computed(() => defaultRepositoryPath.value.trim() !== (store.config?.defaultRepositoryDirectory ?? ''))

function pickDefaultRepo(path: string) {
  defaultRepositoryPath.value = path
  showDefaultRepoPicker.value = false
}

async function saveDefaultRepository() {
  defaultRepoError.value = null
  savingDefaultRepo.value = true
  try {
    await store.setDefaultRepository(defaultRepositoryPath.value.trim() || null)
  } catch (e) {
    defaultRepoError.value = (e as Error).message
  } finally {
    savingDefaultRepo.value = false
  }
}

const exchangeRateInput = ref('')
const savingExchangeRate = ref(false)
const exchangeRateError = ref<string | null>(null)

watch(() => store.config?.workflow.usdToNzdRate, (value) => {
  exchangeRateInput.value = value != null ? String(value) : ''
}, { immediate: true })

const exchangeRateDirty = computed(() => exchangeRateInput.value.trim() !== String(store.config?.workflow.usdToNzdRate ?? ''))

async function saveExchangeRate() {
  exchangeRateError.value = null
  const rate = Number(exchangeRateInput.value.trim())
  if (!Number.isFinite(rate) || rate <= 0) {
    exchangeRateError.value = 'Enter a positive number.'
    return
  }
  savingExchangeRate.value = true
  try {
    await store.setExchangeRate(rate)
  } catch (e) {
    exchangeRateError.value = (e as Error).message
  } finally {
    savingExchangeRate.value = false
  }
}

const roleLabels: Record<string, string> = {
  architect: 'Default Architect',
  // Unset, the API shows the Architect's settings here and the Architect plans Parallel tasks.
  orchestrator: 'Default Orchestrator (separate planner)',
  implementer: 'Default Implementer',
  codeReviewer: 'Default Code Reviewer',
  securityReviewer: 'Default Security Reviewer',
}
</script>

<template>
  <div class="page-narrow">
    <div class="panel">
      <div class="panel-title">
        <h2>Settings</h2>
        <div class="row">
          <button v-if="isDesktop" @click="checkForUpdates">Check for updates…</button>
          <button v-if="isDesktop" @click="changeInstance">Change instance…</button>
          <button @click="reload" :disabled="store.loading">Reload Configuration</button>
        </div>
      </div>
      <div v-if="store.error" class="alert error">{{ store.error }}</div>
      <div v-if="message" class="alert ok">{{ message }}</div>
      <p class="muted small">
        Edit <code>{{ store.config?.configPath }}</code> by hand, then reload. Invalid configuration is rejected with the validation error.
      </p>
      <p v-if="isDesktop" class="muted small">
        To use a different config.json, choose <strong>Change instance…</strong>. AgentFlow restarts into setup; the current instance's files stay where they are.
      </p>
      <dl class="kv" v-if="store.config">
        <dt>Config file</dt><dd class="mono">{{ store.config.configPath }}</dd>
        <dt>Data directory</dt><dd class="mono">{{ store.config.dataDirectory }}</dd>
        <dt>Database</dt><dd class="mono">{{ store.config.databasePath }}</dd>
        <dt>Worktree directory</dt><dd class="mono">{{ store.config.worktreeDirectory }}</dd>
        <dt>Task directory</dt><dd class="mono">{{ store.config.taskDirectory }}</dd>
        <dt>Python</dt><dd class="mono">{{ store.config.python }} ({{ store.config.platform }})</dd>
      </dl>
    </div>

    <div class="panel" v-if="store.config">
      <h3>New Task defaults</h3>
      <label class="field">
        <span>Default repository directory <span class="hint">optional — prefills the New Task form; still editable per task</span></span>
        <div class="input-row">
          <input type="text" v-model="defaultRepositoryPath" placeholder="D:\Repos\Acme" />
          <button type="button" @click="showDefaultRepoPicker = !showDefaultRepoPicker">Browse…</button>
        </div>
      </label>
      <DirectoryPicker v-if="showDefaultRepoPicker" :start="defaultRepositoryPath || null" @select="pickDefaultRepo" @cancel="showDefaultRepoPicker = false" />
      <div v-if="defaultRepoError" class="alert error">{{ defaultRepoError }}</div>
      <div class="row" style="justify-content: flex-end; margin-top: 0.5rem">
        <button type="button" class="primary" :disabled="!defaultRepoDirty || savingDefaultRepo" @click="saveDefaultRepository">
          {{ savingDefaultRepo ? 'Saving…' : 'Save' }}
        </button>
      </div>
    </div>

    <div class="grid-2">
      <div class="panel" v-if="store.config">
        <h3>Terminal</h3>
        <dl v-if="store.config.system.terminal.backend === 'embedded'" class="kv">
          <dt>Runs in</dt><dd>Built-in terminals (no install needed)</dd>
          <dt>Size</dt><dd class="mono">{{ store.config.zellij.headlessCols }}×{{ store.config.zellij.headlessRows }}</dd>
          <template v-if="isDesktop">
            <dt>Zellij</dt><dd>Set <span class="mono">zellij.useInDesktop</span> in config.json to run agents in Zellij instead (restart needed)</dd>
          </template>
        </dl>
        <dl v-else class="kv">
          <dt>Command</dt><dd class="mono">{{ store.config.zellij.command }}</dd>
          <dt>Session prefix</dt><dd class="mono">{{ store.config.zellij.sessionPrefix }}</dd>
          <dt>Headless size</dt><dd class="mono">{{ store.config.zellij.headlessCols }}×{{ store.config.zellij.headlessRows }} (Windows)</dd>
          <dt>Web client</dt><dd>{{ store.config.zellij.webClientEnabled ? store.config.zellij.webClientBaseUrl : 'disabled' }}</dd>
          <dt>Detected</dt><dd>{{ store.config.system.zellij.version ?? 'not found' }}</dd>
        </dl>
        <h3 style="margin-top: 1rem">Workflow</h3>
        <dl class="kv">
          <dt>Max review iterations</dt><dd>{{ store.config.workflow.maxReviewIterations }}</dd>
          <dt>Max requirements revisions</dt><dd>{{ store.config.workflow.maxRequirementsRevisions }}</dd>
          <dt>Implementation route</dt><dd>{{ store.config.workflow.implementationMode }}</dd>
          <dt>Global agent limit</dt><dd>{{ store.config.workflow.maxConcurrentAgents }}</dd>
          <dt>Automatic retries</dt><dd>{{ store.config.workflow.maxAutomaticRetries ? `up to ${store.config.workflow.maxAutomaticRetries} in a row after a harness exit` : 'off (set workflow.maxAutomaticRetries)' }}</dd>
          <dt>Verification commands</dt><dd>{{ store.config.workflow.verificationCommands }}</dd>
          <dt>Default execution mode</dt><dd>{{ store.config.workflow.defaultExecutionMode }}</dd>
          <dt>Requirements approval</dt><dd>{{ store.config.workflow.requireRequirementsApproval ? 'required' : 'automatic' }}</dd>
          <dt>Final approval</dt><dd>{{ store.config.workflow.requireFinalApproval ? 'required' : 'automatic' }}</dd>
          <dt>Commit checkpoints</dt><dd>{{ store.config.workflow.commitCheckpoints ? 'yes' : 'record HEAD only' }}</dd>
        </dl>
        <label class="field" style="margin-top: 0.75rem">
          <span>USD → NZD display rate <span class="hint">fixed approximation for showing run cost; not a live FX lookup</span></span>
          <div class="input-row">
            <input type="number" step="0.01" min="0.01" v-model="exchangeRateInput" style="max-width: 8rem" />
            <button type="button" class="primary" :disabled="!exchangeRateDirty || savingExchangeRate" @click="saveExchangeRate">
              {{ savingExchangeRate ? 'Saving…' : 'Save' }}
            </button>
          </div>
        </label>
        <div v-if="exchangeRateError" class="alert error">{{ exchangeRateError }}</div>
      </div>
      <div class="panel" v-if="store.config">
        <h3>Default agents</h3>
        <p v-for="harness in store.config.harnesses" :key="harness.key" class="small">
            {{ harness.displayName }} automatic workspace trust:
            {{ harness.automaticTrustWorkspaces?.join(', ') || 'disabled; approval stays with the CLI' }}
        </p>
        <table>
          <thead><tr><th>Role</th><th>Harness</th><th>Model</th><th>Reasoning</th></tr></thead>
          <tbody>
            <tr v-for="(agent, role) in store.config.agents" :key="role">
              <td>{{ roleLabels[role] ?? role }}</td>
              <td>{{ store.harnessByKey(agent.harness)?.displayName ?? agent.harness }}
                <span v-if="!store.harnessByKey(agent.harness)?.available" class="badge FAILED">unavailable</span></td>
              <td class="mono">{{ agent.model ?? 'harness default' }}</td>
              <td>{{ agent.reasoning ?? '—' }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <HarnessStatus />

    <div class="panel" v-if="store.config">
      <h3>Configured harness commands</h3>
      <table>
        <thead><tr><th>Key</th><th>Type</th><th>Command</th><th>Resolved</th></tr></thead>
        <tbody>
          <tr v-for="h in store.config.harnesses" :key="h.key">
            <td>{{ h.key }}</td><td>{{ h.type }}</td><td class="mono">{{ h.command }}</td>
            <td class="mono small">{{ h.resolvedPath ?? h.message }}</td>
          </tr>
        </tbody>
      </table>
    </div>
    <CostSettings />
  </div>
</template>
