<script setup lang="ts">
import { computed } from 'vue'
import type { Project, TaskSummary } from '@/api/types'
import { leftoverWorktree, statusTone, type TaskProgress } from '@/taskPresentation'
import { projectColor } from '@/projects'

const props = defineProps<{
  task: TaskSummary
  progress: TaskProgress
  statusLabel: string
  repository: string
  jiraHref: string | null
  busyLabel?: string
  project?: Project | null
}>()

const tone = computed(() => statusTone(props.task.status))
const leftover = computed(() => leftoverWorktree(props.task))
</script>

<template>
  <article class="task-row" :class="{ archived: !!task.archivedAt }">
    <a v-if="jiraHref" class="key" :href="jiraHref" target="_blank" rel="noopener noreferrer" :aria-label="`Open ${task.jiraKey} in Jira (new window)`">
      {{ task.jiraKey }}
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M7 17 17 7M8 7h9v9" /></svg>
    </a>
    <span v-else class="key">{{ task.jiraKey }}</span>

    <div class="main">
      <RouterLink class="title" :to="{ name: 'task', params: { jiraKey: task.jiraKey } }">{{ task.jiraTitle || task.jiraKey }}</RouterLink>
      <div class="meta mono">
        <span v-if="leftover" class="status-pill attention uncleaned" :title="`Merged, but ${leftover} still on disk. Use Clean up to remove it.`">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>
          Not cleaned up
        </span>
        <span v-if="project" class="project-tag project">
          <i class="project-dot" :style="{ '--dot': projectColor(project.color) }" aria-hidden="true" />
          <span>{{ project.name }}</span>
        </span>
        <span class="repo" :title="task.repositoryRoot">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 5a2 2 0 0 1 2-2h13v15H6a2 2 0 0 0-2 2z" /><path d="M4 20a2 2 0 0 0 2 2h13v-4" /></svg>
          <span>{{ repository }}<template v-if="task.projectRelativePath"> / {{ task.projectRelativePath }}</template></span>
        </span>
        <span class="branch">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="5" r="2.5" /><circle cx="6" cy="19" r="2.5" /><circle cx="18" cy="8" r="2.5" /><path d="M6 7.5v9M18 10.5c0 4-12 2-12 6" /></svg>
          <span>{{ task.branchName }}</span>
        </span>
      </div>
    </div>

    <span class="stage">{{ progress.stage }}</span>
    <div class="progress" role="progressbar" aria-label="Workflow progress" :aria-valuenow="progress.percent" aria-valuemin="0" aria-valuemax="100">
      <span class="bar"><b :class="{ done: tone === 'done' }" :style="{ width: `${progress.percent}%` }" /></span>
      <span class="percent">{{ progress.percent }}%</span>
    </div>

    <span class="status-pill" :class="tone"><i class="pulse-dot" />{{ statusLabel }}<span v-if="busyLabel" class="busy">{{ busyLabel }}</span></span>

    <div class="actions"><slot name="actions" /></div>
  </article>
</template>

<style scoped>
.task-row {
  position: relative; display: flex; flex-wrap: wrap; align-items: center; gap: 0.75rem 1.1rem;
  padding: 1rem 1.25rem; background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius-lg);
  /* Rows rise in one after another (HomeView sets --i); capped so a long list doesn't lag. */
  animation: row-in 0.55s var(--spring) both; animation-delay: calc(min(var(--i, 0), 10) * 60ms);
  transition: transform 0.35s var(--spring), border-color 0.3s, box-shadow 0.3s;
}
.task-row:hover { transform: translateX(6px); border-color: var(--accent-border); box-shadow: var(--elev); }
.task-row.archived { opacity: 0.7; }
/* The row's animation and hover transform make each row its own stacking context, so an open
   menu's z-index can't lift it above the rows after it; lift the whole row instead. */
.task-row:has(.menu) { z-index: 3; }

.key {
  position: relative; z-index: 2; display: inline-flex; align-items: center; gap: 0.4rem; min-height: 44px; padding: 0 0.85rem;
  border-radius: var(--radius-pill); background: var(--accent-soft); color: var(--accent-strong);
  font: 500 12px var(--mono); white-space: nowrap; text-decoration: none; transition: background 0.25s, transform 0.3s var(--spring);
}
a.key:hover { background: var(--accent-border); transform: translateY(-2px); text-decoration: none; }

.main { flex: 1 1 240px; min-width: 0; }
/* The title link stretches over the whole row, so the row is one click target; controls that
   must stay independent (Jira chip, menu) sit above it. */
.title { font-weight: 600; font-size: 1rem; letter-spacing: -0.01em; color: var(--text); }
.title::after { content: ""; position: absolute; inset: 0; border-radius: var(--radius-lg); }
.title:hover { color: var(--accent-strong); text-decoration: none; }
.title:focus-visible { outline: none; }
.title:focus-visible::after { box-shadow: 0 0 0 2px var(--accent); }
.meta { display: flex; flex-wrap: wrap; gap: 0.25rem 1rem; margin-top: 0.3rem; font-size: 12px; color: var(--muted); }
.meta > span { display: inline-flex; align-items: center; gap: 0.4rem; min-width: 0; overflow-wrap: anywhere; }
.meta svg { flex: none; }
.meta .repo { color: var(--text); opacity: 0.8; }
/* Same pill as the status, quieter: it flags leftover disk, not the task's state. */
.meta .uncleaned { padding: 0.1rem 0.6rem; font-family: var(--font); font-size: 11.5px; gap: 0.35rem; }
/* The project name reads as a label, not a path. */
.meta .project { font-family: var(--font); font-size: 12px; font-weight: 500; color: var(--text); }

.stage { flex: 0 1 150px; min-width: 0; font-size: 13px; font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.progress { display: flex; align-items: center; gap: 0.6rem; flex: 0 1 170px; min-width: 120px; font-size: 12px; color: var(--muted); }
.bar { flex: 1; min-width: 60px; height: 6px; border-radius: var(--radius-pill); background: var(--info-soft); overflow: hidden; }
.bar b {
  display: block; height: 100%; border-radius: inherit; transition: width 0.8s var(--spring);
  background: linear-gradient(90deg, var(--accent), var(--btn-to), var(--accent)); background-size: 200% 100%; animation: shimmer 2.4s linear infinite;
}
.bar b.done { background: var(--ok); animation: none; }
.percent { width: 34px; text-align: right; font-family: var(--mono); }

.status-pill { flex: 0 0 auto; min-width: 150px; }
.busy { font-weight: 400; opacity: 0.8; }
/* Always reserves the menu's width so the columns line up whether or not a row has actions. */
.actions { flex: none; width: 44px; }

@keyframes row-in { from { opacity: 0; transform: translateX(-14px); } }
@keyframes shimmer { to { background-position: -200% 0; } }
@media (prefers-reduced-motion: reduce) {
  .task-row, .bar b { animation: none !important; transition: none; }
}
</style>
