<script lang="ts" setup>
import { computed, onMounted, onServerPrefetch, ref, watch } from 'vue';
import { instructionApi, instructionRoles } from '@/api/instructions';
import Modal from './Modal.vue';
import InstructionRunHistory from './InstructionRunHistory.vue';
import type { HistoricalInstructions, InstructionPreview, InstructionRole } from '@/api/instructions';
import type { Project, TaskSnapshot } from '@/api/types';

const props = defineProps<{ project?: Project; task?: TaskSnapshot }>();
const emit = defineEmits<{ close: [] }>();
const mode = ref<'source' | 'snapshot' | 'history'>(props.task ? 'snapshot' : 'source');
const nodeKey = ref('architect');
const projectRole = ref<InstructionRole>('architect');
const data = ref<InstructionPreview | null>(null);
const historical = ref<HistoricalInstructions | null>(null);
const runId = ref<number | null>(null);
const draft = ref('');
const busy = ref(false);
const error = ref<string | null>(null);
const notice = ref<string | null>(null);
const fresh = ref(true);
const base = computed(() => props.task
    ? `/api/tasks/${encodeURIComponent(props.task.jiraKey)}`
    : `/api/projects/${props.project?.id}`);
const agentNodes = computed(() => props.task?.nodes.filter((node) => node.role) ?? []);
const historyRuns = computed(() => data.value?.runs
    ?? props.task?.executionEvidence?.map((run) => ({ id: run.runId, nodeKey: run.nodeKey }))
    ?? props.task?.nodes.flatMap((node) => node.lastRun
        ? [{ id: node.lastRun.id, nodeKey: node.key }] : [])
    ?? []);
const role = computed(() => props.task
    ? (agentNodes.value.find((node) => node.key === nodeKey.value)?.role ?? 'architect') as InstructionRole
    : projectRole.value);
const source = computed(() => data.value?.files?.[role.value]);
const changed = computed(() => draft.value !== (source.value?.content ?? ''));
const path = computed(() => source.value?.path
    ?? `.agentflow-instructions/${role.value.replaceAll('_', '-')}.md`);
const uncommitted = computed(() => data.value?.dirtyPaths.some((entry) => {
    // Porcelain v1 has two status columns and a space before the path.
    const paths = /[RC]/.test(entry.slice(0, 2))
        ? entry.slice(3).split(' -> ') : [entry.slice(3)];
    return paths.some((value) => value.replace(/^"|"$/g, '').replaceAll('\\', '/')
        .toLowerCase().startsWith('.agentflow-instructions/'));
}) ?? false);

const load = async () => {
    busy.value = true;
    error.value = null;
    try {
        historical.value = null;
        if (mode.value === 'history') {
            if (runId.value !== null) {
                historical.value = await instructionApi.history(base.value, runId.value);
            }
        } else {
            // A failed selection/reload must not leave another role's draft saveable here.
            data.value = null;
            draft.value = '';
            data.value = await instructionApi.load(
                base.value, nodeKey.value, mode.value === 'source' ? 'source' : 'snapshot',
            );
            draft.value = data.value.files?.[role.value]?.content ?? '';
        }
    } catch (cause) {
        error.value = (cause as Error).message;
    } finally {
        busy.value = false;
    }
};
watch([base, mode, nodeKey, projectRole, runId], load);
onMounted(load);
onServerPrefetch(load);

const save = async () => {
    busy.value = true;
    error.value = null;
    try {
        await instructionApi.save(base.value, role.value, draft.value, source.value?.hash ?? null);
        notice.value = 'Saved to the repository working copy. No commit was created.';
        await load();
    } catch (cause) {
        error.value = (cause as Error).message;
    } finally {
        busy.value = false;
    }
};
const apply = async () => {
    if (!data.value) return;
    busy.value = true;
    error.value = null;
    try {
        const updated = await instructionApi.apply(base.value, data.value, fresh.value);
        notice.value = `Applied version ${updated.version} to future runs. Running agents continue unchanged.`;
        await load();
    } catch (cause) {
        error.value = (cause as Error).message;
    } finally {
        busy.value = false;
    }
};
</script>

<template>
    <Modal title="Repository agent instructions" @close="emit('close')">
        <div class="instructions">
            <p class="muted small">Shared role guidance for any harness. Markdown is treated as text.</p>
            <div v-if="error" class="alert error" role="alert">{{ error }}</div>
            <div v-if="notice" class="alert info" role="status">{{ notice }}</div>
            <label v-if="task" class="field">
                <span>View</span>
                <select v-model="mode" :disabled="busy || changed">
                    <option value="source">Edit source / preview saved source</option>
                    <option value="snapshot">Future run using task snapshot</option>
                    <option value="history">Historical run (read only)</option>
                </select>
            </label>
            <label v-if="mode !== 'history'" class="field">
                <span>Role / workflow stage</span>
                <select v-if="task" v-model="nodeKey" :disabled="busy || changed">
                    <option v-for="node in agentNodes" :key="node.key" :value="node.key">
                        {{ node.label }} ({{ node.role }})
                    </option>
                </select>
                <select v-else v-model="projectRole" :disabled="busy || changed">
                    <option v-for="item in instructionRoles" :key="item" :value="item">{{ item }}</option>
                </select>
            </label>
            <template v-if="mode === 'history'">
                <label class="field">
                    <span>Run</span>
                    <select v-model="runId" :disabled="busy">
                        <option :value="null">Select a run</option>
                        <option v-for="run in historyRuns" :key="run.id" :value="run.id">
                            {{ run.id }} · {{ run.nodeKey }}
                        </option>
                    </select>
                </label>
                <InstructionRunHistory v-if="historical" :history="historical"
                    :repository="task?.worktreePath" />
            </template>
            <template v-else-if="data">
                <p>Target repository: <code>{{ data.repository }}</code></p>
                <p>Instruction path: <code>{{ path }}</code></p>
                <p v-if="data.sourceError" class="alert error">{{ data.sourceError }}</p>
                <p v-if="uncommitted" class="alert warn">Saved instruction edits are uncommitted.</p>
                <p v-else class="muted small">No uncommitted instruction-file changes.</p>
                <p class="muted small">
                    Git worktree creation carries committed files. Saving here does not commit or
                    copy instructions to another worktree. Task editing uses the task worktree.
                </p>
                <template v-if="mode === 'source' && source">
                    <label class="field">
                        <span>Current source (UTF-8, 64 KiB per role; 256 KiB total)</span>
                        <textarea v-model="draft" rows="12" :disabled="busy" spellcheck="false" />
                    </label>
                    <p v-if="source.content === null" class="muted small">Optional file absent. Create a starter explicitly below.</p>
                    <div class="actions">
                        <button v-if="source.content === null" :disabled="busy || changed"
                            @click="draft = `# ${role} guidance\n\nFollow repository conventions within the approved scope.\n`">
                            Draft starter file
                        </button>
                        <button :disabled="busy || (!changed && source.content !== null)" @click="save">
                            Save source
                        </button>
                        <button :disabled="busy" @click="load">Reload source (discard draft)</button>
                    </div>
                </template>
                <template v-if="task">
                    <p>Task instruction version: {{ data.snapshot?.version ?? 'Captured at first agent launch' }}</p>
                    <p v-if="data.snapshot">Captured from {{ data.snapshot.repository }} at {{ data.snapshot.revision }}</p>
                    <p v-if="data.differs === true" class="alert warn">Saved sources differ from the task snapshot.</p>
                    <p v-else-if="data.differs === false" class="muted small">Saved sources match the task snapshot.</p>
                    <details v-if="data.snapshot">
                        <summary>Captured role content (read only)</summary>
                        <pre>{{ data.snapshot.files[role].content ?? '(Optional file absent at capture)' }}</pre>
                    </details>
                    <p class="muted small">
                        Resumed conversations can retain older instructions in their history.
                        Updates leave running agents alone. Use the existing Stop / Retry controls
                        when you choose to end an active run. Parallel batches must finish integration
                        before an update can be applied.
                    </p>
                    <label><input v-model="fresh" type="checkbox" :disabled="busy" />
                        Start fresh conversations on future launches after applying this update
                    </label>
                    <button :disabled="busy || changed || !data.snapshot || !data.files"
                        @click="apply">Apply saved instruction set to future runs</button>
                    <h4>{{ mode === 'source' ? 'Saved-source briefing preview' : 'Effective future briefing' }}</h4>
                    <p class="muted small">Preview uses run ID 0 as a placeholder and the next run directory.
                        Save the draft to refresh this preview. Launch-time workflow state may change it.</p>
                    <pre aria-label="Briefing preview">{{ data.briefing ?? 'Preview unavailable' }}</pre>
                </template>
            </template>
        </div>
    </Modal>
</template>

<style scoped>
.instructions { display: grid; gap: 12px; min-width: 0; }
p { margin: 0; overflow-wrap: anywhere; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 420px; overflow: auto;
    padding: 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 6px; }
textarea { font-family: var(--font-mono, monospace); }
.actions { display: flex; gap: 8px; flex-wrap: wrap; }
</style>
