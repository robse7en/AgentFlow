<script lang="ts" setup>
import { computed, ref } from 'vue';
import { useTaskStore } from '@/stores/tasks';
import RunAgentButton from '@/components/RunAgentButton.vue';
import AgentHeartbeat from '@/components/AgentHeartbeat.vue';
import type { TaskSnapshot, WorkflowNodeDto } from '@/api/types';

const props = defineProps<{ task: TaskSnapshot }>();
const store = useTaskStore();
const workers = computed(() => props.task.nodes.filter((node) => node.worker));
const error = ref<string | null>(null);
const busy = ref<string | null>(null);
const action = async (node: WorkflowNodeDto, kind: 'retry' | 'resume' | 'restart' | 'terminal') => {
    busy.value = node.key;
    error.value = null;
    try {
        if (kind === 'terminal') {
            await store.openZellij(props.task.jiraKey, node.key);
        } else if (kind === 'retry') {
            await store.runNode(props.task.jiraKey, node.key);
        } else {
            await store.recoverNode(props.task.jiraKey, node.key, kind === 'restart');
        }
    } catch (cause) {
        error.value = cause instanceof Error ? cause.message : String(cause);
    } finally {
        busy.value = null;
    }
};
const hasTerminal = (node: WorkflowNodeDto): boolean => props.task.sessions.some(
    (session) => session.nodeKey === node.key && session.status === 'running' && !!session.paneId,
);
</script>

<template>
    <section v-if="workers.length" class="workers" aria-label="Implementer workers">
        <h3>Implementer workers</h3>
        <p class="muted small">Up to three workers · shared Implementer settings · separate contexts</p>
        <p v-if="error" class="alert error">{{ error }}</p>
        <details v-for="node in workers" :key="node.key" class="worker">
            <summary>
                {{ node.label }} <span class="badge" :class="node.state">{{ node.state }}</span>
            </summary>
            <AgentHeartbeat :node="node" />
            <p class="brief">{{ node.description }}</p>
            <p class="small">Owns: {{ node.worker?.ownedPaths.join(', ') }}</p>
            <p class="small">Depends on: {{ node.worker?.dependencies.join(', ') || 'None' }}</p>
            <ul><li v-for="criterion in node.worker?.acceptanceCriteria" :key="criterion">
                {{ criterion }}
            </li></ul>
            <p class="small path">Worktree: {{ node.worker?.worktreePath }}</p>
            <p v-if="node.message" class="small">{{ node.message }}</p>
            <p v-if="node.error" class="alert error small">{{ node.error }}</p>
            <div class="actions">
                <button v-if="hasTerminal(node)" :disabled="busy !== null"
                    @click="action(node, 'terminal')">Show Zellij terminal</button>
                <RunAgentButton :task-key="task.jiraKey" :node="node" />
                <template v-if="node.state === 'INTERRUPTED'">
                    <button v-if="node.resumable !== false" :disabled="busy !== null" @click="action(node, 'resume')">
                        Resume existing agent
                    </button>
                    <button :disabled="busy !== null" @click="action(node, 'restart')">
                        Stop &amp; retry
                    </button>
                </template>
            </div>
            <p v-if="node.checkpointSha" class="small path">Checkpoint: {{ node.checkpointSha }}</p>
        </details>
    </section>
</template>

<style scoped>
.workers { display: grid; gap: 0.6rem; }
.worker { border: 1px solid var(--border-strong); border-radius: 6px; padding: 0.75rem; }
summary { cursor: pointer; }
.brief { white-space: pre-wrap; }
.path { overflow-wrap: anywhere; }
.actions { display: flex; flex-wrap: wrap; gap: 0.5rem; }
</style>
