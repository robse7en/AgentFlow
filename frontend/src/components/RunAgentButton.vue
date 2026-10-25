<script lang="ts" setup>
import { computed, ref } from 'vue';
import AgentReadiness from '@/components/AgentReadiness.vue';
import { useTaskStore } from '@/stores/tasks';
import type { WorkflowNodeDto } from '@/api/types';

const props = defineProps<{ taskKey: string; node: WorkflowNodeDto }>();
const store = useTaskStore();
const readinessConfirmed = ref(false);
const needsReadiness = computed(() => !props.node.lastRun || !props.node.effectiveConfig?.harnessAvailable);
const launchReady = computed(() => !needsReadiness.value || readinessConfirmed.value);
const reason = ref('');
const busy = ref(false);
const error = ref('');
const run = async (): Promise<void> => {
    if (!launchReady.value) return;
    busy.value = true;
    error.value = '';
    try {
        const config = props.node.effectiveConfig;
        const approval = config?.requiresMeteredApproval
            ? { harness: config.harness, model: config.model, reason: reason.value.trim() } : undefined;
        const snapshot = await store.runNode(props.taskKey, props.node.key, null, approval);
        const updated = snapshot.nodes.find((node) => node.key === props.node.key);
        if (updated?.state !== 'RUNNING') error.value = updated?.message ?? 'Agent did not start.';
        else reason.value = '';
    } catch (cause) {
        error.value = cause instanceof Error ? cause.message : String(cause);
    } finally { busy.value = false; }
};
</script>

<template>
    <div v-if="['READY', 'FAILED'].includes(node.state)" class="launch nodrag">
        <AgentReadiness v-if="needsReadiness" :harness="node.effectiveConfig?.harness ?? ''"
            :task-key="taskKey" @ready="readinessConfirmed = $event" />
        <label v-if="node.effectiveConfig?.requiresMeteredApproval" class="field">
            <span>Approve API launch: {{ node.effectiveConfig.harness }} /
                {{ node.effectiveConfig.model ?? 'default model' }}. Enter a reason.</span>
            <input v-model="reason" maxlength="1000" :disabled="busy" />
        </label>
        <button class="sm primary" :disabled="busy || !launchReady || (node.effectiveConfig?.requiresMeteredApproval && !reason.trim())"
            @click="run">
            {{ node.effectiveConfig?.requiresMeteredApproval ? 'Approve API launch' : node.state === 'FAILED' ? 'Retry' : 'Run' }}
        </button>
        <p v-if="error" class="alert error small">{{ error }}</p>
    </div>
</template>

<style scoped>
.launch { max-width: 100%; }
</style>
