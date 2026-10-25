<script lang="ts" setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { WorkflowNodeDto } from '@/api/types';

const props = defineProps<{ node: WorkflowNodeDto }>();
const now = ref(performance.now());
const received = ref(now.value);
watch(() => props.node.lastRun, () => {
    received.value = performance.now();
    now.value = received.value;
});
let timer: ReturnType<typeof setInterval> | undefined;
onMounted(() => { timer = setInterval(() => { now.value = performance.now(); }, 15000); });
onBeforeUnmount(() => { if (timer !== undefined) clearInterval(timer); });

const report = computed(() => props.node.lastRun?.heartbeat);
const visible = computed(() => !!report.value
    && ['RUNNING', 'WAITING_FOR_USER'].includes(props.node.state)
    && ['RUNNING', 'WAITING_FOR_USER'].includes(props.node.lastRun?.state ?? ''));
const age = computed(() => {
    const timestamp = report.value?.receivedAt ?? props.node.lastRun?.startedAt;
    const parsed = timestamp ? Date.parse(timestamp) : NaN;
    const observed = Date.parse(report.value?.observedAt ?? '');
    if (!Number.isFinite(parsed) || !Number.isFinite(observed)) return null;
    return Math.max(0, Math.floor((observed - parsed + now.value - received.value) / 1000));
});
const waiting = computed(() => props.node.state === 'WAITING_FOR_USER');
const overdue = computed(() => {
    if (waiting.value || age.value === null) return false;
    const since = Date.parse(report.value?.warningSince ?? '');
    const observed = Date.parse(report.value?.observedAt ?? '');
    const graceAge = (observed - since + now.value - received.value) / 1000;
    return Math.min(age.value, graceAge) >= (report.value?.warningSeconds ?? 600);
});
const label = computed(() => {
    if (waiting.value) return 'Waiting for your answer · heartbeat warnings paused';
    if (age.value === null) return 'Agent heartbeat unavailable';
    const elapsed = age.value < 60 ? `${age.value}s` : `${Math.floor(age.value / 60)}m`;
    if (!report.value?.receivedAt) {
        const status = overdue.value ? 'Signal overdue' : 'Awaiting first agent signal';
        return `${status} · started ${elapsed} ago`;
    }
    return `${overdue.value ? 'Signal overdue' : 'Agent reported'} · ${elapsed} ago`;
});
</script>

<template>
    <p v-if="visible" class="heartbeat small" :class="{ overdue }" role="status"
        title="Agent report only. Silence can mean a long tool call or sleep. Inspect the pane before recovery; no automatic retry.">
        {{ label }}<span v-if="report?.phase && !waiting"> · {{ report.phase }}</span>
    </p>
</template>

<style scoped>
.heartbeat { margin: 0.4rem 0; color: var(--muted); }
.overdue { font-weight: 600; color: var(--text); }
</style>
