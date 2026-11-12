<script lang="ts" setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { WorkflowNodeDto } from '@/api/types';
import { useActivityStore } from '@/stores/activity';
import { useNow } from '@/now';

const props = defineProps<{ node: WorkflowNodeDto; taskKey: string }>();
const activity = useActivityStore();
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
const liveState = computed(() => ['RUNNING', 'WAITING_FOR_USER'].includes(props.node.state)
    && ['RUNNING', 'WAITING_FOR_USER'].includes(props.node.lastRun?.state ?? ''));
const waiting = computed(() => props.node.state === 'WAITING_FOR_USER');

// What the agent's transcript says: the call that is running and for how long, or that nothing has
// been written for a while. That tells a long call from silence, which the agent's own heartbeat
// reports cannot.
const clock = useNow(() => liveState.value && !waiting.value);
const pulse = computed(() => (liveState.value && !waiting.value
    ? activity.pulse(props.taskKey, props.node.key, clock.value) : null));

const visible = computed(() => liveState.value && (!!report.value || !!pulse.value));
const age = computed(() => {
    const timestamp = report.value?.receivedAt ?? props.node.lastRun?.startedAt;
    const parsed = timestamp ? Date.parse(timestamp) : NaN;
    const observed = Date.parse(report.value?.observedAt ?? '');
    if (!Number.isFinite(parsed) || !Number.isFinite(observed)) return null;
    return Math.max(0, Math.floor((observed - parsed + now.value - received.value) / 1000));
});
const overdue = computed(() => {
    if (waiting.value) return false;
    // A call that is running is not silence; a quiet transcript is, once it passes the warning time.
    if (pulse.value) return pulse.value.silentMs >= (report.value?.warningSeconds ?? 600) * 1000;
    if (age.value === null) return false;
    const since = Date.parse(report.value?.warningSince ?? '');
    const observed = Date.parse(report.value?.observedAt ?? '');
    const graceAge = (observed - since + now.value - received.value) / 1000;
    return Math.min(age.value, graceAge) >= (report.value?.warningSeconds ?? 600);
});
const label = computed(() => {
    if (waiting.value) return 'Waiting for your answer · heartbeat warnings paused';
    if (pulse.value) return pulse.value.text;
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
        :title="pulse
            ? 'From the agent\'s transcript: the call it is making and for how long, or how long since it last wrote. Inspect the pane before recovery; no automatic retry.'
            : 'Agent report only. Silence can mean a long tool call or sleep. Inspect the pane before recovery; no automatic retry.'">
        {{ label }}<span v-if="report?.phase && !waiting && !pulse"> · {{ report.phase }}</span>
    </p>
</template>

<style scoped>
.heartbeat { margin: 0.4rem 0; color: var(--muted); }
/* One line on a card: "Shell · cargo test · 6m" is cut with an ellipsis, not wrapped. */
.wf-node .heartbeat { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.overdue { font-weight: 600; color: var(--text); }
</style>
