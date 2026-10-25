<script lang="ts" setup>
import { ref } from 'vue';
import { api } from '@/api/client';
import { useTaskStore } from '@/stores/tasks';
import type { TaskSnapshot } from '@/api/types';

const props = defineProps<{ task: TaskSnapshot }>();
const store = useTaskStore();
const amount = ref('');
const busy = ref(false);
const error = ref('');
const save = async (): Promise<void> => {
    busy.value = true;
    error.value = '';
    try {
        await api.setTaskBudget(props.task.jiraKey, amount.value ? Number(amount.value) : null);
        await store.loadTask(props.task.jiraKey);
    } catch (cause) {
        error.value = cause instanceof Error ? cause.message : String(cause);
    } finally { busy.value = false; }
};
// Known spend plus reservations against the limit, as the meter's fill.
const used = (kind: 'task' | 'daily'): number => {
    const group = props.task.budget![kind];
    if (!group.limitUsd) return 0;
    return Math.min(100, ((group.knownUsd + group.reservedUsd) / group.limitUsd) * 100);
};
</script>

<template>
    <section
        v-if="task.budget && (task.budget.task.limitUsd != null || task.budget.daily.limitUsd != null)"
        class="panel strip-card"
        aria-label="API budgets"
    >
        <div class="panel-title">
            <h2>API budgets</h2>
            <span v-if="task.budget.task.warning || task.budget.daily.warning" class="status-pill failed">Budget warning</span>
            <span v-else class="status-pill">USD</span>
        </div>
        <template v-for="kind in (['task', 'daily'] as const)" :key="kind">
            <div v-if="task.budget[kind].limitUsd != null" class="budget">
                <div class="meter-row">
                    <span>{{ kind === 'task' ? 'This ticket' : `All tickets · ${task.budget.dayUtc} UTC` }}</span>
                    <span class="mono">
                        ${{ task.budget[kind].knownUsd.toFixed(2) }} + ${{ task.budget[kind].reservedUsd.toFixed(2) }} reserved
                        / ${{ task.budget[kind].limitUsd }}
                    </span>
                </div>
                <div class="meter"><b :class="{ warn: task.budget[kind].warning }" :style="{ width: `${used(kind)}%` }" /></div>
                <p v-if="task.budget[kind].unknownRuns" class="small">
                    <strong>{{ task.budget[kind].unknownRuns }} runs have unknown cost.</strong>
                </p>
            </div>
        </template>
        <p class="small muted">Limits stop new launches; running agents can exceed reservations.
            Daily totals assign each completed run to its completion day in UTC.</p>
        <form class="input-row" @submit.prevent="save">
            <label class="sr-only" for="ticket-limit">New ticket limit (blank restores default)</label>
            <input id="ticket-limit" v-model="amount" type="number" min="0.01" step="0.01" placeholder="New ticket limit (blank restores default)" />
            <button :disabled="busy">Save limit</button>
        </form>
        <p v-if="error" class="alert error">{{ error }}</p>
    </section>
</template>

<style scoped>
.budget { margin-bottom: 0.9rem; }
.meter-row { display: flex; justify-content: space-between; gap: 0.75rem; flex-wrap: wrap; font-size: 13px; margin-bottom: 0.45rem; }
.meter-row .mono { color: var(--muted); font-size: 12px; }
.meter { height: 8px; border-radius: var(--radius-pill); background: var(--info-soft); overflow: hidden; }
.meter b {
    display: block; height: 100%; border-radius: inherit; transform-origin: left;
    background: linear-gradient(90deg, var(--accent), var(--btn-to)); animation: grow 1s var(--spring) both;
}
.meter b.warn { background: var(--danger); }
@keyframes grow { from { transform: scaleX(0); } }
@media (prefers-reduced-motion: reduce) { .meter b { animation: none; } }
</style>
