<script lang="ts" setup>
import { computed, onMounted, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { useTaskStore } from '@/stores/tasks';
import { useOperationStore } from '@/stores/operations';
import Modal from '@/components/Modal.vue';
import type { DeleteTaskPreview } from '@/api/types';

const props = defineProps<{ taskKey: string }>();
/** `deleted` fires once the delete has started; its outcome arrives as a toast. */
const emit = defineEmits<{ close: []; deleted: [] }>();
const store = useTaskStore();
const operations = useOperationStore();
const router = useRouter();

const preview = ref<DeleteTaskPreview | null>(null);
const previewError = ref('');
const busy = ref(false);
const removeWorktree = ref(true);
const removeBranch = ref(false);
const removeTaskFolder = ref(true);
const typedKey = ref('');

const MAX_LISTED = 8;

const load = async (): Promise<void> => {
    busy.value = true;
    previewError.value = '';
    preview.value = null;
    try {
        preview.value = await store.deleteTaskPreview(props.taskKey);
        removeWorktree.value = preview.value.worktree.exists || preview.value.workerWorktrees.length > 0;
        removeBranch.value = false;
        removeTaskFolder.value = preview.value.taskFolder.exists;
    } catch (cause) {
        previewError.value = cause instanceof Error ? cause.message : 'Could not check what would be deleted.';
    } finally {
        busy.value = false;
    }
};
onMounted(load);

// The branches are checked out in the worktrees (the task's and its workers'), so they can only go with them.
const branchBlocked = computed(() => !removeWorktree.value
    && (!!preview.value?.worktree.exists || !!preview.value?.workerWorktrees.length));
watch(branchBlocked, (blocked) => { if (blocked) removeBranch.value = false; });

const branchLoses = computed(() => {
    const branch = preview.value?.branch;
    if (!branch?.exists) return '';
    const parts: string[] = [];
    if (branch.unmergedCommits) parts.push(`${branch.unmergedCommits} commit(s) not on the base branch`);
    if (branch.upstream == null) parts.push('never pushed');
    else if (branch.unpushedCommits) parts.push(`${branch.unpushedCommits} commit(s) not pushed to ${branch.upstream}`);
    return parts.join('; ');
});

const blocked = computed(() => !!preview.value?.unknownCostRunsToday);
const canDelete = computed(() => !!preview.value && !blocked.value && !busy.value
    && !operations.runningFor(props.taskKey)
    && typedKey.value.trim().toUpperCase() === props.taskKey.toUpperCase());

/** Removing the worktree can take minutes: the dialog closes and a toast follows the delete. */
const submit = (): void => {
    if (!canDelete.value || !preview.value) return;
    const key = props.taskKey;
    const options = {
        removeWorktree: removeWorktree.value,
        removeBranch: removeBranch.value,
        removeTaskFolder: removeTaskFolder.value,
        expectedSha: preview.value.branch.sha,
    };
    operations.start({
        kind: 'delete',
        jiraKey: key,
        title: `Deleting ${key}`,
        call: (operationId) => store.deleteTask(key, options, operationId),
        success: () => ({ title: `${key} deleted` }),
        // Disk steps run first, so a failure leaves the task in place to retry.
        failure: (cause) => ({
            title: `Could not delete ${key}`,
            message: cause instanceof Error ? cause.message : 'Delete failed.',
            actions: [{ label: 'Open task', run: () => void router.push({ name: 'task', params: { jiraKey: key } }) }],
        }),
    });
    emit('deleted');
};
</script>

<template>
    <Modal :title="`Delete ${taskKey}`" @close="emit('close')">
        <form class="delete-task" @submit.prevent="submit">
            <p>
                Permanently delete this task, its workflow history and findings. This cannot be undone.
                Its API spend stays in today's budget.
            </p>
            <p v-if="busy && !preview" class="muted">Checking what exists…</p>
            <template v-if="previewError">
                <div class="alert error small" role="alert">{{ previewError }}</div>
                <button type="button" class="sm" :disabled="busy" @click="load">Retry check</button>
            </template>
            <template v-if="preview">
                <div v-if="blocked" class="alert error small" role="alert">
                    {{ preview.unknownCostRunsToday }} of this task's runs today have no known cost, and the daily
                    budget needs them. Import their usage from the task page first.
                </div>
                <p v-if="preview.sessionWillBeClosed" class="small">
                    The task's terminal session will be closed.
                </p>

                <label class="check">
                    <input v-model="removeWorktree" type="checkbox"
                           :disabled="!preview.worktree.exists && !preview.workerWorktrees.length" />
                    <span>
                        Remove the worktree
                        <span class="mono small">{{ preview.worktree.path }}</span>
                        <span v-if="!preview.worktree.exists" class="muted small"> (already gone)</span>
                        <span v-if="preview.workerWorktrees.length" class="small">
                            and {{ preview.workerWorktrees.length }} worker worktree(s)
                        </span>
                    </span>
                </label>
                <p v-if="removeWorktree && preview.worktree.dirtyPaths.length" class="warn-text small">
                    <strong>{{ preview.worktree.dirtyPaths.length }} modified/untracked file(s) will be discarded:</strong>
                    {{ preview.worktree.dirtyPaths.slice(0, MAX_LISTED).join(', ') }}
                    <span v-if="preview.worktree.dirtyPaths.length > MAX_LISTED">
                        , +{{ preview.worktree.dirtyPaths.length - MAX_LISTED }} more
                    </span>
                </p>
                <p v-if="preview.worktree.error" class="warn-text small">{{ preview.worktree.error }}</p>
                <p v-if="preview.worktree.exists && !removeWorktree" class="muted small">
                    The worktree stays on disk. Starting this ticket again needs that folder removed first.
                </p>

                <label class="check">
                    <input v-model="removeBranch" type="checkbox"
                           :disabled="!preview.branch.exists || branchBlocked" />
                    <span>
                        Delete the branch <span class="mono small">{{ preview.branch.name }}</span>
                        <span v-if="preview.workerBranches.length" class="small">
                            and {{ preview.workerBranches.length }} worker branch(es)
                        </span>
                        <span v-if="preview.branch.exists === false" class="muted small"> (already gone)</span>
                        <span v-else-if="branchBlocked" class="muted small"> (needs the worktree removed)</span>
                    </span>
                </label>
                <p v-if="branchLoses" class="small" :class="removeBranch ? 'error-text' : 'muted'">
                    <strong v-if="removeBranch">This work would be lost:</strong>
                    <template v-else>Branch has:</template>
                    {{ branchLoses }}.
                </p>
                <p v-if="preview.branch.error" class="warn-text small">{{ preview.branch.error }}</p>

                <label class="check">
                    <input v-model="removeTaskFolder" type="checkbox" :disabled="!preview.taskFolder.exists" />
                    <span>
                        Delete the task folder (requirements, run logs, findings)
                        <span class="mono small">{{ preview.taskFolder.path }}</span>
                    </span>
                </label>
                <p v-if="preview.taskFolder.exists && !removeTaskFolder" class="muted small">
                    It will be renamed to <span class="mono">{{ taskKey }}.deleted-&lt;time&gt;</span> so the ticket
                    can start again from an empty folder.
                </p>

                <label class="field">
                    <span>Type <strong>{{ taskKey }}</strong> to confirm</span>
                    <input v-model="typedKey" autocomplete="off" :disabled="busy || blocked" />
                </label>
            </template>
            <div class="actions">
                <button type="button" class="sm" :disabled="busy" @click="emit('close')">Keep task</button>
                <button type="submit" class="sm danger solid" :disabled="!canDelete">
                    Delete permanently
                </button>
            </div>
        </form>
    </Modal>
</template>

<style scoped>
.delete-task { display: flex; flex-direction: column; gap: 0.6rem; max-width: 40rem; }
.check { display: flex; gap: 0.5rem; align-items: flex-start; }
.check input { margin-top: 0.2rem; }
.mono { word-break: break-all; }
.warn-text { color: var(--warn); }
.error-text { color: var(--danger); }
.actions { display: flex; gap: 0.5rem; justify-content: flex-end; }
</style>
