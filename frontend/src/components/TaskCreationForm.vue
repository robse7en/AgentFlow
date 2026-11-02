<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, ApiError } from '@/api/client'
import type { AgentOverride, JiraSearchResult, Project, RepositoryValidation, RouteEstimate } from '@/api/types'
import { describeTotals } from '@/estimates'
import { useUsdToNzdRate } from '@/usage'
import { useTaskStore } from '@/stores/tasks'
import { useConfigStore } from '@/stores/config'
import { useOperationStore } from '@/stores/operations'
import { useProjectStore } from '@/stores/projects'
import { pendingDraft, type TaskDraft } from '@/taskDraft'
import { projectColor, samePath } from '@/projects'
import DirectoryPicker from './DirectoryPicker.vue'
import AgentOverridesTable, { cleanOverrides, emptyOverrides } from './AgentOverridesTable.vue'

const router = useRouter()
const route = useRoute()
const tasks = useTaskStore()
const operations = useOperationStore()
const configStore = useConfigStore()
const projects = useProjectStore()
const taskSource = ref<'local' | 'jira'>('local');
const prompt = ref('');
const implementationMode = ref<'direct' | 'parallel' | ''>('');

const form = reactive({
  jiraKey: '',
  jiraTitle: '',
  repositoryPath: '',
  projectRelativePath: '',
  baseBranch: '',
  additionalContext: '',
})
const overrides = reactive<Record<string, AgentOverride>>(emptyOverrides())

// The project, and whether the user chose it (picked, preselected or restored): a chosen project
// is never replaced by a suggestion, and only choosing one fills the form.
const projectId = ref<number | null>(null)
const projectChosen = ref(false)
const suggestedFrom = ref<string | null>(null)
const project = computed<Project | null>(() => (projectId.value == null ? null : projects.byId.get(projectId.value) ?? null))

// What the form filled in itself (the default repository, the current branch, a project's
// defaults) and from where: a field still holding that value may be filled again, while anything
// the user typed stays. `from` names the project behind the hint, or null for settings and git.
type FilledField = 'repositoryPath' | 'projectRelativePath' | 'baseBranch'
const filled = reactive<Partial<Record<FilledField, { value: string; from: string | null }>>>({})
const filledOverrides = reactive<Record<string, string>>({})
const fill = (field: FilledField, value: string, from: string | null) => {
  form[field] = value
  filled[field] = { value, from }
}
const filledFrom = (field: FilledField): string | null =>
  filled[field]?.value === form[field] ? filled[field]?.from ?? null : null
const replaceable = (field: FilledField) => !form[field] || filled[field]?.value === form[field]

watch(() => configStore.config?.defaultRepositoryDirectory, (value) => {
  if (value && !form.repositoryPath) fill('repositoryPath', value, null)
}, { immediate: true })

/** Copy the project's defaults into every field the user has not typed into. */
function applyProject(chosen: Project | null) {
  if (!chosen) return
  const name = chosen.name
  if (chosen.defaultRepositoryPath && replaceable('repositoryPath')) fill('repositoryPath', chosen.defaultRepositoryPath, name)
  if (replaceable('projectRelativePath')) fill('projectRelativePath', chosen.defaultProjectRelativePath ?? '', name)
  if (replaceable('baseBranch')) fill('baseBranch', chosen.defaultBaseBranch ?? '', name)
  for (const role of Object.keys(overrides)) {
    const current = JSON.stringify(cleanOverrides({ role: overrides[role] }))
    if (current !== '{}' && current !== filledOverrides[role]) continue
    overrides[role] = { ...(chosen.agentOverrides[role] ?? {}) }
    filledOverrides[role] = JSON.stringify(cleanOverrides({ role: overrides[role] }))
  }
  if (Object.keys(chosen.agentOverrides).length) showAgents.value = true
}

function pickProjectFromList() {
  projectChosen.value = true
  suggestedFrom.value = null
  applyProject(project.value)
}

const showRepoPicker = ref(false)
const showProjectPicker = ref(false)
const showAgents = ref(false)
const validation = ref<RepositoryValidation | null>(null)
const validating = ref(false)
const error = ref<string | null>(null)

const repoRoot = computed(() => validation.value?.root ?? null)
const branches = computed(() => validation.value?.branches ?? [])

const jiraEnabled = computed(() => configStore.config?.jiraEnabled ?? false)
// With the integration on, Jira stays the default until the user has started a prompt.
watch(jiraEnabled, (enabled) => {
  if (enabled && !prompt.value.trim()) taskSource.value = 'jira'
}, { immediate: true })
const jiraResults = ref<JiraSearchResult[]>([])
const jiraSearching = ref(false)
const jiraError = ref<string | null>(null)
const jiraHasSearched = ref(false)
const showJiraResults = ref(false)
const activeJiraIndex = ref(-1)
const jiraResultsList = ref<HTMLUListElement | null>(null)
watch(activeJiraIndex, (i) => {
  if (i < 0) return
  jiraResultsList.value?.querySelectorAll('li[role="option"]')[i]?.scrollIntoView({ block: 'nearest' })
}, { flush: 'post' })
let lastPickedJiraKey: string | null = null
let jiraTimer: number | undefined
let jiraRequestId = 0
watch(() => form.jiraKey, (value) => {
  window.clearTimeout(jiraTimer)
  activeJiraIndex.value = -1
  jiraHasSearched.value = false
  jiraError.value = null
  if (!jiraEnabled.value || !value.trim() || value === lastPickedJiraKey) {
    jiraResults.value = []
    showJiraResults.value = false
    jiraSearching.value = false
    return
  }
  showJiraResults.value = true
  jiraSearching.value = true // shown immediately so the panel doesn't pop in only once the debounce fires
  jiraTimer = window.setTimeout(() => searchJira(value.trim()), 300)
})

async function searchJira(query: string) {
  const requestId = ++jiraRequestId
  jiraSearching.value = true
  try {
    const results = await api.searchJira(query)
    if (requestId !== jiraRequestId) return // a later keystroke's search has since landed
    jiraResults.value = results
  } catch (e) {
    if (requestId !== jiraRequestId) return
    // Non-fatal: leave the manually-typed key usable even if Jira is unreachable.
    jiraResults.value = []
    jiraError.value = e instanceof ApiError ? e.message : 'Could not search Jira.'
  } finally {
    if (requestId === jiraRequestId) {
      jiraSearching.value = false
      jiraHasSearched.value = true
    }
  }
}

function hideBrokenJiraIcon(e: Event) {
  (e.target as HTMLImageElement).style.visibility = 'hidden'
}

const jiraTitleTouched = ref(false)

function pickJiraIssue(result: JiraSearchResult) {
  lastPickedJiraKey = result.key
  form.jiraKey = result.key
  if (!jiraTitleTouched.value) form.jiraTitle = result.summary
  jiraResults.value = []
  jiraError.value = null
  showJiraResults.value = false
  activeJiraIndex.value = -1
}

function onJiraKeydown(e: KeyboardEvent) {
  if (!jiraEnabled.value || !showJiraResults.value) return
  if (e.key === 'Escape') {
    showJiraResults.value = false
    activeJiraIndex.value = -1
    return
  }
  if (!jiraResults.value.length) return
  if (e.key === 'ArrowDown') {
    e.preventDefault()
    activeJiraIndex.value = (activeJiraIndex.value + 1) % jiraResults.value.length
  } else if (e.key === 'ArrowUp') {
    e.preventDefault()
    activeJiraIndex.value = activeJiraIndex.value <= 0 ? jiraResults.value.length - 1 : activeJiraIndex.value - 1
  } else if (e.key === 'Enter' && activeJiraIndex.value >= 0) {
    e.preventDefault()
    pickJiraIssue(jiraResults.value[activeJiraIndex.value])
  }
}

let timer: number | undefined
// Responses can arrive out of order while typing; only the latest request may update the form.
let validationSequence = 0
watch(() => [form.repositoryPath, form.baseBranch, form.projectRelativePath], () => {
  window.clearTimeout(timer)
  if (!form.repositoryPath) { validationSequence++; validation.value = null; validating.value = false; return }
  timer = window.setTimeout(validate, 350)
}, { immediate: true })

async function validate() {
  const sequence = ++validationSequence
  validating.value = true
  try {
    const result = await api.validateRepository(form.repositoryPath, form.baseBranch || null, form.projectRelativePath || null)
    if (sequence !== validationSequence) return
    validation.value = result
    if (!form.baseBranch && result.currentBranch) fill('baseBranch', result.currentBranch, null)
  } catch (e) {
    if (sequence !== validationSequence) return
    validation.value = { ok: false, branches: [], checks: [{ ok: false, message: (e as Error).message }] }
  } finally {
    if (sequence === validationSequence) validating.value = false
  }
}

function pickRepo(path: string) {
  form.repositoryPath = path
  form.projectRelativePath = ''
  showRepoPicker.value = false
}

function pickProject(_path: string, relative: string | null) {
  form.projectRelativePath = relative ?? ''
  showProjectPicker.value = false
}

const workingDir = (value: string | null | undefined) => (value ?? '').replace(/\\/g, '/').replace(/^\/+|\/+$/g, '')

/**
 * The project this repository and working directory most likely belong to: one whose defaults
 * name them, else the project of the newest task that used them.
 */
function suggestProject(root: string, dir: string): Project | null {
  const byDefaults = projects.active.find((p) => samePath(p.defaultRepositoryPath, root)
    && workingDir(p.defaultProjectRelativePath) === dir)
  if (byDefaults) return byDefaults
  const previous = [...tasks.tasks]
    .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
    .find((t) => t.projectId != null && samePath(t.repositoryRoot, root) && workingDir(t.projectRelativePath) === dir
      && !projects.byId.get(t.projectId)?.archivedAt && projects.byId.has(t.projectId))
  return previous ? projects.byId.get(previous.projectId!) ?? null : null
}

// Picking a repository (or directory) suggests a project, never the other way round.
watch(() => [validation.value?.root, form.projectRelativePath, projects.loaded] as const, ([root]) => {
  if (projectChosen.value || !root) return
  const dir = workingDir(form.projectRelativePath)
  const suggestion = suggestProject(root, dir)
  projectId.value = suggestion?.id ?? null
  suggestedFrom.value = suggestion ? (dir || root.replace(/\\/g, '/').split('/').filter(Boolean).pop() || root) : null
})

// What the chosen route would cost with these agents, from past runs (display only; never a limit).
const routeEstimate = ref<RouteEstimate | null>(null)
const rate = useUsdToNzdRate()
let estimateRequest = 0
watch([implementationMode, () => JSON.stringify(cleanOverrides(overrides))], async () => {
  const request = ++estimateRequest
  try {
    const estimate = await api.costEstimate(implementationMode.value || undefined, cleanOverrides(overrides))
    if (request === estimateRequest) routeEstimate.value = estimate
  } catch {
    if (request === estimateRequest) routeEstimate.value = null // an older backend has no estimate
  }
}, { immediate: true })
const estimateLines = computed(() => {
  const estimate = routeEstimate.value
  if (!estimate) return []
  const onePass = describeTotals(estimate.onePass, rate.value)
  const lines = [onePass ? `One pass through every stage: ${onePass}.` : 'No stage has enough past runs for an estimate yet.']
  const worker = describeTotals(estimate.perWorker, rate.value)
  if (worker) lines.push(`Each worker adds ${worker}.`)
  const fixRound = describeTotals(estimate.perFixRound, rate.value)
  if (fixRound) lines.push(`Each review round that sends fixes back adds ${fixRound}.`)
  if (estimate.missing.length && onePass) lines.push(`Not included, too few past runs: ${estimate.missing.join(', ')}.`)
  return lines
})

// The same ticket can't be created twice; the second request would only fail once the first lands.
const alreadyCreating = computed(() => taskSource.value === 'jira' && !!form.jiraKey.trim()
  && !!operations.runningFor(form.jiraKey.trim().toUpperCase(), 'create'))

const canSubmit = computed(() => (taskSource.value === 'local' ? prompt.value.trim() : form.jiraKey.trim()) && form.repositoryPath && form.baseBranch && validation.value?.ok && !alreadyCreating.value)

/** Creation (fetch, worktree, Jira sync) runs on while the user moves on; a toast reports how it went. */
function submit() {
  error.value = null
  const agentOverrides = cleanOverrides(overrides)
  const payload = {
    implementationMode: implementationMode.value || undefined,
    taskSource: taskSource.value,
    prompt: taskSource.value === 'local' ? prompt.value.trim() : undefined,
    jiraKey: taskSource.value === 'jira' ? form.jiraKey.trim().toUpperCase() : undefined,
    jiraTitle: taskSource.value === 'jira' ? form.jiraTitle.trim() || null : null,
    repositoryPath: form.repositoryPath,
    projectRelativePath: form.projectRelativePath || null,
    baseBranch: form.baseBranch,
    additionalContext: form.additionalContext || null,
    agentOverrides,
    projectId: projectId.value,
  }
  const draft: TaskDraft = JSON.parse(JSON.stringify({
    taskSource: taskSource.value, prompt: prompt.value, implementationMode: implementationMode.value, form, overrides,
    projectId: projectId.value,
  }))
  const label = payload.jiraKey ?? 'local task'
  operations.start({
    kind: 'create',
    jiraKey: payload.jiraKey ?? null,
    title: `Creating ${label}`,
    call: (operationId) => tasks.createTask(payload, operationId),
    success: (snapshot) => ({
      title: `${snapshot.jiraKey} is ready`,
      message: snapshot.jiraTitle ?? 'Worktree, branch and terminal session created.',
      actions: [{ label: 'Open', run: () => void router.push({ name: 'task', params: { jiraKey: snapshot.jiraKey } }) }],
    }),
    failure: (e) => {
      const message = e instanceof Error ? e.message : 'Task creation failed.'
      return {
        title: `Could not create ${label}`,
        message,
        actions: [{
          label: 'Edit & retry',
          run: () => {
            pendingDraft.value = { ...draft, error: message }
            void router.push({ name: 'new-task' })
          },
        }],
      }
    },
  })
  void router.push({ name: 'home' })
}

function restore(draft: TaskDraft) {
  // Restoring the key must not reopen the Jira search results.
  lastPickedJiraKey = draft.form.jiraKey
  taskSource.value = draft.taskSource
  prompt.value = draft.prompt
  implementationMode.value = draft.implementationMode
  Object.assign(form, draft.form)
  jiraTitleTouched.value = !!draft.form.jiraTitle
  for (const [role, override] of Object.entries(draft.overrides)) overrides[role] = { ...override }
  showAgents.value = Object.values(draft.overrides).some((o) => o.harness || o.model || o.reasoning)
  // Whatever the draft had stays, even no project: a later suggestion must not change it.
  projectId.value = draft.projectId ?? null
  projectChosen.value = true
  preselected = true
  error.value = draft.error ?? null
}

// Arriving from a project's dashboard (`/tasks/new?project=<slug>`) starts the task in it.
let preselected = false
watch(() => projects.loaded, (loaded) => {
  const slug = typeof route.query.project === 'string' ? route.query.project : null
  if (!loaded || preselected || !slug) return
  preselected = true
  const found = projects.bySlug.get(slug)
  if (!found || found.archivedAt) return
  projectId.value = found.id
  pickProjectFromList()
}, { immediate: true })

// Declared last so a restored draft wins over the defaults the watches above apply.
watch(pendingDraft, (draft) => {
  if (!draft) return
  pendingDraft.value = null
  restore(draft)
}, { immediate: true })
</script>

<template>
  <form class="panel" @submit.prevent="submit">
    <h2>New Task</h2>
    <p class="muted small">Describe a local task or import a Jira ticket. Connect your coding CLI when you are ready to run an agent.</p>
    <label class="field">
        <span>Task source</span>
        <select v-model="taskSource">
            <option value="local">Local prompt</option>
            <option value="jira">Jira ticket</option>
        </select>
    </label>
    <label v-if="taskSource === 'local'" class="field">
        <span>What would you like to do?</span>
        <textarea v-model="prompt" required autofocus maxlength="100000" placeholder="Describe the change and what a successful result looks like." />
    </label>
    <label class="field">
        <span>Implementation route</span>
        <select v-model="implementationMode">
            <option value="">Use configured default</option>
            <option value="direct">Direct — one implementer for a cohesive change</option>
            <option value="parallel">Parallel — independent workers and integration</option>
        </select>
        <span class="hint">Both routes retain requirements approval, reviews and final approval.</span>
    </label>
    <div v-if="estimateLines.length" class="estimate small">
        <p v-for="line in estimateLines" :key="line" class="muted">{{ line }}</p>
        <p class="hint">From the last runs of each stage with the same harness and model; a guide, not a limit.</p>
    </div>
    <p class="muted small">
      AgentFlow creates a dedicated Git worktree and branch for the task, one terminal session for all its agents,
      and never runs agents in your original checkout.
    </p>
    <div v-if="error" class="alert error">{{ error }}</div>

    <label v-if="taskSource === 'jira'" class="field" style="position: relative">
      <span>Jira ticket <span class="hint">required{{ jiraEnabled ? ' — type a key or search text' : '' }}</span></span>
      <input
        type="text" v-model="form.jiraKey" placeholder="JR-1234" required autofocus
        role="combobox" aria-autocomplete="list" :aria-expanded="jiraEnabled && showJiraResults"
        @focus="showJiraResults = jiraEnabled && !!form.jiraKey.trim()"
        @blur="showJiraResults = false"
        @keydown="onJiraKeydown"
      />
      <div v-if="jiraEnabled && showJiraResults && (jiraResults.length || jiraSearching || jiraError || jiraHasSearched)" class="jira-results-panel">
        <ul v-if="jiraResults.length" ref="jiraResultsList" class="jira-results" role="listbox">
          <template v-for="(r, i) in jiraResults" :key="r.key">
            <li v-if="r.section && r.section !== jiraResults[i - 1]?.section" class="jira-section-label" role="presentation">
              {{ r.section }}
            </li>
            <li
              role="option" :aria-selected="i === activeJiraIndex" :class="{ active: i === activeJiraIndex }"
              @mousedown.prevent="pickJiraIssue(r)" @mouseenter="activeJiraIndex = i"
            >
              <img v-if="r.iconUrl" :src="r.iconUrl" alt="" class="jira-icon" @error="hideBrokenJiraIcon" />
              <strong>{{ r.key }}</strong> <span class="muted small">{{ r.summary }}</span>
            </li>
          </template>
        </ul>
        <p v-else-if="jiraSearching" class="muted small jira-status">Searching Jira…</p>
        <p v-else-if="jiraError" class="jira-status jira-status-error">{{ jiraError }}</p>
        <p v-else-if="jiraHasSearched" class="muted small jira-status">No matching issues found.</p>
      </div>
    </label>

    <label v-if="taskSource === 'jira'" class="field">
      <span>Jira title <span class="hint">optional — shown in the task list; defaults to the ticket's summary, overridable only here</span></span>
      <input
        type="text" v-model="form.jiraTitle" placeholder="Autofilled from the picked ticket"
        @input="jiraTitleTouched = true"
      />
    </label>

    <label class="field">
      <span>Project <span class="hint">optional{{ project && suggestedFrom ? ` — suggested from ${suggestedFrom}` : ' — picking one fills in its defaults' }}</span></span>
      <div class="input-row project-select">
        <span class="project-dot" :class="{ none: !project }" :style="project ? { '--dot': projectColor(project.color) } : undefined" aria-hidden="true" />
        <select v-model="projectId" @change="pickProjectFromList">
          <option :value="null">No project</option>
          <option v-for="p in projects.active" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
      </div>
    </label>

    <label class="field">
      <span>Repository directory <span class="hint">required<template v-if="filledFrom('repositoryPath')"> — from {{ filledFrom('repositoryPath') }}</template></span></span>
      <div class="input-row">
        <input type="text" v-model="form.repositoryPath" placeholder="D:\Repos\Acme" required />
        <button type="button" @click="showRepoPicker = !showRepoPicker">Browse…</button>
      </div>
    </label>
    <DirectoryPicker v-if="showRepoPicker" :start="form.repositoryPath || null" @select="pickRepo" @cancel="showRepoPicker = false" />

    <div v-if="validation || validating" class="alert" :class="validation ? (validation.ok ? 'ok' : 'error') : 'info'">
      <ul v-if="validation" class="check-list">
        <li v-for="(c, i) in validation.checks" :key="i" :class="c.ok ? 'ok' : 'bad'">{{ c.message }}</li>
      </ul>
      <span v-else class="muted small">Validating repository…</span>
    </div>

    <label class="field">
      <span>Working directory <span class="hint">optional — where agents start, inside the repository<template v-if="filledFrom('projectRelativePath')"> — from {{ filledFrom('projectRelativePath') }}</template></span></span>
      <div class="input-row">
        <input type="text" v-model="form.projectRelativePath" placeholder="Apps/Storefront" :disabled="!repoRoot" />
        <button type="button" :disabled="!repoRoot" @click="showProjectPicker = !showProjectPicker">Browse within repository…</button>
      </div>
    </label>
    <DirectoryPicker v-if="showProjectPicker && repoRoot" :root="repoRoot" @select="pickProject" @cancel="showProjectPicker = false" />

    <label class="field">
      <span>Base branch <span class="hint">required<template v-if="filledFrom('baseBranch')"> — from {{ filledFrom('baseBranch') }}</template></span></span>
      <div class="input-row">
        <input type="text" v-model="form.baseBranch" placeholder="develop" required list="branches" />
        <datalist id="branches"><option v-for="b in branches" :key="b" :value="b" /></datalist>
      </div>
    </label>

    <label class="field">
      <span>Additional context <span class="hint">optional — handed to every agent</span></span>
      <textarea v-model="form.additionalContext" placeholder="Issue appears specific to staging… Check the shared core project as well." />
    </label>

    <div class="panel" style="background: var(--bg)">
      <div class="panel-title">
        <h3 style="margin: 0">Agent overrides for this task</h3>
        <button type="button" class="sm" @click="showAgents = !showAgents">{{ showAgents ? 'Hide' : 'Customise' }}</button>
      </div>
      <p class="muted small" v-if="!showAgents">Defaults from config.json are used unless you override them here or per node later.</p>
      <AgentOverridesTable v-else v-model="overrides" />
    </div>

    <div class="row" style="justify-content: flex-end">
      <button type="submit" class="primary" :disabled="!canSubmit">Create Task</button>
      <span v-if="alreadyCreating" class="small muted">{{ form.jiraKey.trim().toUpperCase() }} is already being created.</span>
    </div>
  </form>
</template>

<style scoped>
.estimate p { margin: 0.15rem 0; }
.project-select { align-items: center; }
.project-select .project-dot { width: 11px; height: 11px; margin-inline: 0.2rem; }
.project-select select { flex: 1; }
</style>
