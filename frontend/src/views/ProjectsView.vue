<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '@/api/client'
import type { Project } from '@/api/types'
import { useProjectStore } from '@/stores/projects'
import { useTaskStore } from '@/stores/tasks'
import ActionMenu, { type MenuItem } from '@/components/ActionMenu.vue'
import Modal from '@/components/Modal.vue'
import ProjectEditor from '@/components/ProjectEditor.vue'
import RepositoryInstructions from '@/components/RepositoryInstructions.vue';
import { NO_PROJECT, countByProject, projectColor, timeAgo, type ProjectCounts } from '@/projects'
import { parseTimestamp } from '@/now'
import { formatNzd } from '@/currency'
import { useUsdToNzdRate } from '@/usage'

const projects = useProjectStore()
const tasks = useTaskStore()
const rate = useUsdToNzdRate()

// Counts cover live tasks, as the dashboard's do; archiving a task takes it out of them.
onMounted(() => tasks.loadTasks('exclude'))
const counts = computed(() => countByProject(tasks.tasks.filter((t) => !t.archivedAt)))
const EMPTY: ProjectCounts = { total: 0, active: 0, attention: 0, ready: 0, merged: 0, costUsd: null, lastActivity: null }
const countsOf = (id: number | null) => counts.value.get(id) ?? EMPTY
const showArchived = ref(false)

/** Each status's share of the bar; whatever is left (cancelled, PR created, needs review) is the rest. */
function segments(c: ProjectCounts) {
  if (!c.total) return []
  const share = (n: number) => `${(n / c.total) * 100}%`
  const other = c.total - c.active - c.ready - c.merged
  return [
    { tone: 'active', width: share(c.active), n: c.active },
    { tone: 'ready', width: share(c.ready), n: c.ready },
    { tone: 'merged', width: share(c.merged), n: c.merged },
    { tone: 'other', width: share(other), n: other },
  ].filter((s) => s.n > 0)
}

function footnote(c: ProjectCounts): string {
  const parts: string[] = []
  if (c.costUsd != null) parts.push(formatNzd(c.costUsd, rate.value))
  const last = parseTimestamp(c.lastActivity)
  if (last != null) parts.push(`updated ${timeAgo(last)}`)
  return parts.join(' · ') || 'No tasks yet'
}

const editing = ref<Project | null>(null)
const instructionProject = ref<Project | null>(null);
const creating = ref(false)
const closeEditor = () => { editing.value = null; creating.value = false }

const actionError = ref<string | null>(null)
async function run(action: () => Promise<unknown>) {
  actionError.value = null
  try {
    await action()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Action failed.'
  }
}

const deleting = ref<Project | null>(null)
// Archived tasks are not in the live list, so the dialog asks for every task.
const deletingCount = ref<number | null>(null)
const deleteBusy = ref(false)
const deleteError = ref<string | null>(null)
async function askDelete(project: Project) {
  deleting.value = project
  deletingCount.value = null
  deleteError.value = null
  try {
    deletingCount.value = (await api.tasks('include')).filter((t) => t.projectId === project.id).length
  } catch {
    deletingCount.value = null
  }
}
async function confirmDelete() {
  if (!deleting.value) return
  deleteBusy.value = true
  deleteError.value = null
  try {
    await projects.remove(deleting.value.id)
    deleting.value = null
  } catch (e) {
    deleteError.value = e instanceof Error ? e.message : 'Could not delete the project.'
  } finally {
    deleteBusy.value = false
  }
}

function menu(project: Project): MenuItem[] {
  return [
    { label: 'Edit…', run: () => { editing.value = project } },
    { label: 'Agent instructions…', run: () => { instructionProject.value = project; } },
    project.archivedAt
      ? { label: 'Unarchive', run: () => void run(() => projects.unarchive(project.id)) }
      : { label: 'Archive', hint: 'Hidden from pickers; its tasks keep it', run: () => void run(() => projects.archive(project.id)) },
    { label: 'Delete…', danger: true, run: () => void askDelete(project) },
  ]
}

const deleteMessage = computed(() => {
  const n = deletingCount.value
  if (n == null) return 'Its tasks stay; they will have no project.'
  if (n === 0) return 'No tasks are in it.'
  return `Its ${n} ${n === 1 ? 'task stays' : 'tasks stay'}, with no project.`
})
</script>

<template>
  <RepositoryInstructions v-if="instructionProject" :project="instructionProject"
    @close="instructionProject = null" />
  <header class="projects-head">
    <div>
      <h1>Projects</h1>
      <p class="muted">Group tasks, and start new ones with the repository, branch and agents they need.</p>
    </div>
    <button type="button" class="primary" @click="creating = true">New project</button>
  </header>

  <div v-if="projects.error" class="alert error">{{ projects.error }}</div>
  <div v-if="actionError" class="alert error">{{ actionError }}</div>

  <p v-if="projects.loaded && !projects.projects.length" class="muted empty">
    No projects yet. Create one for each app or piece of work, then pick it when you create a task or move existing tasks into it from their ⋯ menu.
  </p>

  <section class="project-grid" aria-label="Projects">
    <article v-for="(p, index) in projects.active" :key="p.id" class="project-card" :style="{ '--i': index }">
      <div class="card-head">
        <span class="project-dot" :style="{ '--dot': projectColor(p.color) }" aria-hidden="true" />
        <h2>{{ p.name }}</h2>
        <ActionMenu :items="menu(p)" :label="`Actions for ${p.name}`" />
      </div>
      <p v-if="p.description" class="desc muted small">{{ p.description }}</p>
      <p v-if="p.defaultRepositoryPath" class="defaults mono" :title="p.defaultRepositoryPath">
        {{ p.defaultRepositoryPath.replace(/\\/g, '/').split('/').filter(Boolean).pop() }}<template v-if="p.defaultProjectRelativePath"> / {{ p.defaultProjectRelativePath }}</template><template v-if="p.defaultBaseBranch"> · {{ p.defaultBaseBranch }}</template>
      </p>
      <div class="bar" role="img" :aria-label="`${countsOf(p.id).total} live tasks`">
        <i v-for="s in segments(countsOf(p.id))" :key="s.tone" :class="s.tone" :style="{ width: s.width }" />
      </div>
      <dl class="counts">
        <div><dt>Active</dt><dd>{{ countsOf(p.id).active }}</dd></div>
        <div><dt>Needs you</dt><dd :class="{ warn: countsOf(p.id).attention }">{{ countsOf(p.id).attention }}</dd></div>
        <div><dt title="Ready for PR">Ready</dt><dd>{{ countsOf(p.id).ready }}</dd></div>
        <div><dt>Merged</dt><dd>{{ countsOf(p.id).merged }}</dd></div>
      </dl>
      <div class="foot">
        <span class="muted">{{ footnote(countsOf(p.id)) }}</span>
        <RouterLink :to="{ name: 'home', query: { project: p.slug } }">View tasks →</RouterLink>
      </div>
    </article>

    <article v-if="projects.projects.length" class="project-card unassigned">
      <div class="card-head">
        <span class="project-dot none" aria-hidden="true" />
        <h2>No project</h2>
      </div>
      <p class="desc muted small">
        {{ countsOf(null).total }} live {{ countsOf(null).total === 1 ? 'task' : 'tasks' }} outside any project.
        Move them from their ⋯ menu on the task list.
      </p>
      <div class="foot">
        <span />
        <RouterLink :to="{ name: 'home', query: { project: NO_PROJECT } }">View tasks →</RouterLink>
      </div>
    </article>
  </section>

  <section v-if="projects.archived.length" class="archived">
    <button type="button" class="sm" :aria-expanded="showArchived" @click="showArchived = !showArchived">
      {{ showArchived ? 'Hide' : 'Show' }} archived projects ({{ projects.archived.length }})
    </button>
    <ul v-if="showArchived" class="archived-list">
      <li v-for="p in projects.archived" :key="p.id">
        <span class="project-tag"><i class="project-dot" :style="{ '--dot': projectColor(p.color) }" aria-hidden="true" />{{ p.name }}</span>
        <span class="muted small">{{ footnote(countsOf(p.id)) }}</span>
        <RouterLink :to="{ name: 'home', query: { project: p.slug } }" class="small">View tasks</RouterLink>
        <ActionMenu :items="menu(p)" :label="`Actions for ${p.name}`" />
      </li>
    </ul>
  </section>

  <ProjectEditor v-if="creating || editing" :project="editing" @close="closeEditor" @saved="closeEditor" />

  <Modal v-if="deleting" :title="`Delete ${deleting.name}?`" narrow @close="deleting = null">
    <p class="muted">{{ deleteMessage }} This cannot be undone.</p>
    <div v-if="deleteError" class="alert error small">{{ deleteError }}</div>
    <div class="row" style="justify-content: flex-end">
      <button type="button" class="sm" :disabled="deleteBusy" @click="deleting = null">Cancel</button>
      <button type="button" class="sm danger solid" :disabled="deleteBusy" @click="confirmDelete">Delete project</button>
    </div>
  </Modal>
</template>

<style scoped>
.projects-head { display: flex; flex-wrap: wrap; gap: 1.25rem; align-items: center; justify-content: space-between; margin-bottom: 1.6rem; }
.projects-head h1 { margin: 0; font-size: clamp(28px, 3.6vw, 40px); font-weight: 700; letter-spacing: -0.03em; }
.projects-head p { margin: 0.4rem 0 0; font-size: 15px; }
.projects-head button { min-height: 44px; padding-inline: 1.4rem; }
.empty { max-width: 60ch; }

.project-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 280px), 1fr)); gap: 1rem; }
.project-card {
  display: flex; flex-direction: column; gap: 0.7rem; min-width: 0; padding: 1.1rem 1.2rem;
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-lg);
  animation: rise 0.6s var(--spring) both; animation-delay: calc(min(var(--i, 0), 10) * 60ms);
  transition: transform 0.35s var(--spring), border-color 0.3s, box-shadow 0.35s;
}
.project-card:hover { transform: translateY(-4px); border-color: var(--accent-border); box-shadow: var(--elev); }
.project-card.unassigned { background: transparent; border-style: dashed; box-shadow: none; }
.project-card.unassigned:hover { transform: none; }
.card-head { display: flex; align-items: center; gap: 0.6rem; min-height: 44px; }
.card-head .project-dot { width: 12px; height: 12px; }
.card-head h2 { flex: 1; min-width: 0; margin: 0; font-size: 17px; letter-spacing: -0.01em; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.desc { margin: -0.2rem 0 0; }
.defaults { margin: 0; color: var(--muted); font-size: 11.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.bar { display: flex; gap: 2px; height: 8px; border-radius: 4px; overflow: hidden; background: var(--info-soft); }
.bar i { display: block; height: 100%; }
.bar .active { background: var(--run); }
.bar .ready { background: var(--ok); }
.bar .merged { background: var(--ok-border); }
.bar .other { background: var(--info-soft); }

.counts { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 0.4rem; margin: 0; }
.counts div { min-width: 0; }
.counts dt { font-size: 11.5px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.counts dd { margin: 0.1rem 0 0; font-size: 20px; font-weight: 700; letter-spacing: -0.02em; font-variant-numeric: tabular-nums; }
.counts dd.warn { color: var(--warn); }

.foot { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 0.5rem; margin-top: auto; padding-top: 0.7rem; border-top: 1px solid var(--border); font-size: 12.5px; }
.foot a { font-weight: 600; }

.archived { margin-top: 1.6rem; }
.archived-list { list-style: none; margin: 0.8rem 0 0; padding: 0; display: flex; flex-direction: column; gap: 0.4rem; }
.archived-list li {
  display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem 1rem; padding: 0.3rem 0.6rem 0.3rem 1rem;
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-md);
}
.archived-list .project-tag { flex: 1; min-width: 0; font-weight: 500; }

@keyframes rise { from { opacity: 0; transform: translateY(16px); } }
@media (prefers-reduced-motion: reduce) { .project-card { animation: none; transition: none; } }
</style>
