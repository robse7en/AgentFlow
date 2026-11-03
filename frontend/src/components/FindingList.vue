<script setup lang="ts">
// The Findings tab: each finding once (its newest round), filtered and grouped by the iteration that
// last raised it, blocking first. A finding opens in FindingDetail, which pages through this list.
import { computed, ref, watch } from 'vue'
import type { Finding } from '@/api/types'
import FindingDetail from '@/components/FindingDetail.vue'
import { location, shortSha } from '@/findingCode'
import {
  type FindingFilter, type StatusFilter, type TypeFilter, blocksNow, defaultFilter, earlierRows, groupByIteration,
  latestRows, matchesStatus, matchesType, statusLabel, typeLabel,
} from '@/findings'

const props = defineProps<{
  findings: Finding[]
  taskKey: string
  currentSha: string | null
  /** Held by the task view so the graph's findings chips can set it; null until anyone does. */
  filter: FindingFilter | null
}>()
const emit = defineEmits<{ 'update:filter': [filter: FindingFilter] }>()

const latest = computed(() => latestRows(props.findings))
const active = computed<FindingFilter>(() => props.filter ?? defaultFilter(props.findings))
const setFilter = (change: Partial<FindingFilter>) => emit('update:filter', { ...active.value, ...change })

const types = computed(() => [...new Set(latest.value.map((f) => f.type))].sort())
const ofType = computed(() => latest.value.filter((f) => matchesType(f, active.value.type)))
const statuses: { key: StatusFilter; label: string }[] = [
  { key: 'open', label: 'Open' }, { key: 'blocking', label: 'Blocking' }, { key: 'all', label: 'All' },
]
const counts = computed(() => Object.fromEntries(
  statuses.map((s) => [s.key, ofType.value.filter((f) => matchesStatus(f, s.key)).length]),
) as Record<StatusFilter, number>)

const groups = computed(() => groupByIteration(ofType.value.filter((f) => matchesStatus(f, active.value.status))))
/** The order the detail pages through. */
const visible = computed(() => groups.value.flatMap(([, items]) => items))

const summary = computed(() => {
  const open = latest.value.filter((f) => f.status === 'open').length
  return { open, blocking: latest.value.filter(blocksNow).length, closed: latest.value.length - open }
})

// Held by id, not row: a new review round replaces the row while the dialog is open.
const selectedId = ref<string | null>(null)
const selected = computed(() => latest.value.find((f) => f.id === selectedId.value) ?? null)
const selectedIndex = computed(() => visible.value.findIndex((f) => f.id === selectedId.value))
watch(() => props.taskKey, () => { selectedId.value = null })

function step(delta: number) {
  const list = visible.value
  if (!list.length) return
  const next = selectedIndex.value < 0 ? 0 : Math.min(Math.max(selectedIndex.value + delta, 0), list.length - 1)
  selectedId.value = list[next].id
}

const groupBlocking = (items: Finding[]) => items.filter(blocksNow).length
const groupOpen = (items: Finding[]) => items.filter((f) => f.status === 'open').length
</script>

<template>
  <div>
    <p v-if="!findings.length" class="muted small">No review findings recorded yet.</p>
    <template v-else>
      <p class="findings-summary small">
        <span><b>{{ summary.open }}</b> open</span>
        <span :class="{ 'findings-summary__blocking': summary.blocking }"><b>{{ summary.blocking }}</b> blocking</span>
        <span class="muted"><b>{{ summary.closed }}</b> closed</span>
      </p>

      <div class="findings-filters">
        <span class="seg" role="group" aria-label="Status">
          <button v-for="s in statuses" :key="s.key" type="button" :class="{ on: active.status === s.key }"
              :aria-pressed="active.status === s.key" @click="setFilter({ status: s.key })">
            {{ s.label }} <span class="seg__count">{{ counts[s.key] }}</span>
          </button>
        </span>
        <span v-if="types.length > 1 || active.type !== 'all'" class="seg" role="group" aria-label="Reviewer">
          <button type="button" :class="{ on: active.type === 'all' }" :aria-pressed="active.type === 'all'"
              @click="setFilter({ type: 'all' })">Both</button>
          <button v-for="t in types" :key="t" type="button" :class="{ on: active.type === t }"
              :aria-pressed="active.type === t" @click="setFilter({ type: t as TypeFilter })">
            {{ typeLabel(t).replace(' review', '') }}
          </button>
        </span>
      </div>

      <p v-if="!visible.length" class="muted small">
        Nothing {{ active.status === 'all' ? 'here' : active.status }} right now.
        <a href="#" @click.prevent="setFilter({ status: 'all', type: 'all' })">Show all {{ latest.length }}</a>
      </p>

      <section v-for="[iteration, items] in groups" :key="iteration" class="findings-group">
        <h4>
          Iteration {{ iteration }}
          <span class="findings-group__meta">
            · {{ items.length }} finding{{ items.length === 1 ? '' : 's' }}<template v-if="groupOpen(items) && groupOpen(items) < items.length"> · {{ groupOpen(items) }} open</template><template v-if="groupBlocking(items)"> · {{ groupBlocking(items) }} blocking</template><template v-if="items[0].reviewedSha"> · @ {{ shortSha(items[0].reviewedSha) }}</template>
          </span>
        </h4>
        <ul class="finding-rows">
          <li v-for="f in items" :key="f.id">
            <button type="button" class="finding-row" :class="[f.status, { blocking: blocksNow(f) }]" @click="selectedId = f.id">
              <span class="finding-row__dot" aria-hidden="true" />
              <span class="finding-row__main">
                <span class="finding-row__top">
                  <b class="mono">{{ f.id }}</b>
                  <span class="badge" :class="f.severity">{{ f.severity }}</span>
                  <span v-if="blocksNow(f)" class="badge FAILED">blocking</span>
                  <span v-if="f.occurrence > 1" class="badge WAITING_FOR_USER" title="Review rounds that raised it">{{ f.occurrence }}×</span>
                  <span class="finding-row__status">{{ statusLabel(f) }}</span>
                </span>
                <span class="finding-row__title">{{ f.title }}</span>
                <span v-if="f.file" class="finding-row__loc" :title="location(f.file, f.line, f.endLine)">{{ location(f.file, f.line, f.endLine) }}</span>
              </span>
            </button>
          </li>
        </ul>
      </section>
    </template>

    <FindingDetail v-if="selected" :finding="selected" :earlier="earlierRows(findings, selected)" :task-key="taskKey"
        :current-sha="currentSha" :index="selectedIndex" :total="visible.length" @step="step" @close="selectedId = null" />
  </div>
</template>

<style scoped>
.findings-summary { display: flex; flex-wrap: wrap; gap: 0.9rem; margin: 0 0 0.6rem; }
.findings-summary__blocking { color: var(--danger); }

.findings-filters { display: flex; flex-wrap: wrap; gap: 0.5rem; margin-bottom: 0.9rem; }
.seg { display: inline-flex; border: 1px solid var(--border); border-radius: var(--radius-pill); overflow: hidden; }
.seg button {
  border: 0; border-radius: 0; background: transparent; padding: 0.25rem 0.7rem; font-size: 12.5px; color: var(--muted);
}
.seg button:hover:not(:disabled) { transform: none; background: var(--hover); color: var(--text); }
.seg button.on { background: var(--accent-soft); color: var(--accent-strong); font-weight: 600; }
.seg__count { opacity: 0.7; font-variant-numeric: tabular-nums; }

.findings-group { margin-bottom: 1rem; }
.findings-group h4 { margin: 0 0 0.4rem; }
.findings-group__meta { text-transform: none; letter-spacing: 0; font-weight: 400; }

.finding-rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.4rem; }
.finding-row {
  width: 100%; display: flex; align-items: flex-start; gap: 0.6rem; text-align: left;
  padding: 0.55rem 0.75rem; border-radius: var(--radius-md); background: var(--panel);
}
.finding-row:hover:not(:disabled) { transform: none; }
.finding-row__dot { flex: none; width: 8px; height: 8px; border-radius: 50%; margin-top: 0.4rem; background: var(--muted); opacity: 0.5; }
.finding-row.open .finding-row__dot { background: var(--warn); opacity: 1; }
.finding-row.blocking .finding-row__dot { background: var(--danger); opacity: 1; }
.finding-row.resolved .finding-row__dot { background: var(--ok); opacity: 1; }
.finding-row.blocking { border-color: var(--danger-border); }
.finding-row:not(.open) { opacity: 0.7; }
.finding-row:not(.open):hover { opacity: 1; }
.finding-row__main { display: flex; flex-direction: column; gap: 0.2rem; min-width: 0; flex: 1; }
.finding-row__top { display: flex; flex-wrap: wrap; align-items: center; gap: 0.35rem; }
.finding-row__status { margin-left: auto; font-size: 11.5px; color: var(--muted); white-space: nowrap; }
.finding-row__title {
  font-size: 13px; font-weight: 600; line-height: 1.4;
  display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
}
.finding-row__loc { font-family: var(--mono); font-size: 11.5px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
