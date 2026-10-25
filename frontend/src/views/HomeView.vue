<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useTaskStore } from '@/stores/tasks'
import { useConfigStore } from '@/stores/config'
import { useOperationStore } from '@/stores/operations'
import { useProjectStore } from '@/stores/projects'
import { jiraTicketUrl } from '@/api/client'
import Modal from '@/components/Modal.vue'
import DeleteTask from '@/components/DeleteTask.vue'
import WorktreeCleanup from '@/components/WorktreeCleanup.vue'
import ActionMenu, { type MenuItem } from '@/components/ActionMenu.vue'
import TaskRow from '@/components/TaskRow.vue'
import ProjectFilter from '@/components/ProjectFilter.vue'
import MoveToProject from '@/components/MoveToProject.vue'
import { STATUS_LABELS, shortPath, taskProgress } from '@/taskPresentation'
import { inProjectFilter } from '@/projects'
import type { TaskSummary } from '@/api/types'

const store = useTaskStore()
const configStore = useConfigStore()
const operations = useOperationStore()
const projects = useProjectStore()
const router = useRouter()
const route = useRoute()

// The project filter lives in the URL (`/?project=<slug>`, or `none`) so the Projects page can link
// to it. A slug not known yet is kept: the project list may still be loading.
const projectSlug = computed(() => (typeof route.query.project === 'string' && route.query.project) || null)
const setProjectSlug = (slug: string | null) =>
  void router.replace({ query: { ...route.query, project: slug ?? undefined } })
const inProject = (task: TaskSummary) => inProjectFilter(task, projectSlug.value, projects.bySlug)
const filteredProject = computed(() => (projectSlug.value ? projects.bySlug.get(projectSlug.value) ?? null : null))
const projectCounts = computed(() => {
  const counts = new Map<number | null, number>()
  for (const task of store.tasks) counts.set(task.projectId ?? null, (counts.get(task.projectId ?? null) ?? 0) + 1)
  return counts
})
const projectOf = (task: TaskSummary) => (task.projectId == null ? null : projects.byId.get(task.projectId) ?? null)
// New tasks start in the project being looked at.
const newTaskLink = computed(() => (filteredProject.value && !filteredProject.value.archivedAt
  ? { name: 'new-task', query: { project: filteredProject.value.slug } } : { name: 'new-task' }))
const search = ref('');
const filter = ref('all');
const now = ref(Date.now());
let clock: ReturnType<typeof setInterval> | undefined;
onMounted(() => {
    clock = setInterval(() => { now.value = Date.now(); }, 30000);
});
onUnmounted(() => { if (clock) clearInterval(clock); });
const finished = new Set(['MERGED', 'CANCELLED']);
// Archivable/deletable: nothing more will happen to the task (FAILED is reserved but never set today).
const removable = new Set(['MERGED', 'CANCELLED', 'FAILED']);
// Archived tasks are a separate list from the server; every other filter narrows the live one.
const listMode = () => (filter.value === 'archived' ? 'only' : 'exclude');
watch(() => filter.value === 'archived', () => { void store.loadTasks(listMode()); });
const visibleTasks = computed(() => store.tasks.filter((task) => {
    const match = `${task.jiraKey} ${task.jiraTitle ?? ''} ${task.repositoryRoot}`
        .toLowerCase().includes(search.value.trim().toLowerCase());
    return match && inProject(task) && (filter.value === 'all' || filter.value === 'archived'
        || (filter.value === 'attention' && !!task.attention?.length)
        || (filter.value === 'active' && !finished.has(task.status))
        || (filter.value === 'finished' && finished.has(task.status)));
}).sort((a, b) => Number(!!b.attention?.length) - Number(!!a.attention?.length)));
const attentionTasks = computed(() => visibleTasks.value.filter((task) => task.attention?.length));
const waiting = (since: string | null): string => {
    if (!since) return '';
    const minutes = Math.max(0, Math.floor((now.value - Date.parse(since)) / 60000));
    return minutes < 60 ? `${minutes}m waiting` : `${Math.floor(minutes / 60)}h ${minutes % 60}m waiting`;
};
// Counted over the live (unarchived) tasks of the project filter, whatever the status filter; hidden
// while the archived list is loaded in their place.
const stats = computed(() => {
    const live = store.tasks.filter((task) => !task.archivedAt && inProject(task));
    const active = live.filter((task) => task.status === 'ACTIVE');
    return [
        { label: 'Active tasks', value: active.length, tone: '' },
        { label: 'Needs attention', value: live.filter((task) => task.attention?.length).length, tone: 'attention' },
        { label: 'Running now', value: active.filter((task) => task.phase.endsWith(': RUNNING')).length, tone: 'running' },
        { label: 'Ready for PR', value: live.filter((task) => task.status === 'READY_FOR_PR').length, tone: 'done' },
    ];
});

function jiraUrl(jiraKey: string) {
  return jiraTicketUrl(configStore.config?.jiraBaseUrl, jiraKey)
}

onMounted(() => store.loadTasks(listMode()))

const rowBusy = reactive<Record<string, boolean>>({})
// A background cleanup or delete holds the task's lock; its other actions would only queue behind it.
const busy = (jiraKey: string) => !!rowBusy[jiraKey] || !!operations.runningFor(jiraKey)
const runningLabel: Record<string, string> = { cleanup: 'Cleaning up…', delete: 'Deleting…' }
const rowError = reactive<Record<string, string | null>>({})

async function markMerged(jiraKey: string) {
  rowBusy[jiraKey] = true
  rowError[jiraKey] = null
  try {
    await store.markPrMerged(jiraKey)
  } catch (e) {
    rowError[jiraKey] = e instanceof Error ? e.message : 'Action failed.'
  } finally {
    rowBusy[jiraKey] = false
  }
}

async function rowAction(jiraKey: string, action: (key: string) => Promise<unknown>) {
  rowBusy[jiraKey] = true
  rowError[jiraKey] = null
  try {
    await action(jiraKey)
  } catch (e) {
    rowError[jiraKey] = e instanceof Error ? e.message : 'Action failed.'
  } finally {
    rowBusy[jiraKey] = false
  }
}

const deleteKey = ref<string | null>(null)
const moveTask = ref<TaskSummary | null>(null)

const FILTERS = [
  { value: 'all', label: 'All' },
  { value: 'attention', label: 'Needs attention' },
  { value: 'active', label: 'Active' },
  { value: 'finished', label: 'Finished' },
  { value: 'archived', label: 'Archived' },
]

// Same fail-open rule as the task page: offer cleanup unless we know nothing is left on disk.
const canCleanup = (t: TaskSummary): boolean =>
  t.status === 'MERGED' && (t.worktreeExists !== false || (t.workerWorktreeCount ?? 0) > 0)
const cleanupKey = ref<string | null>(null)
// Looked up live so the dialog sees the row update once the cleanup lands.
const cleanupTask = computed(() => store.tasks.find((t) => t.jiraKey === cleanupKey.value) ?? null)

// The row's actions, in the order they were previously shown as buttons.
function rowItems(t: TaskSummary): MenuItem[] {
  const disabled = busy(t.jiraKey)
  const items: MenuItem[] = []
  if (t.status === 'READY_FOR_PR') items.push({ label: 'Mark PR created', disabled, run: () => openPrModal(t.jiraKey) })
  if (t.status === 'PR_CREATED') items.push({ label: 'Mark merged', disabled, run: () => void markMerged(t.jiraKey) })
  if (canCleanup(t)) items.push({ label: 'Clean up', disabled, run: () => { cleanupKey.value = t.jiraKey } })
  items.push({ label: 'Move to project…', disabled, run: () => { moveTask.value = t } })
  if (t.archivedAt) {
    items.push({ label: 'Unarchive', disabled, run: () => void rowAction(t.jiraKey, store.unarchiveTask) })
    items.push({ label: 'Delete…', danger: true, disabled, run: () => { deleteKey.value = t.jiraKey } })
  } else if (removable.has(t.status)) {
    items.push({ label: 'Archive', disabled, run: () => void rowAction(t.jiraKey, store.archiveTask) })
  }
  return items
}

const prModalKey = ref<string | null>(null)
const prModalUrl = ref('')

const prUrlInput = ref<HTMLInputElement | null>(null)

function openPrModal(jiraKey: string) {
  prModalKey.value = jiraKey
  prModalUrl.value = ''
  void nextTick(() => prUrlInput.value?.focus())
}

function closePrModal() {
  if (prModalKey.value) rowError[prModalKey.value] = null
  prModalKey.value = null
  prModalUrl.value = ''
}

async function submitPrModal() {
  const jiraKey = prModalKey.value
  if (!jiraKey) return
  rowBusy[jiraKey] = true
  rowError[jiraKey] = null
  try {
    await store.markPrCreated(jiraKey, prModalUrl.value.trim())
    closePrModal()
  } catch (e) {
    rowError[jiraKey] = e instanceof Error ? e.message : 'Action failed.'
  } finally {
    rowBusy[jiraKey] = false
  }
}
</script>

<template>
  <div class="dashboard">
  <div class="dashboard-top">
  <header class="dashboard-head">
    <div>
      <h1>Tasks</h1>
      <p class="muted">Everything that needs you, in one place.</p>
    </div>
    <RouterLink :to="newTaskLink" class="btn primary">New task</RouterLink>
  </header>
  <section v-if="filter !== 'archived'" class="stats" aria-label="Task counts">
    <div v-for="stat in stats" :key="stat.label" class="stat">
      <span class="muted">{{ stat.label }}</span>
      <strong :class="stat.tone">{{ stat.value }}</strong>
    </div>
  </section>
        <div class="dashboard-filters">
          <label class="search">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg>
            <input v-model="search" type="search" placeholder="Search tasks or repositories" aria-label="Search tasks or repositories" />
          </label>
          <ProjectFilter :model-value="projectSlug" :counts="projectCounts" :total="store.tasks.length" @update:model-value="setProjectSlug" />
          <div class="chips" role="group" aria-label="Filter tasks">
            <button v-for="option in FILTERS" :key="option.value" type="button" class="chip" :class="{ active: filter === option.value }" :aria-pressed="filter === option.value" @click="filter = option.value">{{ option.label }}</button>
          </div>
        </div>
  </div>
  <div class="dashboard-scroll">
        <div v-if="store.error" class="alert error">{{ store.error }}</div>
        <section v-if="attentionTasks.length" aria-label="Needs attention" class="attention-list">
          <h3>Needs attention</h3>
          <article v-for="task in attentionTasks" :key="task.jiraKey" class="attention-ticket">
            <strong>{{ task.jiraKey }} — {{ task.jiraTitle }}</strong>
            <div v-for="item in task.attention" :key="item.nodeKey" class="attention-action">
              <RouterLink :to="{ name: 'task', params: { jiraKey: task.jiraKey }, query: { node: item.nodeKey } }">
                {{ item.action }} · {{ item.label }}<template v-if="item.questions"> ({{ item.questions }})</template>
              </RouterLink>
              <span class="muted small">{{ waiting(item.since) }}</span>
            </div>
          </article>
        </section>
        <p v-if="!store.tasks.length && !store.loading && filter === 'archived'" class="muted">
          No archived tasks.
        </p>
        <p v-else-if="!store.tasks.length && !store.loading" class="muted">
          Welcome to AgentFlow. Create a task with a repository and a prompt. Jira is optional, and you can connect a coding CLI before your first agent run. You will review requirements before implementation and approve the final result.
        </p>
        <p v-else-if="!visibleTasks.length && !store.loading" class="muted">No matching tasks.</p>
        <div v-else class="task-list">
          <template v-for="(t, index) in visibleTasks" :key="t.jiraKey">
            <TaskRow
              :style="{ '--i': index }"
              :task="t"
              :progress="taskProgress(t)"
              :status-label="STATUS_LABELS[t.status] ?? t.status"
              :repository="shortPath(t.repositoryRoot)"
              :project="projectOf(t)"
              :jira-href="t.taskSource !== 'local' ? jiraUrl(t.jiraKey) : null"
              :busy-label="operations.runningFor(t.jiraKey) ? runningLabel[operations.runningFor(t.jiraKey)!.kind] : undefined"
            >
              <template #actions>
                <ActionMenu v-if="rowItems(t).length" :items="rowItems(t)" :label="`Actions for ${t.jiraKey}`" />
              </template>
            </TaskRow>
            <div v-if="rowError[t.jiraKey]" class="alert error small">{{ rowError[t.jiraKey] }}</div>
          </template>
        </div>
  </div>

        <DeleteTask v-if="deleteKey" :task-key="deleteKey" @close="deleteKey = null" @deleted="deleteKey = null" />
        <MoveToProject v-if="moveTask" :task-key="moveTask.jiraKey" :project-id="moveTask.projectId" @close="moveTask = null" />

        <Modal v-if="cleanupTask" title="Clean up worktree" @close="cleanupKey = null">
          <p class="small muted">{{ cleanupTask.jiraKey }}</p>
          <WorktreeCleanup
            :key="cleanupTask.jiraKey"
            start-open
            :task-key="cleanupTask.jiraKey"
            :expected-sha="cleanupTask.currentSha"
            :worktree-exists="cleanupTask.worktreeExists"
            :worker-worktree-count="cleanupTask.workerWorktreeCount"
            @close="cleanupKey = null"
          />
        </Modal>

        <Modal v-if="prModalKey" title="Mark PR created" @close="closePrModal">
          <p class="small muted">{{ prModalKey }}</p>
          <form @submit.prevent="submitPrModal">
            <label class="field">
              <span>Pull request URL</span>
              <input
                ref="prUrlInput"
                v-model="prModalUrl"
                type="url"
                placeholder="https://github.com/org/repo/pull/123"
                required
                :disabled="rowBusy[prModalKey]"
              />
            </label>
            <div v-if="rowError[prModalKey]" class="alert error small">{{ rowError[prModalKey] }}</div>
            <div class="row" style="display: flex; gap: 0.5rem; justify-content: flex-end;">
              <button type="button" class="sm" :disabled="rowBusy[prModalKey]" @click="closePrModal">Cancel</button>
              <button type="submit" class="sm success" :disabled="rowBusy[prModalKey] || !prModalUrl.trim()">Mark PR created</button>
            </div>
          </form>
        </Modal>
  </div>
</template>

<style scoped>
/* Header, stats and filters stay put; only the task list scrolls. The `.page` host is told to stop scrolling by the
   :has() rule in styles.css, and falls back to scrolling as a whole on short windows. */
.dashboard { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.dashboard-top { flex: none; }
/* Padding with a matching negative margin leaves room for the row hover shift and focus rings the scroller would clip. */
.dashboard-scroll { flex: 1; min-height: 0; overflow-y: auto; padding: 0.5rem 0.75rem 1rem 0.5rem; margin: -0.5rem -0.75rem 0 -0.5rem; }
@media (max-height: 640px) {
  .dashboard-scroll { overflow: visible; }
}
.dashboard-head { display: flex; flex-wrap: wrap; gap: 1.25rem; align-items: center; justify-content: space-between; margin-bottom: 1.6rem; }
.dashboard-head h1 { margin: 0; font-size: clamp(28px, 3.6vw, 40px); font-weight: 700; letter-spacing: -0.03em; }
.dashboard-head p { margin: 0.4rem 0 0; font-size: 15px; }
.dashboard-head .btn { min-height: 44px; padding-inline: 1.4rem; }

.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1rem; margin-bottom: 1.6rem; }
.stat {
  display: flex; flex-direction: column; gap: 0.5rem; padding: 1.4rem 1.5rem;
  background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-lg);
  animation: rise 0.7s var(--spring) both; animation-delay: calc(var(--n, 0) * 80ms);
  transition: transform 0.35s var(--spring), border-color 0.3s, box-shadow 0.35s;
}
.stat:nth-child(2) { --n: 1; }
.stat:nth-child(3) { --n: 2; }
.stat:nth-child(4) { --n: 3; }
.stat:hover { transform: translateY(-5px); border-color: var(--accent-border); box-shadow: var(--elev); }
.stat span { font-size: 13px; font-weight: 500; }
.stat strong { font-size: 38px; font-weight: 700; letter-spacing: -0.03em; line-height: 1; }
.stat strong.attention { color: var(--warn); }
.stat strong.running { color: var(--run); }
.stat strong.done { color: var(--ok); }
@keyframes rise { from { opacity: 0; transform: translateY(16px); } }

.dashboard-filters { display: flex; gap: 0.75rem 1rem; flex-wrap: wrap; align-items: center; justify-content: space-between; margin-bottom: 1rem; }
/* The search pill widens while focused. */
.search {
  display: flex; align-items: center; gap: 0.6rem; flex: 1 1 280px; max-width: 420px; min-height: 44px; padding: 0 1.1rem;
  border: 1px solid var(--border); border-radius: var(--radius-pill); background: var(--input-bg); color: var(--muted);
  transition: max-width 0.4s var(--spring), border-color 0.25s, box-shadow 0.3s var(--spring);
}
.search:focus-within { max-width: 520px; color: var(--text); border-color: var(--accent); box-shadow: 0 0 0 5px var(--accent-soft); }
.search svg { flex: none; }
.search input { flex: 1; min-width: 0; padding: 0; border: 0; border-radius: 0; background: transparent; box-shadow: none; }
.search input:focus { box-shadow: none; }
.chips { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-left: auto; }
.chip { min-height: 44px; padding: 0 1.1rem; border: 0; background: var(--info-soft); color: var(--muted); }
.chip:hover:not(:disabled) { color: var(--text); }
.chip.active { color: var(--accent-strong); background: var(--accent-soft); box-shadow: inset 0 0 0 1px var(--accent-border); }
.task-list { display: flex; flex-direction: column; gap: 0.85rem; }
.attention-list { margin-bottom: 1rem; }
.attention-ticket { border: 1px solid var(--warn-border); background: var(--warn-soft); border-radius: var(--radius-md); padding: 0.75rem 1rem; margin-bottom: 0.5rem; }
.attention-action { display: flex; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
@media (prefers-reduced-motion: reduce) { .stat { animation: none; transition: none; } .search { transition: none; } }
</style>
