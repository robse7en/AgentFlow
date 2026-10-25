<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { api } from '@/api/client'
import { PROJECT_COLORS, type AgentOverride, type Project, type ProjectColor, type RepositoryValidation } from '@/api/types'
import { useProjectStore } from '@/stores/projects'
import { projectColor } from '@/projects'
import Modal from './Modal.vue'
import DirectoryPicker from './DirectoryPicker.vue'
import AgentOverridesTable, { cleanOverrides, emptyOverrides } from './AgentOverridesTable.vue'

// Without `project`, a new one.
const props = defineProps<{ project?: Project | null }>()
const emit = defineEmits<{ close: []; saved: [project: Project] }>()

const projects = useProjectStore()
const editing = computed(() => props.project ?? null)
const form = reactive({
  name: props.project?.name ?? '',
  color: (props.project?.color ?? null) as ProjectColor | null,
  description: props.project?.description ?? '',
  repository: props.project?.defaultRepositoryPath ?? '',
  workingDir: props.project?.defaultProjectRelativePath ?? '',
  baseBranch: props.project?.defaultBaseBranch ?? '',
})
const overrides = reactive<Record<string, AgentOverride>>(emptyOverrides())
for (const [role, o] of Object.entries(props.project?.agentOverrides ?? {})) overrides[role] = { ...o }
const showAgents = ref(Object.keys(props.project?.agentOverrides ?? {}).length > 0)
const showRepoPicker = ref(false)
const showDirPicker = ref(false)
const busy = ref(false)
const error = ref<string | null>(null)

// The defaults are checked as the New Task form checks them; the server checks again on save.
const validation = ref<RepositoryValidation | null>(null)
let timer: number | undefined
let sequence = 0
watch(() => [form.repository, form.workingDir, form.baseBranch], () => {
  window.clearTimeout(timer)
  if (!form.repository.trim()) { sequence++; validation.value = null; return }
  timer = window.setTimeout(async () => {
    const mine = ++sequence
    try {
      const result = await api.validateRepository(form.repository.trim(), form.baseBranch.trim() || null, form.workingDir.trim() || null)
      if (mine === sequence) validation.value = result
    } catch (e) {
      if (mine === sequence) validation.value = { ok: false, branches: [], checks: [{ ok: false, message: (e as Error).message }] }
    }
  }, 350)
}, { immediate: true })
const repoRoot = computed(() => validation.value?.root ?? null)

function pickRepo(path: string) {
  form.repository = path
  form.workingDir = ''
  showRepoPicker.value = false
}

function pickDir(_path: string, relative: string | null) {
  form.workingDir = relative ?? ''
  showDirPicker.value = false
}

async function save() {
  busy.value = true
  error.value = null
  const text = (value: string) => value.trim() || null
  try {
    const payload = {
      name: form.name.trim(),
      ...(form.color ? { color: form.color } : {}),
      description: text(form.description),
      defaultRepositoryPath: text(form.repository),
      defaultProjectRelativePath: text(form.repository) ? text(form.workingDir) : null,
      defaultBaseBranch: text(form.baseBranch),
      agentOverrides: cleanOverrides(overrides),
    }
    const saved = editing.value ? await projects.update(editing.value.id, payload) : await projects.create(payload)
    emit('saved', saved)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Could not save the project.'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <Modal :title="editing ? `Edit ${editing.name}` : 'New project'" @close="emit('close')">
    <form @submit.prevent="save">
      <label class="field">
        <span>Name <span class="hint">required</span></span>
        <input v-model="form.name" type="text" maxlength="80" required placeholder="Billing App" autofocus />
      </label>
      <fieldset class="field colors">
        <legend>Colour<span v-if="!editing" class="hint"> — optional, one that stands apart is picked otherwise</span></legend>
        <div class="swatches" role="radiogroup" aria-label="Colour">
          <label v-for="color in PROJECT_COLORS" :key="color" class="swatch" :class="{ on: form.color === color }" :title="color">
            <input v-model="form.color" type="radio" name="project-color" :value="color" class="sr-only" />
            <span class="project-dot" :style="{ '--dot': projectColor(color) }" />
            <span class="sr-only">{{ color }}</span>
          </label>
        </div>
      </fieldset>
      <label class="field">
        <span>Description <span class="hint">optional</span></span>
        <textarea v-model="form.description" maxlength="2000" rows="2" placeholder="Invoices, payments and exports." />
      </label>

      <h4>New task defaults</h4>
      <p class="muted small">Picking this project on the New Task form fills these in, except for anything already typed there.</p>
      <label class="field">
        <span>Repository <span class="hint">optional</span></span>
        <div class="input-row">
          <input v-model="form.repository" type="text" placeholder="D:\Dev\Acme\mono" />
          <button type="button" @click="showRepoPicker = !showRepoPicker">Browse…</button>
        </div>
      </label>
      <DirectoryPicker v-if="showRepoPicker" :start="form.repository || null" @select="pickRepo" @cancel="showRepoPicker = false" />
      <div v-if="validation" class="alert small" :class="validation.ok ? 'ok' : 'error'">
        <ul class="check-list">
          <li v-for="(c, i) in validation.checks" :key="i" :class="c.ok ? 'ok' : 'bad'">{{ c.message }}</li>
        </ul>
      </div>
      <label class="field">
        <span>Working directory <span class="hint">optional — where agents start, inside the repository</span></span>
        <div class="input-row">
          <input v-model="form.workingDir" type="text" placeholder="Acme.Web" :disabled="!form.repository.trim()" />
          <button type="button" :disabled="!repoRoot" @click="showDirPicker = !showDirPicker">Browse within repository…</button>
        </div>
      </label>
      <DirectoryPicker v-if="showDirPicker && repoRoot" :root="repoRoot" @select="pickDir" @cancel="showDirPicker = false" />
      <label class="field">
        <span>Base branch <span class="hint">optional</span></span>
        <input v-model="form.baseBranch" type="text" placeholder="develop" list="project-branches" />
        <datalist id="project-branches"><option v-for="b in validation?.branches ?? []" :key="b" :value="b" /></datalist>
      </label>
      <div class="panel" style="background: var(--bg)">
        <div class="panel-title">
          <h3 style="margin: 0">Agents</h3>
          <button type="button" class="sm" @click="showAgents = !showAgents">{{ showAgents ? 'Hide' : 'Customise' }}</button>
        </div>
        <p v-if="!showAgents" class="muted small">Tasks use the defaults from config.json unless this project overrides them.</p>
        <AgentOverridesTable v-else v-model="overrides" />
      </div>

      <div v-if="error" class="alert error small">{{ error }}</div>
      <div class="row" style="justify-content: flex-end">
        <button type="button" :disabled="busy" @click="emit('close')">Cancel</button>
        <button type="submit" class="primary" :disabled="busy || !form.name.trim()">{{ editing ? 'Save' : 'Create project' }}</button>
      </div>
    </form>
  </Modal>
</template>

<style scoped>
.colors { border: 0; padding: 0; margin: 0 0 0.9rem; }
.colors legend { font-weight: 500; margin-bottom: 0.4rem; padding: 0; }
.colors .hint { font-weight: 400; color: var(--muted); }
.swatches { display: flex; flex-wrap: wrap; gap: 0.4rem; }
.swatch {
  width: 36px; height: 36px; display: grid; place-items: center; border-radius: 50%; cursor: pointer;
  border: 1px solid var(--border); background: var(--panel); transition: transform 0.25s var(--spring), border-color 0.2s;
}
.swatch:hover { transform: scale(1.08); }
.swatch .project-dot { width: 18px; height: 18px; }
.swatch.on { border-color: var(--text); box-shadow: 0 0 0 3px var(--accent-soft); }
.swatch:has(input:focus-visible) { outline: 2px solid var(--accent); outline-offset: 2px; }
h4 { margin-top: 0.4rem; }
@media (prefers-reduced-motion: reduce) { .swatch { transition: none; } }
</style>
