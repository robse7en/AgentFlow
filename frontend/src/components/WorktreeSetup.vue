<script lang="ts" setup>
// Preparation of the task's worktrees from the repository's `workspace.worktreeSetup`: agents after
// the Architect wait for it, and a failure waits for an explicit retry.
import { computed, ref } from 'vue';
import { useTaskStore } from '@/stores/tasks';
import type { TaskSnapshot, WorktreeSetupStatus } from '@/api/types';

const props = defineProps<{ task: TaskSnapshot }>();
const store = useTaskStore();
const busy = ref(false);
const error = ref<string | null>(null);

const BADGES: Record<WorktreeSetupStatus['state'], string> = { running: 'RUNNING', succeeded: 'COMPLETED', failed: 'FAILED' };
const setups = computed(() => props.task.worktreeSetup ?? []);
const failed = computed(() => setups.value.some((s) => s.state === 'failed'));
const running = computed(() => setups.value.some((s) => s.state === 'running'));
const summary = computed(() => failed.value ? 'failed' : running.value ? 'running' : 'ready');

const label = (setup: WorktreeSetupStatus): string => {
    if (setup.name === 'task') return 'Task worktree';
    const worker = props.task.nodes.find((n) => `worker-${n.worker?.uid}` === setup.name);
    return worker ? `Worker: ${worker.label}` : setup.name;
};

async function retry() {
    busy.value = true;
    error.value = null;
    try {
        await store.retryWorktreeSetup(props.task.jiraKey);
    } catch (e) {
        error.value = e instanceof Error ? e.message : 'Retry failed.';
    } finally {
        busy.value = false;
    }
}
</script>

<template>
    <details class="panel" :open="failed || running">
        <summary>Worktree setup: {{ summary }}</summary>
        <div v-if="error" class="alert error small">{{ error }}</div>
        <p v-if="running" class="small muted">Agents after the Architect start once their worktree is ready.</p>
        <div v-for="setup in setups" :key="setup.name">
            <p>
                <span class="badge" :class="BADGES[setup.state]">{{ setup.state.toUpperCase() }}</span>
                {{ label(setup) }}
            </p>
            <p v-if="setup.error" class="small">{{ setup.error }}</p>
            <p class="small muted">Log: <code>{{ setup.logPath }}</code></p>
        </div>
        <button v-if="failed" class="sm" :disabled="busy" @click="retry">Retry setup</button>
    </details>
</template>
