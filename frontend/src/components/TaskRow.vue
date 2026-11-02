<script setup lang="ts">
import { computed } from 'vue'
import type { Project, TaskSummary } from '@/api/types'
import { WORKFLOW_STAGES, leftoverWorktree, phaseLabel, rowState, type TaskProgress } from '@/taskPresentation'
import { formatAge, parseTimestamp, useNow } from '@/now'
import { projectColor } from '@/projects'

const props = defineProps<{
  task: TaskSummary
  progress: TaskProgress
  repository: string
  jiraHref: string | null
  busyLabel?: string
  project?: Project | null
}>()

const state = computed(() => rowState(props.task))
const leftover = computed(() => leftoverWorktree(props.task))
const label = computed(() => phaseLabel(props.task))

const stageCount = WORKFLOW_STAGES.length
const segments = computed(() =>
  WORKFLOW_STAGES.map((name, i) => ({
    name,
    cls: i < props.progress.index ? 'past' : i === props.progress.index ? 'current' : '',
  })),
)
// "4 / 9" while the task is somewhere in the workflow; nothing once finished or when unknown.
const step = computed(() => {
  const { index } = props.progress
  return index >= 0 && index < stageCount ? `${index + 1} / ${stageCount}` : null
})

// A running task's updatedAt doesn't move while its agent works, so it gets no age (it would
// look stale); a waiting one shows how long it has been waiting on you.
const now = useNow(() => state.value !== 'running')
const age = (timestamp: string | null | undefined) => {
  const ms = parseTimestamp(timestamp)
  return ms === null ? null : formatAge(now.value - ms)
}
const when = computed(() => {
  if (props.busyLabel) return props.busyLabel
  if (state.value === 'running') return ''
  if (state.value === 'waiting') {
    const waited = age(props.task.attention?.[0]?.since)
    return waited ? `waiting ${waited === 'just now' ? '<1m' : waited}` : ''
  }
  const updated = age(props.task.updatedAt)
  return updated ? `updated ${updated === 'just now' ? updated : `${updated} ago`}` : ''
})
</script>

<template>
  <article class="task-row" :class="{ archived: !!task.archivedAt }">
    <!-- The state by shape and colour, so it reads without motion too. -->
    <span class="glyph" :class="state" aria-hidden="true">
      <template v-if="state === 'running'">
        <svg class="ring" width="44" height="44" viewBox="0 0 44 44" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round"><circle class="ring-track" cx="22" cy="22" r="20.5" /><circle class="ring-arc" cx="22" cy="22" r="20.5" /></svg>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2 4 14h7l-1 8 9-12h-7z" /></svg>
      </template>
      <svg v-else-if="state === 'waiting'" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="8" r="4" /><path d="M4 21a8 8 0 0 1 16 0" /></svg>
      <svg v-else-if="state === 'attention'" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" /><circle cx="12" cy="12" r="3" /></svg>
      <svg v-else-if="state === 'done'" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7.5" /></svg>
      <svg v-else-if="state === 'failed'" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
      <svg v-else width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"><path d="M6 12h12" /></svg>
    </span>

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

    <div class="track" :class="state">
      <div class="track-head">
        <span class="label" :title="label">{{ label }}</span>
        <span v-if="step" class="step">{{ step }}</span>
      </div>
      <div
        v-if="progress.index >= 0"
        class="segs"
        :style="{ '--stages': stageCount }"
        role="progressbar"
        aria-label="Workflow stage"
        :aria-valuenow="progress.index"
        aria-valuemin="0"
        :aria-valuemax="stageCount"
        :aria-valuetext="step ? `${label}, stage ${step.replace(' / ', ' of ')}` : label"
      >
        <i v-for="seg in segments" :key="seg.name" class="seg" :class="seg.cls" />
      </div>
      <!-- Outside Active, the summary carries no stage, so say why the task stopped instead. -->
      <p v-else-if="task.statusMessage" class="message" :title="task.statusMessage">{{ task.statusMessage }}</p>
    </div>

    <span class="when" :class="[state, { busy: !!busyLabel }]">{{ when }}</span>

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

.glyph {
  position: relative; flex: none; display: grid; place-items: center; width: 44px; height: 44px;
  border-radius: 50%; background: var(--info-soft); color: var(--muted);
}
.glyph.running { background: var(--live-soft); color: var(--live); }
.glyph.waiting, .glyph.attention { background: var(--warn-soft); color: var(--warn); }
.glyph.done { background: var(--ok-soft); color: var(--ok); }
.glyph.failed { background: var(--danger-soft); color: var(--danger); }
/* A loading spinner around a running glyph: a faint full track, and an arc that spins while it
   grows and shrinks, so the row reads as busy at a glance. */
.glyph .ring { position: absolute; inset: 0; animation: spin 1.6s linear infinite; }
.glyph .ring-track { opacity: 0.22; }
.glyph .ring-arc { stroke-dasharray: 96 200; animation: ring-dash 1.4s ease-in-out infinite; }

.key {
  position: relative; z-index: 2; display: inline-flex; align-items: center; gap: 0.4rem; min-height: 44px; padding: 0 0.85rem;
  border-radius: var(--radius-pill); background: var(--accent-soft); color: var(--accent-strong);
  font: 500 12px var(--mono); white-space: nowrap; text-decoration: none; transition: background 0.25s, transform 0.3s var(--spring);
}
a.key:hover { background: var(--accent-border); transform: translateY(-2px); text-decoration: none; }

.main { flex: 1 1 220px; min-width: 0; }
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

/* The workflow as one segment per stage: finished stages filled, the current one in the row's
   state colour. Only a running stage moves. */
.track { display: flex; flex-direction: column; gap: 0.45rem; flex: 0 1 300px; min-width: 200px; }
.track-head { display: flex; align-items: baseline; justify-content: space-between; gap: 0.6rem; min-width: 0; }
.label { min-width: 0; font-size: 13px; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.track.running .label { color: var(--live); }
.track.waiting .label, .track.attention .label { color: var(--warn); }
.track.done .label { color: var(--ok); }
.track.failed .label { color: var(--danger); }
.track.idle .label { color: var(--muted); }
.step { flex: none; font: 11.5px var(--mono); color: var(--muted); }
.segs { display: grid; grid-template-columns: repeat(var(--stages), minmax(0, 1fr)); gap: 3px; }
.seg { height: 8px; border-radius: 3px; background: var(--info-soft); box-shadow: inset 0 0 0 1px var(--border); }
.seg.past { background: color-mix(in srgb, var(--muted) 55%, transparent); box-shadow: none; }
/* Finished work recedes: most rows are Merged, and a column of solid green tracks would outshout
   the few rows that are running. */
.track.done .seg.past { background: color-mix(in srgb, var(--ok) 30%, transparent); }
.seg.current { background: var(--muted); box-shadow: none; }
.track.running .seg.current {
  background: linear-gradient(90deg, var(--live), var(--live-glow), var(--live)); background-size: 200% 100%;
  animation: shimmer 1.8s linear infinite;
}
.track.waiting .seg.current, .track.attention .seg.current { background: var(--warn); }
.track.failed .seg.current { background: var(--danger); }
.message { margin: 0; font-size: 12px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

.when { flex: 0 0 120px; font: 11.5px var(--mono); color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.when.waiting { color: var(--warn); }
.when.busy { color: var(--text); font-family: var(--font); font-size: 12px; font-weight: 500; }
/* Always reserves the menu's width so the columns line up whether or not a row has actions. */
.actions { flex: none; width: 44px; }

@keyframes row-in { from { opacity: 0; transform: translateX(-14px); } }
@keyframes shimmer { to { background-position: -200% 0; } }
@keyframes spin { to { transform: rotate(360deg); } }
/* Circumference of r=20.5 is ~129: the arc starts as a dot, stretches to ~3/4, then its tail catches up. */
@keyframes ring-dash {
  0% { stroke-dasharray: 1 200; stroke-dashoffset: 0; }
  50% { stroke-dasharray: 96 200; stroke-dashoffset: -32; }
  100% { stroke-dasharray: 96 200; stroke-dashoffset: -127; }
}
@media (prefers-reduced-motion: reduce) {
  .task-row, .glyph .ring, .glyph .ring-arc, .seg { animation: none !important; transition: none; }
}
</style>
